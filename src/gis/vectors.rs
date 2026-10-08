use super::cache::{Cache, source_record};
use crate::{
    jobs::JobContext,
    map_core::{City, Projection, Region, SourceRecord},
};
use anyhow::{Context, Result, ensure};
use gdal::{
    Dataset,
    vector::{Geometry, LayerAccess},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::Path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiverReach {
    pub id: u64,
    pub next_down: u64,
    pub discharge: f64,
    pub stream_order: u32,
    pub points_m: Vec<[f64; 2]>,
}

pub fn rivers(
    cache: &Cache,
    region: Region,
    projection: &Projection,
    min_discharge: f64,
    context: &JobContext,
) -> Result<(Vec<RiverReach>, Vec<SourceRecord>)> {
    let continents = [
        ("eu", [-25., 12., 70., 72.]),
        ("af", [-20., -60., 55., 40.]),
        ("as", [50., -15., 180., 60.]),
        ("au", [90., -60., 180., 15.]),
        ("na", [-180., 5., -20., 60.]),
        ("sa", [-95., -60., -25., 15.]),
        ("si", [50., 55., 180., 85.]),
    ];
    let mut reaches = BTreeMap::new();
    let mut sources = Vec::new();
    for (continent, bounds) in continents {
        if region.east < bounds[0]
            || region.west > bounds[2]
            || region.north < bounds[1]
            || region.south > bounds[3]
        {
            continue;
        }
        let filename = format!("HydroRIVERS_v10_{continent}_shp.zip");
        let url = format!("https://data.hydrosheds.org/file/HydroRIVERS/{filename}");
        let (archive, etag) = cache.download(&url, &filename, context, 0.5)?;
        let directory = cache.root.join(format!("rivers-{continent}"));
        let shapefile = extract_shape(&archive, &directory, context)?;
        context.report(0.55, format!("Reading HydroRIVERS ({continent})"))?;
        let dataset = Dataset::open(&shapefile)?;
        let mut layer = dataset.layer(0)?;
        layer.set_spatial_filter_rect(region.west, region.south, region.east, region.north);
        let id_index = layer.defn().field_index("HYRIV_ID")?;
        let down_index = layer.defn().field_index("NEXT_DOWN")?;
        let discharge_index = layer.defn().field_index("DIS_AV_CMS")?;
        let order_index = layer.defn().field_index("ORD_STRA")?;
        for (i, feature) in layer.features().enumerate() {
            if i % 128 == 0 {
                context.check()?;
            }
            let discharge = feature
                .field_as_double(discharge_index)?
                .context("River has no discharge value")?;
            if discharge < min_discharge {
                continue;
            }
            let id = feature
                .field_as_integer64(id_index)?
                .context("River has no ID")? as u64;
            let next_down = feature.field_as_integer64(down_index)?.unwrap_or(0) as u64;
            let stream_order = feature.field_as_integer(order_index)?.unwrap_or(0) as u32;
            let geometry = feature.geometry().context("River has no geometry")?;
            let mut parts = Vec::new();
            line_parts(geometry, &mut parts);
            for (part, mut points_m) in parts.into_iter().enumerate() {
                projection.project(&mut points_m)?;
                reaches.entry((id, part)).or_insert(RiverReach {
                    id,
                    next_down,
                    discharge,
                    stream_order,
                    points_m,
                });
            }
        }
        sources.push(source_record("HydroRIVERS", "v1.0", &url, "https://data.hydrosheds.org/file/technical-documentation/HydroSHEDS_TechDoc_v1_4.pdf", "HydroRIVERS: Lehner, B., Grill G. (2013). Global river hydrography and network routing. Hydrological Processes 27(15), 2171–2186. Data from https://www.hydrosheds.org. HydroSHEDS version 1 license applies; raw data must not be redistributed as a stand-alone product, and distribution of derived data must satisfy its end-user terms.", etag, &archive)?);
    }
    ensure!(
        !sources.is_empty(),
        "No HydroRIVERS coverage for this region"
    );
    Ok((reaches.into_values().collect(), sources))
}

fn line_parts(geometry: &Geometry, result: &mut Vec<Vec<[f64; 2]>>) {
    if geometry.point_count() > 1 {
        result.push(
            (0..geometry.point_count())
                .map(|i| {
                    let (x, y, _) = geometry.get_point(i as i32);
                    [x, y]
                })
                .collect(),
        );
    } else {
        for i in 0..geometry.geometry_count() {
            line_parts(&geometry.get_geometry(i), result);
        }
    }
}

fn extract_shape(
    archive_path: &Path,
    directory: &Path,
    context: &JobContext,
) -> Result<std::path::PathBuf> {
    fs::create_dir_all(directory)?;
    let mut archive = zip::ZipArchive::new(File::open(archive_path)?)?;
    let mut shapefile = None;
    for i in 0..archive.len() {
        context.check()?;
        let mut entry = archive.by_index(i)?;
        let path = entry
            .enclosed_name()
            .context("Unsafe path in river archive")?;
        if entry.is_dir() {
            continue;
        }
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !["shp", "shx", "dbf", "prj", "cpg"].contains(&extension) {
            continue;
        }
        let destination = directory.join(path.file_name().context("Missing archive filename")?);
        if !destination.is_file() || fs::metadata(&destination)?.len() != entry.size() {
            ensure!(
                entry.size() <= 2_000_000_000,
                "Uncompressed river file exceeds limit"
            );
            let mut temp = tempfile::NamedTempFile::new_in(directory)?;
            let mut buffer = [0_u8; 65536];
            use std::io::{Read, Write};
            loop {
                context.check()?;
                let count = entry.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                temp.write_all(&buffer[..count])?;
            }
            temp.persist(&destination)?;
        }
        if extension == "shp" {
            shapefile = Some(destination);
        }
    }
    shapefile.context("HydroRIVERS archive contained no shapefile")
}

pub fn cities(
    cache: &Cache,
    region: Region,
    minimum_population: u64,
    context: &JobContext,
) -> Result<(Vec<City>, SourceRecord)> {
    let url = "https://download.geonames.org/export/dump/cities1000.zip";
    let (path, etag) = cache.download(url, "cities1000.zip", context, 0.6)?;
    let mut archive = zip::ZipArchive::new(File::open(&path)?)?;
    let entry = archive.by_name("cities1000.txt")?;
    let reader = BufReader::new(entry);
    let mut cities = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        if i % 1024 == 0 {
            context.check()?;
        }
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        ensure!(fields.len() >= 19, "Malformed GeoNames record");
        let lon: f64 = fields[5].parse()?;
        let lat: f64 = fields[4].parse()?;
        let population: u64 = fields[14].parse()?;
        if !region.contains(lon, lat) || population < minimum_population {
            continue;
        }
        cities.push(City {
            id: fields[0].parse()?,
            name: fields[1].into(),
            lon,
            lat,
            population,
        });
    }
    cities.sort_by_key(|c| c.id);
    let source = source_record(
        "GeoNames cities1000",
        "daily snapshot",
        url,
        "https://creativecommons.org/licenses/by/4.0/",
        "Contains modified GeoNames data, https://www.geonames.org, licensed under Creative Commons Attribution 4.0.",
        etag,
        &path,
    )?;
    Ok((cities, source))
}
