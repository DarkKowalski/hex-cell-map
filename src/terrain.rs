//! Engine-independent continuous geometry. All chunks derive shared vertices and
//! area-weighted normals from the same document, including neighboring cells.
use crate::{jobs::JobContext, map_core::*};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

type CornerKey = (i32, i32);

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
    pub boundaries: BTreeMap<CornerKey, Corner>,
}

pub struct TerrainGeometry {
    pub chunks: Vec<ChunkGeometry>,
    pub corners: BTreeMap<CornerKey, Corner>,
    pub exaggeration: f32,
}

// Six center triangles and twelve triangles joining inset and outer rings.
pub fn cell_triangles() -> [[usize; 3]; 18] {
    let mut triangles = [[0; 3]; 18];
    for i in 0..6 {
        let next = (i + 1) % 6;
        triangles[i] = [0, 1 + i, 1 + next];
        triangles[6 + 2 * i] = [1 + i, 7 + i, 7 + next];
        triangles[7 + 2 * i] = [1 + i, 7 + next, 1 + next];
    }
    triangles
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

pub fn world_position(point_m: [f64; 2], height_m: f64, exaggeration: f32) -> [f32; 3] {
    [
        (point_m[0] / 1000.) as f32,
        (height_m / 1000.) as f32 * exaggeration,
        (-point_m[1] / 1000.) as f32,
    ]
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
    let mut sums: BTreeMap<CornerKey, (f64, [f32; 4], u32)> = BTreeMap::new();
    for cell in &document.cells {
        for i in 0..6 {
            let entry = sums.entry(corner_key(cell.hex, i)).or_default();
            entry.0 += cell.elevation_m;
            for (sum, weight) in entry.1.iter_mut().zip(cell_weights(cell)) {
                *sum += weight;
            }
            entry.2 += 1;
        }
    }
    let mut corners: BTreeMap<_, _> = sums
        .into_iter()
        .map(|(key, (height, weights, count))| {
            (
                key,
                Corner {
                    position: world_position(
                        vertex_position(key, spacing),
                        height / f64::from(count),
                        exaggeration,
                    ),
                    normal: [0.; 3],
                    weights: weights.map(|v| v / count as f32),
                },
            )
        })
        .collect();
    let triangles = cell_triangles();
    // Accumulate across the complete shared neighborhood before splitting into
    // chunks. This supplies the same halo information on both sides of a seam.
    for (i, cell) in document.cells.iter().enumerate() {
        if i % 512 == 0 {
            context.check()?;
        }
        let positions = cell_positions(cell, spacing, exaggeration, &corners);
        for [a, b, c] in triangles {
            let normal = face_normal(positions[a], positions[b], positions[c]);
            for vertex in [a, b, c] {
                if vertex >= 7 {
                    let corner = corners.get_mut(&corner_key(cell.hex, vertex - 7)).unwrap();
                    for (sum, n) in corner.normal.iter_mut().zip(normal) {
                        *sum += n;
                    }
                }
            }
        }
    }
    for corner in corners.values_mut() {
        corner.normal = normalized(corner.normal);
    }
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
            positions: Vec::new(),
            normals: Vec::new(),
            weights: Vec::new(),
            indices: Vec::new(),
            triangle_cells: Vec::new(),
            boundaries: BTreeMap::new(),
        };
        for cell_id in cell_ids {
            let cell = &document.cells[cell_id];
            let positions = cell_positions(cell, spacing, exaggeration, &corners);
            let mut normals = [[0_f32; 3]; 13];
            for [a, b, c] in triangles {
                let normal = face_normal(positions[a], positions[b], positions[c]);
                for vertex in [a, b, c] {
                    for (sum, n) in normals[vertex].iter_mut().zip(normal) {
                        *sum += n;
                    }
                }
            }
            let base = chunk.positions.len() as u32;
            chunk.positions.extend(positions);
            for (v, normal) in normals.into_iter().enumerate() {
                if v < 7 {
                    chunk.normals.push(normalized(normal));
                    chunk.weights.push(cell_weights(cell));
                } else {
                    let key = corner_key(cell.hex, v - 7);
                    let corner = corners[&key];
                    chunk.normals.push(corner.normal);
                    chunk.weights.push(corner.weights);
                    chunk.boundaries.insert(key, corner);
                }
            }
            for triangle in triangles {
                chunk.indices.extend(triangle.map(|v| base + v as u32));
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

pub fn cell_positions(
    cell: &Cell,
    spacing: f64,
    exaggeration: f32,
    corners: &BTreeMap<CornerKey, Corner>,
) -> [[f32; 3]; 13] {
    let mut positions = [[0.; 3]; 13];
    let center = world_position(cell.center_m, cell.elevation_m, exaggeration);
    positions[0] = center;
    for i in 0..6 {
        let key = corner_key(cell.hex, i);
        let p = vertex_position(key, spacing);
        let inset = [
            cell.center_m[0] + (p[0] - cell.center_m[0]) * 0.62,
            cell.center_m[1] + (p[1] - cell.center_m[1]) * 0.62,
        ];
        positions[1 + i] = world_position(inset, cell.elevation_m, exaggeration);
        positions[7 + i] = corners[&key].position;
    }
    positions
}

pub fn height_at(
    cell: &Cell,
    point_m: [f64; 2],
    spacing: f64,
    geometry: &TerrainGeometry,
) -> Option<f32> {
    let positions = cell_positions(cell, spacing, geometry.exaggeration, &geometry.corners);
    let point = [(point_m[0] / 1000.) as f32, (-point_m[1] / 1000.) as f32];
    for [a, b, c] in cell_triangles() {
        let [pa, pb, pc] = [positions[a], positions[b], positions[c]];
        let determinant = (pb[2] - pc[2]) * (pa[0] - pc[0]) + (pc[0] - pb[0]) * (pa[2] - pc[2]);
        let wa = ((pb[2] - pc[2]) * (point[0] - pc[0]) + (pc[0] - pb[0]) * (point[1] - pc[2]))
            / determinant;
        let wb = ((pc[2] - pa[2]) * (point[0] - pc[0]) + (pa[0] - pc[0]) * (point[1] - pc[2]))
            / determinant;
        let wc = 1. - wa - wb;
        if wa >= -1e-5 && wb >= -1e-5 && wc >= -1e-5 {
            return Some(wa * pa[1] + wb * pb[1] + wc * pc[1]);
        }
    }
    None
}

fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let u = std::array::from_fn::<_, 3, _>(|i| b[i] - a[i]);
    let v = std::array::from_fn::<_, 3, _>(|i| c[i] - a[i]);
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let length = v.iter().map(|n| n * n).sum::<f32>().sqrt();
    if length > 1e-12 {
        v.map(|n| n / length)
    } else {
        [0., 1., 0.]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_core::{GenerationSettings, Projection, build_grid};

    fn fixture() -> Result<MapDocument> {
        let settings = GenerationSettings::default();
        let projection = Projection::new(settings.region)?;
        let (mut cells, bounds_m) = build_grid(&settings, &projection)?;
        for c in &mut cells {
            c.elevation_m = f64::from((c.hex.x * 47 + c.hex.y * 31).rem_euclid(1000));
        }
        let mut document = MapDocument {
            schema_version: 1,
            generator_version: "geometry-test".into(),
            settings,
            projection_wkt: projection.wkt,
            bounds_m,
            cells,
            sources: vec![],
            river_network: vec![],
            index: Default::default(),
        };
        document.rebuild_index()?;
        Ok(document)
    }

    fn check_seams(geometry: &TerrainGeometry) -> usize {
        let mut shared: BTreeMap<CornerKey, Corner> = BTreeMap::new();
        let mut matches = 0;
        for chunk in &geometry.chunks {
            for (key, corner) in &chunk.boundaries {
                if let Some(previous) = shared.insert(*key, *corner) {
                    assert_eq!(previous.position, corner.position);
                    assert_eq!(previous.normal, corner.normal);
                    assert_eq!(previous.weights, corner.weights);
                    matches += 1;
                }
            }
            for triangle in chunk.indices.chunks_exact(3) {
                let [a, b, c] =
                    [triangle[0], triangle[1], triangle[2]].map(|v| chunk.positions[v as usize]);
                assert!(face_normal(a, b, c)[1] > 0.);
            }
        }
        matches
    }

    #[test]
    fn chunk_seams_match_before_and_after_boundary_height_change() -> Result<()> {
        let mut document = fixture()?;
        let before = build(&document, 5., &JobContext::default())?;
        assert!(check_seams(&before) > 20);
        let cell = document
            .cells
            .iter_mut()
            .find(|c| c.hex.x == 0 && c.hex.y == 0)
            .unwrap();
        cell.elevation_m += 1700.;
        let hex = cell.hex;
        let after = build(&document, 5., &JobContext::default())?;
        assert!(check_seams(&after) > 20);
        assert_ne!(
            before.corners[&corner_key(hex, 0)].position,
            after.corners[&corner_key(hex, 0)].position
        );
        Ok(())
    }

    #[test]
    fn river_interiors_and_triangle_ownership_are_preserved_on_slopes() -> Result<()> {
        let mut document = fixture()?;
        document.cells[0].surface = Surface::River;
        let geometry = build(&document, 5., &JobContext::default())?;
        for chunk in &geometry.chunks {
            for (triangle, id) in chunk.indices.chunks_exact(3).zip(&chunk.triangle_cells) {
                let point = [
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
                assert_eq!(point_hex(point, 2000.), document.cells[*id as usize].hex);
                assert!(
                    height_at(&document.cells[*id as usize], point, 2000., &geometry).is_some()
                );
            }
        }
        assert_eq!(cell_weights(&document.cells[0]), [0., 0., 0., 1.]);
        Ok(())
    }
}
