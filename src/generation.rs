use crate::i18n::Message;
use crate::{
    gis::{
        cache::Cache,
        raster::{self, RasterKind, RasterLayer},
        vectors::{self, RiverReach},
    },
    jobs::JobContext,
    map_core::*,
};
use anyhow::{Context, Result, ensure};
use geo::Intersects;
use hexx::Hex;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};

/// Acquires all required GIS layers and returns a complete map atomically.
pub fn generate(
    settings: &GenerationSettings,
    cache: &Cache,
    context: &JobContext,
) -> Result<MapDocument> {
    settings.validate()?;
    context.report(0.02, Message::new("progress.validating"))?;
    let projection = Projection::new(settings.region)?;
    let (mut cells, bounds_m) = build_grid(settings, &projection)?;
    let spacing = settings.spacing_km * 1000.;
    let padding_degrees = spacing * 3.
        / 100_000.
        / settings
            .region
            .north
            .abs()
            .max(settings.region.south.abs())
            .to_radians()
            .cos();
    let input_region = settings.region.expanded(padding_degrees);
    ensure!(
        settings.region.west - padding_degrees >= -180.
            && settings.region.east + padding_degrees <= 180.,
        "Region's boundary hexes cross the antimeridian; move the bounds inward"
    );
    let elevation = raster::acquire(
        cache,
        input_region,
        spacing / 8.,
        RasterKind::Elevation,
        context,
    )?;
    let cover = raster::acquire(
        cache,
        input_region,
        spacing / 8.,
        RasterKind::LandCover,
        context,
    )?;
    let (rivers, river_sources) = vectors::rivers(
        cache,
        input_region,
        &projection,
        settings.min_river_discharge,
        context,
    )?;
    let (cities, city_source) = vectors::cities(
        cache,
        settings.region,
        settings.min_city_population,
        context,
    )?;
    context.report(0.7, Message::new("progress.aggregating"))?;
    aggregate_cells(&mut cells, settings, &elevation, &cover, context)?;
    let index: HashMap<_, _> = cells.iter().enumerate().map(|(i, c)| (c.hex, i)).collect();
    context.report(0.85, Message::new("progress.rivers"))?;
    rasterize_rivers(&mut cells, &index, &rivers, spacing, bounds_m, context)?;
    context.report(0.92, Message::new("progress.cities"))?;
    assign_cities(&mut cells, &index, cities, &projection, spacing)?;
    let mut sources = elevation.sources();
    sources.extend(cover.sources());
    sources.extend(river_sources);
    sources.push(city_source);
    let river_network: BTreeMap<_, _> = rivers
        .iter()
        .map(|r| {
            (
                r.id,
                RiverRecord {
                    id: r.id,
                    next_down: r.next_down,
                    discharge: r.discharge,
                    stream_order: r.stream_order,
                },
            )
        })
        .collect();
    let height_field = source_height_field(
        &cells,
        spacing,
        settings.region,
        &elevation,
        &cover,
        context,
    )?;
    let mut document = MapDocument {
        schema_version: 2,
        generator_version: "gis-hex-v3".into(),
        settings: settings.clone(),
        projection_wkt: projection.wkt,
        bounds_m,
        cells,
        sources,
        river_network: river_network.into_values().collect(),
        height_field,
        river_paths: rivers,
        heights: HeightSettings::default(),
        index,
    };
    document.rebuild_index()?;
    context.report(
        1.,
        Message::new("progress.generated").arg("count", document.cells.len()),
    )?;
    Ok(document)
}

fn source_height_field(
    cells: &[Cell],
    spacing: f64,
    region: Region,
    elevation: &RasterLayer,
    cover: &RasterLayer,
    context: &JobContext,
) -> Result<HeightField> {
    context.report(0.93, Message::new("progress.sampling"))?;
    let step = spacing / 4.;
    let radius = spacing / 3_f64.sqrt();
    let min_x = cells
        .iter()
        .map(|c| c.center_m[0] - radius - step)
        .fold(f64::INFINITY, f64::min);
    let min_y = cells
        .iter()
        .map(|c| c.center_m[1] - radius - step)
        .fold(f64::INFINITY, f64::min);
    let max_x = cells
        .iter()
        .map(|c| c.center_m[0] + radius + step)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = cells
        .iter()
        .map(|c| c.center_m[1] + radius + step)
        .fold(f64::NEG_INFINITY, f64::max);
    let origin_m = [(min_x / step).floor() * step, (min_y / step).floor() * step];
    let width = ((max_x - origin_m[0]) / step).ceil() as usize + 1;
    let height = ((max_y - origin_m[1]) / step).ceil() as usize + 1;
    ensure!(
        width * height <= 2_000_000,
        "Continuous DEM surface exceeds sample limit"
    );
    let mut elevations_m = vec![0.; width * height];
    let mut land_cover = vec![0; width * height];
    // Each task owns its coordinate transform; GDAL handles are never shared.
    let batch_size = width * 8;
    elevations_m
        .par_chunks_mut(batch_size)
        .zip(land_cover.par_chunks_mut(batch_size))
        .enumerate()
        .try_for_each_init(
            || None::<Projection>,
            |projection, (batch, (elevations, classes))| -> Result<()> {
                context.check()?;
                let projection = match projection {
                    Some(projection) => projection,
                    slot @ None => slot.insert(Projection::new(region)?),
                };
                let mut positions: Vec<_> = (batch * batch_size
                    ..batch * batch_size + elevations.len())
                    .map(|i| {
                        [
                            origin_m[0] + (i % width) as f64 * step,
                            origin_m[1] + (i / width) as f64 * step,
                        ]
                    })
                    .collect();
                projection.unproject(&mut positions)?;
                for ((elevation_m, class), [lon, lat]) in
                    elevations.iter_mut().zip(classes).zip(positions)
                {
                    let value = elevation
                        .sample_bilinear(lon, lat)
                        .or_else(|| (cover.sample(lon, lat) == Some(80.)).then_some(0.))
                        .with_context(|| {
                            format!("Missing continuous DEM coverage at {lat:.5}°, {lon:.5}°")
                        })?;
                    *elevation_m = value as f32;
                    *class = cover
                        .sample(lon, lat)
                        .context("Missing surface land cover")? as u8;
                }
                Ok(())
            },
        )?;
    Ok(HeightField {
        origin_m,
        step_m: step,
        width,
        height,
        elevations_m,
        land_cover,
    })
}

fn aggregate_cells(
    cells: &mut [Cell],
    settings: &GenerationSettings,
    elevation: &RasterLayer,
    cover: &RasterLayer,
    context: &JobContext,
) -> Result<()> {
    let spacing = settings.spacing_km * 1000.;
    let radius = spacing / 3_f64.sqrt();
    // A deterministic center and two six-point rings sample each hex's interior.
    let mut offsets = vec![[0., 0.]];
    for scale in [0.4, 0.82] {
        for corner in 0..6 {
            let angle = (30. + f64::from(corner) * 60.).to_radians();
            offsets.push([radius * scale * angle.cos(), radius * scale * angle.sin()]);
        }
    }
    cells.par_chunks_mut(512).try_for_each_init(
        || None::<Projection>,
        |projection, batch| {
            context.check()?;
            let projection = match projection {
                Some(projection) => projection,
                slot @ None => slot.insert(Projection::new(settings.region)?),
            };
            let mut positions: Vec<_> = batch
                .iter()
                .flat_map(|c| {
                    offsets
                        .iter()
                        .map(move |o| [c.center_m[0] + o[0], c.center_m[1] + o[1]])
                })
                .collect();
            projection.unproject(&mut positions)?;
            for (cell, samples) in batch.iter_mut().zip(positions.chunks(offsets.len())) {
                let mut heights = Vec::new();
                let mut trees = 0_u32;
                let mut water = 0_u32;
                for [lon, lat] in samples {
                    let class = cover.sample(*lon, *lat).with_context(|| {
                        format!("Missing required land cover at {lat:.5}°, {lon:.5}°")
                    })? as u8;
                    ensure!(
                        [10, 20, 30, 40, 50, 60, 70, 80, 90, 95, 100].contains(&class),
                        "Unknown WorldCover class {class} at {lat}, {lon}"
                    );
                    if class == 10 || class == 95 {
                        trees += 1;
                    }
                    if class == 80 {
                        water += 1;
                    }
                    let height = match elevation.sample(*lon, *lat) {
                        Some(h) => h,
                        None if class == 80 => 0.,
                        None => {
                            anyhow::bail!("Missing required land elevation at {lat:.5}°, {lon:.5}°")
                        }
                    };
                    heights.push(height);
                }
                let mean = heights.iter().sum::<f64>() / heights.len() as f64;
                let min = heights.iter().copied().fold(f64::INFINITY, f64::min);
                let max = heights.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let relief = max - min;
                let slope = (relief / (radius * 1.64)).atan().to_degrees();
                cell.generated_elevation_m = mean;
                cell.elevation_m = mean;
                cell.relief_m = relief;
                cell.slope_degrees = slope;
                cell.forest_fraction = f64::from(trees) / samples.len() as f64;
                cell.water_fraction = f64::from(water) / samples.len() as f64;
                cell.landscape = if relief >= settings.mountain_relief_m
                    || slope >= settings.mountain_slope_degrees
                {
                    Landscape::Mountain
                } else if cell.forest_fraction >= settings.forest_fraction {
                    Landscape::Forest
                } else {
                    Landscape::Plains
                };
                if cell.water_fraction >= 0.5 {
                    cell.surface = Surface::Water;
                }
            }
            Ok(())
        },
    )
}

pub fn rasterize_rivers(
    cells: &mut [Cell],
    index: &HashMap<Hex, usize>,
    reaches: &[RiverReach],
    spacing: f64,
    bounds: [f64; 4],
    context: &JobContext,
) -> Result<()> {
    for (reach_index, reach) in reaches.iter().enumerate() {
        if reach_index % 128 == 0 {
            context.check()?;
        }
        for segment in reach.points_m.windows(2) {
            let [a, b] = [segment[0], segment[1]];
            let min_x = a[0].min(b[0]).max(bounds[0] - spacing);
            let max_x = a[0].max(b[0]).min(bounds[2] + spacing);
            let min_y = a[1].min(b[1]).max(bounds[1] - spacing);
            let max_y = a[1].max(b[1]).min(bounds[3] + spacing);
            if min_x > max_x || min_y > max_y {
                continue;
            }
            let r0 = point_hex([min_x, min_y], spacing).y - 2;
            let r1 = point_hex([max_x, max_y], spacing).y + 2;
            let line = geo::Line::new(
                geo::Coord { x: a[0], y: a[1] },
                geo::Coord { x: b[0], y: b[1] },
            );
            for r in r0..=r1 {
                let q0 = (min_x / spacing - f64::from(r) / 2.).floor() as i32 - 1;
                let q1 = (max_x / spacing - f64::from(r) / 2.).ceil() as i32 + 1;
                for q in q0..=q1 {
                    let hex = Hex::new(q, r);
                    if let Some(&i) = index.get(&hex)
                        && line.intersects(&hex_polygon(hex, spacing))
                    {
                        let cell = &mut cells[i];
                        if !cell.river_ids.contains(&reach.id) {
                            cell.river_ids.push(reach.id);
                        }
                        // Lakes and coastal water remain open water; river identity is retained.
                        if cell.surface != Surface::Water {
                            cell.surface = Surface::River;
                        }
                    }
                }
            }
        }
    }
    for cell in cells {
        cell.river_ids.sort_unstable();
    }
    Ok(())
}

pub fn assign_cities(
    cells: &mut [Cell],
    index: &HashMap<Hex, usize>,
    cities: Vec<City>,
    projection: &Projection,
    spacing: f64,
) -> Result<()> {
    let mut positions: Vec<_> = cities.iter().map(|c| [c.lon, c.lat]).collect();
    projection.project(&mut positions)?;
    for (city, position) in cities.into_iter().zip(positions) {
        let hex = point_hex(position, spacing);
        let i = *index
            .get(&hex)
            .with_context(|| format!("City {} lies outside generated coverage", city.name))?;
        cells[i].cities.push(city);
    }
    for cell in cells {
        cell.cities.sort_by_key(|c| c.id);
        if !cell.cities.is_empty() {
            cell.urban = Some(UrbanTerrain {
                population: cell.cities.iter().map(|c| c.population).sum(),
                style: UrbanStyle::Mixed,
            });
            if cell.surface == Surface::Land {
                cell.surface = Surface::City;
            }
        }
    }
    Ok(())
}

pub fn summary(document: &MapDocument) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for cell in &document.cells {
        *counts.entry(format!("{:?}", cell.landscape)).or_insert(0) += 1;
        if cell.surface != Surface::Land {
            *counts.entry(format!("{:?}", cell.surface)).or_insert(0) += 1;
        }
        *counts.entry("Cities".into()).or_insert(0) += cell.cities.len();
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gis::raster::RasterWindow;
    use std::collections::HashSet;

    #[test]
    fn parallel_sampling_preserves_cells_and_surface_and_propagates_errors() -> Result<()> {
        let settings = GenerationSettings::default();
        let projection = Projection::new(settings.region)?;
        let (cells, _) = build_grid(&settings, &projection)?;
        assert!(cells.len() > 512);
        let layer = |values: Vec<f32>| RasterLayer {
            windows: vec![RasterWindow {
                bounds: [7., 45.5, 9.2, 47.8],
                width: 64,
                height: 64,
                values,
                nodata: None,
                source: SourceRecord {
                    name: "sampling fixture".into(),
                    version: "1".into(),
                    url: String::new(),
                    license: String::new(),
                    attribution: String::new(),
                    acquired_unix: 0,
                    etag: None,
                    sha256: String::new(),
                },
            }],
        };
        let elevation = layer((0..64 * 64).map(|i| (i % 31) as f32 * 100.).collect());
        let cover = layer((0..64 * 64).map(|i| [10., 30., 80.][i % 3]).collect());
        let sample = || -> Result<_> {
            let mut cells = cells.clone();
            let context = JobContext::default();
            aggregate_cells(&mut cells, &settings, &elevation, &cover, &context)?;
            let field = source_height_field(
                &cells,
                settings.spacing_km * 1000.,
                settings.region,
                &elevation,
                &cover,
                &context,
            )?;
            Ok((cells, field))
        };
        let serial = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()?
            .install(sample)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let parallel = pool.install(sample)?;
        assert_eq!(serial.0, parallel.0);
        assert_eq!(serial.1.origin_m, parallel.1.origin_m);
        assert_eq!(serial.1.width, parallel.1.width);
        assert_eq!(serial.1.height, parallel.1.height);
        assert_eq!(serial.1.elevations_m, parallel.1.elevations_m);
        assert_eq!(serial.1.land_cover, parallel.1.land_cover);
        let mut cells = cells;
        let context = JobContext::default();
        let missing = RasterLayer { windows: vec![] };
        assert!(
            pool.install(|| aggregate_cells(&mut cells, &settings, &missing, &cover, &context))
                .is_err()
        );
        context.cancel();
        assert!(
            pool.install(|| aggregate_cells(&mut cells, &settings, &elevation, &cover, &context))
                .is_err()
        );
        assert!(
            pool.install(|| source_height_field(
                &cells,
                settings.spacing_km * 1000.,
                settings.region,
                &elevation,
                &cover,
                &context
            ))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn river_supercover_is_connected_across_boundaries_and_junctions() -> Result<()> {
        let settings = GenerationSettings::default();
        let projection = Projection::new(settings.region)?;
        let (mut cells, bounds) = build_grid(&settings, &projection)?;
        let index = cells.iter().enumerate().map(|(i, c)| (c.hex, i)).collect();
        let river = RiverReach {
            id: 1,
            next_down: 0,
            discharge: 100.,
            stream_order: 5,
            points_m: vec![[-20_000., -20_000.], [0., 0.], [20_000., 20_000.]],
        };
        rasterize_rivers(
            &mut cells,
            &index,
            &[river],
            2000.,
            bounds,
            &JobContext::default(),
        )?;
        let water: HashSet<_> = cells
            .iter()
            .filter(|c| c.surface == Surface::River)
            .map(|c| c.hex)
            .collect();
        assert!(water.len() > 10);
        let mut seen = HashSet::new();
        let mut pending = vec![*water.iter().next().unwrap()];
        while let Some(h) = pending.pop() {
            if !seen.insert(h) {
                continue;
            }
            pending.extend(
                h.all_neighbors()
                    .into_iter()
                    .filter(|n| water.contains(n) && !seen.contains(n)),
            );
        }
        assert_eq!(water.len(), seen.len());
        Ok(())
    }

    #[test]
    fn cancellation_prevents_generation_and_cache_publication() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let cache = Cache::new(dir.path().into())?;
        let context = JobContext::default();
        context.cancel();
        assert!(generate(&GenerationSettings::default(), &cache, &context).is_err());
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
        Ok(())
    }
}
