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
    lake_coverage: Vec<f64>,
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
            lake_coverage: vec![0.; field.elevations_m.len()],
            ..Default::default()
        };
        // Connected WorldCover open-water samples define flat bodies. Reject
        // DEM-contaminated shoreline samples rather than painting steep walls.
        let mut seen = vec![false; field.elevations_m.len()];
        let mut owners = vec![None; seen.len()];
        let mut body_levels = Vec::new();
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
            let owner = body_levels.len();
            body_levels.push(level + edit_offset);
            for i in component {
                if (f64::from(field.elevations_m[i]) - level).abs() <= 15. {
                    result.lake_levels[i] = Some(level + edit_offset);
                    owners[i] = Some(owner);
                }
            }
        }
        // Blend the categorical boundary slightly, then evaluate its bilinear
        // contour at finer steps only along shorelines. Interior water stays coarse.
        result.blend_lakes(document, &owners, &body_levels);
        for y in 0..field.height - 1 {
            context.check()?;
            for x in 0..field.width - 1 {
                let i = y * field.width + x;
                let corner = [i, i + 1, i + field.width, i + field.width + 1];
                let max = corner
                    .iter()
                    .map(|j| result.lake_coverage[*j])
                    .fold(0., f64::max);
                if max < 0.5 {
                    continue;
                }
                let min = corner
                    .iter()
                    .map(|j| result.lake_coverage[*j])
                    .fold(1., f64::min);
                let subdivisions = if min >= 0.9 { 1 } else { 3 };
                let origin = field.point(i);
                let step = field.step_m / subdivisions as f64;
                for yy in 0..subdivisions {
                    for xx in 0..subdivisions {
                        let p = [origin[0] + xx as f64 * step, origin[1] + yy as f64 * step];
                        for tri in [
                            [
                                [p[0], p[1]],
                                [p[0] + step, p[1]],
                                [p[0] + step, p[1] + step],
                            ],
                            [
                                [p[0], p[1]],
                                [p[0] + step, p[1] + step],
                                [p[0], p[1] + step],
                            ],
                        ] {
                            let values = tri.map(|q| result.lake(document, q).unwrap_or((0., 0.)));
                            let mut vertices = Vec::new();
                            for k in 0..3 {
                                let next = (k + 1) % 3;
                                let a = values[k];
                                let b = values[next];
                                if a.1 >= 0.5 {
                                    vertices.push(WaterVertex {
                                        point: tri[k],
                                        level: a.0,
                                        edge: (1. - (a.1 - 0.5) * 2.).clamp(0., 1.),
                                    });
                                }
                                if (a.1 >= 0.5) != (b.1 >= 0.5) {
                                    let t = ((0.5 - a.1) / (b.1 - a.1)).clamp(0., 1.);
                                    let q = lerp(tri[k], tri[next], t);
                                    let level = result.lake(document, q).map_or(a.0, |l| l.0);
                                    vertices.push(WaterVertex {
                                        point: q,
                                        level,
                                        edge: 1.,
                                    });
                                }
                            }
                            if vertices.len() >= 3 {
                                result.polygons.push(WaterPolygon { vertices });
                            }
                        }
                    }
                }
            }
        }
        let mut node_index = HashMap::new();
        let mut positions = Vec::new();
        let mut node_paths = Vec::new();
        let mut node_stations = Vec::new();
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
            points = smooth_path(&points, field.step_m * 0.5, spacing * 0.08);
            let half_width = (spacing * 0.075 + path.discharge.sqrt() * 3.5)
                .clamp(spacing * 0.08, spacing * 0.18);
            let mut station = 0.;
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
                        node_paths.push(path.id);
                        node_stations.push(station + length * k as f64 / count as f64);
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
                        if let Some(lake) = result.lake(document, p)
                            && lake.1 >= 0.5
                        {
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
                        graph[index].push((prev, length * 0.10));
                        runs.push((prev, index, half_width));
                    }
                    previous = Some(index);
                }
                station += length;
            }
        }
        // Wide render footprints at tight bends can touch even when the source
        // nodes are not consecutive. Limit the difference across these joins
        // before blending their water surfaces.
        let mut widths: Vec<f64> = vec![0.; positions.len()];
        for &(a, b, w) in &runs {
            widths[a] = widths[a].max(w);
            widths[b] = widths[b].max(w);
        }
        let mut spatial = HashMap::<(i32, i32), Vec<usize>>::new();
        let step = spacing * 0.5;
        for (i, p) in positions.iter().enumerate() {
            spatial
                .entry(((p[0] / step).floor() as i32, (p[1] / step).floor() as i32))
                .or_default()
                .push(i);
        }
        for (i, p) in positions.iter().enumerate() {
            let bucket = ((p[0] / step).floor() as i32, (p[1] / step).floor() as i32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if let Some(nodes) = spatial.get(&(bucket.0 + dx, bucket.1 + dy)) {
                        for &j in nodes {
                            if j <= i || graph[i].iter().any(|(n, _)| *n == j) {
                                continue;
                            }
                            let d = distance(*p, positions[j]);
                            if node_paths[i] == node_paths[j]
                                && (node_stations[i] - node_stations[j]).abs() <= d * 1.15
                            {
                                continue;
                            }
                            if d <= widths[i] + widths[j] + field.step_m * 0.25 {
                                graph[i].push((j, d * 0.025));
                                graph[j].push((i, d * 0.025));
                            }
                        }
                    }
                }
            }
        }
        // A symbolic channel can overlap a small source-mask body even when its
        // coarse center sample misses the centerline. Give that complete body
        // one graph node so later downstream conditioning also lowers the whole
        // body and every crossing together, rather than leaving a tilted join.
        let body_start = levels.len();
        levels.extend(body_levels.iter().copied());
        graph.resize_with(levels.len(), Vec::new);
        for &(a, b, half_width) in &runs {
            let radius = half_width + field.step_m * 0.5;
            let channel = Channel {
                a: positions[a],
                b: positions[b],
                levels: [levels[a], levels[b]],
                half_width,
                bank_width: 0.,
            };
            let min_x = ((positions[a][0].min(positions[b][0]) - radius - field.origin_m[0])
                / field.step_m)
                .floor()
                .max(0.) as usize;
            let max_x = ((positions[a][0].max(positions[b][0]) + radius - field.origin_m[0])
                / field.step_m)
                .ceil()
                .min((field.width - 1) as f64)
                .max(0.) as usize;
            let min_y = ((positions[a][1].min(positions[b][1]) - radius - field.origin_m[1])
                / field.step_m)
                .floor()
                .max(0.) as usize;
            let max_y = ((positions[a][1].max(positions[b][1]) + radius - field.origin_m[1])
                / field.step_m)
                .ceil()
                .min((field.height - 1) as f64)
                .max(0.) as usize;
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let i = y * field.width + x;
                    if let Some(owner) = owners[i] {
                        let (d, _) = Self::closest(&channel, field.point(i));
                        if d <= radius {
                            let body = body_start + owner;
                            for node in [a, b] {
                                if !graph[node].iter().any(|(id, _)| *id == body) {
                                    graph[node].push((body, 0.));
                                    graph[body].push((node, 0.));
                                }
                            }
                        }
                    }
                }
            }
        }
        condition_levels(&mut levels, &graph);
        body_levels.copy_from_slice(&levels[body_start..]);
        result.blend_lakes(document, &owners, &body_levels);
        for (a, b, half_width) in runs {
            let midpoint = lerp(positions[a], positions[b], 0.5);
            let cut = (crate::terrain::source_height(document, midpoint)?
                - (levels[a] + levels[b]) * 0.5)
                .max(0.);
            // Wider bank ramps absorb source misalignment without a narrow cliff.
            let bank_width = (spacing * 0.22 + cut * 0.8).clamp(spacing * 0.22, spacing * 0.75);
            let channel = Channel {
                a: positions[a],
                b: positions[b],
                levels: [levels[a], levels[b]],
                half_width,
                bank_width,
            };
            let length = distance(channel.a, channel.b);
            let normal = [
                -(channel.b[1] - channel.a[1]) / length,
                (channel.b[0] - channel.a[0]) / length,
            ];
            // Deep center and shallow edges are separate ribbon vertices.
            for side in [-1., 1.] {
                let vertices = [
                    (channel.a, levels[a], 0.),
                    (channel.b, levels[b], 0.),
                    (channel.b, levels[b], side),
                    (channel.a, levels[a], side),
                ]
                .map(|(p, h, s)| WaterVertex {
                    point: [
                        p[0] + normal[0] * half_width * s,
                        p[1] + normal[1] * half_width * s,
                    ],
                    level: h,
                    edge: s.abs(),
                })
                .to_vec();
                result.polygons.push(WaterPolygon { vertices });
            }
            // These footprints are unioned before triangulation. A complete
            // disk closes a bend without adding overlapping rendered caps.
            for (p, h) in [(channel.a, levels[a]), (channel.b, levels[b])] {
                let vertices = (0..12)
                    .map(|i| {
                        let angle = i as f64 * std::f64::consts::TAU / 12.;
                        WaterVertex {
                            point: [
                                p[0] + half_width * angle.cos(),
                                p[1] + half_width * angle.sin(),
                            ],
                            level: h,
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
    fn blend_lakes(&mut self, document: &MapDocument, owners: &[Option<usize>], levels: &[f64]) {
        let f = &document.height_field;
        for i in 0..owners.len() {
            let x = (i % f.width) as isize;
            let y = (i / f.width) as isize;
            let mut coverage = 0.;
            let mut best = 0.;
            let mut owner = owners[i];
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let xx = (x + dx).clamp(0, f.width as isize - 1) as usize;
                    let yy = (y + dy).clamp(0, f.height as isize - 1) as usize;
                    let w = if dx == 0 && dy == 0 {
                        4.
                    } else if dx == 0 || dy == 0 {
                        2.
                    } else {
                        1.
                    };
                    if let Some(id) = owners[yy * f.width + xx] {
                        coverage += w;
                        if owners[i].is_none() && w > best {
                            best = w;
                            owner = Some(id);
                        }
                    }
                }
            }
            self.lake_coverage[i] = 0.65 * f64::from(owners[i].is_some()) + 0.35 * coverage / 16.;
            // A neighboring pool at a different altitude must not tilt this
            // body's interior. Only the categorical coverage is softened.
            self.lake_levels[i] = owner.map(|id| levels[id]);
        }
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
        let mut best = 0.;
        for (dx, dy, w) in [
            (0, 0, (1. - tx) * (1. - ty)),
            (1, 0, tx * (1. - ty)),
            (0, 1, (1. - tx) * ty),
            (1, 1, tx * ty),
        ] {
            if let Some(level) = self.lake_levels[(iy + dy) * f.width + ix + dx] {
                let weight = w * self.lake_coverage[(iy + dy) * f.width + ix + dx];
                coverage += weight;
                if weight > best {
                    best = weight;
                    h = level;
                }
            }
        }
        (coverage > 0.001).then_some((h, coverage))
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
                let depth = 12. + 12. * (1. - (d / c.half_width).clamp(0., 1.));
                h = h.min(source + (level - depth - source) * blend);
            }
        }
        h
    }
    pub fn bank_moisture(&self, document: &MapDocument, p: [f64; 2]) -> f64 {
        let mut wet = self.lake(document, p).map_or(0., |l| smooth(l.1 / 0.5));
        for c in self.channels_near(p) {
            let (d, _) = Self::closest(c, p);
            wet = wet.max(1. - smooth((d - c.half_width) / (c.bank_width * 0.65)));
        }
        wet
    }
    pub fn dry_weight(&self, document: &MapDocument, p: [f64; 2]) -> f64 {
        let water = document.height_field.cover_weights(p)[7];
        let mut dry = 1. - smooth(water / 0.25);
        for c in self.channels_near(p) {
            let (d, _) = Self::closest(c, p);
            dry = dry.min(smooth((d - c.half_width) / c.bank_width));
        }
        dry
    }
    pub fn water_level(&self, document: &MapDocument, p: [f64; 2]) -> Option<f64> {
        let in_lake = self.lake(document, p).is_some_and(|l| l.1 >= 0.5);
        let in_channel = self
            .channels_near(p)
            .any(|c| Self::closest(c, p).0 <= c.half_width);
        if in_lake || in_channel {
            self.surface_sample(document, p).map(|s| s.0)
        } else {
            None
        }
    }
    /// One reference surface for all overlapping footprints. Samples also extend
    /// slightly outside water so shoreline vertices and normals are continuous.
    /// Lakes take their own level; river segment interpolation blends at joins.
    pub fn surface_sample(&self, document: &MapDocument, p: [f64; 2]) -> Option<(f64, f64)> {
        let lake = self.lake(document, p);
        let mut sum = 0.;
        let mut weight = 0.;
        let mut edge: f64 = lake.map_or(1., |l| 1. - smooth((l.1 - 0.5) / 0.3));
        for c in self.channels_near(p) {
            let (d, h) = Self::closest(c, p);
            let w = (1. - d / (c.half_width * 2.)).max(0.).powi(2);
            sum += h * w;
            weight += w;
            edge = edge.min(smooth((d / c.half_width - 0.55) / 0.45));
        }
        let river = (weight > 1e-12).then_some(sum / weight);
        let level = match (lake, river) {
            (Some((h, coverage)), Some(r)) => r + (h - r) * smooth((coverage - 0.35) / 0.15),
            (Some((h, _)), None) => h,
            (None, Some(r)) => r,
            (None, None) => return None,
        };
        Some((level, edge))
    }
}

fn condition_levels(levels: &mut [f64], graph: &[Vec<(usize, f64)>]) {
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

/// Interpolate source knots with bounded Hermite bends. The authoritative GIS
/// paths are preserved; only this derived rendering curve is smoothed.
fn smooth_path(points: &[[f64; 2]], step: f64, max_deviation: f64) -> Vec<[f64; 2]> {
    let mut output = vec![points[0]];
    for i in 0..points.len() - 1 {
        let a = points[i];
        let b = points[i + 1];
        let before = points[i.saturating_sub(1)];
        let after = points[(i + 2).min(points.len() - 1)];
        let length = distance(a, b);
        let count = (length / step).ceil().clamp(1., 10000.) as usize;
        let m0 = [(b[0] - before[0]) * 0.5, (b[1] - before[1]) * 0.5];
        let m1 = [(after[0] - a[0]) * 0.5, (after[1] - a[1]) * 0.5];
        for k in 1..=count {
            let t = k as f64 / count as f64;
            let line = lerp(a, b, t);
            let t2 = t * t;
            let t3 = t2 * t;
            let curve = [0, 1].map(|j| {
                (2. * t3 - 3. * t2 + 1.) * a[j]
                    + (t3 - 2. * t2 + t) * m0[j]
                    + (-2. * t3 + 3. * t2) * b[j]
                    + (t3 - t2) * m1[j]
            });
            let offset = [curve[0] - line[0], curve[1] - line[1]];
            let deviation = offset[0].hypot(offset[1]);
            let limit = max_deviation.min(length * 0.15);
            let gain = if deviation > limit {
                limit / deviation
            } else {
                1.
            };
            output.push([line[0] + offset[0] * gain, line[1] + offset[1] * gain]);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downstream_conditioning_updates_complete_bodies_at_wide_channel_joins() -> Result<()> {
        let mut d = crate::terrain::fixture()?;
        d.height_field.elevations_m.fill(250.);
        for i in 0..d.height_field.elevations_m.len() {
            let p = d.height_field.point(i);
            if p[1] == 500. {
                if (-1000. ..=0.).contains(&p[0]) {
                    d.height_field.land_cover[i] = 80;
                    d.height_field.elevations_m[i] = 100.;
                } else if (2000. ..=3000.).contains(&p[0]) {
                    d.height_field.land_cover[i] = 80;
                }
            }
        }
        d.river_paths.push(RiverPath {
            id: 1,
            next_down: 0,
            discharge: 50.,
            stream_order: 4,
            points_m: vec![[-4000., 150.], [4000., 150.]],
        });
        let source = d.height_field.elevations_m.clone();
        let h = Hydrology::build(&d, &JobContext::default())?;
        // The centerline misses both masks, but its wide footprint joins them.
        // Lowering the upstream pool must propagate into the entire next pool.
        for p in [[-500., 500.], [2000., 500.], [2500., 500.], [3000., 500.]] {
            assert_eq!(h.water_level(&d, p), Some(100.));
        }
        assert_eq!(d.height_field.elevations_m, source);
        Ok(())
    }
    #[test]
    fn a_river_inside_a_lake_uses_the_lake_surface_without_an_internal_bank() -> Result<()> {
        let d = crate::terrain::fixture()?;
        let channel = Channel {
            a: [-1000., 0.],
            b: [1000., 0.],
            levels: [90., 80.],
            half_width: 160.,
            bank_width: 440.,
        };
        let h = Hydrology {
            channels: vec![channel],
            bucket_size: 2000.,
            buckets: HashMap::from([((0, 0), vec![0])]),
            lake_levels: vec![Some(100.); d.height_field.elevations_m.len()],
            lake_coverage: vec![1.; d.height_field.elevations_m.len()],
            ..Default::default()
        };
        // Both the river center and its old ribbon edge are interior lake water.
        for p in [[0., 0.], [0., 160.], [0., 350.]] {
            assert_eq!(h.water_level(&d, p), Some(100.));
            assert_eq!(h.surface_sample(&d, p), Some((100., 0.)));
        }
        Ok(())
    }
    #[test]
    fn rendering_curve_preserves_knots_and_limits_deviation() {
        let source = vec![[0., 0.], [1000., 0.], [1000., 1000.], [2000., 1000.]];
        let curve = smooth_path(&source, 100., 80.);
        for p in &source {
            assert!(curve.contains(p));
        }
        for pair in source.windows(2) {
            for p in curve.iter().filter(|p| {
                p[0] >= pair[0][0].min(pair[1][0]) - 80.
                    && p[0] <= pair[0][0].max(pair[1][0]) + 80.
                    && p[1] >= pair[0][1].min(pair[1][1]) - 80.
                    && p[1] <= pair[0][1].max(pair[1][1]) + 80.
            }) {
                let ab = [pair[1][0] - pair[0][0], pair[1][1] - pair[0][1]];
                let length = ab[0].hypot(ab[1]);
                let d = ((p[0] - pair[0][0]) * ab[1] - (p[1] - pair[0][1]) * ab[0]).abs() / length;
                assert!(d <= 80.0001);
            }
        }
    }
}
