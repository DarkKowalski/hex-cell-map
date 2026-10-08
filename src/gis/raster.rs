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
    pub fn sample_bilinear(&self, lon: f64, lat: f64) -> Option<f64> {
        let window = self.windows.iter().find(|w| {
            lon >= w.bounds[0] && lon < w.bounds[2] && lat > w.bounds[1] && lat <= w.bounds[3]
        })?;
        let dx = (window.bounds[2] - window.bounds[0]) / window.width as f64;
        let dy = (window.bounds[3] - window.bounds[1]) / window.height as f64;
        let x = (lon - window.bounds[0]) / dx - 0.5;
        let y = (lat - window.bounds[1]) / dy - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let tx = x - x0;
        let ty = y - y0;
        let mut weighted = 0.;
        let mut total = 0.;
        for (sx, wx) in [(x0, 1. - tx), (x0 + 1., tx)] {
            for (sy, wy) in [(y0, 1. - ty), (y0 + 1., ty)] {
                if let Some(value) = self.sample(
                    window.bounds[0] + (sx + 0.5) * dx,
                    window.bounds[1] + (sy + 0.5) * dy,
                ) {
                    weighted += value * wx * wy;
                    total += wx * wy;
                }
            }
        }
        (total > 0.).then(|| weighted / total)
    }
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
            if let Some(index) = &dem_index
                && !index.contains(&tile)
            {
                continue;
            }
            let url = match kind {
                RasterKind::Elevation => {
                    format!("https://copernicus-dem-90m.s3.amazonaws.com/{tile}/{tile}.tif")
                }
                RasterKind::LandCover => format!(
                    "https://esa-worldcover.s3.eu-central-1.amazonaws.com/v200/2021/map/{tile}.tif"
                ),
            };
            let key = Cache::key(&("raster-window-v2", &url, region, resolution_m))?;
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
            // One resampling grid avoids integer-rounded strip boundaries. GDAL's
            // progress callback checks cancellation during the bounded range reads.
            let values = read_cancellable(
                &band,
                (x0, y0),
                (x1 - x0, y1 - y0),
                (width, height),
                resampling,
                context,
            )?;
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

fn read_cancellable(
    band: &gdal::raster::RasterBand<'_>,
    origin: (usize, usize),
    size: (usize, usize),
    shape: (usize, usize),
    resampling: ResampleAlg,
    context: &JobContext,
) -> Result<Vec<f32>> {
    use gdal_sys::{CPLErr, GDALDataType, GDALRWFlag};
    unsafe extern "C" fn progress(
        _: f64,
        _: *const std::ffi::c_char,
        data: *mut std::ffi::c_void,
    ) -> i32 {
        // SAFETY: data points to the borrowed JobContext for the synchronous call.
        let context = unsafe { &*data.cast::<JobContext>() };
        i32::from(context.check().is_ok())
    }
    let mut options = gdal_sys::GDALRasterIOExtraArg {
        nVersion: 2,
        eResampleAlg: resampling as u32,
        pfnProgress: Some(progress),
        pProgressData: std::ptr::from_ref(context).cast_mut().cast(),
        bFloatingPointWindowValidity: 0,
        dfXOff: 0.,
        dfYOff: 0.,
        dfXSize: 0.,
        dfYSize: 0.,
        bUseOnlyThisScale: 0,
    };
    let mut values = vec![0_f32; shape.0 * shape.1];
    // SAFETY: the band remains alive, dimensions are checked conversions, the
    // output holds shape.0 * shape.1 f32 values, and callback data outlives the call.
    let result = unsafe {
        gdal_sys::GDALRasterIOEx(
            band.c_rasterband(),
            GDALRWFlag::GF_Read,
            origin.0.try_into()?,
            origin.1.try_into()?,
            size.0.try_into()?,
            size.1.try_into()?,
            values.as_mut_ptr().cast(),
            shape.0.try_into()?,
            shape.1.try_into()?,
            GDALDataType::GDT_Float32,
            0,
            0,
            &mut options,
        )
    };
    context.check()?;
    if result != CPLErr::CE_None {
        // SAFETY: GDAL owns a null-terminated thread-local error string.
        let message =
            unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }.to_string_lossy();
        anyhow::bail!("Raster read failed: {message}");
    }
    Ok(values)
}

/// Compare processed windows against an independent gdal-rs RasterIO call using
/// the identical footprint and resampling. Used only by the real-data probe.
pub fn audit_references(
    cache: &Cache,
    sources: &[SourceRecord],
    context: &JobContext,
) -> Result<usize> {
    let mut checked = 0;
    for entry in fs::read_dir(&cache.root)? {
        let path = entry?.path();
        if !path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .starts_with("raster-")
            || path.extension().is_none_or(|e| e != "json")
        {
            continue;
        }
        let window: RasterWindow = serde_json::from_reader(fs::File::open(&path)?)?;
        if !sources
            .iter()
            .any(|s| s.sha256 == window.source.sha256 && s.url == window.source.url)
        {
            continue;
        }
        context.check()?;
        let dataset = Dataset::open(format!("/vsicurl/{}", window.source.url))?;
        let t = dataset.geo_transform()?;
        let x = ((window.bounds[0] - t[0]) / t[1]).round() as isize;
        let y = ((window.bounds[3] - t[3]) / t[5]).round() as isize;
        let width = ((window.bounds[2] - window.bounds[0]) / t[1]).round() as usize;
        let height = ((window.bounds[1] - window.bounds[3]) / t[5]).round() as usize;
        let algorithm = if window.source.name == "ESA WorldCover" {
            ResampleAlg::NearestNeighbour
        } else {
            ResampleAlg::Average
        };
        let reference = dataset.rasterband(1)?.read_as::<f32>(
            (x, y),
            (width, height),
            (window.width, window.height),
            Some(algorithm),
        )?;
        let max_error = reference
            .data()
            .iter()
            .zip(&window.values)
            .filter(|(a, b)| a.is_finite() && b.is_finite())
            .map(|(a, b)| (a - b).abs())
            .fold(0_f32, f32::max);
        ensure!(
            max_error <= 0.001,
            "GDAL reference differs by {max_error}: {}",
            window.source.url
        );
        checked += 1;
    }
    ensure!(checked > 0, "No acquired raster windows to audit");
    Ok(checked)
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
