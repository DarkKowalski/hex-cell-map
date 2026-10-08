use super::cache::{Cache, source_record};
use crate::{
    jobs::JobContext,
    map_core::{Region, SourceRecord},
};
use anyhow::{Context, Result, ensure};
use gdal::{Dataset, raster::ResampleAlg};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs};

#[derive(Clone, Copy)]
pub enum RasterKind {
    Elevation,
    LandCover,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RasterWindow {
    pub bounds: [f64; 4],
    pub width: usize,
    pub height: usize,
    pub values: Vec<f32>,
    pub nodata: Option<f32>,
    pub source: SourceRecord,
}

impl RasterWindow {
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f64> {
        if lon < self.bounds[0]
            || lon >= self.bounds[2]
            || lat <= self.bounds[1]
            || lat > self.bounds[3]
        {
            return None;
        }
        let x = ((lon - self.bounds[0]) / (self.bounds[2] - self.bounds[0]) * self.width as f64)
            .floor() as usize;
        let y = ((self.bounds[3] - lat) / (self.bounds[3] - self.bounds[1]) * self.height as f64)
            .floor() as usize;
        let value = *self
            .values
            .get(y.min(self.height - 1) * self.width + x.min(self.width - 1))?;
        if !value.is_finite() || self.nodata.is_some_and(|n| (n - value).abs() < 1e-5) {
            None
        } else {
            Some(f64::from(value))
        }
    }
}

pub struct RasterLayer {
    pub windows: Vec<RasterWindow>,
}

impl RasterLayer {
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f64> {
        self.windows.iter().find_map(|w| w.sample(lon, lat))
    }
    pub fn sources(&self) -> Vec<SourceRecord> {
        self.windows.iter().map(|w| w.source.clone()).collect()
    }
}

pub fn acquire(
    cache: &Cache,
    region: Region,
    resolution_m: f64,
    kind: RasterKind,
    context: &JobContext,
) -> Result<RasterLayer> {
    super::initialize()?;
    let (tile_degrees, base_progress, label) = match kind {
        RasterKind::Elevation => (1_i32, 0.15, "elevation"),
        RasterKind::LandCover => (3, 0.3, "land cover"),
    };
    let dem_index = if matches!(kind, RasterKind::Elevation) {
        let (path, _) = cache.download(
            "https://copernicus-dem-90m.s3.amazonaws.com/tileList.txt",
            "dem-90-tile-list.txt",
            context,
            0.1,
        )?;
        Some(
            fs::read_to_string(path)?
                .lines()
                .map(|l| l.trim().trim_end_matches('/').to_owned())
                .collect::<HashSet<_>>(),
        )
    } else {
        None
    };
    let west = (region.west.floor() as i32).div_euclid(tile_degrees) * tile_degrees;
    let south = (region.south.floor() as i32).div_euclid(tile_degrees) * tile_degrees;
    let mut windows = Vec::new();
    for lat in (south..region.north.ceil() as i32).step_by(tile_degrees as usize) {
        for lon in (west..region.east.ceil() as i32).step_by(tile_degrees as usize) {
            context.check()?;
            let tile = tile_name(lon, lat, kind);
            if let Some(index) = &dem_index {
                if !index.contains(&tile) {
                    continue;
                }
            }
            let url = match kind {
                RasterKind::Elevation => {
                    format!("https://copernicus-dem-90m.s3.amazonaws.com/{tile}/{tile}.tif")
                }
                RasterKind::LandCover => format!(
                    "https://esa-worldcover.s3.eu-central-1.amazonaws.com/v200/2021/map/{tile}.tif"
                ),
            };
            let key = Cache::key(&("raster-window-v1", &url, region, resolution_m))?;
            let filename = format!("raster-{key}.json");
            if let Some(window) = cache.read_json::<RasterWindow>(&filename)? {
                ensure!(
                    window.width > 0
                        && window.height > 0
                        && window.values.len() == window.width * window.height,
                    "Invalid cached raster dimensions"
                );
                windows.push(window);
                continue;
            }
            context.report(base_progress, format!("Reading {label}: {lat}°, {lon}°"))?;
            let dataset = Dataset::open(format!("/vsicurl/{url}"))
                .with_context(|| format!("Open required {label} tile {url}"))?;
            let transform = dataset.geo_transform()?;
            ensure!(
                transform[2] == 0. && transform[4] == 0. && transform[1] > 0. && transform[5] < 0.,
                "Unexpected raster orientation in {url}"
            );
            ensure!(
                dataset.spatial_ref()?.auth_code()? == 4326,
                "Expected WGS84 raster in {url}"
            );
            let band = dataset.rasterband(1)?;
            let (full_width, full_height) = band.size();
            let x0 = ((region.west - transform[0]) / transform[1])
                .floor()
                .max(0.) as usize;
            let x1 = ((region.east - transform[0]) / transform[1])
                .ceil()
                .min(full_width as f64) as usize;
            let y0 = ((region.north - transform[3]) / transform[5])
                .floor()
                .max(0.) as usize;
            let y1 = ((region.south - transform[3]) / transform[5])
                .ceil()
                .min(full_height as f64) as usize;
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let bounds = [
                transform[0] + x0 as f64 * transform[1],
                transform[3] + y1 as f64 * transform[5],
                transform[0] + x1 as f64 * transform[1],
                transform[3] + y0 as f64 * transform[5],
            ];
            let cos_lat = ((bounds[1] + bounds[3]) / 2.).to_radians().cos().max(0.1);
            let width = (((bounds[2] - bounds[0]) * 111_320. * cos_lat / resolution_m).ceil()
                as usize)
                .clamp(1, x1 - x0);
            let height = (((bounds[3] - bounds[1]) * 111_320. / resolution_m).ceil() as usize)
                .clamp(1, y1 - y0);
            ensure!(
                width * height <= 16_000_000,
                "Raster window exceeds memory limit"
            );
            let resampling = match kind {
                RasterKind::Elevation => ResampleAlg::Average,
                RasterKind::LandCover => ResampleAlg::NearestNeighbour,
            };
            // Read strips to provide cancellation opportunities during large COG reads.
            let mut values = Vec::with_capacity(width * height);
            for row in (0..height).step_by(64) {
                context.check()?;
                let rows = (height - row).min(64);
                let source_start = y0 + row * (y1 - y0) / height;
                let source_end = y0 + (row + rows) * (y1 - y0) / height;
                let buffer = band.read_as::<f32>(
                    (x0 as isize, source_start as isize),
                    (x1 - x0, source_end - source_start),
                    (width, rows),
                    Some(resampling),
                )?;
                values.extend_from_slice(buffer.data());
            }
            let nodata = band.no_data_value().map(|v| v as f32);
            let payload_path = cache.root.join(format!("raster-{key}.samples"));
            // The hash covers acquired sample values, not the unread portion of the COG.
            let sample_bytes: Vec<_> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            fs::write(&payload_path, sample_bytes)?;
            let (name, version, license, attribution) = match kind {
                RasterKind::Elevation => (
                    "Copernicus DEM GLO-90",
                    "2021 AWS COG",
                    "https://dataspace.copernicus.eu/sites/default/files/media/files/2025-06/copernicus_contributing_mission_data_access_v2_cop_dem_licenses.pdf",
                    "Produced using Copernicus WorldDEM-90 © DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved. The organisations in charge of the Copernicus programme by law or by delegation do not incur any liability for any use of the Copernicus WorldDEM-90.",
                ),
                RasterKind::LandCover => (
                    "ESA WorldCover",
                    "2021 v200",
                    "https://creativecommons.org/licenses/by/4.0/",
                    "© ESA WorldCover project 2021 / Contains modified Copernicus Sentinel data (2021) processed by ESA WorldCover consortium",
                ),
            };
            let source = source_record(
                name,
                version,
                &url,
                license,
                attribution,
                None,
                &payload_path,
            )?;
            fs::remove_file(payload_path)?;
            let window = RasterWindow {
                bounds,
                width,
                height,
                values,
                nodata,
                source,
            };
            context.check()?;
            cache.write_json(&filename, &window)?;
            windows.push(window);
        }
    }
    Ok(RasterLayer { windows })
}

pub fn tile_name(lon: i32, lat: i32, kind: RasterKind) -> String {
    let north = if lat < 0 { 'S' } else { 'N' };
    let east = if lon < 0 { 'W' } else { 'E' };
    match kind {
        RasterKind::Elevation => format!(
            "Copernicus_DSM_COG_30_{north}{:02}_00_{east}{:03}_00_DEM",
            lat.abs(),
            lon.abs()
        ),
        RasterKind::LandCover => format!(
            "ESA_WorldCover_10m_2021_v200_{north}{:02}{east}{:03}_Map",
            lat.abs(),
            lon.abs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn southern_and_western_tile_names() {
        assert_eq!(
            tile_name(-75, 39, RasterKind::LandCover),
            "ESA_WorldCover_10m_2021_v200_N39W075_Map"
        );
        assert_eq!(
            tile_name(18, -34, RasterKind::Elevation),
            "Copernicus_DSM_COG_30_S34_00_E018_00_DEM"
        );
        assert_eq!((-1_i32).div_euclid(3) * 3, -3);
    }
}
