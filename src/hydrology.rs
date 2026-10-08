//! Derived valley channels and source-mask water bodies. Source geometry and DEM
//! samples remain unchanged; conditioning only affects the rendered landscape.
use crate::{jobs::JobContext, map_core::*};
use anyhow::Result;
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap, VecDeque},
};

#[derive(Clone, Copy, Debug)]
pub struct WaterVertex {
    pub point: [f64; 2],
    pub level: f64,
    pub edge: f64,
}
#[derive(Clone, Debug)]
pub struct WaterPolygon {
    pub vertices: Vec<WaterVertex>,
}
#[derive(Clone, Copy, Debug)]
pub struct Channel {
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub levels: [f64; 2],
    pub half_width: f64,
    pub bank_width: f64,
}
#[derive(Default)]
pub struct Hydrology {
    pub channels: Vec<Channel>,
    pub polygons: Vec<WaterPolygon>,
    buckets: HashMap<(i32, i32), Vec<usize>>,
    bucket_size: f64,
    lake_levels: Vec<Option<f64>>,
}
#[derive(Clone, Copy)]
struct Queue(f64, usize);
impl PartialEq for Queue {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1
    }
}
impl Eq for Queue {}
impl PartialOrd for Queue {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Queue {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then_with(|| o.1.cmp(&self.1))
    }
}
fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}
fn smooth(t: f64) -> f64 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

impl Hydrology {
    pub fn build(document: &MapDocument, context: &JobContext) -> Result<Self> {
        let field = &document.height_field;
        let spacing = document.settings.spacing_km * 1000.;
        let mut result = Self {
            bucket_size: spacing,
            lake_levels: vec![None; field.elevations_m.len()],
            ..Default::default()
        };
        // Connected WorldCover open-water samples define flat bodies. Reject
        // DEM-contaminated shoreline samples rather than painting steep walls.
        let mut seen = vec![false; field.elevations_m.len()];
        for start in 0..seen.len() {
            if seen[start] || field.land_cover[start] != 80 {
                continue;
            }
            let mut queue = VecDeque::from([start]);
            let mut component = Vec::new();
            seen[start] = true;
            while let Some(i) = queue.pop_front() {
                component.push(i);
                let x = i % field.width;
                let y = i / field.width;
                for j in [
                    x.checked_sub(1).map(|_| i - 1),
                    (x + 1 < field.width).then_some(i + 1),
                    y.checked_sub(1).map(|_| i - field.width),
                    (y + 1 < field.height).then_some(i + field.width),
                ]
                .into_iter()
                .flatten()
                {
                    if !seen[j] && field.land_cover[j] == 80 {
                        seen[j] = true;
                        queue.push_back(j);
                    }
                }
            }
            let mut heights: Vec<_> = component
                .iter()
                .map(|i| f64::from(field.elevations_m[*i]))
                .collect();
            heights.sort_by(f64::total_cmp);
            let level = heights[heights.len() / 2];
            let edit_offset = component
                .iter()
                .map(|i| crate::terrain::edit_delta(document, field.point(*i)))
                .sum::<f64>()
                / component.len() as f64;
            for i in component {
                if (f64::from(field.elevations_m[i]) - level).abs() <= 15. {
                    result.lake_levels[i] = Some(level + edit_offset);
                }
            }
        }
        // Each grid square is split consistently, then clipped at a coverage of
        // 0.5. Shorelines follow the source mask, independently of hex edges.
        for y in 0..field.height - 1 {
            context.check()?;
            for x in 0..field.width - 1 {
                let i = y * field.width + x;
                for tri in [
                    [i, i + 1, i + field.width + 1],
                    [i, i + field.width + 1, i + field.width],
                ] {
                    let levels: Vec<_> =
                        tri.iter().filter_map(|j| result.lake_levels[*j]).collect();
                    if levels.is_empty() {
                        continue;
                    }
                    let level = levels.iter().sum::<f64>() / levels.len() as f64;
                    let mut polygon = Vec::new();
                    for k in 0..3 {
                        let a = tri[k];
                        let b = tri[(k + 1) % 3];
                        let inside_a = result.lake_levels[a].is_some();
                        let inside_b = result.lake_levels[b].is_some();
                        if inside_a {
                            polygon.push(WaterVertex {
                                point: field.point(a),
                                level,
                                edge: 0.,
                            });
                        }
                        if inside_a != inside_b {
                            polygon.push(WaterVertex {
                                point: lerp(field.point(a), field.point(b), 0.5),
                                level,
                                edge: 1.,
                            });
                        }
                    }
                    if polygon.len() >= 3 {
                        result.polygons.push(WaterPolygon { vertices: polygon });
                    }
                }
            }
        }
        let mut node_index = HashMap::new();
        let mut positions = Vec::new();
        let mut levels = Vec::new();
        let mut graph: Vec<Vec<(usize, f64)>> = Vec::new();
        let mut runs = Vec::new();
        let bounds = [
            field.origin_m[0],
            field.origin_m[1],
            field.origin_m[0] + (field.width - 1) as f64 * field.step_m,
            field.origin_m[1] + (field.height - 1) as f64 * field.step_m,
        ];
        let by_id: HashMap<_, _> = document.river_paths.iter().map(|p| (p.id, p)).collect();
        for (n, path) in document.river_paths.iter().enumerate() {
            if n % 128 == 0 {
                context.check()?;
            }
            let mut points = path.points_m.clone();
            let first = points[0];
            let last = *points.last().unwrap();
            let reverse = if let Some(down) = by_id.get(&path.next_down) {
                let endpoints = [down.points_m[0], *down.points_m.last().unwrap()];
                endpoints
                    .iter()
                    .map(|p| distance(first, *p))
                    .fold(f64::INFINITY, f64::min)
                    < endpoints
                        .iter()
                        .map(|p| distance(last, *p))
                        .fold(f64::INFINITY, f64::min)
            } else {
                field.sample(first).unwrap_or(f64::INFINITY)
                    < field.sample(last).unwrap_or(f64::INFINITY)
            };
            if reverse {
                points.reverse();
            }
            let half_width = (spacing * 0.035 + path.discharge.sqrt() * 2.)
                .clamp(spacing * 0.04, spacing * 0.12);
            for pair in points.windows(2) {
                let Some((a, b)) = clip_segment(pair[0], pair[1], bounds) else {
                    continue;
                };
                let length = distance(a, b);
                if length < 0.1 {
                    continue;
                }
                let count = (length / (field.step_m * 0.5)).ceil().max(1.) as usize;
                let mut previous: Option<usize> = None;
                for k in 0..=count {
                    let p = lerp(a, b, k as f64 / count as f64);
                    let key = (p[0].round() as i64, p[1].round() as i64);
                    let index = if let Some(index) = node_index.get(&key) {
                        *index
                    } else {
                        let index = positions.len();
                        positions.push(p);
                        node_index.insert(key, index);
                        graph.push(vec![]);
                        // A local cross-section minimum absorbs modest source
                        // alignment error without rerouting the GIS centerline.
                        let v = [-(b[1] - a[1]) / length, (b[0] - a[0]) / length];
                        let mut h = crate::terrain::source_height(document, p)?;
                        for offset in [-1., -0.5, 0.5, 1.] {
                            let q = [
                                p[0] + v[0] * half_width * offset,
                                p[1] + v[1] * half_width * offset,
                            ];
                            if let Some(value) = field.sample(q) {
                                h = h.min(value + crate::terrain::edit_delta(document, q));
                            }
                        }
                        if let Some(lake) = result.lake(document, p) {
                            h = h.min(lake.0);
                        }
                        levels.push(h);
                        index
                    };
                    if let Some(prev) = previous
                        && prev != index
                    {
                        let length = distance(positions[prev], p);
                        graph[prev].push((index, 0.));
                        graph[index].push((prev, length * 0.03));
                        runs.push((prev, index, half_width));
                    }
                    previous = Some(index);
                }
            }
        }
        let mut pending = BinaryHeap::new();
        for (i, h) in levels.iter().enumerate() {
            pending.push(Queue(*h, i));
        }
        while let Some(Queue(h, i)) = pending.pop() {
            if h > levels[i] {
                continue;
            }
            for &(j, cost) in &graph[i] {
                if h + cost + 1e-7 < levels[j] {
                    levels[j] = h + cost;
                    pending.push(Queue(levels[j], j));
                }
            }
        }
        for (a, b, half_width) in runs {
            let channel = Channel {
                a: positions[a],
                b: positions[b],
                levels: [levels[a], levels[b]],
                half_width,
                bank_width: spacing * 0.22,
            };
            let length = distance(channel.a, channel.b);
            let normal = [
                -(channel.b[1] - channel.a[1]) / length,
                (channel.b[0] - channel.a[0]) / length,
            ];
            let mut vertices = Vec::new();
            for (p, h, side) in [
                (channel.a, levels[a], -1.),
                (channel.b, levels[b], -1.),
                (channel.b, levels[b], 1.),
                (channel.a, levels[a], 1.),
            ] {
                vertices.push(WaterVertex {
                    point: [
                        p[0] + normal[0] * half_width * side,
                        p[1] + normal[1] * half_width * side,
                    ],
                    level: h + 0.5,
                    edge: 1.,
                });
            }
            result.polygons.push(WaterPolygon { vertices });
            // Rounded joins close bends and tributary junctions without miters.
            for (p, h) in [(channel.a, levels[a]), (channel.b, levels[b])] {
                let vertices = (0..8)
                    .map(|i| {
                        let angle = i as f64 * std::f64::consts::TAU / 8.;
                        WaterVertex {
                            point: [
                                p[0] + half_width * angle.cos(),
                                p[1] + half_width * angle.sin(),
                            ],
                            level: h + 0.5,
                            edge: 1.,
                        }
                    })
                    .collect();
                result.polygons.push(WaterPolygon { vertices });
            }
            result.channels.push(channel);
        }
        for (i, c) in result.channels.iter().enumerate() {
            let radius = c.half_width + c.bank_width;
            let min = [c.a[0].min(c.b[0]) - radius, c.a[1].min(c.b[1]) - radius];
            let max = [c.a[0].max(c.b[0]) + radius, c.a[1].max(c.b[1]) + radius];
            for y in (min[1] / spacing).floor() as i32..=(max[1] / spacing).floor() as i32 {
                for x in (min[0] / spacing).floor() as i32..=(max[0] / spacing).floor() as i32 {
                    result.buckets.entry((x, y)).or_default().push(i);
                }
            }
        }
        Ok(result)
    }
    fn lake(&self, document: &MapDocument, p: [f64; 2]) -> Option<(f64, f64)> {
        let f = &document.height_field;
        let x = (p[0] - f.origin_m[0]) / f.step_m;
        let y = (p[1] - f.origin_m[1]) / f.step_m;
        if x < 0. || y < 0. || x >= (f.width - 1) as f64 || y >= (f.height - 1) as f64 {
            return None;
        }
        let ix = x.floor() as usize;
        let iy = y.floor() as usize;
        let tx = x - ix as f64;
        let ty = y - iy as f64;
        let mut coverage = 0.;
        let mut h = 0.;
        for (dx, dy, w) in [
            (0, 0, (1. - tx) * (1. - ty)),
            (1, 0, tx * (1. - ty)),
            (0, 1, (1. - tx) * ty),
            (1, 1, tx * ty),
        ] {
            if let Some(level) = self.lake_levels[(iy + dy) * f.width + ix + dx] {
                coverage += w;
                h += level * w;
            }
        }
        (coverage > 0.001).then_some((h / coverage, coverage))
    }
    pub fn channels_near(&self, p: [f64; 2]) -> impl Iterator<Item = &Channel> {
        self.buckets
            .get(&(
                (p[0] / self.bucket_size).floor() as i32,
                (p[1] / self.bucket_size).floor() as i32,
            ))
            .into_iter()
            .flatten()
            .map(|i| &self.channels[*i])
    }
    pub fn closest(c: &Channel, p: [f64; 2]) -> (f64, f64) {
        let ab = [c.b[0] - c.a[0], c.b[1] - c.a[1]];
        let t = (((p[0] - c.a[0]) * ab[0] + (p[1] - c.a[1]) * ab[1])
            / (ab[0] * ab[0] + ab[1] * ab[1]))
            .clamp(0., 1.);
        (
            distance(p, lerp(c.a, c.b, t)),
            c.levels[0] + (c.levels[1] - c.levels[0]) * t,
        )
    }
    pub fn ground(&self, document: &MapDocument, p: [f64; 2], source: f64) -> f64 {
        let mut h = source;
        if let Some((level, coverage)) = self.lake(document, p) {
            let weight = smooth(coverage / 0.5);
            h = h.min(source + (level - 4. - source) * weight);
        }
        for c in self.channels_near(p) {
            let (d, level) = Self::closest(c, p);
            if d < c.half_width + c.bank_width {
                let blend = 1. - smooth((d - c.half_width) / (c.bank_width));
                let depth = 3. + 3. * (1. - (d / c.half_width).clamp(0., 1.));
                h = h.min(source + (level - depth - source) * blend);
            }
        }
        h
    }
    pub fn water_level(&self, document: &MapDocument, p: [f64; 2]) -> Option<f64> {
        let mut level = self.lake(document, p).filter(|l| l.1 >= 0.5).map(|l| l.0);
        for c in self.channels_near(p) {
            let (d, h) = Self::closest(c, p);
            if d <= c.half_width {
                level = Some(level.map_or(h, |l| l.min(h)));
            }
        }
        level
    }
}

fn clip_segment(a: [f64; 2], b: [f64; 2], bounds: [f64; 4]) -> Option<([f64; 2], [f64; 2])> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let mut lo: f64 = 0.;
    let mut hi: f64 = 1.;
    for (p, q) in [
        (-d[0], a[0] - bounds[0]),
        (d[0], bounds[2] - a[0]),
        (-d[1], a[1] - bounds[1]),
        (d[1], bounds[3] - a[1]),
    ] {
        if p.abs() < 1e-10 {
            if q < 0. {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0. {
            lo = lo.max(t);
        } else {
            hi = hi.min(t);
        }
        if lo > hi {
            return None;
        }
    }
    Some((lerp(a, b, lo), lerp(a, b, hi)))
}

/// Convex clipping preserves elevation interpolation and canonical hex ownership.
pub fn clip_polygon(polygon: &[WaterVertex], outline: &[[f64; 2]; 6]) -> Vec<WaterVertex> {
    let mut output = polygon.to_vec();
    for i in 0..6 {
        let a = outline[i];
        let b = outline[(i + 1) % 6];
        let side = |p: [f64; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        let input = std::mem::take(&mut output);
        if input.is_empty() {
            break;
        }
        for j in 0..input.len() {
            let p = input[j];
            let q = input[(j + 1) % input.len()];
            let dp = side(p.point);
            let dq = side(q.point);
            if dp >= -1e-7 {
                output.push(p);
            }
            if (dp >= 0.) != (dq >= 0.) {
                let t = (dp / (dp - dq)).clamp(0., 1.);
                output.push(WaterVertex {
                    point: lerp(p.point, q.point, t),
                    level: p.level + (q.level - p.level) * t,
                    edge: p.edge + (q.edge - p.edge) * t,
                });
            }
        }
    }
    output
}
