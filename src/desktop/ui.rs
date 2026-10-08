use super::*;
use bevy_egui::{EguiContexts, egui};

#[derive(Resource, Default)]
pub struct Overview {
    rings: Vec<Vec<[f64; 2]>>,
    drag_start: Option<egui::Pos2>,
}

pub fn setup(mut commands: Commands, mut contexts: EguiContexts) -> bevy::prelude::Result {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("../../assets/ne_110m_land.geojson"))?;
    let mut overview = Overview::default();
    for feature in value["features"].as_array().unwrap() {
        let geometry = &feature["geometry"];
        let coordinates = &geometry["coordinates"];
        let polygons: Vec<_> = if geometry["type"] == "Polygon" {
            vec![coordinates]
        } else {
            coordinates.as_array().unwrap().iter().collect()
        };
        for polygon in polygons {
            for ring in polygon.as_array().unwrap() {
                overview.rings.push(
                    ring.as_array()
                        .unwrap()
                        .iter()
                        .map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()])
                        .collect(),
                );
            }
        }
    }
    commands.insert_resource(overview);
    if let Ok(ctx) = contexts.ctx_mut() {
        ctx.set_visuals(egui::Visuals::dark());
        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        style.spacing.item_spacing = egui::vec2(8., 9.);
        style.visuals.panel_fill = egui::Color32::from_rgb(24, 30, 37);
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(46, 105, 130);
        ctx.set_style_of(egui::Theme::Dark, style);
    }
    Ok(())
}

pub fn show(
    mut contexts: EguiContexts,
    mut state: ResMut<UiState>,
    mut jobs: ResMut<Jobs>,
    view: Res<MapView>,
    mut overview: ResMut<Overview>,
    mut orbit: ResMut<OrbitCamera>,
) -> bevy::prelude::Result {
    let ctx = contexts.ctx_mut()?;
    ctx.set_visuals(egui::Visuals::dark());
    let active = jobs.active.is_some();
    let mut viewport_ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("map-viewport"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    egui::Panel::left("map-controls").exact_size(320.).resizable(false).show(&mut viewport_ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(10.);
            ui.heading("HEX CELL MAP");
            ui.label(egui::RichText::new("Real geography · hex terrain").color(egui::Color32::from_rgb(145, 168, 181)));
            ui.add_space(12.);
            ui.strong("Generate a map");
            egui::ComboBox::from_id_salt("preset").selected_text("Choose a region…").width(270.).show_ui(ui, |ui| {
                for (name, region) in [
                    ("Swiss Alps", Region { west: 7.6, south: 46.3, east: 8.6, north: 47.1 }),
                    ("Hudson & New York", Region { west: -74.5, south: 40.4, east: -73.4, north: 41.4 }),
                    ("Lower Yangtze", Region { west: 118.2, south: 30.8, east: 120., north: 32.4 }),
                ] { if ui.selectable_label(false, name).clicked() { state.settings.region = region; } }
            });
            geographic_selector(ui, &mut overview, &mut state.settings.region);
            ui.small("Drag a rectangle; refine its bounds below.");
            egui::Grid::new("bounds").num_columns(4).spacing([6., 7.]).show(ui, |ui| {
                ui.label("West"); ui.add(egui::DragValue::new(&mut state.settings.region.west).speed(0.01).range(-180. ..=180.).max_decimals(4));
                ui.label("East"); ui.add(egui::DragValue::new(&mut state.settings.region.east).speed(0.01).range(-180. ..=180.).max_decimals(4)); ui.end_row();
                ui.label("South"); ui.add(egui::DragValue::new(&mut state.settings.region.south).speed(0.01).range(-60. ..=60.).max_decimals(4));
                ui.label("North"); ui.add(egui::DragValue::new(&mut state.settings.region.north).speed(0.01).range(-60. ..=60.).max_decimals(4)); ui.end_row();
            });
            ui.add(egui::Slider::new(&mut state.settings.spacing_km, 1. ..=20.).text("km / hex").step_by(0.5));
            if state.last_estimated.as_ref() != Some(&state.settings) {
                state.estimate = estimate(&state.settings).map_err(|e| format!("{e:#}"));
                state.last_estimated = Some(state.settings.clone());
            }
            match &state.estimate {
                Ok(count) => { ui.small(format!("Approximately {count} hexes · limit 100,000")); },
                Err(error) => { ui.colored_label(egui::Color32::from_rgb(242, 170, 133), error); },
            }
            ui.collapsing("Data filters", |ui| {
                ui.horizontal(|ui| { ui.label("River discharge ≥"); ui.add(egui::DragValue::new(&mut state.settings.min_river_discharge).speed(1.).range(0. ..=100_000.).suffix(" m³/s")); });
                ui.horizontal(|ui| { ui.label("City population ≥"); ui.add(egui::DragValue::new(&mut state.settings.min_city_population).speed(100.).range(0..=100_000_000)); });
                ui.add(egui::Slider::new(&mut state.settings.forest_fraction, 0.1..=1.).text("Forest fraction"));
                ui.add(egui::Slider::new(&mut state.settings.mountain_relief_m, 100. ..=1500.).text("Relief (m)"));
                ui.small("Mountains use relief or slope; elevation is never invented.");
            });
            let tiles = worldcover_tile_estimate(state.settings.region);
            ui.small(format!("First-use storage budget: ~{} MB–{} GB. Cached inputs are reused.", 100 * tiles, (tiles + 2).max(1)));
            ui.small("Copernicus DEM · WorldCover · HydroRIVERS · GeoNames");
            if ui.add_enabled(!active && state.estimate.is_ok(), egui::Button::new("Generate real GIS map").min_size(egui::vec2(284., 34.))).clicked() { state.generate_requested = true; }
            if let Some(job) = jobs.active.as_mut() {
                ui.add(egui::ProgressBar::new(job.fraction).show_percentage());
                if ui.button("Cancel").clicked() { job.context.cancel(); state.status = "Cancelling…".into(); }
            }
            ui.label(&state.status);
            if let Some(error) = &state.error { ui.colored_label(egui::Color32::from_rgb(242, 140, 125), error); }
            ui.separator();
            ui.strong("View");
            ui.horizontal(|ui| { ui.checkbox(&mut state.models, "Trees & cities"); ui.checkbox(&mut state.grid, "Hex outlines"); });
            if state.grid && view.document.as_ref().is_some_and(|d| orbit.distance / d.settings.spacing_km as f32 > 400.) { ui.small("Zoom in to see hex outlines."); }
            if ui.add_enabled(!active, egui::Slider::new(&mut state.heights.scale, 0.2..=2.).text("Height scale")).changed() { state.rebuild_requested = true; }
            if ui.add_enabled(!active, egui::Slider::new(&mut state.heights.compression_m, 100. ..=5000.).logarithmic(true).text("Compress above (m)")).changed() { state.rebuild_requested=true; }
            if ui.add_enabled(!active,egui::Slider::new(&mut state.heights.hill_boost,0. ..=1.5).text("Local relief boost")).changed(){state.rebuild_requested=true;}
            ui.small("Ordinary relief keeps its height; extremes soften above the threshold. Local detail stays continuous, with level water and unchanged GIS data.");
            if ui.add_enabled(view.document.is_some(), egui::Button::new("Fit map")).clicked() && let Some(document) = &view.document { orbit.fit(document, view.heights); }
            ui.small("Middle drag / Shift + left drag: pan\nRight drag / Q, E: rotate\nWheel / trackpad: zoom · WASD: pan\nLeft click: inspect a hex");
            ui.separator();
            if let Some(document) = &view.document {
                let colors = [("Plains", egui::Color32::from_rgb(121, 160, 95)), ("Forest", egui::Color32::from_rgb(68, 111, 72)), ("Mountain", egui::Color32::from_rgb(167, 165, 156)), ("River / water", egui::Color32::from_rgb(65, 135, 169)), ("City", egui::Color32::from_rgb(218, 178, 122))];
                ui.horizontal_wrapped(|ui| { for (name, color) in colors { ui.colored_label(color, format!("● {name}")); } });
                if let Some(id) = view.selected.or(view.hovered) { inspector(ui, &document.cells[id as usize]); } else { ui.small("Select a hex to inspect source terrain and features."); }
                if ui.button("Data sources & credits").clicked() { state.credits = true; }
            }
            ui.add_space(8.);
            ui.collapsing("Urban cell editor",|ui| {
                ui.checkbox(&mut state.city_paint,"Paint cities on click");
                ui.add(egui::Slider::new(&mut state.city_radius,0..=3).text("Brush radius"));
                ui.horizontal(|ui| {ui.label("Population");ui.add(egui::DragValue::new(&mut state.city_population).range(1000..=100_000_000));});
                egui::ComboBox::from_id_salt("urban-style").selected_text(format!("{:?}",state.city_style)).show_ui(ui,|ui|{for style in [UrbanStyle::Mixed,UrbanStyle::LowRise,UrbanStyle::Dense]{ui.selectable_value(&mut state.city_style,style,format!("{style:?}"));}});
                ui.add_enabled_ui(!active && view.selected.is_some(),|ui| {ui.horizontal(|ui|{
                    if ui.button("Apply city").clicked(){state.edit_requested=Some(true);}
                    if ui.button("Remove city").clicked(){state.edit_requested=Some(false);}
                });});
                ui.small("River cells retain their river identity; urban buildings use dry ground. Source settlements are retained after removal.");
            });
            ui.add_enabled_ui(!active,|ui| {ui.horizontal(|ui|{
                if ui.add_enabled(!state.undo.is_empty(),egui::Button::new("Undo")).clicked(){state.undo_requested=true;}
                if ui.add_enabled(!state.redo.is_empty(),egui::Button::new("Redo")).clicked(){state.redo_requested=true;}
            });});
            ui.collapsing("Project",|ui| {
                ui.text_edit_singleline(&mut state.project_path);
                ui.add_enabled_ui(!active,|ui| {ui.horizontal(|ui|{
                    if ui.add_enabled(view.document.is_some(),egui::Button::new("Save")).clicked(){state.save_requested=true;}
                    if ui.button("Open").clicked(){state.load_requested=true;}
                });});
                ui.small("Self-contained JSON includes source data, height settings and urban edits.");
            });
            ui.small("General terrain and elevation brushes arrive in the next milestones.");
        });
    });
    if view.document.is_none() && !active {
        egui::Area::new("welcome".into())
            .anchor(egui::Align2::CENTER_CENTER, [160., 0.])
            .interactable(false)
            .show(ctx, |ui| {
                ui.heading("Choose your theater");
                ui.label("Select a region and generate its terrain from real GIS data.");
            });
    }
    if state.credits {
        egui::Window::new("Data sources & credits").open(&mut state.credits).default_width(560.).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(650.).show(ui, |ui| {
                ui.label("Rivers retain logical cell identity; visible channels follow GIS paths with symbolic width. Urban cells use building clusters. Elevations come from Copernicus surface data; land cover is the 2021 snapshot.");
                if let Some(document) = &view.document {
                    for source in &document.sources { ui.separator(); ui.strong(format!("{} · {}", source.name, source.version)); ui.label(&source.attribution); ui.hyperlink_to("Source data", &source.url); ui.hyperlink_to("License & terms", &source.license); ui.small(format!("Acquired Unix time: {} · SHA-256: {}", source.acquired_unix, source.sha256)); }
                }
                ui.separator(); ui.label("Overview: Natural Earth, public domain.");
            });
        });
    }
    state.height_edit_finished = !ctx.input(|i| i.pointer.any_down());
    state.pointer_blocked = pointer_blocked(ctx, viewport_ui.available_rect_before_wrap());
    state.keyboard_blocked = ctx.egui_wants_keyboard_input();
    Ok(())
}

fn pointer_blocked(ctx: &egui::Context, map_rect: egui::Rect) -> bool {
    ctx.egui_is_using_pointer()
        || ctx.any_popup_open()
        || ctx.pointer_interact_pos().is_some_and(|p| {
            !map_rect.contains(p)
                || ctx
                    .layer_id_at(p)
                    .is_some_and(|layer| layer.order != egui::Order::Background)
        })
}

fn inspector(ui: &mut egui::Ui, cell: &Cell) {
    ui.add_space(10.);
    ui.strong(format!("Hex {}, {}", cell.hex.x, cell.hex.y));
    ui.label(format!("{:?} · {:?}", cell.landscape, cell.surface));
    ui.small(format!(
        "{:.4}° N, {:.4}° E",
        cell.lon_lat[1], cell.lon_lat[0]
    ));
    ui.label(format!(
        "Elevation {:.0} m · relief {:.0} m",
        cell.elevation_m, cell.relief_m
    ));
    ui.small(format!(
        "Forest {:.0}% · open water {:.0}%",
        cell.forest_fraction * 100.,
        cell.water_fraction * 100.
    ));
    if !cell.river_ids.is_empty() {
        ui.small(format!("{} source river reaches", cell.river_ids.len()));
    }
    if let Some(urban) = &cell.urban {
        ui.label(format!(
            "Urban cell · {:?} · population {}",
            urban.style, urban.population
        ));
    }
    for city in &cell.cities {
        ui.label(format!("{} · population {}", city.name, city.population));
    }
}

fn estimate(settings: &GenerationSettings) -> Result<usize> {
    settings.validate()?;
    let projection = crate::map_core::Projection::new(settings.region)?;
    let b = projection.bounds(settings.region)?;
    let (width, height) = (b[2] - b[0], b[3] - b[1]);
    anyhow::ensure!(
        width.max(height) <= 1_000_000.,
        "Region must be at most 1,000 km across"
    );
    let spacing = settings.spacing_km * 1000.;
    let count = (width * height / (spacing * spacing * 3_f64.sqrt() / 2.)
        + 2. * (width + height) / spacing)
        .ceil() as usize;
    anyhow::ensure!(
        count <= MAX_CELLS,
        "Increase spacing or reduce the region to stay below 100,000 hexes"
    );
    Ok(count)
}

fn worldcover_tile_estimate(region: Region) -> usize {
    (((region.east / 3.).ceil() - (region.west / 3.).floor()).max(1.)
        * ((region.north / 3.).ceil() - (region.south / 3.).floor()).max(1.)) as usize
}

fn geographic_selector(ui: &mut egui::Ui, overview: &mut Overview, region: &mut Region) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(284., 142.), egui::Sense::drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4., egui::Color32::from_rgb(16, 39, 53));
    let screen = |p: [f64; 2]| {
        egui::pos2(
            rect.left() + ((p[0] + 180.) / 360.) as f32 * rect.width(),
            rect.top() + ((90. - p[1]) / 180.) as f32 * rect.height(),
        )
    };
    for ring in &overview.rings {
        painter.add(egui::Shape::line(
            ring.iter().map(|p| screen(*p)).collect(),
            egui::Stroke::new(0.8, egui::Color32::from_rgb(94, 120, 120)),
        ));
    }
    for latitude in [-60., 60.] {
        painter.line_segment(
            [screen([-180., latitude]), screen([180., latitude])],
            egui::Stroke::new(0.5, egui::Color32::from_rgb(92, 70, 66)),
        );
    }
    let geographic = |p: egui::Pos2| {
        [
            (-180. + f64::from(((p.x - rect.left()) / rect.width()).clamp(0., 1.)) * 360.),
            (90. - f64::from(((p.y - rect.top()) / rect.height()).clamp(0., 1.)) * 180.)
                .clamp(-60., 60.),
        ]
    };
    if response.drag_started() {
        overview.drag_start = ui.input(|i| i.pointer.press_origin());
    }
    if (response.dragged() || response.drag_stopped())
        && let (Some(start), Some(end)) = (overview.drag_start, response.interact_pointer_pos())
    {
        let a = geographic(start);
        let b = geographic(end);
        if (a[0] - b[0]).abs() > 0.01 && (a[1] - b[1]).abs() > 0.01 {
            *region = Region {
                west: a[0].min(b[0]),
                east: a[0].max(b[0]),
                south: a[1].min(b[1]),
                north: a[1].max(b[1]),
            };
        }
    }
    if response.drag_stopped() {
        overview.drag_start = None;
    }
    let selection = egui::Rect::from_two_pos(
        screen([region.west, region.north]),
        screen([region.east, region.south]),
    );
    painter.rect_filled(
        selection,
        0.,
        egui::Color32::from_rgba_unmultiplied(103, 197, 211, 45),
    );
    painter.rect_stroke(
        selection.expand(1.),
        0.,
        egui::Stroke::new(1.2, egui::Color32::from_rgb(132, 217, 228)),
        egui::StrokeKind::Outside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sidebar_captures_pointer_and_map_remains_interactive() {
        let ctx = egui::Context::default();
        for (position, blocked) in [
            (egui::pos2(100., 100.), true),
            (egui::pos2(700., 100.), false),
        ] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 800.),
                )),
                events: vec![egui::Event::PointerMoved(position)],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::Panel::left("test").exact_size(320.).show(ui, |ui| {
                    ui.button("Generate").clicked();
                });
                assert_eq!(
                    pointer_blocked(&ctx, ui.available_rect_before_wrap()),
                    blocked
                );
            });
            output.textures_delta.clear();
        }
    }
}
