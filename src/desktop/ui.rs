use super::*;
use bevy_egui::{EguiContexts, egui};

#[derive(Resource, Default)]
pub struct Overview {
    rings: Vec<Vec<[f64; 2]>>,
    drag_start: Option<egui::Pos2>,
    selector_open: bool,
}

pub fn setup(mut commands: Commands) -> bevy::prelude::Result {
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
    Ok(())
}

// The primary egui context is created after Startup. Install fonts in its UI
// schedule so the first available context receives the bundled CJK fallback.
pub fn setup_context(
    mut contexts: EguiContexts,
    mut initialized: Local<bool>,
) -> bevy::prelude::Result {
    if !*initialized {
        let ctx = contexts.ctx_mut()?;
        ctx.set_fonts(language::fonts());
        ctx.set_visuals(egui::Visuals::dark());
        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        style.spacing.item_spacing = egui::vec2(8., 9.);
        style.visuals.panel_fill = egui::Color32::from_rgb(24, 30, 37);
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(46, 105, 130);
        ctx.set_style_of(egui::Theme::Dark, style);
        *initialized = true;
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
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
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
    egui::Panel::left("map-controls")
        .exact_size(320.)
        .resizable(false)
        .show(&mut viewport_ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(10.);
                ui.horizontal(|ui| {
                    ui.label(state.locale.text("language.label"));
                    let previous = state.locale;
                    egui::ComboBox::from_id_salt("language")
                        .selected_text(state.locale.name())
                        .show_ui(ui, |ui| {
                            for locale in Locale::ALL {
                                ui.selectable_value(&mut state.locale, locale, locale.name());
                            }
                        });
                    if state.locale != previous {
                        language::save_preference(state.locale);
                    }
                });
                let locale = state.locale;
                let t = |key| locale.text(key);
                ui.heading(t("app.title"));
                ui.label(
                    egui::RichText::new(t("app.tagline"))
                        .color(egui::Color32::from_rgb(145, 168, 181)),
                );
                ui.add_space(12.);
                ui.strong(t("generate.heading"));
                egui::ComboBox::from_id_salt("preset")
                    .selected_text(t("generate.choose_region"))
                    .width(270.)
                    .show_ui(ui, |ui| {
                        for (name, region) in [
                            (
                                t("region.alps"),
                                Region {
                                    west: 7.6,
                                    south: 46.3,
                                    east: 8.6,
                                    north: 47.1,
                                },
                            ),
                            (
                                t("region.hudson"),
                                Region {
                                    west: -74.5,
                                    south: 40.4,
                                    east: -73.4,
                                    north: 41.4,
                                },
                            ),
                            (
                                t("region.yangtze"),
                                Region {
                                    west: 118.2,
                                    south: 30.8,
                                    east: 120.,
                                    north: 32.4,
                                },
                            ),
                            (
                                t("region.grand_canyon"),
                                Region {
                                    west: -113.,
                                    south: 35.7,
                                    east: -111.5,
                                    north: 36.7,
                                },
                            ),
                            (
                                t("region.mount_fuji"),
                                Region {
                                    west: 138.1,
                                    south: 34.8,
                                    east: 139.3,
                                    north: 36.,
                                },
                            ),
                            (
                                t("region.nile_delta"),
                                Region {
                                    west: 29.5,
                                    south: 30.,
                                    east: 32.3,
                                    north: 31.6,
                                },
                            ),
                            (
                                t("region.rio"),
                                Region {
                                    west: -43.8,
                                    south: -23.2,
                                    east: -42.8,
                                    north: -22.5,
                                },
                            ),
                            (
                                t("region.queenstown"),
                                Region {
                                    west: 168.,
                                    south: -45.4,
                                    east: 169.3,
                                    north: -44.5,
                                },
                            ),
                        ] {
                            if ui.selectable_label(false, name).clicked() {
                                state.settings.region = region;
                            }
                        }
                    });
                geographic_preview(ui, &mut overview, &mut state.settings.region, locale);
                ui.small(t("region.open_hint"));
                region_bounds(ui, &mut state.settings.region, locale);
                ui.add(
                    egui::Slider::new(&mut state.settings.spacing_km, 1. ..=20.)
                        .text(t("region.spacing"))
                        .step_by(0.5),
                );
                if state.last_estimated.as_ref() != Some(&state.settings) {
                    state.estimate = estimate(&state.settings).map_err(|e| format!("{e:#}"));
                    state.last_estimated = Some(state.settings.clone());
                }
                match &state.estimate {
                    Ok(count) => {
                        ui.small(
                            Message::new("region.estimate")
                                .arg("count", count)
                                .localized(locale),
                        );
                    }
                    Err(error) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(242, 170, 133),
                            locale.diagnostic(error),
                        );
                    }
                }
                egui::CollapsingHeader::new(t("filters.heading"))
                    .id_salt("data-filters")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(t("filters.river_discharge"));
                            ui.add(
                                egui::DragValue::new(&mut state.settings.min_river_discharge)
                                    .speed(1.)
                                    .range(0. ..=100_000.)
                                    .suffix(" m³/s"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(t("filters.city_population"));
                            ui.add(
                                egui::DragValue::new(&mut state.settings.min_city_population)
                                    .speed(100.)
                                    .range(0..=100_000_000),
                            );
                        });
                        ui.add(
                            egui::Slider::new(&mut state.settings.forest_fraction, 0.1..=1.)
                                .text(t("filters.forest_fraction")),
                        );
                        ui.add(
                            egui::Slider::new(&mut state.settings.mountain_relief_m, 100. ..=1500.)
                                .text(t("filters.relief")),
                        );
                        ui.small(t("filters.mountain_hint"));
                    });
                let tiles = worldcover_tile_estimate(state.settings.region);
                ui.small(
                    Message::new("generate.storage")
                        .arg("mb", 100 * tiles)
                        .arg("gb", (tiles + 2).max(1))
                        .localized(locale),
                );
                ui.small("Copernicus DEM · WorldCover · HydroRIVERS · GeoNames");
                if ui
                    .add_enabled(
                        !active && state.estimate.is_ok(),
                        egui::Button::new(t("generate.button")).min_size(egui::vec2(284., 34.)),
                    )
                    .clicked()
                {
                    state.generate_requested = true;
                }
                if let Some(job) = jobs.active.as_mut() {
                    ui.add(egui::ProgressBar::new(job.fraction).show_percentage());
                    if ui.button(t("action.cancel")).clicked() {
                        job.context.cancel();
                        state.status = Message::new("status.cancelling");
                    }
                }
                ui.label(state.status.localized(locale));
                if let Some(error) = &state.error {
                    ui.colored_label(
                        egui::Color32::from_rgb(242, 140, 125),
                        locale.diagnostic(error),
                    );
                }
                ui.separator();
                ui.strong(t("view.heading"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.models, t("view.models"));
                    ui.checkbox(&mut state.grid, t("view.outlines"));
                });
                if state.grid
                    && view
                        .document
                        .as_ref()
                        .is_some_and(|d| orbit.distance / d.settings.spacing_km as f32 > 400.)
                {
                    ui.small(t("view.zoom_hint"));
                }
                if ui
                    .add_enabled(
                        !active,
                        egui::Slider::new(&mut state.heights.scale, 0.2..=2.)
                            .text(t("view.height_scale")),
                    )
                    .changed()
                {
                    state.rebuild_requested = true;
                }
                if ui
                    .add_enabled(
                        !active,
                        egui::Slider::new(&mut state.heights.compression_m, 100. ..=5000.)
                            .logarithmic(true)
                            .text(t("view.compression")),
                    )
                    .changed()
                {
                    state.rebuild_requested = true;
                }
                if ui
                    .add_enabled(
                        !active,
                        egui::Slider::new(&mut state.heights.hill_boost, 0. ..=1.5)
                            .text(t("view.hill_boost")),
                    )
                    .changed()
                {
                    state.rebuild_requested = true;
                }
                ui.small(t("view.height_hint"));
                if ui
                    .add_enabled(view.document.is_some(), egui::Button::new(t("view.fit")))
                    .clicked()
                    && let Some(document) = &view.document
                {
                    orbit.fit(document, view.heights);
                }
                ui.small(t("view.controls"));
                ui.separator();
                if let Some(document) = &view.document {
                    let colors = [
                        (t("landscape.plains"), egui::Color32::from_rgb(121, 160, 95)),
                        (t("landscape.forest"), egui::Color32::from_rgb(68, 111, 72)),
                        (
                            t("landscape.mountain"),
                            egui::Color32::from_rgb(167, 165, 156),
                        ),
                        (t("legend.water"), egui::Color32::from_rgb(65, 135, 169)),
                        (t("surface.city"), egui::Color32::from_rgb(218, 178, 122)),
                    ];
                    ui.horizontal_wrapped(|ui| {
                        for (name, color) in colors {
                            ui.colored_label(color, format!("● {name}"));
                        }
                    });
                    if let Some(id) = view.selected.or(view.hovered) {
                        inspector(ui, &document.cells[id as usize], locale);
                    } else {
                        ui.small(t("inspector.hint"));
                    }
                    if ui.button(t("credits.heading")).clicked() {
                        state.credits = true;
                    }
                }
                ui.add_space(8.);
                egui::CollapsingHeader::new(t("urban.heading"))
                    .id_salt("urban-editor")
                    .show(ui, |ui| {
                        ui.checkbox(&mut state.city_paint, t("urban.paint"));
                        ui.add(
                            egui::Slider::new(&mut state.city_radius, 0..=3)
                                .text(t("urban.radius")),
                        );
                        ui.horizontal(|ui| {
                            ui.label(t("urban.population"));
                            ui.add(
                                egui::DragValue::new(&mut state.city_population)
                                    .range(1000..=100_000_000),
                            );
                        });
                        egui::ComboBox::from_id_salt("urban-style")
                            .selected_text(urban_style(locale, state.city_style))
                            .show_ui(ui, |ui| {
                                for style in
                                    [UrbanStyle::Mixed, UrbanStyle::LowRise, UrbanStyle::Dense]
                                {
                                    ui.selectable_value(
                                        &mut state.city_style,
                                        style,
                                        urban_style(locale, style),
                                    );
                                }
                            });
                        ui.add_enabled_ui(!active && view.selected.is_some(), |ui| {
                            ui.horizontal(|ui| {
                                if ui.button(t("urban.apply")).clicked() {
                                    state.edit_requested = Some(true);
                                }
                                if ui.button(t("urban.remove")).clicked() {
                                    state.edit_requested = Some(false);
                                }
                            });
                        });
                        ui.small(t("urban.hint"));
                    });
                ui.add_enabled_ui(!active, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !state.undo.is_empty(),
                                egui::Button::new(t("action.undo")),
                            )
                            .clicked()
                        {
                            state.undo_requested = true;
                        }
                        if ui
                            .add_enabled(
                                !state.redo.is_empty(),
                                egui::Button::new(t("action.redo")),
                            )
                            .clicked()
                        {
                            state.redo_requested = true;
                        }
                    });
                });
                egui::CollapsingHeader::new(t("project.heading"))
                    .id_salt("project")
                    .show(ui, |ui| {
                        ui.text_edit_singleline(&mut state.project_path);
                        ui.add_enabled_ui(!active, |ui| {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        view.document.is_some(),
                                        egui::Button::new(t("action.save")),
                                    )
                                    .clicked()
                                {
                                    state.save_requested = true;
                                }
                                if ui.button(t("action.open")).clicked() {
                                    state.load_requested = true;
                                }
                            });
                        });
                        ui.small(t("project.hint"));
                    });
                ui.small(t("editor.future_hint"));
            });
        });
    let locale = state.locale;
    let t = |key| locale.text(key);
    if let Ok(mut window) = windows.single_mut()
        && window.title != t("window.title")
    {
        window.title = t("window.title").into();
    }
    if view.document.is_none() && !active {
        egui::Area::new("welcome".into())
            .anchor(egui::Align2::CENTER_CENTER, [160., 0.])
            .interactable(false)
            .show(ctx, |ui| {
                ui.heading(t("welcome.heading"));
                ui.label(t("welcome.hint"));
            });
    }
    if state.credits {
        egui::Window::new(t("credits.heading"))
            .id(egui::Id::new("credits"))
            .open(&mut state.credits)
            .default_width(560.)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(650.)
                    .show(ui, |ui| {
                        ui.label(t("credits.hint"));
                        if let Some(document) = &view.document {
                            for source in &document.sources {
                                ui.separator();
                                ui.strong(format!("{} · {}", source.name, source.version));
                                ui.label(&source.attribution);
                                ui.hyperlink_to(t("credits.source"), &source.url);
                                ui.hyperlink_to(t("credits.license"), &source.license);
                                ui.small(
                                    Message::new("credits.acquired")
                                        .arg("time", source.acquired_unix)
                                        .arg("hash", &source.sha256)
                                        .localized(locale),
                                );
                            }
                        }
                        ui.separator();
                        ui.label(t("credits.overview"));
                        ui.separator();
                        ui.strong(t("credits.art"));
                        ui.hyperlink_to("Kenney Nature Kit", "https://kenney.nl/assets/nature-kit");
                        ui.hyperlink_to(
                            "Kenney City Kit (Suburban)",
                            "https://kenney.nl/assets/city-kit-suburban",
                        );
                        ui.hyperlink_to(t("credits.materials"), "https://polyhaven.com/license");
                        ui.separator();
                        ui.hyperlink_to(
                            t("credits.font"),
                            "https://github.com/notofonts/noto-cjk/blob/main/Sans/LICENSE",
                        );
                    });
            });
    }
    let selecting_region = overview.selector_open;
    geographic_popup(ctx, &mut overview, &mut state.settings.region, locale);
    state.height_edit_finished = !ctx.input(|i| i.pointer.any_down());
    state.pointer_blocked =
        selecting_region || pointer_blocked(ctx, viewport_ui.available_rect_before_wrap());
    state.keyboard_blocked = selecting_region || ctx.egui_wants_keyboard_input();
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

fn urban_style(locale: Locale, style: UrbanStyle) -> &'static str {
    locale.text(match style {
        UrbanStyle::Mixed => "urban.mixed",
        UrbanStyle::LowRise => "urban.low_rise",
        UrbanStyle::Dense => "urban.dense",
    })
}

fn inspector(ui: &mut egui::Ui, cell: &Cell, locale: Locale) {
    ui.add_space(10.);
    ui.strong(
        Message::new("inspector.hex")
            .arg("q", cell.hex.x)
            .arg("r", cell.hex.y)
            .localized(locale),
    );
    let landscape = locale.text(match cell.landscape {
        Landscape::Plains => "landscape.plains",
        Landscape::Forest => "landscape.forest",
        Landscape::Mountain => "landscape.mountain",
    });
    let surface = locale.text(match cell.surface {
        Surface::Land => "surface.land",
        Surface::River => "surface.river",
        Surface::Water => "surface.water",
        Surface::City => "surface.city",
    });
    ui.label(format!("{landscape} · {surface}"));
    ui.small(
        Message::new("inspector.coordinates")
            .arg("lat", format!("{:.4}", cell.lon_lat[1]))
            .arg("lon", format!("{:.4}", cell.lon_lat[0]))
            .localized(locale),
    );
    ui.label(
        Message::new("inspector.elevation")
            .arg("elevation", format!("{:.0}", cell.elevation_m))
            .arg("relief", format!("{:.0}", cell.relief_m))
            .localized(locale),
    );
    ui.small(
        Message::new("inspector.cover")
            .arg("forest", format!("{:.0}", cell.forest_fraction * 100.))
            .arg("water", format!("{:.0}", cell.water_fraction * 100.))
            .localized(locale),
    );
    if !cell.river_ids.is_empty() {
        ui.small(
            Message::new("inspector.rivers")
                .arg("count", cell.river_ids.len())
                .localized(locale),
        );
    }
    if let Some(urban) = &cell.urban {
        ui.label(
            Message::new("inspector.urban")
                .arg("style", urban_style(locale, urban.style))
                .arg("population", urban.population)
                .localized(locale),
        );
    }
    for city in &cell.cities {
        ui.label(
            Message::new("inspector.city")
                .arg("name", &city.name)
                .arg("population", city.population)
                .localized(locale),
        );
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

fn region_bounds(ui: &mut egui::Ui, region: &mut Region, locale: Locale) {
    egui::Grid::new("bounds")
        .num_columns(4)
        .spacing([6., 7.])
        .show(ui, |ui| {
            for (key, value, limit) in [
                ("region.west", &mut region.west, 180.),
                ("region.east", &mut region.east, 180.),
                ("region.south", &mut region.south, 60.),
                ("region.north", &mut region.north, 60.),
            ] {
                ui.label(locale.text(key));
                ui.add(
                    egui::DragValue::new(value)
                        .speed(0.01)
                        .range(-limit..=limit)
                        .max_decimals(4),
                );
                if matches!(key, "region.east" | "region.north") {
                    ui.end_row();
                }
            }
        });
}

fn geographic_preview(
    ui: &mut egui::Ui,
    overview: &mut Overview,
    region: &mut Region,
    locale: Locale,
) -> egui::Response {
    let response = geographic_selector(
        ui,
        overview,
        region,
        egui::vec2(284., 142.),
        egui::Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .on_hover_text(locale.text("region.open_hint"));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            locale.text("region.selector_title"),
        )
    });
    if response.clicked() {
        overview.drag_start = None;
        overview.selector_open = true;
    }
    response
}

fn geographic_popup(
    ctx: &egui::Context,
    overview: &mut Overview,
    region: &mut Region,
    locale: Locale,
) {
    if !overview.selector_open {
        return;
    }
    let available = ctx.content_rect().size();
    let width = (available.x - 64.)
        .min((available.y - 240.) * 2.)
        .clamp(1., 1120.);
    let popup = egui::Modal::new(egui::Id::new("geographic-selector")).show(ctx, |ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.heading(locale.text("region.selector_title"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(locale.text("action.close")).clicked() {
                    ui.close();
                }
            });
        });
        ui.small(locale.text("region.drag_hint"));
        geographic_selector(
            ui,
            overview,
            region,
            egui::vec2(width, width / 2.),
            egui::Sense::drag(),
        )
        .on_hover_cursor(egui::CursorIcon::Crosshair);
        region_bounds(ui, region, locale);
    });
    if popup.should_close() {
        overview.selector_open = false;
        overview.drag_start = None;
    }
}

fn geographic_selector(
    ui: &mut egui::Ui,
    overview: &mut Overview,
    region: &mut Region,
    size: egui::Vec2,
    sense: egui::Sense,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, sense);
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
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn selector_frame(
        ctx: &egui::Context,
        overview: &mut Overview,
        region: &mut Region,
        size: egui::Vec2,
        locale: Locale,
        events: Vec<egui::Event>,
    ) -> egui::Rect {
        let mut preview = egui::Rect::NOTHING;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |ui| {
                preview = geographic_preview(ui, overview, region, locale).rect;
                geographic_popup(ctx, overview, region, locale);
            },
        );
        output.textures_delta.clear();
        preview
    }

    #[test]
    fn preview_opens_popup_and_dragged_selection_survives_escape() {
        let ctx = egui::Context::default();
        let mut overview = Overview::default();
        let original = GenerationSettings::default().region;
        let mut region = original;
        let size = egui::vec2(1440., 960.);
        let mut frame =
            |events| selector_frame(&ctx, &mut overview, &mut region, size, Locale::En, events);
        let preview = frame(vec![]);
        let position = preview.center();
        frame(vec![
            egui::Event::PointerMoved(position),
            pointer_button(position, true),
        ]);
        frame(vec![pointer_button(position, false)]);
        frame(vec![]);
        assert!(overview.selector_open);
        assert_eq!(region, original);
        let popup = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("geographic-selector")))
            .unwrap();
        assert!(popup.width() > preview.width() * 3.);
        assert!(ctx.content_rect().contains_rect(popup));

        let start = popup.center() + egui::vec2(40., 40.);
        let end = popup.center() - egui::vec2(40., 40.);
        for events in [
            vec![
                egui::Event::PointerMoved(start),
                pointer_button(start, true),
            ],
            vec![egui::Event::PointerMoved(end)],
            vec![pointer_button(end, false)],
        ] {
            selector_frame(&ctx, &mut overview, &mut region, size, Locale::En, events);
        }
        assert_ne!(region, original);
        region.validate().unwrap();
        assert!(overview.drag_start.is_none());
        let selected = region;

        selector_frame(
            &ctx,
            &mut overview,
            &mut region,
            size,
            Locale::En,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(!overview.selector_open);
        assert_eq!(region, selected);
    }

    #[test]
    fn popup_fits_smaller_windows_and_captures_outside_clicks() {
        for (size, locale) in [
            (egui::vec2(800., 600.), Locale::ZhCn),
            (egui::vec2(640., 480.), Locale::En),
        ] {
            let ctx = egui::Context::default();
            ctx.set_fonts(language::fonts());
            let mut overview = Overview {
                selector_open: true,
                ..Default::default()
            };
            let mut region = GenerationSettings::default().region;
            for _ in 0..2 {
                selector_frame(&ctx, &mut overview, &mut region, size, locale, vec![]);
            }
            let popup = ctx
                .memory(|memory| memory.area_rect(egui::Id::new("geographic-selector")))
                .unwrap();
            assert!(
                ctx.content_rect().contains_rect(popup),
                "{size:?}: {popup:?}"
            );
            let original = region;
            let outside = egui::pos2(size.x - 5., size.y - 5.);
            selector_frame(
                &ctx,
                &mut overview,
                &mut region,
                size,
                locale,
                vec![
                    egui::Event::PointerMoved(outside),
                    pointer_button(outside, true),
                ],
            );
            assert!(pointer_blocked(&ctx, ctx.content_rect()));
            assert!(overview.selector_open);
            selector_frame(
                &ctx,
                &mut overview,
                &mut region,
                size,
                locale,
                vec![pointer_button(outside, false)],
            );
            assert!(!overview.selector_open);
            assert_eq!(region, original);
        }
    }

    #[test]
    fn chinese_font_is_installed_when_context_appears_after_startup() {
        let mut app = App::new();
        app.init_resource::<bevy_egui::EguiUserTextures>()
            .add_systems(Startup, setup)
            .add_systems(EguiPrimaryContextPass, setup_context);
        app.update();
        let entity = app
            .world_mut()
            .spawn((
                bevy_egui::EguiContext::default(),
                bevy_egui::PrimaryEguiContext,
            ))
            .id();
        app.world_mut().run_schedule(EguiPrimaryContextPass);
        let ctx = app
            .world_mut()
            .get_mut::<bevy_egui::EguiContext>(entity)
            .unwrap()
            .get_mut()
            .clone();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("简体中文");
            ctx.fonts_mut(|fonts| {
                assert!(fonts.has_glyphs(&egui::FontId::proportional(14.), "简体中文"));
            });
        });
        output.textures_delta.clear();
    }

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
