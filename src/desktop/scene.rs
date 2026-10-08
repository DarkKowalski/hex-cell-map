use super::{
    camera::{MapCamera, OrbitCamera},
    *,
};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
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
pub struct EnvironmentModel;

#[derive(Resource, Default)]
pub struct SceneAssets {
    terrain: Handle<TerrainMaterial>,
    tree: Handle<Mesh>,
    trunk: Handle<Mesh>,
    building: Handle<Mesh>,
    roof: Handle<Mesh>,
    foliage: Handle<StandardMaterial>,
    bark: Handle<StandardMaterial>,
    wall: Handle<StandardMaterial>,
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
                settings: Vec4::ONE,
            },
        }),
        tree: meshes.add(Cone::new(0.55, 1.5)),
        trunk: meshes.add(Cylinder::new(0.07, 0.7)),
        building: meshes.add(Cuboid::new(0.8, 0.85, 0.6)),
        roof: meshes.add(Cone::new(0.62, 0.5).mesh().resolution(4)),
        foliage: materials.add(Color::srgb(0.12, 0.30, 0.13)),
        bark: materials.add(Color::srgb(0.28, 0.19, 0.105)),
        wall: materials.add(Color::srgb(0.84, 0.72, 0.51)),
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
            illuminance: 12_000.,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-80., 120., 60.).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

pub(super) fn install(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    assets: &SceneAssets,
    prepared: &PreparedMap,
) {
    for chunk in &prepared.geometry.chunks {
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
            TerrainChunk {
                triangle_cells: chunk.triangle_cells.clone(),
            },
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(assets.terrain.clone()),
            Transform::default(),
        ));
    }
    for model in &prepared.models {
        let (parts, mesh, material, upper_mesh, upper_material) = match model.kind {
            ModelKind::Tree => (
                [0.35, 1.15],
                &assets.trunk,
                &assets.bark,
                &assets.tree,
                &assets.foliage,
            ),
            ModelKind::Building => (
                [0.425, 1.10],
                &assets.building,
                &assets.wall,
                &assets.roof,
                &assets.roof_material,
            ),
        };
        for (height, mesh, material) in [
            (parts[0], mesh, material),
            (parts[1], upper_mesh, upper_material),
        ] {
            commands.spawn((
                MapEntity,
                EnvironmentModel,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(model.position + Vec3::Y * height * model.scale)
                    .with_scale(Vec3::splat(model.scale))
                    .with_rotation(Quat::from_rotation_y(model.yaw)),
            ));
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

#[allow(clippy::too_many_arguments)] // Bevy system parameters.
pub fn pick(
    ui: Res<UiState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<MapCamera>>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    chunks: Query<&TerrainChunk>,
    mut raycast: MeshRayCast,
    mut view: ResMut<MapView>,
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
    let filter = |entity| chunks.contains(entity);
    if let Some((entity, hit)) = raycast
        .cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter))
        .first()
    {
        let id = hit
            .triangle_index
            .and_then(|t| chunks.get(*entity).ok()?.triangle_cells.get(t).copied());
        view.hovered = id;
        if buttons.just_pressed(MouseButton::Left) && !keys.pressed(KeyCode::ShiftLeft) {
            view.selected = id;
        }
    }
}

pub fn overlay(view: Res<MapView>, ui: Res<UiState>, mut gizmos: Gizmos) {
    let Some(document) = &view.document else {
        return;
    };
    let spacing = document.settings.spacing_km as f32;
    let lift = Vec3::Y * (spacing * 0.008);
    let draw_cell = |id: u32, color: Color, gizmos: &mut Gizmos| {
        if let Some(cell) = document.cells.get(id as usize) {
            let points: Vec<_> = (0..=6 * terrain::SUBDIVISIONS)
                .map(|i| {
                    let side = (i / terrain::SUBDIVISIONS) as usize % 6;
                    let step = i % terrain::SUBDIVISIONS;
                    let a = terrain::outer_key(cell.hex, side);
                    let b = terrain::outer_key(cell.hex, (side + 1) % 6);
                    let key = (
                        a.0 + (b.0 - a.0) * step / terrain::SUBDIVISIONS,
                        a.1 + (b.1 - a.1) * step / terrain::SUBDIVISIONS,
                    );
                    Vec3::from_array(view.corners[&key].position) + lift
                })
                .collect();
            gizmos.linestrip(points, color);
        }
    };
    if ui.grid && document.cells.len() <= 12_000 {
        for cell in &document.cells {
            draw_cell(cell.id, Color::srgba(0.12, 0.17, 0.12, 0.38), &mut gizmos);
        }
    }
    if let Some(id) = view.hovered {
        draw_cell(id, Color::srgb(0.62, 0.82, 0.93), &mut gizmos);
    }
    if let Some(id) = view.selected {
        draw_cell(id, Color::srgb(1., 0.77, 0.27), &mut gizmos);
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
            let triangle = terrain::cell_triangles(cell.hex)[12];
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
