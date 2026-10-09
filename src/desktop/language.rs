use crate::i18n::Locale;
use bevy_egui::egui;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct Preferences {
    locale: Locale,
}

fn preferences_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("hex-cell-map/preferences.json"))
}

fn read_preference(path: &Path) -> Option<Locale> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Preferences>(&bytes)
        .ok()
        .map(|preferences| preferences.locale)
}

pub fn initial_locale() -> Locale {
    preferences_path()
        .as_deref()
        .and_then(read_preference)
        .unwrap_or_else(|| {
            sys_locale::get_locale()
                .map(|tag| Locale::from_language_tag(&tag))
                .unwrap_or_default()
        })
}

fn write_preference(path: &Path, locale: Locale) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(&Preferences { locale })?)?;
    Ok(())
}

pub fn save_preference(locale: Locale) {
    if let Some(path) = preferences_path()
        && let Err(error) = write_preference(&path, locale)
    {
        bevy::log::warn!("Could not save language preference: {error}");
    }
}

pub fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let name = "noto-sans-sc".to_owned();
    fonts.font_data.insert(
        name.clone(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/NotoSansSC-Regular.otf"))
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(name.clone());
    }
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_round_trip_and_invalid_file_fallback() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings/preferences.json");
        assert_eq!(read_preference(&path), None);
        for locale in Locale::ALL {
            write_preference(&path, locale)?;
            assert_eq!(read_preference(&path), Some(locale));
        }
        std::fs::write(&path, b"invalid JSON")?;
        assert_eq!(read_preference(&path), None);
        std::fs::write(&path, br#"{"locale":"unsupported"}"#)?;
        assert_eq!(read_preference(&path), None);
        Ok(())
    }

    #[test]
    fn bundled_font_covers_chinese_catalog_and_city_names() {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts());
        let catalog: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../../locales/zh-CN.json")).unwrap();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("简体中文");
            ctx.fonts_mut(|fonts| {
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    let font = egui::FontId::new(14., family);
                    for value in catalog
                        .values()
                        .map(String::as_str)
                        .chain(["上海 南京 杭州 苏州"])
                    {
                        // Default fonts supply Latin text and common symbols;
                        // verify the CJK glyphs supplied by the bundled fallback.
                        for character in value
                            .chars()
                            .filter(|c| matches!(u32::from(*c), 0x3000..=0x9fff | 0xff00..=0xffef))
                        {
                            assert!(
                                fonts.has_glyph(&font, character),
                                "missing glyph {character}"
                            );
                        }
                    }
                }
            });
        });
        output.textures_delta.clear();
    }
}
