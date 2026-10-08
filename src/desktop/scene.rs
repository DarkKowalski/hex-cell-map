use super::{
    camera::{MapCamera, OrbitCamera},
    *,
};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    picking::mesh_picking::ray_cast::RayCastVisibility,
    render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk},
    window::PrimaryWindow,
};

#[derive(Component)]
pub struct MapEntity;

#[derive(Component)]
pub struct TerrainChunk {
    pub triangle_cells: Vec<u32>,
}

#[derive(Component)]
pub struct EnvironmentModel {
    pub cell_id: u32,
    pub point_m: [f64; 2],
    pub offset: f32,
}
#[derive(Component)]
pub struct GridLines {
    handle: Handle<GizmoAsset>,
    segments: Vec<[terrain::VertexKey; 2]>,
}

#[derive(Component)]
pub struct DisplayMesh {
    vertices: Vec<crate::elevation::DisplayVertex>,
}

#[derive(Resource, Default)]
pub struct SceneAssets {
    terrain: Handle<TerrainMaterial>,
    water: Handle<TerrainMaterial>,
    tree: Handle<Mesh>,
    trunk: Handle<Mesh>,
    building: Handle<Mesh>,
    roof: Handle<Mesh>,
    rock: Handle<Mesh>,
    grass: Handle<Mesh>,
    shrub: Handle<Mesh>,
    stone: Handle<StandardMaterial>,
    foliage: [Handle<StandardMaterial>; 3],
    bark: Handle<StandardMaterial>,
    wall: [Handle<StandardMaterial>; 3],
    roof_material: Handle<StandardMaterial>,
}

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut terrain_materials: ResMut<Assets<TerrainMaterial>>,
) {
    commands.insert_resource(SceneAssets {
        terrain: terrain_materials.add(TerrainMaterial {
            base: StandardMaterial {
                perceptual_roughness: 0.9,
                ..default()
            },
            extension: TerrainBlend {
                settings: Vec4::new(1., 0., 0., 0.),
            },
        }),
        water: terrain_materials.add(TerrainMaterial {
            base: StandardMaterial {
                perceptual_roughness: 0.28,
                ..default()
            },
            extension: TerrainBlend {
                settings: Vec4::new(1., 1., 0., 0.),
            },
        }),
        rock: meshes.add(Sphere::new(0.5).mesh().ico(1).unwrap()),
        grass: meshes.add(Cone::new(0.75, 0.3)),
        shrub: meshes.add(Sphere::new(0.5).mesh().ico(1).unwrap()),
        stone: materials.add(Color::srgb(0.39, 0.36, 0.30)),
        tree: meshes.add(Cone::new(0.55, 1.5)),
        trunk: meshes.add(Cylinder::new(0.07, 0.7)),
        building: meshes.add(Cuboid::new(0.8, 0.85, 0.6)),
        roof: meshes.add(Cone::new(0.62, 0.5).mesh().resolution(4)),
        foliage: [
            Color::srgb(0.16, 0.29, 0.10),
            Color::srgb(0.10, 0.23, 0.13),
            Color::srgb(0.24, 0.32, 0.13),
        ]
        .map(|c| materials.add(c)),
        bark: materials.add(Color::srgb(0.28, 0.19, 0.105)),
        wall: [
            Color::srgb(0.59, 0.54, 0.43),
            Color::srgb(0.68, 0.61, 0.48),
            Color::srgb(0.46, 0.48, 0.47),
        ]
        .map(|c| materials.add(c)),
        roof_material: materials.add(Color::srgb(0.49, 0.19, 0.095)),
    });
    commands.spawn((
        Camera3d::default(),
        camera::MapCamera,
        Transform::from_xyz(0., 80., 60.).looking_at(Vec3::ZERO, Vec3::Y),
        bevy::prelude::Projection::Perspective(PerspectiveProjection {
            near: 0.01,
            far: 10_000.,
            ..default()
        }),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-80., 120., 60.).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

pub(super) fn install(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    gizmo_assets: &mut Assets<GizmoAsset>,
    assets: &SceneAssets,
    prepared: &PreparedMap,
) {
    for (is_water, chunk) in prepared
        .geometry
        .chunks
        .iter()
        .map(|c| (false, c))
        .chain(prepared.geometry.water_chunks.iter().map(|c| (true, c)))
    {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, chunk.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, chunk.normals.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, chunk.weights.clone());
        mesh.insert_indices(Indices::U32(chunk.indices.clone()));
        commands.spawn((
            MapEntity,
            DisplayMesh {
                vertices: chunk.display.clone(),
            },
            TerrainChunk {
                triangle_cells: chunk.triangle_cells.clone(),
            },
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(if is_water {
                assets.water.clone()
            } else {
                assets.terrain.clone()
            }),
            Transform::default(),
        ));
    }
    let outline_water = prepared
        .geometry
        .water_chunks
        .iter()
        .flat_map(|c| {
            c.boundaries
                .iter()
                .map(|(k, v)| (*k, v.display.samples[0].height))
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for (_, ids) in prepared.document.chunk_ids() {
        let mut segments = Vec::new();
        for id in ids {
            let outline = &prepared.geometry.outlines[id];
            for j in 0..outline.len() {
                let a = outline[j];
                let b = outline[(j + 1) % outline.len()];
                let edge = if a < b { [a, b] } else { [b, a] };
                if seen.insert(edge) {
                    segments.push(edge);
                }
            }
        }
        let grid = grid_asset(
            &segments,
            &prepared.geometry.corners,
            &outline_water,
            prepared.geometry.heights,
            prepared.document.settings.spacing_km as f32 * 0.008,
        );
        let handle = gizmo_assets.add(grid);
        commands.spawn((
            MapEntity,
            GridLines {
                handle: handle.clone(),
                segments,
            },
            Gizmo {
                handle: Handle::default(),
                depth_bias: -0.0001,
                ..default()
            },
        ));
    }
    for model in &prepared.models {
        let single = match model.kind {
            ModelKind::Rock => Some((&assets.rock, &assets.stone, Vec3::new(1., 0.65, 0.8), 0.325)),
            ModelKind::Shrub => Some((
                &assets.shrub,
                &assets.foliage[model.variant],
                Vec3::new(1., 0.75, 1.),
                0.375,
            )),
            ModelKind::Grass => Some((
                &assets.grass,
                &assets.foliage[model.variant],
                Vec3::new(1., 1., 1.),
                0.15,
            )),
            _ => None,
        };
        if let Some((mesh, material, scale, height)) = single {
            commands.spawn((
                MapEntity,
                EnvironmentModel {
                    cell_id: model.cell_id,
                    point_m: model.point_m,
                    offset: height * model.scale,
                },
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(model.position + Vec3::Y * height * model.scale)
                    .with_scale(scale * model.scale)
                    .with_rotation(Quat::from_rotation_y(model.yaw)),
            ));
            continue;
        }
        let (parts, mesh, material, upper_mesh, upper_material) = match model.kind {
            ModelKind::Tree => (
                [0.35, 1.15],
                &assets.trunk,
                &assets.bark,
                &assets.tree,
                &assets.foliage[model.variant],
            ),
            ModelKind::Building => (
                [0.425, 1.10],
                &assets.building,
                &assets.wall[model.variant],
                &assets.roof,
                &assets.roof_material,
            ),
            _ => unreachable!(),
        };
        for (height, mesh, material) in [
            (parts[0], mesh, material),
            (parts[1], upper_mesh, upper_material),
        ] {
            commands.spawn((
                MapEntity,
                EnvironmentModel {
                    cell_id: model.cell_id,
                    point_m: model.point_m,
                    offset: height * model.scale,
                },
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(model.position + Vec3::Y * height * model.scale)
                    .with_scale(Vec3::splat(model.scale))
                    .with_rotation(Quat::from_rotation_y(model.yaw)),
            ));
        }
    }
}

/// A display-only change updates existing assets immediately. No source access,
/// channel regeneration or triangulation is needed while dragging a slider.
#[allow(clippy::too_many_arguments)] // Bevy system parameters.
pub fn height_preview(
    mut ui: ResMut<UiState>,
    mut view: ResMut<MapView>,
    jobs: Res<Jobs>,
    chunks: Query<(&Mesh3d, &DisplayMesh)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut models: Query<(&mut Transform, &EnvironmentModel)>,
    grids: Query<&GridLines>,
    mut gizmo_assets: ResMut<Assets<GizmoAsset>>,
) {
    if jobs.active.is_some() {
        return;
    }
    if ui.rebuild_requested {
        ui.rebuild_requested = false;
        if let Some(document) = view.document.clone()
            && view.heights != ui.heights
        {
            if ui.height_edit_origin.is_none() {
                ui.height_edit_origin = Some(document.clone());
            }
            let settings = ui.heights;
            for (handle, display) in &chunks {
                if let Some(mut mesh) = meshes.get_mut(&handle.0) {
                    if let Some(VertexAttributeValues::Float32x3(positions)) =
                        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
                    {
                        for (p, h) in positions.iter_mut().zip(&display.vertices) {
                            p[1] = (h.meters(settings) / 1000.) as f32;
                        }
                    }
                    if let Some(VertexAttributeValues::Float32x3(normals)) =
                        mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL)
                    {
                        for (n, vertex) in normals.iter_mut().zip(&display.vertices) {
                            *n = vertex.normal(settings);
                        }
                    }
                }
            }
            for corner in view.corners.values_mut() {
                corner.position[1] = (corner.display.meters(settings) / 1000.) as f32;
                corner.normal = corner.display.normal(settings);
            }
            for grid in &grids {
                if let Some(mut asset) = gizmo_assets.get_mut(&grid.handle) {
                    *asset = grid_asset(
                        &grid.segments,
                        &view.corners,
                        &view.outline_water,
                        settings,
                        document.settings.spacing_km as f32 * 0.008,
                    );
                }
            }
            for (mut transform, model) in &mut models {
                if let Some(height) = terrain::surface_height(
                    &document.cells[model.cell_id as usize],
                    model.point_m,
                    &view.triangles,
                    &view.corners,
                ) {
                    transform.translation.y = height + model.offset;
                }
            }
            let mut changed = (*document).clone();
            changed.heights = settings;
            view.document = Some(Arc::new(changed));
            view.heights = settings;
        }
    }
    if ui.height_edit_finished {
        ui.height_edit_finished = false;
        if let Some(previous) = ui.height_edit_origin.take() {
            ui.undo.push(previous);
            ui.redo.clear();
            if ui.undo.len() > 64 {
                ui.undo.remove(0);
            }
        }
    }
}

pub fn models_visibility(
    ui: Res<UiState>,
    orbit: Res<OrbitCamera>,
    mut models: Query<(&Transform, &mut Visibility), With<EnvironmentModel>>,
) {
    let range = (orbit.distance * 1.2).max(10.);
    for (transform, mut visibility) in &mut models {
        let visible =
            ui.models && transform.translation.distance_squared(orbit.target) < range * range;
        let desired = if visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != desired {
            *visibility = desired;
        }
    }
}

#[derive(PartialEq)]
struct PickKey {
    ray: Ray3d,
    revision: u64,
    heights: HeightSettings,
}
pub struct CachedPick {
    key: PickKey,
    cell: Option<u32>,
}

#[allow(clippy::too_many_arguments)] // Bevy system parameters.
pub fn pick(
    mut ui: ResMut<UiState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<MapCamera>>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    chunks: Query<&TerrainChunk>,
    mut raycast: MeshRayCast,
    mut view: ResMut<MapView>,
    mut cached: Local<Option<CachedPick>>,
) {
    view.hovered = None;
    if ui.pointer_blocked || view.document.is_none() {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, transform)) = cameras.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(transform, cursor) else {
        return;
    };
    let key = PickKey {
        ray,
        revision: view.revision,
        heights: view.heights,
    };
    let id = if let Some(previous) = cached.as_ref().filter(|p| p.key == key) {
        previous.cell
    } else {
        let filter = |entity| chunks.contains(entity);
        let id = raycast
            .cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter))
            .first()
            .and_then(|(entity, hit)| {
                hit.triangle_index
                    .and_then(|t| chunks.get(*entity).ok()?.triangle_cells.get(t).copied())
            });
        *cached = Some(CachedPick { key, cell: id });
        id
    };
    view.hovered = id;
    if buttons.just_pressed(MouseButton::Left) && !keys.pressed(KeyCode::ShiftLeft) {
        view.selected = id;
        if ui.city_paint && id.is_some() {
            ui.edit_requested = Some(true);
        }
    }
}

fn outline_position(
    key: terrain::VertexKey,
    corners: &BTreeMap<terrain::VertexKey, terrain::Corner>,
    water: &BTreeMap<terrain::VertexKey, f64>,
    heights: HeightSettings,
    lift: f32,
) -> Vec3 {
    let mut p = Vec3::from_array(corners[&key].position);
    if let Some(h) = water.get(&key) {
        p.y = p.y.max((heights.meters(*h) / 1000.) as f32);
    }
    p + Vec3::Y * lift
}
fn grid_asset(
    segments: &[[terrain::VertexKey; 2]],
    corners: &BTreeMap<terrain::VertexKey, terrain::Corner>,
    water: &BTreeMap<terrain::VertexKey, f64>,
    heights: HeightSettings,
    lift: f32,
) -> GizmoAsset {
    let mut asset = GizmoAsset::default();
    for [a, b] in segments {
        asset.line(
            outline_position(*a, corners, water, heights, lift),
            outline_position(*b, corners, water, heights, lift),
            Color::srgba(0.12, 0.17, 0.12, 0.38),
        );
    }
    asset
}
pub fn grid_visibility(
    ui: Res<UiState>,
    view: Res<MapView>,
    orbit: Res<OrbitCamera>,
    mut grids: Query<(&GridLines, &mut Gizmo)>,
) {
    // Avoid drawing subpixel lines across huge maps. The cached assets remain
    // available immediately when zoomed in or toggled back on.
    let show = ui.grid
        && view
            .document
            .as_ref()
            .is_some_and(|d| orbit.distance / (d.settings.spacing_km as f32) < 400.);
    for (grid, mut gizmo) in &mut grids {
        let desired = if show {
            grid.handle.clone()
        } else {
            Handle::default()
        };
        if gizmo.handle != desired {
            gizmo.handle = desired;
        }
    }
}
pub fn overlay(view: Res<MapView>, mut gizmos: Gizmos) {
    let Some(document) = &view.document else {
        return;
    };
    let lift = document.settings.spacing_km as f32 * 0.008;
    let draw = |id: u32, color: Color, gizmos: &mut Gizmos| {
        if let Some(keys) = view.outlines.get(id as usize)
            && let Some(first) = keys.first()
        {
            gizmos.linestrip(
                keys.iter().chain(std::iter::once(first)).map(|key| {
                    outline_position(*key, &view.corners, &view.outline_water, view.heights, lift)
                }),
                color,
            );
        }
    };
    if let Some(id) = view.hovered {
        draw(id, Color::srgb(0.62, 0.82, 0.93), &mut gizmos);
    }
    if let Some(id) = view.selected {
        draw(id, Color::srgb(1., 0.77, 0.27), &mut gizmos);
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system parameters.
pub fn smoke_validation(
    mut options: ResMut<LaunchOptions>,
    view: Res<MapView>,
    ui: Res<UiState>,
    chunks: Query<&TerrainChunk>,
    mut raycast: MeshRayCast,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    time: Res<Time>,
    orbit: Res<OrbitCamera>,
    cameras: Query<(&Camera, &GlobalTransform), With<MapCamera>>,
) {
    if !options.smoke {
        return;
    }
    if let Some(error) = &ui.error {
        error!("Smoke failed: {error}");
        exit.write(AppExit::error());
        return;
    }
    if time.elapsed_secs() > 90. {
        error!("Smoke validation timed out");
        exit.write(AppExit::error());
        return;
    }
    let Some(document) = &view.document else {
        return;
    };
    options.ready_frames += 1;
    if options.ready_frames < 90 || options.capture_started {
        return;
    }
    if !options.picking_checked {
        let filter = |entity| chunks.contains(entity);
        let settings = MeshRayCastSettings::default()
            .with_filter(&filter)
            .with_visibility(RayCastVisibility::Any);
        let mut checked = 0;
        for cell in document
            .cells
            .iter()
            .filter(|c| c.hex.x.rem_euclid(32) == 0 || c.hex.y.rem_euclid(32) == 0)
            .step_by(3)
            .take(32)
        {
            // Refined river triangles can be millimeter slivers at a hex edge.
            // Use the largest face of a boundary cell for a stable interior ray.
            let triangle = *view.triangles[cell.id as usize]
                .iter()
                .max_by(|a, b| {
                    let area = |keys: &&[terrain::VertexKey; 3]| {
                        let p = keys.map(|k| view.corners[&k].position);
                        ((p[1][0] - p[0][0]) * (p[2][2] - p[0][2])
                            - (p[1][2] - p[0][2]) * (p[2][0] - p[0][0]))
                            .abs()
                    };
                    area(a).total_cmp(&area(b))
                })
                .unwrap();
            let point = triangle
                .map(|key| Vec3::from_array(view.corners[&key].position))
                .into_iter()
                .sum::<Vec3>()
                / 3.;
            let ray = Ray3d::new(point + Vec3::Y * 100., Dir3::NEG_Y);
            let hit = raycast.cast_ray(ray, &settings).first();
            let actual = hit.and_then(|(entity, hit)| {
                hit.triangle_index
                    .and_then(|i| chunks.get(*entity).ok()?.triangle_cells.get(i).copied())
            });
            if actual != Some(cell.id) {
                error!(
                    "Terrain pick mismatch: expected {}, got {:?}",
                    cell.id, actual
                );
                exit.write(AppExit::error());
                return;
            }
            checked += 1;
        }
        if checked == 0 {
            error!("Smoke had no boundary cells to check");
            exit.write(AppExit::error());
            return;
        }
        info!("SMOKE: {checked} terrain picks passed on chunk-boundary slopes");
        if let Ok((camera, transform)) = cameras.single() {
            info!(
                "SMOKE: camera distance {} at {:?}, viewport {:?}",
                orbit.distance,
                transform.translation(),
                camera.logical_viewport_size()
            );
        }
        options.picking_checked = true;
    }
    if let Some(path) = options.screenshot.clone() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path))
            .observe(
                |_: On<ScreenshotCaptured>, mut exit: MessageWriter<AppExit>| {
                    info!("SMOKE: native frame captured");
                    exit.write(AppExit::Success);
                },
            );
        options.capture_started = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outline_toggles_reuse_cached_assets_without_rebuilding_lines() {
        let document = Arc::new(terrain::fixture().unwrap());
        let mut assets = Assets::<GizmoAsset>::default();
        let mut lines = GizmoAsset::default();
        lines.line(Vec3::ZERO, Vec3::X, Color::WHITE);
        let handle = assets.add(lines);
        let mut app = App::new();
        app.insert_resource(assets)
            .insert_resource(UiState::default())
            .insert_resource(MapView {
                document: Some(document),
                ..default()
            })
            .insert_resource(OrbitCamera {
                distance: 100.,
                ..default()
            })
            .add_systems(Update, grid_visibility);
        let entity = app
            .world_mut()
            .spawn((
                GridLines {
                    handle: handle.clone(),
                    segments: vec![],
                },
                Gizmo::default(),
            ))
            .id();
        for frame in 0..120 {
            let on = frame % 2 == 0;
            app.world_mut().resource_mut::<UiState>().grid = on;
            app.update();
            let active = &app.world().get::<Gizmo>(entity).unwrap().handle;
            assert_eq!(
                *active,
                if on {
                    handle.clone()
                } else {
                    Handle::default()
                }
            );
            assert_eq!(app.world().resource::<Assets<GizmoAsset>>().len(), 1);
        }
        app.world_mut().resource_mut::<OrbitCamera>().distance = 1000.;
        app.world_mut().resource_mut::<UiState>().grid = true;
        app.update();
        assert_eq!(
            app.world().get::<Gizmo>(entity).unwrap().handle,
            Handle::default()
        );
    }
    #[test]
    fn live_height_preview_keeps_water_models_and_document_aligned() {
        let document = Arc::new(terrain::fixture().unwrap());
        let original = document.height_field.elevations_m.clone();
        let geometry = terrain::build(
            &document,
            document.heights,
            &crate::jobs::JobContext::default(),
        )
        .unwrap();
        let settings = HeightSettings {
            scale: 1.2,
            compression_m: 1200.,
            hill_boost: 0.7,
        };
        let mut meshes = Assets::<Mesh>::default();
        let chunk = &geometry.chunks[0];
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, chunk.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, chunk.normals.clone());
        let handle = meshes.add(mesh);
        let cell = document
            .cells
            .iter()
            .find(|c| c.hex == hexx::Hex::ZERO)
            .unwrap();
        let cell_id = cell.id;
        let point = cell.center_m;
        let mut app = App::new();
        app.insert_resource(meshes)
            .insert_resource(Assets::<GizmoAsset>::default())
            .insert_resource(Jobs::default())
            .insert_resource(MapView {
                document: Some(document),
                heights: HeightSettings::default(),
                corners: geometry.corners,
                triangles: geometry.triangles,
                ..default()
            })
            .insert_resource(UiState {
                heights: settings,
                rebuild_requested: true,
                height_edit_finished: true,
                ..default()
            })
            .add_systems(Update, height_preview);
        app.world_mut().spawn((
            Mesh3d(handle.clone()),
            DisplayMesh {
                vertices: chunk.display.clone(),
            },
        ));
        let model = app
            .world_mut()
            .spawn((
                Transform::default(),
                EnvironmentModel {
                    cell_id,
                    point_m: point,
                    offset: 0.1,
                },
            ))
            .id();
        app.update();
        let assets = app.world().resource::<Assets<Mesh>>();
        let positions = assets
            .get(&handle)
            .unwrap()
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .unwrap()
            .as_float3()
            .unwrap();
        assert!((positions[0][1] - chunk.display[0].meters(settings) as f32 / 1000.).abs() < 1e-6);
        let view = app.world().resource::<MapView>();
        let doc = view.document.as_ref().unwrap();
        let height = terrain::surface_height(
            &doc.cells[cell_id as usize],
            point,
            &view.triangles,
            &view.corners,
        )
        .unwrap();
        assert!(
            (app.world().get::<Transform>(model).unwrap().translation.y - height - 0.1).abs()
                < 1e-6
        );
        assert_eq!(doc.heights, settings);
        assert_eq!(doc.height_field.elevations_m, original);
        assert_eq!(app.world().resource::<UiState>().undo.len(), 1);
    }
}
