//! Native generation UI and derived 3D view. Heavy GIS and mesh work runs on one
//! cancellable worker; only complete, current revisions reach the live scene.
mod art;
mod camera;
mod language;
mod scene;
mod ui;

use crate::{
    generation,
    gis::cache::Cache,
    i18n::{Locale, Message},
    jobs::{JobContext, Progress},
    map_core::*,
    terrain::{self, TerrainGeometry},
};
use anyhow::{Context, Result};
use bevy::{
    asset::embedded_asset,
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::AsBindGroup,
    shader::ShaderRef,
};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use camera::OrbitCamera;
use hexx::Hex;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};

pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainBlend>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TerrainBlend {
    #[uniform(100)]
    pub settings: Vec4,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub color: Handle<Image>,
    #[texture(103, dimension = "2d_array")]
    #[sampler(104)]
    pub detail: Handle<Image>,
}

impl MaterialExtension for TerrainBlend {
    fn fragment_shader() -> ShaderRef {
        "embedded://hex_cell_map/desktop/terrain.wgsl".into()
    }
}

#[derive(Resource, Default)]
pub struct MapView {
    pub document: Option<Arc<MapDocument>>,
    pub corners: BTreeMap<terrain::VertexKey, terrain::Corner>,
    pub heights: HeightSettings,
    pub triangles: Vec<Vec<[terrain::VertexKey; 3]>>,
    pub outlines: Vec<Vec<terrain::VertexKey>>,
    pub outline_water: BTreeMap<terrain::VertexKey, f64>,
    pub revision: u64,
    pub selected: Option<u32>,
    pub hovered: Option<u32>,
}

#[derive(Resource)]
pub struct UiState {
    pub locale: Locale,
    pub settings: GenerationSettings,
    pub estimate: std::result::Result<usize, String>,
    pub last_estimated: Option<GenerationSettings>,
    pub pointer_blocked: bool,
    pub keyboard_blocked: bool,
    pub generate_requested: bool,
    pub rebuild_requested: bool,
    pub height_edit_origin: Option<Arc<MapDocument>>,
    pub height_edit_finished: bool,
    pub heights: HeightSettings,
    pub city_paint: bool,
    pub city_radius: i32,
    pub city_population: u64,
    pub city_style: UrbanStyle,
    pub edit_requested: Option<bool>,
    pub undo_requested: bool,
    pub redo_requested: bool,
    pub undo: Vec<Arc<MapDocument>>,
    pub redo: Vec<Arc<MapDocument>>,
    pub project_path: String,
    pub save_requested: bool,
    pub load_requested: bool,
    pub models: bool,
    pub grid: bool,
    pub credits: bool,
    pub status: Message,
    pub error: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            locale: Locale::En,
            settings: GenerationSettings::default(),
            estimate: Ok(0),
            last_estimated: None,
            pointer_blocked: false,
            keyboard_blocked: false,
            generate_requested: false,
            rebuild_requested: false,
            height_edit_origin: None,
            height_edit_finished: false,
            heights: HeightSettings::default(),
            city_paint: false,
            city_radius: 0,
            city_population: 50_000,
            city_style: UrbanStyle::Mixed,
            edit_requested: None,
            undo_requested: false,
            redo_requested: false,
            undo: vec![],
            redo: vec![],
            project_path: "map.hexmap.json".into(),
            save_requested: false,
            load_requested: false,
            models: true,
            grid: true,
            credits: false,
            status: Message::new("status.begin"),
            error: None,
        }
    }
}

#[derive(Resource, Default)]
pub struct Jobs {
    active: Option<ActiveJob>,
    sequence: u64,
}

struct ActiveJob {
    revision: u64,
    context: JobContext,
    progress: Mutex<mpsc::Receiver<Progress>>,
    result: Mutex<mpsc::Receiver<std::result::Result<PreparedMap, String>>>,
    fraction: f32,
    refit: bool,
}

struct PreparedMap {
    document: Arc<MapDocument>,
    geometry: TerrainGeometry,
    models: Vec<ModelPlacement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Tree,
    Conifer,
    Building,
    Rock,
    Shrub,
    Grass,
}

pub struct ModelPlacement {
    kind: ModelKind,
    position: Vec3,
    scale: f32,
    yaw: f32,
    variant: usize,
    cell_id: u32,
    point_m: [f64; 2],
}

#[derive(Resource, Default)]
struct LaunchOptions {
    input: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    smoke: bool,
    outlines: bool,
    focus: Option<Hex>,
    distance: Option<f32>,
    ready_frames: u32,
    capture_started: bool,
    picking_checked: bool,
}

pub fn run() -> Result<()> {
    let mut options = LaunchOptions::default();
    let mut locale = language::initial_locale();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" => {
                let tag = args.next().context("Missing language: use en or zh-CN")?;
                locale = match tag.as_str() {
                    "en" => Locale::En,
                    "zh-CN" => Locale::ZhCn,
                    _ => anyhow::bail!("Unsupported language {tag}; use en or zh-CN"),
                };
            }
            "--preview" => {
                options.input = Some(PathBuf::from(
                    args.next().context("Missing validation artifact")?,
                ))
            }
            "--smoke" => options.smoke = true,
            "--outlines" => options.outlines = true,
            "--focus" => {
                options.focus = Some(Hex {
                    x: args.next().context("Missing focus hex q")?.parse()?,
                    y: args.next().context("Missing focus hex r")?.parse()?,
                });
            }
            "--distance" => {
                let distance: f32 = args
                    .next()
                    .context("Missing camera distance in km")?
                    .parse()?;
                anyhow::ensure!(
                    distance.is_finite() && (0.5..=3000.).contains(&distance),
                    "Camera distance must be 0.5–3000 km"
                );
                options.distance = Some(distance);
            }
            "--screenshot" => {
                options.screenshot = Some(PathBuf::from(
                    args.next().context("Missing screenshot path")?,
                ))
            }
            "--help" => {
                println!(
                    "hex-cell-map [--lang en|zh-CN] [--preview GIS_PROBE_JSON] [--smoke --screenshot FILE.png] [--outlines] [--focus Q R --distance KM]\nGenerate real GIS maps using the native window. Preview inputs and smoke camera options are development validation tools, not portable user projects."
                );
                return Ok(());
            }
            _ => anyhow::bail!("Unknown argument {arg}"),
        }
    }
    if options.smoke {
        anyhow::ensure!(
            options.input.is_some() && options.screenshot.is_some(),
            "Smoke validation requires --preview and --screenshot"
        );
    }
    anyhow::ensure!(
        options.smoke || (options.focus.is_none() && options.distance.is_none()),
        "Focus and distance options require --smoke"
    );
    let grid = !options.smoke || options.outlines;
    let asset_root = art::asset_root()?;
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: asset_root.to_string_lossy().into_owned(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: locale.text("window.title").into(),
                    resolution: (1440, 960).into(),
                    ..default()
                }),
                ..default()
            }),
    )
    .add_plugins(EguiPlugin::default())
    .add_plugins(MaterialPlugin::<TerrainMaterial>::default())
    .insert_resource(ClearColor(Color::srgb(0.075, 0.095, 0.12)))
    .insert_resource(GlobalAmbientLight {
        brightness: 100.,
        ..default()
    })
    .insert_resource(options)
    .init_resource::<MapView>()
    .insert_resource(UiState {
        locale,
        grid,
        ..default()
    })
    .init_resource::<Jobs>()
    .init_resource::<OrbitCamera>()
    .add_systems(
        Startup,
        (art::start, scene::setup, ui::setup, launch_preview).chain(),
    )
    .add_systems(
        Update,
        (
            art::poll,
            poll_jobs,
            scene::height_preview,
            start_jobs,
            camera::animate,
            scene::models_visibility,
            scene::grid_visibility,
        )
            .chain(),
    )
    .add_systems(
        EguiPrimaryContextPass,
        (
            ui::setup_context,
            ui::show,
            camera::controls,
            scene::pick,
            scene::overlay,
        )
            .chain(),
    )
    .add_systems(PostUpdate, scene::smoke_validation);
    embedded_asset!(app, "terrain.wgsl");
    let exit = app.run();
    anyhow::ensure!(
        exit.is_success(),
        "Native application or smoke validation failed"
    );
    Ok(())
}

fn launch_preview(options: Res<LaunchOptions>, mut jobs: ResMut<Jobs>, mut ui: ResMut<UiState>) {
    if let Some(path) = options.input.clone() {
        jobs.sequence += 1;
        jobs.active = Some(spawn_job(jobs.sequence, true, move |context| {
            let mut document: MapDocument = serde_json::from_reader(std::fs::File::open(&path)?)?;
            document.rebuild_index()?;
            let heights = document.heights;
            prepare(Arc::new(document), heights, context)
        }));
        ui.status = Message::new("status.preview");
    }
}

fn spawn_job(
    revision: u64,
    refit: bool,
    work: impl FnOnce(&JobContext) -> Result<PreparedMap> + Send + 'static,
) -> ActiveJob {
    let (progress_tx, progress_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let context = JobContext::with_progress(progress_tx);
    let worker_context = context.clone();
    std::thread::spawn(move || {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&worker_context)))
                .map_err(|_| "Generation worker stopped unexpectedly".to_string())
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
        let _ = result_tx.send(result);
    });
    ActiveJob {
        revision,
        context,
        progress: Mutex::new(progress_rx),
        result: Mutex::new(result_rx),
        fraction: 0.,
        refit,
    }
}

fn start_jobs(mut jobs: ResMut<Jobs>, view: Res<MapView>, mut ui: ResMut<UiState>) {
    if jobs.active.is_some() {
        return;
    }
    if ui.save_requested {
        ui.save_requested = false;
        if let Some(document) = &view.document {
            match crate::project::save(document, std::path::Path::new(&ui.project_path)) {
                Ok(()) => {
                    ui.status = Message::new("status.saved");
                    ui.error = None;
                }
                Err(e) => ui.error = Some(format!("{e:#}")),
            }
        }
    }
    if ui.load_requested {
        ui.load_requested = false;
        ui.error = None;
        ui.status = Message::new("status.opening");
        let path = PathBuf::from(&ui.project_path);
        jobs.sequence += 1;
        jobs.active = Some(spawn_job(jobs.sequence, true, move |context| {
            let document = crate::project::load(&path)?;
            let heights = document.heights;
            prepare(Arc::new(document), heights, context)
        }));
    } else if ui.generate_requested {
        ui.generate_requested = false;
        ui.error = None;
        ui.status = Message::new("status.generating");
        let settings = ui.settings.clone();
        let heights = ui.heights;
        jobs.sequence += 1;
        jobs.active = Some(spawn_job(jobs.sequence, true, move |context| {
            let cache = Cache::new(Cache::default_path())?;
            prepare(
                Arc::new(generation::generate(&settings, &cache, context)?),
                heights,
                context,
            )
        }));
    } else if let Some(current) = view.document.clone() {
        let mut next = None;
        if ui.undo_requested {
            ui.undo_requested = false;
            if let Some(previous) = ui.undo.pop() {
                ui.redo.push(current.clone());
                ui.heights = previous.heights;
                next = Some(previous);
            }
        }
        if ui.redo_requested {
            ui.redo_requested = false;
            if let Some(previous) = ui.redo.pop() {
                ui.undo.push(current.clone());
                ui.heights = previous.heights;
                next = Some(previous);
            }
        }
        if let Some(add) = ui.edit_requested.take()
            && let Some(id) = view.selected
        {
            let mut edited = (*current).clone();
            edit_urban(
                &mut edited,
                id,
                ui.city_radius,
                add,
                ui.city_population,
                ui.city_style,
            );
            next = Some(Arc::new(edited));
            ui.undo.push(current.clone());
            ui.redo.clear();
        }
        if ui.undo.len() > 64 {
            ui.undo.remove(0);
        }
        if let Some(document) = next {
            ui.status = Message::new("status.updating");
            let heights = ui.heights;
            jobs.sequence += 1;
            jobs.active = Some(spawn_job(jobs.sequence, false, move |context| {
                prepare(document, heights, context)
            }));
        }
    }
}

fn edit_urban(
    document: &mut MapDocument,
    id: u32,
    radius: i32,
    add: bool,
    population: u64,
    style: UrbanStyle,
) {
    let center = document.cells[id as usize].hex;
    for cell in &mut document.cells {
        if cell.hex.unsigned_distance_to(center) > radius as u32 {
            continue;
        }
        if add {
            if cell.surface == Surface::Water {
                continue;
            }
            cell.urban = Some(UrbanTerrain { population, style });
            if cell.surface == Surface::Land {
                cell.surface = Surface::City;
            }
        } else {
            cell.urban = None;
            if cell.surface == Surface::City {
                cell.surface = Surface::Land;
            }
        }
        cell.edited = true;
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system parameters.
fn poll_jobs(
    mut jobs: ResMut<Jobs>,
    mut ui: ResMut<UiState>,
    mut view: ResMut<MapView>,
    mut orbit: ResMut<OrbitCamera>,
    mut commands: Commands,
    old: Query<Entity, With<scene::MapEntity>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut gizmo_assets: ResMut<Assets<GizmoAsset>>,
    assets: Res<scene::SceneAssets>,
    art: Res<art::ArtLibrary>,
) {
    let Some(job) = jobs.active.as_mut() else {
        return;
    };
    for p in job.progress.lock().unwrap().try_iter() {
        job.fraction = job.fraction.max(p.fraction.min(0.99));
        ui.status = p.message;
    }
    if let Some(error) = &art.error {
        job.context.cancel();
        ui.error = Some(error.clone());
        jobs.active = None;
        return;
    }
    if !art.ready && job.context.check().is_ok() {
        ui.status = Message::new("status.loading_art");
        return;
    }
    let result = job.result.lock().unwrap().try_recv();
    match result {
        Ok(result) => {
            let revision = job.revision;
            let refit = job.refit;
            let cancelled = job.context.check().is_err();
            jobs.active = None;
            if cancelled {
                ui.status = Message::new("status.cancelled");
                return;
            }
            match result {
                Ok(prepared) if revision > view.revision => {
                    if refit {
                        ui.settings = prepared.document.settings.clone();
                        ui.heights = prepared.geometry.heights;
                        ui.undo.clear();
                        ui.redo.clear();
                        ui.height_edit_origin = None;
                        ui.height_edit_finished = false;
                    }
                    for entity in &old {
                        commands.entity(entity).despawn();
                    }
                    scene::install(
                        &mut commands,
                        &mut meshes,
                        &mut gizmo_assets,
                        &assets,
                        &art,
                        &prepared,
                    );
                    if refit {
                        orbit.fit(&prepared.document, prepared.geometry.heights);
                        view.selected = None;
                    }
                    ui.status = Message::new("status.complete")
                        .arg("hexes", prepared.document.cells.len())
                        .arg(
                            "cities",
                            prepared
                                .document
                                .cells
                                .iter()
                                .map(|c| c.cities.len())
                                .sum::<usize>(),
                        );
                    view.heights = prepared.geometry.heights;
                    view.outlines = prepared.geometry.outlines;
                    view.outline_water = prepared
                        .geometry
                        .water_chunks
                        .iter()
                        .flat_map(|c| {
                            c.boundaries
                                .iter()
                                .map(|(k, v)| (*k, v.display.samples[0].height))
                        })
                        .collect();
                    view.triangles = prepared.geometry.triangles;
                    view.corners = prepared.geometry.corners;
                    view.document = Some(prepared.document);
                    view.revision = revision;
                    view.hovered = None;
                }
                Ok(_) => {}
                Err(error) => {
                    ui.status = Message::new("status.failed");
                    ui.error = Some(error);
                }
            }
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            jobs.active = None;
            ui.error = Some("Generation worker disconnected".into());
        }
        Err(mpsc::TryRecvError::Empty) => {}
    }
}

fn prepare(
    document: Arc<MapDocument>,
    heights: HeightSettings,
    context: &JobContext,
) -> Result<PreparedMap> {
    let mut document = (*document).clone();
    document.heights = heights;
    let document = Arc::new(document);
    let geometry = terrain::build(&document, heights, context)?;
    let spacing = document.settings.spacing_km * 1000.;
    let mut models = Vec::new();
    // Imported meshes have more detail than the old primitives. Divide a fixed
    // vegetation allowance across the complete region rather than filling it
    // in cell order and leaving the far end undecorated.
    let forest_demand: f64 = document
        .cells
        .iter()
        .filter(|c| {
            c.landscape == Landscape::Forest && c.surface == Surface::Land && c.urban.is_none()
        })
        .map(|c| c.forest_fraction * 18.)
        .sum();
    let density = (24_000. / forest_demand.max(1.)).min(1.);
    let decoration_density = (12_000. / (document.cells.len().max(1) as f64 * 3.)).min(1.);
    let urban_count = document
        .cells
        .iter()
        .filter(|c| c.urban.is_some())
        .count()
        .max(1);
    let city_limit = (12_000 / urban_count).clamp(1, 25);
    for (i, cell) in document.cells.iter().enumerate() {
        if i % 256 == 0 {
            context.check()?;
        }
        if cell.landscape == Landscape::Forest
            && cell.surface == Surface::Land
            && cell.urban.is_none()
            && models.len() < 60_000
        {
            let count =
                placement_count(cell.forest_fraction * 18. * density, u64::from(cell.id), 17);
            for n in 0..count {
                let seed = (u64::from(cell.id) * 7919 + n * 104729 + 17) % 65521;
                let angle = seed as f64 * 0.61803398875 * std::f64::consts::TAU;
                let radius = spacing * (0.08 + 0.36 * (seed % 100) as f64 / 100.);
                let p = [
                    cell.center_m[0] + radius * angle.cos(),
                    cell.center_m[1] + radius * angle.sin(),
                ];
                let cover = document.height_field.cover_weights(p);
                if cover[0] + cover[9] < 0.35
                    || geometry.hydrology.water_level(&document, p).is_some()
                {
                    continue;
                }
                if let Some(height) = terrain::height_at(cell, p, spacing, &geometry) {
                    let temperate = document
                        .settings
                        .region
                        .south
                        .abs()
                        .min(document.settings.region.north.abs())
                        > 25.;
                    let kind = if temperate && cell.generated_elevation_m > 1000. && seed % 3 != 0 {
                        ModelKind::Conifer
                    } else {
                        ModelKind::Tree
                    };
                    models.push(ModelPlacement {
                        kind,
                        position: Vec3::new((p[0] / 1000.) as f32, height, (-p[1] / 1000.) as f32),
                        scale: document.settings.spacing_km as f32
                            * (0.11 + 0.045 * (seed % 10) as f32 / 10.),
                        yaw: angle as f32,
                        variant: seed as usize,
                        cell_id: cell.id,
                        point_m: p,
                    });
                }
            }
        }
        if cell.urban.is_none() && cell.surface != Surface::Water && models.len() < 60_000 {
            let count = placement_count(3. * decoration_density, u64::from(cell.id), 97);
            for n in 0..count {
                let seed = (u64::from(cell.id) * 3571 + n * 7919 + 97) % 65521;
                let angle = seed as f64 * 0.61803398875 * std::f64::consts::TAU;
                let radius = spacing * (0.15 + 0.2 * (seed % 100) as f64 / 100.);
                let p = [
                    cell.center_m[0] + radius * angle.cos(),
                    cell.center_m[1] + radius * angle.sin(),
                ];
                if geometry.hydrology.water_level(&document, p).is_some() {
                    continue;
                }
                let cover = document.height_field.cover_weights(p);
                let kind = if cover[5] > 0.4 || (cell.slope_degrees > 20. && cover[6] < 0.4) {
                    ModelKind::Rock
                } else if cover[0] + cover[1] > 0.4 {
                    ModelKind::Shrub
                } else if cover[2] + cover[3] > 0.4 {
                    ModelKind::Grass
                } else {
                    continue;
                };
                if let Some(h) = terrain::height_at(cell, p, spacing, &geometry) {
                    models.push(ModelPlacement {
                        kind,
                        position: Vec3::new((p[0] / 1000.) as f32, h, (-p[1] / 1000.) as f32),
                        scale: document.settings.spacing_km as f32
                            * if matches!(kind, ModelKind::Rock) {
                                0.095
                            } else {
                                0.06
                            },
                        yaw: angle as f32,
                        variant: seed as usize,
                        cell_id: cell.id,
                        point_m: p,
                    });
                }
            }
        }
        if let Some(urban) = &cell.urban {
            let importance = (urban.population.max(1000) as f64).log10();
            let size = match urban.style {
                UrbanStyle::LowRise => 3,
                UrbanStyle::Dense => 5,
                UrbanStyle::Mixed => (importance.round() as i32 - 1).clamp(3, 5),
            };
            let mut offsets: Vec<_> = (-size / 2..=size / 2)
                .flat_map(|y| (-size / 2..=size / 2).map(move |x| (x, y)))
                .collect();
            offsets.sort_by_key(|(x, y)| x * x + y * y);
            let mut placed = 0;
            for (x, y) in offsets {
                if placed >= city_limit {
                    break;
                }
                let p = [
                    cell.center_m[0] + x as f64 * spacing * 0.16,
                    cell.center_m[1] + y as f64 * spacing * 0.16,
                ];
                if point_hex(p, spacing) != cell.hex
                    || geometry.hydrology.water_level(&document, p).is_some()
                {
                    continue;
                }
                let Some(h) = terrain::height_at(cell, p, spacing, &geometry) else {
                    continue;
                };
                let seed = (cell.id as i64 * 7919 + x as i64 * 61 + y as i64 * 127).unsigned_abs();
                let scale = document.settings.spacing_km as f32
                    * (0.09 + (importance as f32 - 3.).max(0.) * 0.012)
                    * (0.8 + (seed % 5) as f32 * 0.10);
                placed += 1;
                models.push(ModelPlacement {
                    kind: ModelKind::Building,
                    position: Vec3::new((p[0] / 1000.) as f32, h, (-p[1] / 1000.) as f32),
                    scale,
                    yaw: (seed % 4) as f32 * std::f32::consts::FRAC_PI_2,
                    variant: seed as usize,
                    cell_id: cell.id,
                    point_m: p,
                });
            }
        }
    }
    context.check()?;
    Ok(PreparedMap {
        document,
        geometry,
        models,
    })
}

fn placement_count(expected: f64, id: u64, salt: u64) -> u64 {
    let seed = id.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(salt);
    let fraction = (seed >> 11) as f64 / (1_u64 << 53) as f64;
    expected.floor() as u64 + u64::from(fraction < expected.fract())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_model_placement_uses_gis_cover_dry_ground_and_urban_cells() -> Result<()> {
        let mut document = crate::terrain::fixture()?;
        document.height_field.land_cover.fill(10);
        for cell in &mut document.cells {
            cell.forest_fraction = 0.8;
            cell.landscape = match cell.id % 3 {
                0 => Landscape::Forest,
                1 => Landscape::Plains,
                _ => Landscape::Mountain,
            };
            if cell.id % 9 == 0 {
                cell.surface = Surface::River;
            } else if cell.id % 9 == 3 {
                cell.surface = Surface::Water;
            }
        }
        let city = document.index[&hexx::Hex::ZERO];
        document.cells[city].urban = Some(UrbanTerrain {
            population: 10000,
            style: UrbanStyle::LowRise,
        });
        document.river_paths.push(RiverPath {
            id: 1,
            next_down: 0,
            discharge: 50.,
            stream_order: 4,
            points_m: vec![[-5000., 3000.], [5000., 3000.]],
        });
        let original = document.height_field.elevations_m.clone();
        let prepared = prepare(
            Arc::new(document),
            HeightSettings::default(),
            &JobContext::default(),
        )?;
        assert_eq!(original, prepared.document.height_field.elevations_m);
        assert!(
            prepared
                .models
                .iter()
                .any(|m| matches!(m.kind, ModelKind::Tree | ModelKind::Conifer))
        );
        assert_eq!(
            prepared
                .models
                .iter()
                .filter(|m| m.kind == ModelKind::Building)
                .count(),
            9
        );
        for model in &prepared.models {
            let cell = &prepared.document.cells[model.cell_id as usize];
            assert_eq!(point_hex(model.point_m, 2000.), cell.hex);
            assert!(
                prepared
                    .geometry
                    .hydrology
                    .water_level(&prepared.document, model.point_m)
                    .is_none()
            );
            let height =
                terrain::height_at(cell, model.point_m, 2000., &prepared.geometry).unwrap();
            assert!((model.position.y - height).abs() < 1e-6);
            if model.kind == ModelKind::Building {
                assert!(cell.urban.is_some());
            }
            if matches!(model.kind, ModelKind::Tree | ModelKind::Conifer) {
                assert_eq!(cell.landscape, Landscape::Forest);
                assert_eq!(cell.surface, Surface::Land);
                assert!(cell.urban.is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn urban_edits_use_complete_cells_and_preserve_rivers_and_source_settlements() {
        let mut document = crate::terrain::fixture().unwrap();
        let id = document.index[&hexx::Hex::ZERO];
        document.cells[id].surface = Surface::River;
        document.cells[id].river_ids = vec![123];
        document.cells[id].cities = vec![City {
            id: 1,
            name: "Source city".into(),
            lon: 8.1,
            lat: 46.7,
            population: 10000,
        }];
        edit_urban(&mut document, id as u32, 1, true, 100000, UrbanStyle::Dense);
        assert_eq!(
            document.cells.iter().filter(|c| c.urban.is_some()).count(),
            7
        );
        assert_eq!(document.cells[id].surface, Surface::River);
        assert_eq!(document.cells[id].river_ids, vec![123]);
        assert_eq!(document.cells[id].cities.len(), 1);
        edit_urban(
            &mut document,
            id as u32,
            1,
            false,
            100000,
            UrbanStyle::Dense,
        );
        assert!(document.cells.iter().all(|c| c.urban.is_none()));
        assert_eq!(document.cells[id].surface, Surface::River);
        assert_eq!(document.cells[id].cities[0].name, "Source city");
    }

    #[test]
    fn failed_cancelled_and_stale_jobs_preserve_the_live_document_and_scene() {
        let document = Arc::new(crate::terrain::fixture().unwrap());
        for case in ["failure", "cancelled", "stale"] {
            let mut app = App::new();
            let context = JobContext::default();
            if case == "cancelled" {
                context.cancel();
            }
            let (sender, receiver) = mpsc::channel();
            if case == "failure" {
                sender
                    .send(Err("Required GIS layer unavailable".into()))
                    .unwrap();
            } else {
                sender
                    .send(Ok(PreparedMap {
                        document: document.clone(),
                        geometry: TerrainGeometry {
                            chunks: vec![],
                            corners: BTreeMap::new(),
                            heights: HeightSettings::default(),
                            water_chunks: vec![],
                            triangles: vec![],
                            outlines: vec![],
                            hydrology: Default::default(),
                        },
                        models: vec![],
                    }))
                    .unwrap();
            }
            let (_, progress) = mpsc::channel();
            app.insert_resource(MapView {
                document: Some(document.clone()),
                revision: 2,
                ..default()
            })
            .insert_resource(UiState::default())
            .insert_resource(OrbitCamera::default())
            .insert_resource(Assets::<Mesh>::default())
            .insert_resource(Assets::<GizmoAsset>::default())
            .insert_resource(scene::SceneAssets::default())
            .insert_resource({
                let mut art = art::ArtLibrary::default();
                art.ready = true;
                art
            })
            .insert_resource(Jobs {
                active: Some(ActiveJob {
                    revision: if case == "stale" { 1 } else { 3 },
                    context,
                    result: Mutex::new(receiver),
                    progress: Mutex::new(progress),
                    fraction: 0.,
                    refit: true,
                }),
                sequence: 3,
            })
            .add_systems(Update, poll_jobs);
            let entity = app.world_mut().spawn(scene::MapEntity).id();
            app.update();
            let view = app.world().resource::<MapView>();
            assert!(Arc::ptr_eq(view.document.as_ref().unwrap(), &document));
            assert_eq!(view.revision, 2);
            assert!(app.world().get_entity(entity).is_ok());
        }
    }
}
