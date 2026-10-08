//! Continuous DEM surface with local river refinement and hex-owned triangles.
use crate::{
    hydrology::{Hydrology, WaterVertex, clip_polygon},
    jobs::JobContext,
    map_core::*,
};
use anyhow::Result;
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};
use std::collections::{BTreeMap, BTreeSet, HashMap};
pub const SUBDIVISIONS: i32 = 4;
/// Millimeter local coordinates allow arbitrary source river/bank vertices.
pub type VertexKey = (i64, i64);
#[derive(Debug, Clone, Copy)]
pub struct Corner {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub weights: [f32; 4],
    pub raw_height: f64,
    pub raw_gradient: [f32; 2],
}
#[derive(Debug)]
pub struct ChunkGeometry {
    pub id: (i32, i32),
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Linear RGB and urban coverage (land), or water color and opacity (water).
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub triangle_cells: Vec<u32>,
    pub boundaries: BTreeMap<VertexKey, Corner>,
}
impl ChunkGeometry {
    fn new(id: (i32, i32)) -> Self {
        Self {
            id,
            positions: vec![],
            normals: vec![],
            weights: vec![],
            indices: vec![],
            triangle_cells: vec![],
            boundaries: BTreeMap::new(),
        }
    }
}
pub struct TerrainGeometry {
    pub chunks: Vec<ChunkGeometry>,
    pub water_chunks: Vec<ChunkGeometry>,
    pub corners: BTreeMap<VertexKey, Corner>,
    pub triangles: Vec<Vec<[VertexKey; 3]>>,
    pub heights: HeightSettings,
    pub hydrology: Hydrology,
}
pub fn vertex_key(point: [f64; 2]) -> VertexKey {
    (
        (point[0] * 1000.).round() as i64,
        (point[1] * 1000.).round() as i64,
    )
}
pub fn vertex_point(key: VertexKey, _spacing: f64) -> [f64; 2] {
    [key.0 as f64 / 1000., key.1 as f64 / 1000.]
}
pub fn outer_key(hex: hexx::Hex, corner: usize, spacing: f64) -> VertexKey {
    vertex_key(vertex_position(corner_key(hex, corner), spacing))
}
pub fn cell_triangles(hex: hexx::Hex, spacing: f64) -> Vec<[VertexKey; 3]> {
    let center = (2 * hex.x + hex.y, 3 * hex.y);
    let mut triangles = Vec::new();
    for sector in 0..6 {
        let a = CORNER_OFFSETS[sector];
        let b = CORNER_OFFSETS[(sector + 1) % 6];
        let key = |i, j| {
            vertex_key(vertex_position(
                (
                    center.0 * SUBDIVISIONS + a.0 * i + b.0 * j,
                    center.1 * SUBDIVISIONS + a.1 * i + b.1 * j,
                ),
                spacing / f64::from(SUBDIVISIONS),
            ))
        };
        for i in 0..SUBDIVISIONS {
            for j in 0..SUBDIVISIONS - i {
                triangles.push([key(i, j), key(i + 1, j), key(i, j + 1)]);
                if i + j + 1 < SUBDIVISIONS {
                    triangles.push([key(i + 1, j), key(i + 1, j + 1), key(i, j + 1)]);
                }
            }
        }
    }
    triangles
}
pub fn world_position(point: [f64; 2], height: f64, settings: HeightSettings) -> [f32; 3] {
    [
        (point[0] / 1000.) as f32,
        (settings.meters(height) / 1000.) as f32,
        (-point[1] / 1000.) as f32,
    ]
}
fn neighborhood(document: &MapDocument, point: [f64; 2]) -> (f64, f64) {
    let spacing = document.settings.spacing_km * 1000.;
    let hex = point_hex(point, spacing);
    let (mut delta, mut urban, mut total) = (0., 0., 0.);
    for dq in -2..=2 {
        for dr in -2..=2 {
            if let Some(c) = document.cell(hex + hexx::Hex::new(dq, dr)) {
                let d =
                    (c.center_m[0] - point[0]).hypot(c.center_m[1] - point[1]) / (spacing * 1.1);
                if d >= 1. {
                    continue;
                }
                let w = (1. - d).powi(4) * (4. * d + 1.);
                total += w;
                delta += (c.elevation_m - c.generated_elevation_m) * w;
                if c.urban.is_some() {
                    urban += w;
                }
            }
        }
    }
    if total > 0. {
        (delta / total, urban / total)
    } else {
        (0., 0.)
    }
}
pub fn edit_delta(document: &MapDocument, p: [f64; 2]) -> f64 {
    neighborhood(document, p).0
}
pub fn source_height(document: &MapDocument, p: [f64; 2]) -> Result<f64> {
    Ok(document
        .height_field
        .sample(p)
        .ok_or_else(|| anyhow::anyhow!("Source DEM does not cover terrain vertex"))?
        + edit_delta(document, p))
}
pub fn terrain_height(document: &MapDocument, hydrology: &Hydrology, p: [f64; 2]) -> Result<f64> {
    Ok(hydrology.ground(document, p, source_height(document, p)?))
}
fn palette(document: &MapDocument, p: [f64; 2], slope: f64) -> [f32; 4] {
    let cover = document.height_field.cover_weights(p);
    // Tree, shrub, grass, crop, built, bare, snow, water bed, wetland, mangrove, moss.
    let colors: [[f64; 3]; 11] = [
        [0.105, 0.20, 0.07],
        [0.25, 0.28, 0.105],
        [0.29, 0.37, 0.13],
        [0.36, 0.39, 0.16],
        [0.31, 0.28, 0.23],
        [0.34, 0.29, 0.22],
        [0.70, 0.74, 0.73],
        [0.23, 0.25, 0.16],
        [0.15, 0.27, 0.13],
        [0.09, 0.19, 0.10],
        [0.27, 0.30, 0.17],
    ];
    let mut rgb = [0.; 3];
    for (w, color) in cover.iter().zip(colors) {
        for k in 0..3 {
            rgb[k] += w * color[k];
        }
    }
    // Actual uncompressed DEM slope exposes rock. Altitude alone never adds snow.
    let rock = ((slope - 0.30) / 0.65).clamp(0., 0.85) * (1. - cover[6]);
    let warmth = (0.5 + 0.5 * (p[0] / 1300. + p[1] / 2100.).sin()) * 0.05;
    for k in 0..3 {
        rgb[k] = rgb[k] * (1. - rock) + [0.32 + warmth, 0.30 + warmth * 0.6, 0.265][k] * rock;
    }
    let urban = neighborhood(document, p).1;
    // Broad coverage reads as a complete urban cell, with a soft outer blend.
    let urban = (urban * 1.5).clamp(0., 1.);
    for k in 0..3 {
        rgb[k] = rgb[k] * (1. - urban) + [0.29, 0.265, 0.225][k] * urban;
    }
    [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, urban as f32]
}
fn normal(v: [f32; 3]) -> [f32; 3] {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.map(|x| x / n)
}
fn hex_outline(hex: hexx::Hex, spacing: f64) -> [[f64; 2]; 6] {
    (0..6)
        .map(|i| vertex_point(outer_key(hex, i, spacing), spacing))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}
fn assign_polygon(
    document: &MapDocument,
    polygon: &[WaterVertex],
    assigned: &mut HashMap<u32, Vec<Vec<WaterVertex>>>,
) {
    let spacing = document.settings.spacing_km * 1000.;
    let min_x = polygon
        .iter()
        .map(|v| v.point[0])
        .fold(f64::INFINITY, f64::min);
    let max_x = polygon
        .iter()
        .map(|v| v.point[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = polygon
        .iter()
        .map(|v| v.point[1])
        .fold(f64::INFINITY, f64::min);
    let max_y = polygon
        .iter()
        .map(|v| v.point[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let row = spacing * 3_f64.sqrt() / 2.;
    for r in (min_y / row).floor() as i32 - 1..=(max_y / row).ceil() as i32 + 1 {
        for q in (min_x / spacing - r as f64 / 2.).floor() as i32 - 1
            ..=(max_x / spacing - r as f64 / 2.).ceil() as i32 + 1
        {
            if let Some(cell) = document.cell(hexx::Hex::new(q, r)) {
                let clipped = clip_polygon(polygon, &hex_outline(cell.hex, spacing));
                if clipped.len() >= 3 {
                    assigned.entry(cell.id).or_default().push(clipped);
                }
            }
        }
    }
}
pub fn build(
    document: &MapDocument,
    heights: HeightSettings,
    context: &JobContext,
) -> Result<TerrainGeometry> {
    heights.validate()?;
    context.report(0.94, "Conditioning valley channels and shorelines")?;
    let hydrology = Hydrology::build(document, context)?;
    let spacing = document.settings.spacing_km * 1000.;
    let mut waters = HashMap::new();
    let mut banks = HashMap::new();
    for (i, p) in hydrology.polygons.iter().enumerate() {
        if i % 512 == 0 {
            context.check()?;
        }
        assign_polygon(document, &p.vertices, &mut waters);
    }
    for c in &hydrology.channels {
        let length = (c.b[0] - c.a[0]).hypot(c.b[1] - c.a[1]);
        let n = [-(c.b[1] - c.a[1]) / length, (c.b[0] - c.a[0]) / length];
        for width in [
            0.,
            c.half_width * 0.5,
            c.half_width + c.bank_width * 0.5,
            c.half_width + c.bank_width,
        ] {
            // Thin ribbons also insert centerline and intermediate bed samples.
            let w = width.max(c.half_width * 0.05);
            let p = [(c.a, -1.), (c.b, -1.), (c.b, 1.), (c.a, 1.)].map(|(p, side)| WaterVertex {
                point: [p[0] + n[0] * w * side, p[1] + n[1] * w * side],
                level: 0.,
                edge: 0.,
            });
            assign_polygon(document, &p, &mut banks);
        }
    }
    let mut corners = BTreeMap::new();
    let mut chunks = Vec::new();
    let mut water_chunks = Vec::new();
    let mut all_triangles = vec![vec![]; document.cells.len()];
    let groups = document.chunk_ids();
    let total = groups.len();
    for (i, (id, ids)) in groups.into_iter().enumerate() {
        context.report(
            0.95 + 0.04 * i as f32 / total as f32,
            format!("Building terrain chunk {}/{}", i + 1, total),
        )?;
        let mut chunk = ChunkGeometry::new(id);
        let mut water = ChunkGeometry::new(id);
        let mut local = HashMap::new();
        for (n, cell_id) in ids.into_iter().enumerate() {
            if n % 64 == 0 {
                context.check()?;
            }
            let cell = &document.cells[cell_id];
            let base = cell_triangles(cell.hex, spacing);
            let water_polygons = waters.get(&cell.id);
            let bank_polygons = banks.get(&cell.id);
            let triangles = if water_polygons.is_none() && bank_polygons.is_none() {
                base
            } else {
                let mut points: BTreeSet<_> = base.iter().flatten().copied().collect();
                for poly in water_polygons
                    .into_iter()
                    .flatten()
                    .chain(bank_polygons.into_iter().flatten())
                {
                    points.extend(poly.iter().map(|v| vertex_key(v.point)));
                }
                let mut cdt = ConstrainedDelaunayTriangulation::<Point2<f64>>::new();
                let mut handles = HashMap::new();
                for key in points {
                    let p = vertex_point(key, spacing);
                    handles.insert(key, cdt.insert(Point2::new(p[0], p[1]))?);
                }
                let outline = hex_outline(cell.hex, spacing);
                for j in 0..6 {
                    let a = handles[&vertex_key(outline[j])];
                    let b = handles[&vertex_key(outline[(j + 1) % 6])];
                    cdt.add_constraint(a, b);
                }
                // Boundary constraints and water outlines keep triangles from
                // bridging narrow channels. Intersecting junction outlines use
                // the already-inserted vertices instead of crossing constraints.
                for poly in water_polygons
                    .into_iter()
                    .flatten()
                    .chain(bank_polygons.into_iter().flatten())
                {
                    for j in 0..poly.len() {
                        let a = handles[&vertex_key(poly[j].point)];
                        let b = handles[&vertex_key(poly[(j + 1) % poly.len()].point)];
                        if a != b && cdt.can_add_constraint(a, b) {
                            cdt.add_constraint(a, b);
                        }
                    }
                }
                cdt.inner_faces()
                    .filter_map(|f| {
                        let p = f.vertices().map(|v| [v.position().x, v.position().y]);
                        let centroid = [
                            (p[0][0] + p[1][0] + p[2][0]) / 3.,
                            (p[0][1] + p[1][1] + p[2][1]) / 3.,
                        ];
                        (point_hex(centroid, spacing) == cell.hex).then(|| p.map(vertex_key))
                    })
                    .collect()
            };
            for keys in &triangles {
                for &key in keys {
                    let index = if let Some(index) = local.get(&key) {
                        *index
                    } else {
                        let vertex = if let Some(v) = corners.get(&key) {
                            *v
                        } else {
                            let p = vertex_point(key, spacing);
                            let h = terrain_height(document, &hydrology, p)?;
                            let epsilon = (document.height_field.step_m * 0.08).max(1.);
                            let east =
                                terrain_height(document, &hydrology, [p[0] + epsilon, p[1]])?;
                            let west =
                                terrain_height(document, &hydrology, [p[0] - epsilon, p[1]])?;
                            let north =
                                terrain_height(document, &hydrology, [p[0], p[1] + epsilon])?;
                            let south =
                                terrain_height(document, &hydrology, [p[0], p[1] - epsilon])?;
                            let source_east = source_height(document, [p[0] + epsilon, p[1]])?;
                            let source_west = source_height(document, [p[0] - epsilon, p[1]])?;
                            let source_north = source_height(document, [p[0], p[1] + epsilon])?;
                            let source_south = source_height(document, [p[0], p[1] - epsilon])?;
                            let slope = (source_east - source_west)
                                .hypot(source_north - source_south)
                                / (2. * epsilon);
                            let vertex = Corner {
                                position: world_position(p, h, heights),
                                normal: normal([
                                    (-(heights.meters(east) - heights.meters(west))
                                        / (2. * epsilon))
                                        as f32,
                                    1.,
                                    ((heights.meters(north) - heights.meters(south))
                                        / (2. * epsilon))
                                        as f32,
                                ]),
                                weights: palette(document, p, slope),
                                raw_height: h,
                                raw_gradient: [
                                    ((east - west) / (2. * epsilon)) as f32,
                                    ((north - south) / (2. * epsilon)) as f32,
                                ],
                            };
                            corners.insert(key, vertex);
                            vertex
                        };
                        let index = chunk.positions.len() as u32;
                        chunk.positions.push(vertex.position);
                        chunk.normals.push(vertex.normal);
                        chunk.weights.push(vertex.weights);
                        chunk.boundaries.insert(key, vertex);
                        local.insert(key, index);
                        index
                    };
                    chunk.indices.push(index);
                }
                chunk.triangle_cells.push(cell.id);
            }
            all_triangles[cell_id] = triangles;
            for poly in water_polygons.into_iter().flatten() {
                for j in 1..poly.len() - 1 {
                    let verts = [poly[0], poly[j], poly[j + 1]];
                    let area = (verts[1].point[0] - verts[0].point[0])
                        * (verts[2].point[1] - verts[0].point[1])
                        - (verts[1].point[1] - verts[0].point[1])
                            * (verts[2].point[0] - verts[0].point[0]);
                    if area.abs() < 0.01 {
                        continue;
                    }
                    let verts = if area < 0. {
                        [verts[0], verts[2], verts[1]]
                    } else {
                        verts
                    };
                    let p = verts.map(|v| world_position(v.point, v.level + 0.5, heights));
                    let a = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
                    let b = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
                    let n = normal([
                        a[1] * b[2] - a[2] * b[1],
                        a[2] * b[0] - a[0] * b[2],
                        a[0] * b[1] - a[1] * b[0],
                    ]);
                    for (v, position) in verts.into_iter().zip(p) {
                        water.indices.push(water.positions.len() as u32);
                        water.positions.push(position);
                        water.normals.push(n);
                        water.weights.push([
                            0.025 + 0.018 * v.edge as f32,
                            0.17 + 0.025 * v.edge as f32,
                            0.20,
                            1.,
                        ]);
                    }
                    water.triangle_cells.push(cell.id);
                }
            }
        }
        chunks.push(chunk);
        if !water.indices.is_empty() {
            water_chunks.push(water);
        }
    }
    context.check()?;
    Ok(TerrainGeometry {
        chunks,
        water_chunks,
        corners,
        triangles: all_triangles,
        heights,
        hydrology,
    })
}
pub fn height_at(
    cell: &Cell,
    p: [f64; 2],
    _spacing: f64,
    geometry: &TerrainGeometry,
) -> Option<f32> {
    surface_height(cell, p, &geometry.triangles, &geometry.corners)
}
pub fn surface_height(
    cell: &Cell,
    p: [f64; 2],
    triangles: &[Vec<[VertexKey; 3]>],
    corners: &BTreeMap<VertexKey, Corner>,
) -> Option<f32> {
    let p = [(p[0] / 1000.) as f32, (-p[1] / 1000.) as f32];
    for keys in &triangles[cell.id as usize] {
        let [a, b, c] = keys.map(|k| corners[&k].position);
        let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        if d.abs() < 1e-12 {
            continue;
        }
        let wa = ((b[2] - c[2]) * (p[0] - c[0]) + (c[0] - b[0]) * (p[1] - c[2])) / d;
        let wb = ((c[2] - a[2]) * (p[0] - c[0]) + (a[0] - c[0]) * (p[1] - c[2])) / d;
        let wc = 1. - wa - wb;
        if wa >= -1e-5 && wb >= -1e-5 && wc >= -1e-5 {
            return Some(wa * a[1] + wb * b[1] + wc * c[1]);
        }
    }
    None
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
    let origin_m = [-47000., -52000.];
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
    let mut d = MapDocument {
        schema_version: 2,
        generator_version: "geometry-test".into(),
        settings,
        projection_wkt: projection.wkt,
        bounds_m,
        cells,
        sources: vec![],
        river_network: vec![],
        river_paths: vec![],
        heights: HeightSettings::default(),
        height_field: HeightField {
            origin_m,
            step_m: step,
            width,
            height,
            elevations_m,
            land_cover: vec![30; width * height],
        },
        index: Default::default(),
    };
    d.rebuild_index()?;
    Ok(d)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn seams(g: &TerrainGeometry) -> usize {
        let mut shared = BTreeMap::<VertexKey, Corner>::new();
        let mut n = 0;
        for c in &g.chunks {
            for (k, v) in &c.boundaries {
                if let Some(prev) = shared.insert(*k, *v) {
                    assert_eq!(prev.position, v.position);
                    assert_eq!(prev.normal, v.normal);
                    n += 1;
                }
            }
        }
        n
    }
    #[test]
    fn continuous_ridges_compress_monotonically_without_changing_dem() -> Result<()> {
        let d = fixture()?;
        let original = d.height_field.elevations_m.clone();
        let g = build(&d, d.heights, &JobContext::default())?;
        assert!(seams(&g) > 100);
        for h in [-100., 0., 100., 500., 1000., 4000.] {
            assert!(d.heights.meters(h + 1.) > d.heights.meters(h));
        }
        assert!(d.heights.meters(4000.) < 1200.);
        assert_eq!(original, d.height_field.elevations_m);
        for c in &d.cells {
            let k = vertex_key(c.center_m);
            let expected = d.heights.meters(source_height(&d, c.center_m)?) / 1000.;
            assert!((f64::from(g.corners[&k].position[1]) - expected).abs() < 1e-5);
        }
        Ok(())
    }
    #[test]
    fn rivers_are_separate_gentle_water_and_carved_beds_with_hex_picking() -> Result<()> {
        let mut d = fixture()?;
        d.river_paths.push(RiverPath {
            id: 1,
            next_down: 0,
            discharge: 50.,
            stream_order: 4,
            points_m: vec![[-5000., 15000.], [0., 0.], [5000., -15000.]],
        });
        let g = build(&d, d.heights, &JobContext::default())?;
        assert!(seams(&g) > 100);
        assert!(!g.water_chunks.is_empty());
        for c in &g.hydrology.channels {
            let length = (c.b[0] - c.a[0]).hypot(c.b[1] - c.a[1]);
            assert!(c.levels[0] >= c.levels[1] - 1e-6);
            assert!((c.levels[0] - c.levels[1]) / length <= 0.030001);
            let p = [(c.a[0] + c.b[0]) / 2., (c.a[1] + c.b[1]) / 2.];
            assert!(terrain_height(&d, &g.hydrology, p)? < g.hydrology.water_level(&d, p).unwrap());
        }
        for c in &g.water_chunks {
            assert!(c.normals.iter().all(|n| n[1] > 0.99));
            for (t, id) in c.indices.as_chunks::<3>().0.iter().zip(&c.triangle_cells) {
                let p = [
                    t.iter()
                        .map(|v| f64::from(c.positions[*v as usize][0]) * 1000.)
                        .sum::<f64>()
                        / 3.,
                    t.iter()
                        .map(|v| -f64::from(c.positions[*v as usize][2]) * 1000.)
                        .sum::<f64>()
                        / 3.,
                ];
                assert_eq!(point_hex(p, 2000.), d.cells[*id as usize].hex);
            }
        }
        let i = d.index[&hexx::Hex::ZERO];
        d.cells[i].elevation_m += 500.;
        let after = build(&d, d.heights, &JobContext::default())?;
        assert!(seams(&after) > 100);
        assert_ne!(
            g.corners[&outer_key(hexx::Hex::ZERO, 0, 2000.)].position,
            after.corners[&outer_key(hexx::Hex::ZERO, 0, 2000.)].position
        );
        Ok(())
    }
    #[test]
    fn snow_requires_source_snow_and_urban_ground_spans_cell() -> Result<()> {
        let mut d = fixture()?;
        let p = [0., 0.];
        let grass = palette(&d, p, 0.1);
        d.height_field.land_cover.fill(70);
        let snow = palette(&d, p, 0.1);
        assert!(snow[0] > grass[0] + 0.3);
        d.height_field.land_cover.fill(30);
        let i = d.index[&hexx::Hex::ZERO];
        d.cells[i].urban = Some(UrbanTerrain {
            population: 100000,
            style: UrbanStyle::Mixed,
        });
        assert!(palette(&d, [600., 0.], 0.1)[3] > 0.8);
        Ok(())
    }
}
