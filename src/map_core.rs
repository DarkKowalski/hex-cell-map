use anyhow::{Result, bail, ensure};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use hexx::Hex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub const MAX_CELLS: usize = 100_000;
pub const CHUNK_SIZE: i32 = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Region {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl Region {
    pub fn validate(self) -> Result<()> {
        ensure!(
            [self.west, self.south, self.east, self.north]
                .iter()
                .all(|v| v.is_finite()),
            "Region coordinates must be finite"
        );
        ensure!(
            self.west >= -180. && self.east <= 180. && self.west < self.east,
            "Use west < east within -180° to 180°; antimeridian crossings are not supported"
        );
        ensure!(
            self.south >= -60. && self.north <= 60. && self.south < self.north,
            "Use south < north within -60° to 60°"
        );
        Ok(())
    }

    pub fn contains(self, lon: f64, lat: f64) -> bool {
        lon >= self.west && lon <= self.east && lat >= self.south && lat <= self.north
    }

    pub fn expanded(self, degrees: f64) -> Self {
        Self {
            west: (self.west - degrees).max(-180.),
            south: (self.south - degrees).max(-89.),
            east: (self.east + degrees).min(180.),
            north: (self.north + degrees).min(89.),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationSettings {
    pub region: Region,
    pub spacing_km: f64,
    pub min_river_discharge: f64,
    pub min_city_population: u64,
    pub forest_fraction: f64,
    pub mountain_relief_m: f64,
    pub mountain_slope_degrees: f64,
}

impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            region: Region {
                west: 7.6,
                south: 46.3,
                east: 8.6,
                north: 47.1,
            },
            spacing_km: 2.,
            min_river_discharge: 5.,
            min_city_population: 5_000,
            forest_fraction: 0.4,
            mountain_relief_m: 300.,
            mountain_slope_degrees: 15.,
        }
    }
}

impl GenerationSettings {
    pub fn validate(&self) -> Result<()> {
        self.region.validate()?;
        ensure!(
            self.spacing_km.is_finite() && (1. ..=20.).contains(&self.spacing_km),
            "Hex spacing must be between 1 and 20 km"
        );
        ensure!(
            self.min_river_discharge.is_finite() && self.min_river_discharge >= 0.,
            "River discharge threshold must be finite and nonnegative"
        );
        ensure!(
            self.forest_fraction.is_finite() && (0. ..=1.).contains(&self.forest_fraction),
            "Forest fraction must be between 0 and 1"
        );
        ensure!(
            self.mountain_relief_m.is_finite() && self.mountain_relief_m > 0.,
            "Mountain relief threshold must be positive"
        );
        ensure!(
            self.mountain_slope_degrees.is_finite()
                && (0. ..=90.).contains(&self.mountain_slope_degrees),
            "Mountain slope threshold must be between 0° and 90°"
        );
        Ok(())
    }
}

pub struct Projection {
    pub wkt: String,
    forward: CoordTransform,
    inverse: CoordTransform,
}

impl Projection {
    pub fn new(region: Region) -> Result<Self> {
        region.validate()?;
        crate::gis::initialize()?;
        let mut geographic = SpatialRef::from_epsg(4326)?;
        geographic.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let local = SpatialRef::from_proj4(&format!(
            "+proj=aeqd +lat_0={} +lon_0={} +datum=WGS84 +units=m +no_defs",
            (region.south + region.north) / 2.,
            (region.west + region.east) / 2.
        ))?;
        Ok(Self {
            wkt: local.to_wkt()?,
            forward: CoordTransform::new(&geographic, &local)?,
            inverse: CoordTransform::new(&local, &geographic)?,
        })
    }

    pub fn project(&self, points: &mut [[f64; 2]]) -> Result<()> {
        Self::transform(&self.forward, points)
    }
    pub fn unproject(&self, points: &mut [[f64; 2]]) -> Result<()> {
        Self::transform(&self.inverse, points)
    }

    fn transform(transform: &CoordTransform, points: &mut [[f64; 2]]) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let mut x: Vec<_> = points.iter().map(|p| p[0]).collect();
        let mut y: Vec<_> = points.iter().map(|p| p[1]).collect();
        transform.transform_coords(&mut x, &mut y, &mut [])?;
        for (i, p) in points.iter_mut().enumerate() {
            ensure!(
                x[i].is_finite() && y[i].is_finite(),
                "Projection produced invalid coordinates"
            );
            *p = [x[i], y[i]];
        }
        Ok(())
    }

    pub fn bounds(&self, region: Region) -> Result<[f64; 4]> {
        let mut edge = Vec::new();
        for i in 0..=32 {
            let t = f64::from(i) / 32.;
            let lon = region.west + (region.east - region.west) * t;
            let lat = region.south + (region.north - region.south) * t;
            edge.extend([
                [lon, region.south],
                [lon, region.north],
                [region.west, lat],
                [region.east, lat],
            ]);
        }
        self.project(&mut edge)?;
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for [x, y] in edge {
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(y);
        }
        Ok(bounds)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Landscape {
    Plains,
    Forest,
    Mountain,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Surface {
    Land,
    River,
    Water,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct City {
    pub id: u64,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub population: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cell {
    pub id: u32,
    pub hex: Hex,
    pub center_m: [f64; 2],
    pub lon_lat: [f64; 2],
    pub generated_elevation_m: f64,
    pub elevation_m: f64,
    pub relief_m: f64,
    pub slope_degrees: f64,
    pub forest_fraction: f64,
    pub water_fraction: f64,
    pub landscape: Landscape,
    pub surface: Surface,
    pub river_ids: Vec<u64>,
    pub cities: Vec<City>,
    pub edited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceRecord {
    pub name: String,
    pub version: String,
    pub url: String,
    pub license: String,
    pub attribution: String,
    pub acquired_unix: u64,
    pub etag: Option<String>,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapDocument {
    pub schema_version: u32,
    pub generator_version: String,
    pub settings: GenerationSettings,
    pub projection_wkt: String,
    pub bounds_m: [f64; 4],
    pub cells: Vec<Cell>,
    pub sources: Vec<SourceRecord>,
    pub river_network: Vec<RiverRecord>,
    pub height_field: HeightField,
    #[serde(skip)]
    pub index: HashMap<Hex, usize>,
}

/// Projected DEM samples independent of the gameplay hex grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeightField {
    pub origin_m: [f64; 2],
    pub step_m: f64,
    pub width: usize,
    pub height: usize,
    pub elevations_m: Vec<f32>,
}

impl HeightField {
    pub fn sample(&self, point: [f64; 2]) -> Option<f64> {
        let x = (point[0] - self.origin_m[0]) / self.step_m;
        let y = (point[1] - self.origin_m[1]) / self.step_m;
        if x < 0. || y < 0. || x > (self.width - 1) as f64 || y > (self.height - 1) as f64 {
            return None;
        }
        let ix = (x.floor() as usize).min(self.width - 2);
        let iy = (y.floor() as usize).min(self.height - 2);
        let tx = x - ix as f64;
        let ty = y - iy as f64;
        let at = |dx, dy| f64::from(self.elevations_m[(iy + dy) * self.width + ix + dx]);
        Some(
            (at(0, 0) * (1. - tx) + at(1, 0) * tx) * (1. - ty)
                + (at(0, 1) * (1. - tx) + at(1, 1) * tx) * ty,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RiverRecord {
    pub id: u64,
    pub next_down: u64,
    pub discharge: f64,
    pub stream_order: u32,
}

impl MapDocument {
    pub fn rebuild_index(&mut self) -> Result<()> {
        let field = &self.height_field;
        ensure!(
            field.width >= 2
                && field.height >= 2
                && field.width.checked_mul(field.height) == Some(field.elevations_m.len())
                && field.elevations_m.len() <= 2_000_000
                && field.step_m.is_finite()
                && field.step_m > 0.
                && field.elevations_m.iter().all(|h| h.is_finite()),
            "Invalid source height field"
        );
        ensure!(self.schema_version == 1, "Unsupported cached map schema");
        ensure!(
            self.cells.len() <= MAX_CELLS && !self.cells.is_empty(),
            "Map has an invalid number of cells"
        );
        self.index.clear();
        for (i, cell) in self.cells.iter().enumerate() {
            ensure!(
                cell.id as usize == i,
                "Cell IDs must match their canonical order"
            );
            ensure!(
                cell.elevation_m.is_finite() && cell.generated_elevation_m.is_finite(),
                "Invalid elevation in cached map"
            );
            ensure!(
                self.index.insert(cell.hex, i).is_none(),
                "Duplicate hex in map"
            );
        }
        Ok(())
    }

    pub fn cell(&self, hex: Hex) -> Option<&Cell> {
        self.index.get(&hex).map(|i| &self.cells[*i])
    }

    pub fn chunk_ids(&self) -> BTreeMap<(i32, i32), Vec<usize>> {
        let mut chunks = BTreeMap::new();
        for (i, cell) in self.cells.iter().enumerate() {
            chunks
                .entry((
                    cell.hex.x.div_euclid(CHUNK_SIZE),
                    cell.hex.y.div_euclid(CHUNK_SIZE),
                ))
                .or_insert_with(Vec::new)
                .push(i);
        }
        chunks
    }
}

pub fn hex_center(hex: Hex, spacing: f64) -> [f64; 2] {
    [
        spacing * (f64::from(hex.x) + f64::from(hex.y) / 2.),
        spacing * 3_f64.sqrt() / 2. * f64::from(hex.y),
    ]
}

pub fn point_hex(point: [f64; 2], spacing: f64) -> Hex {
    let r = 2. * point[1] / (3_f64.sqrt() * spacing);
    let q = point[0] / spacing - r / 2.;
    let s = -q - r;
    let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
    let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
    if dq > dr && dq > ds {
        rq = -rr - rs;
    } else if dr > ds {
        rr = -rq - rs;
    }
    Hex::new(rq as i32, rr as i32)
}

pub const CORNER_OFFSETS: [(i32, i32); 6] = [(1, 1), (0, 2), (-1, 1), (-1, -1), (0, -2), (1, -1)];

pub fn corner_key(hex: Hex, corner: usize) -> (i32, i32) {
    (
        2 * hex.x + hex.y + CORNER_OFFSETS[corner].0,
        3 * hex.y + CORNER_OFFSETS[corner].1,
    )
}

pub fn vertex_position(key: (i32, i32), spacing: f64) -> [f64; 2] {
    [
        f64::from(key.0) * spacing / 2.,
        f64::from(key.1) * spacing / (2. * 3_f64.sqrt()),
    ]
}

pub fn hex_polygon(hex: Hex, spacing: f64) -> geo::Polygon<f64> {
    let corners: Vec<_> = (0..=6)
        .map(|c| {
            let p = vertex_position(corner_key(hex, c % 6), spacing);
            (p[0], p[1])
        })
        .collect();
    geo::Polygon::new(geo::LineString::from(corners), vec![])
}

pub fn build_grid(
    settings: &GenerationSettings,
    projection: &Projection,
) -> Result<(Vec<Cell>, [f64; 4])> {
    use geo::Intersects;
    settings.validate()?;
    let bounds = projection.bounds(settings.region)?;
    ensure!(
        (bounds[2] - bounds[0]).max(bounds[3] - bounds[1]) <= 1_000_000.,
        "Region must be at most 1,000 km across"
    );
    let spacing = settings.spacing_km * 1000.;
    // Include intersecting boundary hexes so settlements retain their true cell.
    let region = settings.region;
    let mut outline = Vec::new();
    for i in 0..32 {
        let t = f64::from(i) / 32.;
        outline.push([region.west + t * (region.east - region.west), region.south]);
    }
    for i in 0..32 {
        let t = f64::from(i) / 32.;
        outline.push([
            region.east,
            region.south + t * (region.north - region.south),
        ]);
    }
    for i in 0..32 {
        let t = f64::from(i) / 32.;
        outline.push([region.east - t * (region.east - region.west), region.north]);
    }
    for i in 0..32 {
        let t = f64::from(i) / 32.;
        outline.push([
            region.west,
            region.north - t * (region.north - region.south),
        ]);
    }
    projection.project(&mut outline)?;
    outline.push(outline[0]);
    let polygon = geo::Polygon::new(
        geo::LineString::from(
            outline
                .into_iter()
                .map(|p| (p[0], p[1]))
                .collect::<Vec<_>>(),
        ),
        vec![],
    );
    let estimate =
        (bounds[2] - bounds[0]) * (bounds[3] - bounds[1]) / (spacing * spacing * 3_f64.sqrt() / 2.);
    ensure!(
        estimate <= MAX_CELLS as f64 * 1.05,
        "Region exceeds 100,000 hexes; increase hex spacing or reduce the region"
    );
    let r_min = (bounds[1] / (spacing * 3_f64.sqrt() / 2.)).floor() as i32 - 1;
    let r_max = (bounds[3] / (spacing * 3_f64.sqrt() / 2.)).ceil() as i32 + 1;
    let mut hexes = Vec::new();
    for r in r_min..=r_max {
        let q_min = (bounds[0] / spacing - f64::from(r) / 2.).floor() as i32 - 1;
        let q_max = (bounds[2] / spacing - f64::from(r) / 2.).ceil() as i32 + 1;
        hexes.extend((q_min..=q_max).map(|q| Hex::new(q, r)));
    }
    let mut geographic: Vec<_> = hexes.iter().map(|h| hex_center(*h, spacing)).collect();
    projection.unproject(&mut geographic)?;
    let mut cells = Vec::new();
    for (hex, lon_lat) in hexes.into_iter().zip(geographic) {
        if !hex_polygon(hex, spacing).intersects(&polygon) {
            continue;
        }
        if cells.len() == MAX_CELLS {
            bail!("Region exceeds 100,000 hexes; increase hex spacing");
        }
        cells.push(Cell {
            id: cells.len() as u32,
            hex,
            center_m: hex_center(hex, spacing),
            lon_lat,
            generated_elevation_m: 0.,
            elevation_m: 0.,
            relief_m: 0.,
            slope_degrees: 0.,
            forest_fraction: 0.,
            water_fraction: 0.,
            landscape: Landscape::Plains,
            surface: Surface::Land,
            river_ids: vec![],
            cities: vec![],
            edited: false,
        });
    }
    ensure!(
        !cells.is_empty(),
        "Region is too small for this hex spacing"
    );
    Ok((cells, bounds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trip_and_grid_assignment() -> Result<()> {
        let settings = GenerationSettings::default();
        let p = Projection::new(settings.region)?;
        let originals = [[7.6, 46.3], [8.6, 47.1], [8.1, 46.7]];
        let mut points = originals;
        p.project(&mut points)?;
        p.unproject(&mut points)?;
        for (a, b) in originals.iter().zip(points) {
            assert!((a[0] - b[0]).abs() < 1e-8 && (a[1] - b[1]).abs() < 1e-8);
        }
        let (cells, _) = build_grid(&settings, &p)?;
        for cell in cells {
            assert_eq!(
                point_hex(cell.center_m, settings.spacing_km * 1000.),
                cell.hex
            );
        }
        Ok(())
    }

    #[test]
    fn invalid_regions_and_excessive_maps_are_rejected() -> Result<()> {
        let mut s = GenerationSettings::default();
        s.region.west = f64::NAN;
        assert!(s.validate().is_err());
        s.region = Region {
            west: -5.,
            south: 45.,
            east: 5.,
            north: 52.,
        };
        s.spacing_km = 1.;
        let projection = Projection::new(s.region)?;
        assert!(build_grid(&s, &projection).is_err());
        Ok(())
    }

    #[test]
    fn negative_hexes_have_shared_canonical_corners() {
        let a = Hex::new(-1, -1);
        for b in a.all_neighbors() {
            let shared = (0..6)
                .filter(|i| (0..6).any(|j| corner_key(a, *i) == corner_key(b, j)))
                .count();
            assert_eq!(shared, 2);
        }
    }
}
