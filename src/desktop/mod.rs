//! Native generation UI and derived 3D view. Heavy GIS and mesh work runs on one
//! cancellable worker; only complete, current revisions reach the live scene.
mod camera;
mod scene;
mod ui;

use crate::{
    generation,
    gis::cache::Cache,
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
}

impl MaterialExtension for TerrainBlend {
    fn fragment_shader() -> ShaderRef {
        "embedded://hex_cell_map/desktop/terrain.wgsl".into()
    }
}

#[derive(Resource, Default)]
pub struct MapView {
    pub document: Option<Arc<MapDocument>>,
    pub corners: BTreeMap<(i32, i32), terrain::Corner>,
    pub exaggeration: f32,
    pub revision: u64,
    pub selected: Option<u32>,
    pub hovered: Option<u32>,
}

#[derive(Resource)]
pub struct UiState {
    pub settings: GenerationSettings,
    pub estimate: std::result::Result<usize, String>,
    pub last_estimated: Option<GenerationSettings>,
    pub pointer_blocked: bool,
    pub keyboard_blocked: bool,
    pub generate_requested: bool,
    pub rebuild_requested: bool,
    pub exaggeration: f32,
    pub models: bool,
    pub grid: bool,
    pub credits: bool,
    pub status: String,
    pub error: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            settings: GenerationSettings::default(),
            estimate: Ok(0),
            last_estimated: None,
            pointer_blocked: false,
            keyboard_blocked: false,
            generate_requested: false,
            rebuild_requested: false,
            exaggeration: 5.,
            models: true,
            grid: false,
            credits: false,
            status: "Select a region to begin".into(),
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

#[derive(Clone, Copy)]
pub enum ModelKind {
    Tree,
    Building,
}

pub struct ModelPlacement {
    kind: ModelKind,
    position: Vec3,
    scale: f32,
    yaw: f32,
}

#[derive(Resource, Default)]
struct LaunchOptions {
    input: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    smoke: bool,
    ready_frames: u32,
    capture_started: bool,
    picking_checked: bool,
}

pub fn run() -> Result<()> {
    let mut options = LaunchOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--preview" => {
                options.input = Some(PathBuf::from(
                    args.next().context("Missing validation artifact")?,
                ))
            }
            "--smoke" => options.smoke = true,
            "--screenshot" => {
                options.screenshot = Some(PathBuf::from(
                    args.next().context("Missing screenshot path")?,
                ))
            }
            "--help" => {
                println!(
                    "hex-cell-map [--preview GIS_PROBE_JSON] [--smoke --screenshot FILE.png]\nGenerate real GIS maps using the native window. Preview inputs are development validation artifacts, not portable user projects."
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
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Hex Cell Map".into(),
            resolution: (1440, 960).into(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(EguiPlugin::default())
    .add_plugins(MaterialPlugin::<TerrainMaterial>::default())
    .insert_resource(ClearColor(Color::srgb(0.075, 0.095, 0.12)))
    .insert_resource(GlobalAmbientLight {
        brightness: 450.,
        ..default()
    })
    .insert_resource(options)
    .init_resource::<MapView>()
    .init_resource::<UiState>()
    .init_resource::<Jobs>()
    .init_resource::<OrbitCamera>()
    .add_systems(Startup, (scene::setup, ui::setup, launch_preview).chain())
    .add_systems(
        Update,
        (
            poll_jobs,
            start_jobs,
            camera::animate,
            scene::models_visibility,
        )
            .chain(),
    )
    .add_systems(
        EguiPrimaryContextPass,
        (ui::show, camera::controls, scene::pick, scene::overlay).chain(),
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
        let exaggeration = ui.exaggeration;
        jobs.active = Some(spawn_job(jobs.sequence, true, move |context| {
            let mut document: MapDocument = serde_json::from_reader(std::fs::File::open(&path)?)?;
            document.rebuild_index()?;
            prepare(Arc::new(document), exaggeration, context)
        }));
        ui.status = "Preparing real GIS preview".into();
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
    if ui.generate_requested {
        ui.generate_requested = false;
        ui.error = None;
        let settings = ui.settings.clone();
        let exaggeration = ui.exaggeration;
        jobs.sequence += 1;
        jobs.active = Some(spawn_job(jobs.sequence, true, move |context| {
            let cache = Cache::new(Cache::default_path())?;
            let document = Arc::new(generation::generate(&settings, &cache, context)?);
            prepare(document, exaggeration, context)
        }));
    } else if ui.rebuild_requested {
        ui.rebuild_requested = false;
        if let Some(document) = view.document.clone() {
            let exaggeration = ui.exaggeration;
            jobs.sequence += 1;
            jobs.active = Some(spawn_job(jobs.sequence, false, move |context| {
                prepare(document, exaggeration, context)
            }));
        }
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
    assets: Res<scene::SceneAssets>,
) {
    let Some(job) = jobs.active.as_mut() else {
        return;
    };
    for p in job.progress.lock().unwrap().try_iter() {
        job.fraction = job.fraction.max(p.fraction.min(0.99));
        ui.status = p.message;
    }
    let result = job.result.lock().unwrap().try_recv();
    match result {
        Ok(result) => {
            let revision = job.revision;
            let refit = job.refit;
            let cancelled = job.context.check().is_err();
            jobs.active = None;
            if cancelled {
                ui.status = "Generation cancelled".into();
                return;
            }
            match result {
                Ok(prepared) if revision > view.revision => {
                    if refit {
                        ui.settings = prepared.document.settings.clone();
                        ui.exaggeration = prepared.geometry.exaggeration;
                    }
                    for entity in &old {
                        commands.entity(entity).despawn();
                    }
                    scene::install(&mut commands, &mut meshes, &assets, &prepared);
                    if refit {
                        orbit.fit(&prepared.document, prepared.geometry.exaggeration);
                        view.selected = None;
                    }
                    ui.status = format!(
                        "{} hexes · {} cities",
                        prepared.document.cells.len(),
                        prepared
                            .document
                            .cells
                            .iter()
                            .map(|c| c.cities.len())
                            .sum::<usize>()
                    );
                    view.exaggeration = prepared.geometry.exaggeration;
                    view.corners = prepared.geometry.corners;
                    view.document = Some(prepared.document);
                    view.revision = revision;
                    view.hovered = None;
                }
                Ok(_) => {}
                Err(error) => {
                    ui.status = "Generation failed".into();
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
    exaggeration: f32,
    context: &JobContext,
) -> Result<PreparedMap> {
    let geometry = terrain::build(&document, exaggeration, context)?;
    let spacing = document.settings.spacing_km * 1000.;
    let projection = crate::map_core::Projection::new(document.settings.region)?;
    let mut models = Vec::new();
    for (i, cell) in document.cells.iter().enumerate() {
        if i % 512 == 0 {
            context.check()?;
        }
        if cell.landscape == Landscape::Forest
            && cell.surface == Surface::Land
            && models.len() < 40_000
        {
            for n in 0..4_u64 {
                let seed = (u64::from(cell.id) * 7919 + n * 104729 + 17) % 65521;
                let angle = seed as f64 * 0.61803398875 * std::f64::consts::TAU;
                let radius = spacing * (0.10 + 0.16 * (seed % 100) as f64 / 100.);
                let p = [
                    cell.center_m[0] + radius * angle.cos(),
                    cell.center_m[1] + radius * angle.sin(),
                ];
                let height = terrain::height_at(cell, p, spacing, &geometry)
                    .context("Tree outside terrain surface")?;
                let position = Vec3::new((p[0] / 1000.) as f32, height, (-p[1] / 1000.) as f32);
                models.push(ModelPlacement {
                    kind: ModelKind::Tree,
                    position,
                    scale: document.settings.spacing_km as f32 * 0.16,
                    yaw: angle as f32,
                });
            }
        }
        let mut positions: Vec<_> = cell.cities.iter().map(|c| [c.lon, c.lat]).collect();
        projection.project(&mut positions)?;
        for (city, p) in cell.cities.iter().zip(positions) {
            let height = terrain::height_at(cell, p, spacing, &geometry)
                .context("City outside terrain surface")?;
            let position = Vec3::new((p[0] / 1000.) as f32, height, (-p[1] / 1000.) as f32);
            let scale = document.settings.spacing_km as f32
                * (0.10 + (city.population as f32).log10().min(7.) * 0.016);
            models.push(ModelPlacement {
                kind: ModelKind::Building,
                position,
                scale,
                yaw: (city.id % 360) as f32 * std::f32::consts::PI / 180.,
            });
        }
    }
    context.check()?;
    Ok(PreparedMap {
        document,
        geometry,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
                            exaggeration: 5.,
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
            .insert_resource(scene::SceneAssets::default())
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
