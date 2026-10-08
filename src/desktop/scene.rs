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
    pub raw_ground: f64,
    pub offset: f32,
}
#[derive(Component)]
pub struct DisplayMesh {
    heights: Vec<f64>,
    gradients: Vec<[f32; 2]>,
}

#[derive(Resource, Default)]
pub struct SceneAssets {
    terrain: Handle<TerrainMaterial>,
    water: Handle<TerrainMaterial>,
    tree: Handle<Mesh>,
    trunk: Handle<Mesh>,
    building: Handle<Mesh>,
    roof: Handle<Mesh>,
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
        let settings = prepared.geometry.heights;
        let raw_heights: Vec<_> = chunk
            .positions
            .iter()
            .map(|p| settings.inverse_meters(f64::from(p[1]) * 1000.))
            .collect();
        let gradients = chunk
            .normals
            .iter()
            .zip(&raw_heights)
            .map(|(n, h)| {
                let derivative = settings.derivative(*h) as f32;
                [-n[0] / n[1] / derivative, n[2] / n[1] / derivative]
            })
            .collect();
        commands.spawn((
            MapEntity,
            DisplayMesh {
                heights: raw_heights,
                gradients,
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
    for model in &prepared.models {
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
        };
        for (height, mesh, material) in [
            (parts[0], mesh, material),
            (parts[1], upper_mesh, upper_material),
        ] {
            commands.spawn((
                MapEntity,
                EnvironmentModel {
                    raw_ground: prepared
                        .geometry
                        .heights
                        .inverse_meters(f64::from(model.position.y) * 1000.),
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
pub fn height_preview(
    mut ui: ResMut<UiState>,
    mut view: ResMut<MapView>,
    jobs: Res<Jobs>,
    chunks: Query<(&Mesh3d, &DisplayMesh)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut models: Query<(&mut Transform, &EnvironmentModel)>,
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
                        for (p, h) in positions.iter_mut().zip(&display.heights) {
                            p[1] = (settings.meters(*h) / 1000.) as f32;
                        }
                    }
                    if let Some(VertexAttributeValues::Float32x3(normals)) =
                        mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL)
                    {
                        for ((n, h), g) in normals
                            .iter_mut()
                            .zip(&display.heights)
                            .zip(&display.gradients)
                        {
                            let derivative = settings.derivative(*h) as f32;
                            *n = Vec3::new(-g[0] * derivative, 1., g[1] * derivative)
                                .normalize()
                                .to_array();
                        }
                    }
                }
            }
            for (mut transform, model) in &mut models {
                transform.translation.y =
                    (settings.meters(model.raw_ground) / 1000.) as f32 + model.offset;
            }
            for corner in view.corners.values_mut() {
                corner.position[1] = (settings.meters(corner.raw_height) / 1000.) as f32;
                let d = settings.derivative(corner.raw_height) as f32;
                corner.normal =
                    Vec3::new(-corner.raw_gradient[0] * d, 1., corner.raw_gradient[1] * d)
                        .normalize()
                        .to_array();
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
            if ui.city_paint && id.is_some() {
                ui.edit_requested = Some(true);
            }
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
                .filter_map(|i| {
                    let side = (i / terrain::SUBDIVISIONS) as usize % 6;
                    let t = (i % terrain::SUBDIVISIONS) as f64 / f64::from(terrain::SUBDIVISIONS);
                    let a = vertex_position(corner_key(cell.hex, side), f64::from(spacing) * 1000.);
                    let b = vertex_position(
                        corner_key(cell.hex, (side + 1) % 6),
                        f64::from(spacing) * 1000.,
                    );
                    let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                    let height = terrain::surface_height(cell, p, &view.triangles, &view.corners)?;
                    Some(Vec3::new((p[0] / 1000.) as f32, height, (-p[1] / 1000.) as f32) + lift)
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
    fn live_height_preview_keeps_water_models_and_document_aligned() {
        let document = Arc::new(terrain::fixture().unwrap());
        let original = document.height_field.elevations_m.clone();
        let settings = HeightSettings {
            scale: 0.7,
            compression_m: 300.,
        };
        let mut meshes = Assets::<Mesh>::default();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0., 0., 0.], [1., 0., 1.]]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0., 1., 0.]; 2]);
        let handle = meshes.add(mesh);
        let mut app = App::new();
        app.insert_resource(meshes)
            .insert_resource(Jobs::default())
            .insert_resource(MapView {
                document: Some(document),
                heights: HeightSettings::default(),
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
                heights: vec![2000., 20.],
                gradients: vec![[0.3, 0.1], [0.02, 0.]],
            },
        ));
        let model = app
            .world_mut()
            .spawn((
                Transform::default(),
                EnvironmentModel {
                    raw_ground: 2000.,
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
        assert!((positions[0][1] - settings.meters(2000.) as f32 / 1000.).abs() < 1e-6);
        assert!((positions[1][1] - settings.meters(20.) as f32 / 1000.).abs() < 1e-6);
        assert!(
            (app.world().get::<Transform>(model).unwrap().translation.y - positions[0][1] - 0.1)
                .abs()
                < 1e-6
        );
        let view = app.world().resource::<MapView>();
        assert_eq!(view.document.as_ref().unwrap().heights, settings);
        assert_eq!(
            view.document.as_ref().unwrap().height_field.elevations_m,
            original
        );
        assert_eq!(app.world().resource::<UiState>().undo.len(), 1);
    }
}
