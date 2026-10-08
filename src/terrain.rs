//! Continuous source DEM surface clipped into hex-owned chunks. The hex lattice
//! controls topology and selection, while a separate height field controls shape.
use crate::{jobs::JobContext, map_core::*};
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, HashMap};

pub const SUBDIVISIONS: i32 = 4;
type VertexKey = (i32, i32);

#[derive(Debug, Clone, Copy)]
pub struct Corner {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub weights: [f32; 4],
}

#[derive(Debug)]
pub struct ChunkGeometry {
    pub id: (i32, i32),
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub triangle_cells: Vec<u32>,
    pub boundaries: BTreeMap<VertexKey, Corner>,
}

pub struct TerrainGeometry {
    pub chunks: Vec<ChunkGeometry>,
    /// All lattice samples, including subdivided hex edges and interiors.
    pub corners: BTreeMap<VertexKey, Corner>,
    pub exaggeration: f32,
}

pub fn cell_triangles(hex: hexx::Hex) -> Vec<[VertexKey; 3]> {
    let center = (2 * hex.x + hex.y, 3 * hex.y);
    let mut triangles = Vec::with_capacity((6 * SUBDIVISIONS * SUBDIVISIONS) as usize);
    for sector in 0..6 {
        let a = CORNER_OFFSETS[sector];
        let b = CORNER_OFFSETS[(sector + 1) % 6];
        let key = |i, j| {
            (
                center.0 * SUBDIVISIONS + a.0 * i + b.0 * j,
                center.1 * SUBDIVISIONS + a.1 * i + b.1 * j,
            )
        };
        for i in 0..SUBDIVISIONS {
            for j in 0..(SUBDIVISIONS - i) {
                triangles.push([key(i, j), key(i + 1, j), key(i, j + 1)]);
                if i + j + 1 < SUBDIVISIONS {
                    triangles.push([key(i + 1, j), key(i + 1, j + 1), key(i, j + 1)]);
                }
            }
        }
    }
    triangles
}

pub fn vertex_point(key: VertexKey, spacing: f64) -> [f64; 2] {
    vertex_position(key, spacing / f64::from(SUBDIVISIONS))
}

pub fn outer_key(hex: hexx::Hex, corner: usize) -> VertexKey {
    let key = corner_key(hex, corner);
    (key.0 * SUBDIVISIONS, key.1 * SUBDIVISIONS)
}

pub fn world_position(point_m: [f64; 2], height_m: f64, exaggeration: f32) -> [f32; 3] {
    [
        (point_m[0] / 1000.) as f32,
        (height_m / 1000.) as f32 * exaggeration,
        (-point_m[1] / 1000.) as f32,
    ]
}

pub fn cell_weights(cell: &Cell) -> [f32; 4] {
    if cell.surface != Surface::Land {
        return [0., 0., 0., 1.];
    }
    match cell.landscape {
        Landscape::Plains => [1., 0., 0., 0.],
        Landscape::Forest => [0., 1., 0., 0.],
        Landscape::Mountain => [0., 0., 1., 0.],
    }
}

// Smooth, compact weights influence future editable elevation offsets and land
// materials. The source DEM is sampled directly, never replaced by cell means.
fn neighborhood(document: &MapDocument, point: [f64; 2]) -> ([f32; 4], f64) {
    let spacing = document.settings.spacing_km * 1000.;
    let hex = point_hex(point, spacing);
    let mut weights = [0.; 4];
    let mut delta = 0.;
    let mut total = 0.;
    for dq in -2..=2 {
        for dr in -2..=2 {
            if let Some(cell) = document.cell(hex + hexx::Hex::new(dq, dr)) {
                let d = ((cell.center_m[0] - point[0]).powi(2)
                    + (cell.center_m[1] - point[1]).powi(2))
                .sqrt()
                    / (spacing * 1.5);
                if d >= 1. {
                    continue;
                }
                let weight = (1. - d).powi(4) * (4. * d + 1.);
                let mut land_weights = cell_weights(cell);
                // Water is an explicit owning-cell surface; land blending stays
                // independent so rivers cannot spread into unrelated neighbors.
                if cell.surface != Surface::Land {
                    land_weights = match cell.landscape {
                        Landscape::Plains => [1., 0., 0., 0.],
                        Landscape::Forest => [0., 1., 0., 0.],
                        Landscape::Mountain => [0., 0., 1., 0.],
                    };
                }
                for (sum, value) in weights.iter_mut().zip(land_weights) {
                    *sum += value * weight as f32;
                }
                delta += (cell.elevation_m - cell.generated_elevation_m) * weight;
                total += weight;
            }
        }
    }
    if total > 0. {
        weights = weights.map(|w| w / total as f32);
        delta /= total;
    } else {
        weights = [1., 0., 0., 0.];
    }
    if let Some(cell) = document.cell(hex) {
        if cell.surface != Surface::Land {
            weights = [0., 0., 0., 1.];
        }
    } else {
        // Exact exterior boundary samples retain water from adjacent source cells.
        let water = hex
            .all_neighbors()
            .into_iter()
            .filter_map(|h| document.cell(h))
            .filter(|c| c.surface != Surface::Land)
            .count();
        if water > 0 {
            weights = [0., 0., 0., 1.];
        }
    }
    (weights, delta)
}

pub fn terrain_height(document: &MapDocument, point: [f64; 2]) -> Result<f64> {
    let base = document
        .height_field
        .sample(point)
        .ok_or_else(|| anyhow::anyhow!("Source DEM does not cover terrain vertex"))?;
    Ok(base + neighborhood(document, point).1)
}

pub fn build(
    document: &MapDocument,
    exaggeration: f32,
    context: &JobContext,
) -> Result<TerrainGeometry> {
    ensure!(
        exaggeration.is_finite() && (1. ..=12.).contains(&exaggeration),
        "Invalid vertical exaggeration"
    );
    context.check()?;
    let spacing = document.settings.spacing_km * 1000.;
    let mut corners = BTreeMap::new();
    let mut chunks = Vec::new();
    let groups = document.chunk_ids();
    let total = groups.len();
    for (i, (id, cell_ids)) in groups.into_iter().enumerate() {
        context.report(
            0.95 + 0.04 * i as f32 / total as f32,
            format!("Building terrain chunk {}/{}", i + 1, total),
        )?;
        let mut chunk = ChunkGeometry {
            id,
            positions: vec![],
            normals: vec![],
            weights: vec![],
            indices: vec![],
            triangle_cells: vec![],
            boundaries: BTreeMap::new(),
        };
        let mut local_vertices = HashMap::new();
        for (n, cell_id) in cell_ids.into_iter().enumerate() {
            if n % 128 == 0 {
                context.check()?;
            }
            let cell = &document.cells[cell_id];
            for triangle in cell_triangles(cell.hex) {
                for key in triangle {
                    let index = if let Some(index) = local_vertices.get(&key) {
                        *index
                    } else {
                        let vertex = if let Some(vertex) = corners.get(&key) {
                            *vertex
                        } else {
                            let point = vertex_point(key, spacing);
                            let (weights, delta) = neighborhood(document, point);
                            let height = document.height_field.sample(point).ok_or_else(|| {
                                anyhow::anyhow!("Source DEM does not cover terrain vertex")
                            })? + delta;
                            let epsilon = document.height_field.step_m * 0.35;
                            let east = terrain_height(document, [point[0] + epsilon, point[1]])?;
                            let west = terrain_height(document, [point[0] - epsilon, point[1]])?;
                            let north = terrain_height(document, [point[0], point[1] + epsilon])?;
                            let south = terrain_height(document, [point[0], point[1] - epsilon])?;
                            let normal = normalized([
                                (-(east - west) / (2. * epsilon)) as f32 * exaggeration,
                                1.,
                                ((north - south) / (2. * epsilon)) as f32 * exaggeration,
                            ]);
                            let vertex = Corner {
                                position: world_position(point, height, exaggeration),
                                normal,
                                weights,
                            };
                            corners.insert(key, vertex);
                            vertex
                        };
                        let index = chunk.positions.len() as u32;
                        chunk.positions.push(vertex.position);
                        chunk.normals.push(vertex.normal);
                        chunk.weights.push(vertex.weights);
                        chunk.boundaries.insert(key, vertex);
                        local_vertices.insert(key, index);
                        index
                    };
                    chunk.indices.push(index);
                }
                chunk.triangle_cells.push(cell.id);
            }
        }
        chunks.push(chunk);
    }
    context.check()?;
    Ok(TerrainGeometry {
        chunks,
        corners,
        exaggeration,
    })
}

pub fn height_at(
    cell: &Cell,
    point: [f64; 2],
    spacing: f64,
    geometry: &TerrainGeometry,
) -> Option<f32> {
    surface_height(
        cell,
        point,
        spacing,
        geometry.exaggeration,
        &geometry.corners,
    )
}

pub fn surface_height(
    cell: &Cell,
    point_m: [f64; 2],
    _spacing: f64,
    _: f32,
    corners: &BTreeMap<VertexKey, Corner>,
) -> Option<f32> {
    let point = [(point_m[0] / 1000.) as f32, (-point_m[1] / 1000.) as f32];
    for keys in cell_triangles(cell.hex) {
        let [a, b, c] = keys.map(|key| corners[&key].position);
        let determinant = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        let wa =
            ((b[2] - c[2]) * (point[0] - c[0]) + (c[0] - b[0]) * (point[1] - c[2])) / determinant;
        let wb =
            ((c[2] - a[2]) * (point[0] - c[0]) + (a[0] - c[0]) * (point[1] - c[2])) / determinant;
        let wc = 1. - wa - wb;
        if wa >= -1e-5 && wb >= -1e-5 && wc >= -1e-5 {
            return Some(wa * a[1] + wb * b[1] + wc * c[1]);
        }
    }
    None
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let length = v.iter().map(|n| n * n).sum::<f32>().sqrt();
    v.map(|n| n / length)
}

#[cfg(test)]
pub(crate) fn fixture() -> Result<MapDocument> {
    let settings = GenerationSettings::default();
    let projection = Projection::new(settings.region)?;
    let (mut cells, bounds_m) = build_grid(&settings, &projection)?;
    for c in &mut cells {
        c.elevation_m = 800.;
        c.generated_elevation_m = 800.;
    }
    let step = 500.;
    let width = 190;
    let height = 210;
    let origin_m = [-47_000., -52_000.];
    // A diagonal ridge with a valley, used only to test terrain geometry.
    let elevations_m = (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let east = origin_m[0] + x as f64 * step;
                let north = origin_m[1] + y as f64 * step;
                (400. + 1800. * (-((east - north * 0.4) / 8000.).powi(2)).exp() + north / 150.)
                    as f32
            })
        })
        .collect();
    let mut document = MapDocument {
        schema_version: 1,
        generator_version: "geometry-test".into(),
        settings,
        projection_wkt: projection.wkt,
        bounds_m,
        cells,
        sources: vec![],
        river_network: vec![],
        height_field: HeightField {
            origin_m,
            step_m: step,
            width,
            height,
            elevations_m,
        },
        index: Default::default(),
    };
    document.rebuild_index()?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check_seams(geometry: &TerrainGeometry) -> usize {
        let mut shared = BTreeMap::<VertexKey, Corner>::new();
        let mut matches = 0;
        for chunk in &geometry.chunks {
            for (key, vertex) in &chunk.boundaries {
                if let Some(previous) = shared.insert(*key, *vertex) {
                    assert_eq!(previous.position, vertex.position);
                    assert_eq!(previous.normal, vertex.normal);
                    matches += 1;
                }
            }
        }
        matches
    }

    #[test]
    fn source_ridge_crosses_hexes_without_flat_interiors() -> Result<()> {
        let document = fixture()?;
        let geometry = build(&document, 3., &JobContext::default())?;
        assert!(check_seams(&geometry) > 100);
        let mut sloped_cells = 0;
        for cell in &document.cells {
            let center = (2 * cell.hex.x + cell.hex.y, 3 * cell.hex.y);
            let key = (center.0 * SUBDIVISIONS, center.1 * SUBDIVISIONS);
            let near = (key.0 + 1, key.1 + 1);
            let a = geometry.corners[&key].position[1];
            let b = geometry.corners[&near].position[1];
            if (a - b).abs() > 0.002 {
                sloped_cells += 1;
            }
            let expected = document
                .height_field
                .sample(vertex_point(key, 2000.))
                .unwrap() as f32
                * 0.003;
            assert!((a - expected).abs() < 1e-5);
        }
        assert!(sloped_cells > document.cells.len() / 2);
        Ok(())
    }

    #[test]
    fn chunk_seams_match_after_boundary_elevation_change() -> Result<()> {
        let mut document = fixture()?;
        let before = build(&document, 3., &JobContext::default())?;
        let index = document.index[&hexx::Hex::ZERO];
        document.cells[index].elevation_m += 1700.;
        let after = build(&document, 3., &JobContext::default())?;
        assert!(check_seams(&after) > 100);
        let key = outer_key(hexx::Hex::ZERO, 0);
        assert_ne!(before.corners[&key].position, after.corners[&key].position);
        Ok(())
    }

    #[test]
    fn triangles_retain_hex_ownership_and_river_interiors() -> Result<()> {
        let mut document = fixture()?;
        let cell = &mut document.cells[0];
        cell.surface = Surface::River;
        let hex = cell.hex;
        let geometry = build(&document, 3., &JobContext::default())?;
        let key = ((2 * hex.x + hex.y) * SUBDIVISIONS, 3 * hex.y * SUBDIVISIONS);
        assert_eq!(geometry.corners[&key].weights, [0., 0., 0., 1.]);
        for chunk in &geometry.chunks {
            for (triangle, id) in chunk
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .zip(&chunk.triangle_cells)
            {
                let p = [
                    triangle
                        .iter()
                        .map(|v| f64::from(chunk.positions[*v as usize][0]) * 1000.)
                        .sum::<f64>()
                        / 3.,
                    triangle
                        .iter()
                        .map(|v| -f64::from(chunk.positions[*v as usize][2]) * 1000.)
                        .sum::<f64>()
                        / 3.,
                ];
                assert_eq!(point_hex(p, 2000.), document.cells[*id as usize].hex);
                assert!(height_at(&document.cells[*id as usize], p, 2000., &geometry).is_some());
            }
        }
        Ok(())
    }
}
