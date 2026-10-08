//! Continuous DEM surface with local river refinement and hex-owned triangles.
use crate::{
    elevation::{DisplayVertex, ReliefField},
    hydrology::{Hydrology, WaterVertex, clip_polygon},
    jobs::JobContext,
    map_core::*,
};
use anyhow::Result;
use geo::{Contains, Coord, LineString, MultiPolygon, Polygon, algorithm::unary_union};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};
use std::collections::{BTreeMap, BTreeSet, HashMap};
pub const SUBDIVISIONS: i32 = 4;
/// Integer coordinates in the normalized hex basis. Hex edges are exactly
/// vertical or diagonal, so clipped vertices can share exact straight edges.
pub type VertexKey = (i64, i64);
const KEY_SCALE: f64 = 1_000_000.;
#[derive(Debug, Clone, Copy)]
pub struct Corner {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub weights: [f32; 4],
    pub materials: [f32; 4],
    pub display: DisplayVertex,
}
#[derive(Debug)]
pub struct ChunkGeometry {
    pub id: (i32, i32),
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Linear RGB and urban coverage (land), or water color and shallowness (water).
    pub weights: Vec<[f32; 4]>,
    /// Grass, soil, gravel and rock weights from source cover and slope.
    pub materials: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub triangle_cells: Vec<u32>,
    pub boundaries: BTreeMap<VertexKey, Corner>,
    pub display: Vec<DisplayVertex>,
}
impl ChunkGeometry {
    fn new(id: (i32, i32)) -> Self {
        Self {
            id,
            positions: vec![],
            normals: vec![],
            weights: vec![],
            materials: vec![],
            indices: vec![],
            triangle_cells: vec![],
            boundaries: BTreeMap::new(),
            display: vec![],
        }
    }
}
pub struct TerrainGeometry {
    pub chunks: Vec<ChunkGeometry>,
    pub water_chunks: Vec<ChunkGeometry>,
    pub corners: BTreeMap<VertexKey, Corner>,
    pub triangles: Vec<Vec<[VertexKey; 3]>>,
    pub outlines: Vec<Vec<VertexKey>>,
    pub heights: HeightSettings,
    pub hydrology: Hydrology,
}
pub fn vertex_key(point: [f64; 2], spacing: f64) -> VertexKey {
    (
        (point[0] * 2. * KEY_SCALE / spacing).round() as i64,
        (point[1] * 2. * 3_f64.sqrt() * KEY_SCALE / spacing).round() as i64,
    )
}
pub fn vertex_point(key: VertexKey, spacing: f64) -> [f64; 2] {
    [
        key.0 as f64 * spacing / (2. * KEY_SCALE),
        key.1 as f64 * spacing / (2. * 3_f64.sqrt() * KEY_SCALE),
    ]
}
pub fn outer_key(hex: hexx::Hex, corner: usize, _spacing: f64) -> VertexKey {
    let k = corner_key(hex, corner);
    (
        i64::from(k.0) * KEY_SCALE as i64,
        i64::from(k.1) * KEY_SCALE as i64,
    )
}
fn snap_key(key: VertexKey, hex: hexx::Hex, spacing: f64) -> VertexKey {
    for i in 0..6 {
        let (a, b) = (
            outer_key(hex, i, spacing),
            outer_key(hex, (i + 1) % 6, spacing),
        );
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (x, y) = (key.0 - a.0, key.1 - a.1);
        let t = (x as f64 * dx as f64 + y as f64 * dy as f64)
            / (dx as f64 * dx as f64 + dy as f64 * dy as f64);
        if !(0. ..=1.).contains(&t) {
            continue;
        }
        let candidate = if dx == 0 {
            (a.0, key.1.clamp(a.1.min(b.1), a.1.max(b.1)))
        } else {
            let step = ((x as f64 * dx.signum() as f64 + y as f64 * dy.signum() as f64) * 0.5)
                .round() as i64;
            (a.0 + dx.signum() * step, a.1 + dy.signum() * step)
        };
        let [p, q] = [key, candidate].map(|k| vertex_point(k, spacing));
        if (p[0] - q[0]).hypot(p[1] - q[1]) <= 0.02 {
            return candidate;
        }
    }
    key
}
pub fn cell_triangles(hex: hexx::Hex, _spacing: f64) -> Vec<[VertexKey; 3]> {
    let center = (2 * hex.x + hex.y, 3 * hex.y);
    let mut triangles = Vec::new();
    for sector in 0..6 {
        let a = CORNER_OFFSETS[sector];
        let b = CORNER_OFFSETS[(sector + 1) % 6];
        let key = |i, j| {
            (
                (i64::from(center.0 * SUBDIVISIONS + a.0 * i + b.0 * j) * KEY_SCALE as i64)
                    / i64::from(SUBDIVISIONS),
                (i64::from(center.1 * SUBDIVISIONS + a.1 * i + b.1 * j) * KEY_SCALE as i64)
                    / i64::from(SUBDIVISIONS),
            )
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
fn palette(
    document: &MapDocument,
    p: [f64; 2],
    slope: f64,
    hydrology: Option<&Hydrology>,
) -> [f32; 4] {
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
    let wet = hydrology.map_or(0., |h| h.bank_moisture(document, p)) * (1. - urban);
    for k in 0..3 {
        rgb[k] = rgb[k] * (1. - wet * 0.45) + [0.16, 0.13, 0.085][k] * wet * 0.25;
    }
    [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, urban as f32]
}
fn material_weights(
    document: &MapDocument,
    p: [f64; 2],
    slope: f64,
    hydrology: &Hydrology,
) -> [f32; 4] {
    let c = document.height_field.cover_weights(p);
    let rock = ((slope - 0.25) / 0.65).clamp(0., 0.9) * (1. - c[6] - c[4]).max(0.);
    let wet = hydrology.bank_moisture(document, p);
    let grass =
        (c[0] * 0.5 + c[1] * 0.5 + c[2] + c[3] * 0.8 + c[8] * 0.5 + c[9] * 0.4 + c[10] * 0.5)
            * (1. - rock)
            * (1. - wet * 0.6);
    let exposed = c[5] * (1. - rock);
    let soil = ((1. - c[4] - c[6]) - grass - exposed - rock).max(0.);
    [
        grass as f32,
        soil as f32,
        (exposed * 0.65) as f32,
        (rock + exposed * 0.35) as f32,
    ]
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
                let mut clipped = clip_polygon(polygon, &hex_outline(cell.hex, spacing));
                for v in &mut clipped {
                    v.point = vertex_point(
                        snap_key(vertex_key(v.point, spacing), cell.hex, spacing),
                        spacing,
                    );
                }
                if clipped.len() >= 3 {
                    assigned.entry(cell.id).or_default().push(clipped);
                }
            }
        }
    }
}
/// Remove internal cap/ribbon/lake edges before producing any visible surface.
/// Normalize winding so the union accumulates overlapping input footprints.
fn water_footprint(
    polygons: &[Vec<WaterVertex>],
    hex: hexx::Hex,
    spacing: f64,
) -> MultiPolygon<f64> {
    let input: Vec<_> = polygons
        .iter()
        .filter_map(|poly| {
            let mut points: Vec<_> = poly
                .iter()
                .map(|v| {
                    let key = snap_key(vertex_key(v.point, spacing), hex, spacing);
                    Coord {
                        x: key.0 as f64,
                        y: key.1 as f64,
                    }
                })
                .collect();
            points.dedup();
            if points.first() == points.last() {
                points.pop();
            }
            if points.len() < 3 {
                return None;
            }
            let area: f64 = (0..points.len())
                .map(|i| {
                    let a = points[i];
                    let b = points[(i + 1) % points.len()];
                    a.x * b.y - a.y * b.x
                })
                .sum();
            if area.abs() < 0.01 {
                return None;
            }
            if area < 0. {
                points.reverse();
            }
            Some(Polygon::new(LineString::new(points), vec![]))
        })
        .collect();
    let mut footprint = unary_union(&input);
    // Match the same canonical coordinates used by terrain, picking and seams.
    for polygon in &mut footprint.0 {
        let snap = |ring: &mut LineString<f64>| {
            for c in &mut ring.0 {
                let p = vertex_point(
                    snap_key((c.x.round() as i64, c.y.round() as i64), hex, spacing),
                    spacing,
                );
                *c = Coord { x: p[0], y: p[1] };
            }
            ring.0.dedup();
        };
        polygon.exterior_mut(snap);
        polygon.interiors_mut(|rings| rings.iter_mut().for_each(snap));
    }
    footprint
}
type EdgeKey = (VertexKey, VertexKey);
fn edge_key(a: VertexKey, b: VertexKey) -> EdgeKey {
    if a < b { (a, b) } else { (b, a) }
}
fn edge_parameter(p: VertexKey, a: VertexKey, b: VertexKey) -> Option<f64> {
    let (x, y) = (i128::from(p.0 - a.0), i128::from(p.1 - a.1));
    let (dx, dy) = (i128::from(b.0 - a.0), i128::from(b.1 - a.1));
    let dot = x * dx + y * dy;
    let length = dx * dx + dy * dy;
    (x * dy - y * dx == 0 && (0..=length).contains(&dot)).then_some(dot as f64 / length as f64)
}

fn shared_edges(
    document: &MapDocument,
    footprints: &HashMap<u32, MultiPolygon<f64>>,
    banks: &HashMap<u32, Vec<Vec<WaterVertex>>>,
) -> BTreeMap<EdgeKey, BTreeSet<VertexKey>> {
    let spacing = document.settings.spacing_km * 1000.;
    let mut edges = BTreeMap::<EdgeKey, BTreeSet<VertexKey>>::new();
    for cell in &document.cells {
        let outline = (0..6)
            .map(|i| outer_key(cell.hex, i, spacing))
            .collect::<Vec<_>>();
        let mut points: BTreeSet<_> = cell_triangles(cell.hex, spacing)
            .into_iter()
            .flatten()
            .collect();
        if let Some(water) = footprints.get(&cell.id) {
            points.extend(
                water
                    .0
                    .iter()
                    .flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors()))
                    .flat_map(|r| r.0.iter().map(|c| vertex_key([c.x, c.y], spacing))),
            );
        }
        points.extend(
            banks
                .get(&cell.id)
                .into_iter()
                .flatten()
                .flatten()
                .map(|v| vertex_key(v.point, spacing)),
        );
        for i in 0..6 {
            let (a, b) = (outline[i], outline[(i + 1) % 6]);
            let edge = edges.entry(edge_key(a, b)).or_default();
            edge.extend(
                points
                    .iter()
                    .copied()
                    .filter(|p| edge_parameter(*p, a, b).is_some()),
            );
        }
    }
    edges
}
fn boundary_chain(
    hex: hexx::Hex,
    spacing: f64,
    edges: &BTreeMap<EdgeKey, BTreeSet<VertexKey>>,
) -> Vec<VertexKey> {
    let mut boundary = Vec::new();
    for i in 0..6 {
        let (a, b) = (
            outer_key(hex, i, spacing),
            outer_key(hex, (i + 1) % 6, spacing),
        );
        let mut keys: Vec<_> = edges[&edge_key(a, b)].iter().copied().collect();
        keys.sort_by(|p, q| {
            edge_parameter(*p, a, b)
                .unwrap()
                .total_cmp(&edge_parameter(*q, a, b).unwrap())
                .then_with(|| p.cmp(q))
        });
        boundary.extend(keys.into_iter().filter(|k| *k != b));
    }
    boundary.dedup();
    boundary
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
    let relief = ReliefField::build(&document.height_field, spacing);
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
    let footprints: HashMap<_, _> = waters
        .iter()
        .map(|(id, p)| {
            (
                *id,
                water_footprint(p, document.cells[*id as usize].hex, spacing),
            )
        })
        .collect();
    let edges = shared_edges(document, &footprints, &banks);
    let mut corners = BTreeMap::new();
    let mut water_corners = BTreeMap::<VertexKey, Corner>::new();
    let mut chunks = Vec::new();
    let mut water_chunks = Vec::new();
    let mut all_triangles = vec![vec![]; document.cells.len()];
    let mut outlines = vec![vec![]; document.cells.len()];
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
            let bank_polygons = banks.get(&cell.id);
            let empty = MultiPolygon::<f64>::new(vec![]);
            let footprint = footprints.get(&cell.id).unwrap_or(&empty);
            let boundary = boundary_chain(cell.hex, spacing, &edges);
            let boundary_polygon = Polygon::new(
                LineString::from(
                    boundary
                        .iter()
                        .map(|k| (k.0 as f64, k.1 as f64))
                        .collect::<Vec<_>>(),
                ),
                vec![],
            );
            let water_rings: Vec<_> = footprint
                .0
                .iter()
                .flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors()))
                .map(|r| {
                    r.0.iter()
                        .map(|c| vertex_key([c.x, c.y], spacing))
                        .collect::<Vec<_>>()
                })
                .collect();
            let triangles: Vec<[VertexKey; 3]> = {
                let mut points: BTreeSet<_> = base.iter().flatten().copied().collect();
                points.extend(boundary.iter().copied());
                points.extend(water_rings.iter().flatten().copied());
                for poly in bank_polygons.into_iter().flatten() {
                    points.extend(poly.iter().map(|v| vertex_key(v.point, spacing)));
                }
                let mut cdt = ConstrainedDelaunayTriangulation::<Point2<f64>>::new();
                let mut handles = HashMap::new();
                for key in points {
                    handles.insert(key, cdt.insert(Point2::new(key.0 as f64, key.1 as f64))?);
                }
                for j in 0..boundary.len() {
                    let a = handles[&boundary[j]];
                    let b = handles[&boundary[(j + 1) % boundary.len()]];
                    if a != b && cdt.can_add_constraint(a, b) {
                        cdt.add_constraint(a, b);
                    }
                }
                // Union outlines have no internal crossings. Insert them before
                // bank detail constraints so no face bridges a water boundary.
                for ring in &water_rings {
                    for pair in ring.windows(2) {
                        let a = handles[&pair[0]];
                        let b = handles[&pair[1]];
                        if a != b && cdt.can_add_constraint(a, b) {
                            cdt.add_constraint(a, b);
                        }
                    }
                }
                for poly in bank_polygons.into_iter().flatten() {
                    for j in 0..poly.len() {
                        let a = handles[&vertex_key(poly[j].point, spacing)];
                        let b = handles[&vertex_key(poly[(j + 1) % poly.len()].point, spacing)];
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
                        boundary_polygon
                            .contains(&geo::Point::from(centroid))
                            .then(|| p.map(|p| (p[0].round() as i64, p[1].round() as i64)))
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
                            let samples = [
                                (p, h),
                                ([p[0] + epsilon, p[1]], east),
                                ([p[0] - epsilon, p[1]], west),
                                ([p[0], p[1] + epsilon], north),
                                ([p[0], p[1] - epsilon], south),
                            ]
                            .map(|(point, height)| {
                                relief.sample(document, &hydrology, point, height)
                            });
                            let display = DisplayVertex { samples, epsilon };
                            let mut position = world_position(p, h, heights);
                            position[1] = (display.meters(heights) / 1000.) as f32;
                            let vertex = Corner {
                                position,
                                normal: display.normal(heights),
                                weights: palette(document, p, slope, Some(&hydrology)),
                                materials: material_weights(document, p, slope, &hydrology),
                                display,
                            };
                            corners.insert(key, vertex);
                            vertex
                        };
                        let index = chunk.positions.len() as u32;
                        chunk.positions.push(vertex.position);
                        chunk.normals.push(vertex.normal);
                        chunk.weights.push(vertex.weights);
                        chunk.materials.push(vertex.materials);
                        chunk.display.push(vertex.display);
                        chunk.boundaries.insert(key, vertex);
                        local.insert(key, index);
                        index
                    };
                    chunk.indices.push(index);
                }
                chunk.triangle_cells.push(cell.id);
            }
            // Both land and water use one refined planar triangulation. A face
            // is rendered as water only inside the union of all wet footprints.
            for keys in &triangles {
                let keys = *keys;
                let points = keys.map(|k| vertex_point(k, spacing));
                let centroid = [
                    points.iter().map(|p| p[0]).sum::<f64>() / 3.,
                    points.iter().map(|p| p[1]).sum::<f64>() / 3.,
                ];
                if !footprint.contains(&geo::Point::from(centroid)) {
                    continue;
                }
                let area = (points[1][0] - points[0][0]) * (points[2][1] - points[0][1])
                    - (points[1][1] - points[0][1]) * (points[2][0] - points[0][0]);
                if area.abs() < 0.01 {
                    continue;
                }
                let keys = if area < 0. {
                    [keys[0], keys[2], keys[1]]
                } else {
                    keys
                };
                let mut vertices = Vec::with_capacity(3);
                for key in keys {
                    let vertex = if let Some(v) = water_corners.get(&key) {
                        *v
                    } else {
                        let p = vertex_point(key, spacing);
                        let (level, edge) =
                            hydrology.surface_sample(document, p).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Water footprint has no reference level at {p:?} in hex {:?}",
                                    cell.hex
                                )
                            })?;
                        let epsilon = (document.height_field.step_m * 0.08).max(1.);
                        // Dry samples outside the wet footprint must not tilt
                        // a shoreline normal toward a different nearby pool.
                        let sample = |q| hydrology.water_level(document, q).unwrap_or(level);
                        let gradient = [
                            (sample([p[0] + epsilon, p[1]]) - sample([p[0] - epsilon, p[1]]))
                                / (2. * epsilon),
                            (sample([p[0], p[1] + epsilon]) - sample([p[0], p[1] - epsilon]))
                                / (2. * epsilon),
                        ];
                        let display = DisplayVertex::water(level + 0.5, gradient);
                        let vertex = Corner {
                            position: world_position(p, level + 0.5, heights),
                            normal: display.normal(heights),
                            weights: [
                                0.018 + 0.025 * edge as f32,
                                0.10 + 0.045 * edge as f32,
                                0.14 + 0.02 * edge as f32,
                                edge as f32,
                            ],
                            materials: [0.; 4],
                            display,
                        };
                        water_corners.insert(key, vertex);
                        vertex
                    };
                    vertices.push((key, vertex));
                }
                // Meter coordinates become f32 kilometers on the GPU. Discard
                // faces that collapse at that precision before they reach it.
                let [a, b, c] = std::array::from_fn::<_, 3, _>(|i| vertices[i].1.position);
                let area_gpu = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
                let centroid_gpu = [
                    (f64::from(a[0]) + f64::from(b[0]) + f64::from(c[0])) * 1000. / 3.,
                    -(f64::from(a[2]) + f64::from(b[2]) + f64::from(c[2])) * 1000. / 3.,
                ];
                if area_gpu >= 0. || point_hex(centroid_gpu, spacing) != cell.hex {
                    continue;
                }
                for (key, v) in vertices {
                    water.indices.push(water.positions.len() as u32);
                    water.positions.push(v.position);
                    water.normals.push(v.normal);
                    water.weights.push(v.weights);
                    water.materials.push(v.materials);
                    water.display.push(v.display);
                    water.boundaries.insert(key, v);
                }
                water.triangle_cells.push(cell.id);
            }
            all_triangles[cell_id] = triangles;
            outlines[cell_id] = boundary;
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
        outlines,
        heights,
        hydrology,
    })
}
/// Check edge connectivity, including vertices introduced by river refinement.
/// Matching coordinates alone do not detect a missing boundary subdivision.
pub fn validate_topology(g: &TerrainGeometry) -> Result<()> {
    for (id, (triangles, outline)) in g.triangles.iter().zip(&g.outlines).enumerate() {
        let mut counts = BTreeMap::<[VertexKey; 2], usize>::new();
        for triangle in triangles {
            for i in 0..3 {
                let mut edge = [triangle[i], triangle[(i + 1) % 3]];
                edge.sort();
                *counts.entry(edge).or_default() += 1;
            }
        }
        anyhow::ensure!(
            counts.values().all(|n| *n <= 2),
            "Overlapping terrain faces in cell {id}"
        );
        let actual: BTreeSet<_> = counts
            .into_iter()
            .filter_map(|(e, n)| (n == 1).then_some(e))
            .collect();
        let expected: BTreeSet<_> = (0..outline.len())
            .map(|i| {
                let mut e = [outline[i], outline[(i + 1) % outline.len()]];
                e.sort();
                e
            })
            .collect();
        anyhow::ensure!(
            actual == expected,
            "Terrain boundary connectivity mismatch in cell {id}"
        );
    }
    Ok(())
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
    use geo::Area;
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
    fn closed_boundaries(g: &TerrainGeometry) {
        validate_topology(g).unwrap();
    }

    #[test]
    fn continuous_ridges_compress_monotonically_without_changing_dem() -> Result<()> {
        let d = fixture()?;
        let original = d.height_field.elevations_m.clone();
        let g = build(&d, d.heights, &JobContext::default())?;
        assert!(seams(&g) > 100);
        closed_boundaries(&g);
        for h in [-100., 0., 100., 500., 1000., 4000.] {
            assert!(d.heights.meters(h + 1.) > d.heights.meters(h));
        }
        assert!(d.heights.meters(4000.) > 3500. && d.heights.meters(4000.) < 5000.);
        assert_eq!(original, d.height_field.elevations_m);
        for c in &d.cells {
            let k = vertex_key(c.center_m, d.settings.spacing_km * 1000.);
            let expected = g.corners[&k].display.meters(d.heights) / 1000.;
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
        closed_boundaries(&g);
        assert!(!g.water_chunks.is_empty());
        for c in &g.hydrology.channels {
            let length = (c.b[0] - c.a[0]).hypot(c.b[1] - c.a[1]);
            assert!(c.levels[0] >= c.levels[1] - 1e-6);
            assert!((c.levels[0] - c.levels[1]) / length <= 0.100001);
            let p = [(c.a[0] + c.b[0]) / 2., (c.a[1] + c.b[1]) / 2.];
            assert!(terrain_height(&d, &g.hydrology, p)? < g.hydrology.water_level(&d, p).unwrap());
        }
        for c in &g.water_chunks {
            assert!(c.normals.iter().all(|n| n[1] > 0.97));
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
        closed_boundaries(&after);
        assert_ne!(
            g.corners[&outer_key(hexx::Hex::ZERO, 0, 2000.)].position,
            after.corners[&outer_key(hexx::Hex::ZERO, 0, 2000.)].position
        );
        Ok(())
    }
    #[test]
    fn river_bends_and_lakes_have_single_coverage_and_shared_water_vertices() -> Result<()> {
        let mut d = fixture()?;
        for i in 0..d.height_field.elevations_m.len() {
            let p = d.height_field.point(i);
            if p[0].abs() <= 3500. && p[1].abs() <= 3500. {
                d.height_field.land_cover[i] = 80;
                d.height_field.elevations_m[i] = 100.;
            }
        }
        d.river_paths.push(RiverPath {
            id: 1,
            next_down: 0,
            discharge: 100.,
            stream_order: 5,
            points_m: vec![[-6000., 1500.], [-2000., 0.], [2000., 0.], [6000., -1500.]],
        });
        let original = d.height_field.elevations_m.clone();
        let g = build(&d, d.heights, &JobContext::default())?;
        let mut areas = HashMap::<u32, f64>::new();
        let mut faces = BTreeSet::new();
        let mut vertices = BTreeMap::<VertexKey, ([f32; 3], [f32; 3], [f32; 4])>::new();
        for c in &g.water_chunks {
            for (t, id) in c.indices.as_chunks::<3>().0.iter().zip(&c.triangle_cells) {
                let p = t.map(|i| c.positions[i as usize]);
                let area = ((f64::from(p[1][0]) - f64::from(p[0][0]))
                    * (f64::from(p[2][2]) - f64::from(p[0][2]))
                    - (f64::from(p[1][2]) - f64::from(p[0][2]))
                        * (f64::from(p[2][0]) - f64::from(p[0][0])))
                .abs()
                    * 500_000.;
                *areas.entry(*id).or_default() += area;
                let mut keys = p.map(|p| {
                    vertex_key([f64::from(p[0]) * 1000., -f64::from(p[2]) * 1000.], 2000.)
                });
                keys.sort();
                assert!(faces.insert(keys), "Duplicate water face");
            }
            for (k, v) in &c.boundaries {
                let values = (v.position, v.normal, v.weights);
                if let Some(previous) = vertices.insert(*k, values) {
                    assert_eq!(previous, values, "Water seam mismatch");
                }
            }
        }
        for hex in [
            hexx::Hex::ZERO,
            hexx::Hex::new(-3, 1),
            hexx::Hex::new(3, -1),
        ] {
            let c = d.cell(hex).unwrap();
            let outline = hex_outline(hex, 2000.);
            let clipped: Vec<_> = g
                .hydrology
                .polygons
                .iter()
                .map(|p| clip_polygon(&p.vertices, &outline))
                .filter(|p| p.len() >= 3)
                .collect();
            let expected = water_footprint(&clipped, hex, 2000.).unsigned_area();
            let actual = areas.get(&c.id).copied().unwrap_or(0.);
            assert!(
                (actual - expected).abs() < expected * 0.0001 + 1.,
                "Water area {actual} differs from union {expected}"
            );
        }
        // No false shoreline stripe survives inside the lake's central hex.
        let center_id = d.cell(hexx::Hex::ZERO).unwrap().id;
        for c in &g.water_chunks {
            for (t, id) in c.indices.as_chunks::<3>().0.iter().zip(&c.triangle_cells) {
                if *id == center_id {
                    for i in t {
                        assert!(c.weights[*i as usize][3] < 1e-5);
                        assert!((c.display[*i as usize].samples[0].height - 100.5).abs() < 1e-6);
                    }
                }
            }
        }
        assert_eq!(original, d.height_field.elevations_m);
        Ok(())
    }
    #[test]
    fn snow_requires_source_snow_and_urban_ground_spans_cell() -> Result<()> {
        let mut d = fixture()?;
        let p = [0., 0.];
        let grass = palette(&d, p, 0.1, None);
        d.height_field.land_cover.fill(70);
        let snow = palette(&d, p, 0.1, None);
        assert!(snow[0] > grass[0] + 0.3);
        d.height_field.land_cover.fill(30);
        let i = d.index[&hexx::Hex::ZERO];
        d.cells[i].urban = Some(UrbanTerrain {
            population: 100000,
            style: UrbanStyle::Mixed,
        });
        assert!(palette(&d, [600., 0.], 0.1, None)[3] > 0.8);
        Ok(())
    }
}
