use super::{MapView, UiState};
use bevy::{
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit},
    prelude::*,
};

#[derive(Component)]
pub struct MapCamera;

#[derive(Resource)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub target_goal: Vec3,
    pub yaw: f32,
    pub yaw_goal: f32,
    pub distance: f32,
    pub distance_goal: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            target_goal: Vec3::ZERO,
            yaw: 0.,
            yaw_goal: 0.,
            distance: 100.,
            distance_goal: 100.,
        }
    }
}

impl OrbitCamera {
    pub fn fit(
        &mut self,
        document: &crate::map_core::MapDocument,
        heights: crate::map_core::HeightSettings,
    ) {
        let b = document.bounds_m;
        let average =
            document.cells.iter().map(|c| c.elevation_m).sum::<f64>() / document.cells.len() as f64;
        self.target_goal = Vec3::new(
            ((b[0] + b[2]) / 2000.) as f32,
            (heights.meters(average) / 1000.) as f32,
            (-(b[1] + b[3]) / 2000.) as f32,
        );
        self.distance_goal =
            (((b[2] - b[0]).max(b[3] - b[1]) / 1000.) as f32 * 1.5).clamp(2., 3000.);
        self.target = self.target_goal;
        self.distance = self.distance_goal;
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system parameters.
pub fn controls(
    options: Res<super::LaunchOptions>,
    ui: Res<UiState>,
    view: Res<MapView>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut orbit: ResMut<OrbitCamera>,
) {
    if options.smoke || view.document.is_none() {
        return;
    }
    let dt = time.delta_secs().min(0.1);
    let right = Vec3::new(orbit.yaw.cos(), 0., -orbit.yaw.sin());
    let forward = Vec3::new(-orbit.yaw.sin(), 0., -orbit.yaw.cos());
    if !ui.pointer_blocked {
        if buttons.pressed(MouseButton::Right) {
            orbit.yaw_goal -= motion.delta.x * 0.007;
        }
        if buttons.pressed(MouseButton::Middle)
            || (buttons.pressed(MouseButton::Left) && keys.pressed(KeyCode::ShiftLeft))
        {
            let speed = orbit.distance * 0.0012;
            orbit.target_goal += (-right * motion.delta.x + forward * motion.delta.y) * speed;
        }
        let wheel = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y * 0.12,
            MouseScrollUnit::Pixel => scroll.delta.y * 0.003,
        };
        orbit.distance_goal = (orbit.distance_goal * (-wheel).exp()).clamp(0.5, 3000.);
    }
    if !ui.keyboard_blocked {
        let x = f32::from(keys.pressed(KeyCode::KeyD)) - f32::from(keys.pressed(KeyCode::KeyA));
        let y = f32::from(keys.pressed(KeyCode::KeyW)) - f32::from(keys.pressed(KeyCode::KeyS));
        let speed = orbit.distance * dt * 0.5;
        orbit.target_goal += (right * x + forward * y) * speed;
        orbit.yaw_goal +=
            (f32::from(keys.pressed(KeyCode::KeyQ)) - f32::from(keys.pressed(KeyCode::KeyE))) * dt;
    }
    if let Some(document) = &view.document {
        let b = document.bounds_m;
        let margin = orbit.distance_goal * 0.5;
        orbit.target_goal.x = orbit
            .target_goal
            .x
            .clamp(b[0] as f32 / 1000. - margin, b[2] as f32 / 1000. + margin);
        orbit.target_goal.z = orbit
            .target_goal
            .z
            .clamp(-b[3] as f32 / 1000. - margin, -b[1] as f32 / 1000. + margin);
    }
}

pub fn animate(
    time: Res<Time>,
    view: Res<MapView>,
    mut orbit: ResMut<OrbitCamera>,
    mut cameras: Query<&mut Transform, With<MapCamera>>,
) {
    if let Some(document) = &view.document {
        let point = [
            f64::from(orbit.target_goal.x) * 1000.,
            -f64::from(orbit.target_goal.z) * 1000.,
        ];
        let spacing = document.settings.spacing_km * 1000.;
        if let Some(cell) = document.cell(crate::map_core::point_hex(point, spacing))
            && let Some(height) =
                crate::terrain::surface_height(cell, point, &view.triangles, &view.corners)
        {
            orbit.target_goal.y = height;
        }
    }
    let alpha = 1. - (-10. * time.delta_secs()).exp();
    let target_goal = orbit.target_goal;
    orbit.target = orbit.target.lerp(target_goal, alpha);
    orbit.distance += (orbit.distance_goal - orbit.distance) * alpha;
    orbit.yaw += (orbit.yaw_goal - orbit.yaw) * alpha;
    let pitch = 55_f32.to_radians();
    let offset = Vec3::new(
        orbit.yaw.sin() * pitch.cos(),
        pitch.sin(),
        orbit.yaw.cos() * pitch.cos(),
    ) * orbit.distance;
    let mut position = orbit.target + offset;
    if let Some(document) = &view.document {
        let point = [
            f64::from(position.x) * 1000.,
            -f64::from(position.z) * 1000.,
        ];
        let spacing = document.settings.spacing_km * 1000.;
        if let Some(cell) = document.cell(crate::map_core::point_hex(point, spacing))
            && let Some(height) =
                crate::terrain::surface_height(cell, point, &view.triangles, &view.corners)
        {
            position.y = position
                .y
                .max(height + document.settings.spacing_km as f32 * 0.12);
        }
    }
    for mut transform in &mut cameras {
        *transform = Transform::from_translation(position).looking_at(orbit.target, Vec3::Y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_and_keyboard_capture_block_camera_input() {
        let document = crate::terrain::fixture().unwrap();
        let mut app = App::new();
        let mut buttons = ButtonInput::<MouseButton>::default();
        buttons.press(MouseButton::Right);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyD);
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(50));
        app.insert_resource(UiState {
            pointer_blocked: true,
            keyboard_blocked: true,
            ..Default::default()
        })
        .insert_resource(MapView {
            document: Some(std::sync::Arc::new(document)),
            ..Default::default()
        })
        .insert_resource(time)
        .insert_resource(super::super::LaunchOptions::default())
        .insert_resource(keys)
        .insert_resource(buttons)
        .insert_resource(AccumulatedMouseMotion {
            delta: Vec2::new(100., 100.),
        })
        .insert_resource(AccumulatedMouseScroll {
            unit: MouseScrollUnit::Line,
            delta: Vec2::new(0., 10.),
        })
        .init_resource::<OrbitCamera>()
        .add_systems(Update, controls);
        app.update();
        let orbit = app.world().resource::<OrbitCamera>();
        assert_eq!(orbit.yaw_goal, 0.);
        assert_eq!(orbit.distance_goal, 100.);
        assert_eq!(orbit.target_goal, Vec3::ZERO);
        let mut ui = app.world_mut().resource_mut::<UiState>();
        ui.pointer_blocked = false;
        ui.keyboard_blocked = false;
        app.update();
        let orbit = app.world().resource::<OrbitCamera>();
        assert!(orbit.yaw_goal < -0.6);
        assert!(orbit.distance_goal < 100.);
        assert!(orbit.target_goal.x > 0.);
    }
}
