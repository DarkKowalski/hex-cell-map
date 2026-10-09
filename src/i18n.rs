//! Embedded UI translations. Messages retain their arguments so changing the
//! language also updates in-flight progress and previously displayed statuses.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, sync::OnceLock};

type Catalog = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN")]
    ZhCn,
}

impl Locale {
    pub const ALL: [Self; 2] = [Self::En, Self::ZhCn];

    pub fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::ZhCn => "简体中文",
        }
    }

    /// Accept BCP 47 and POSIX locale names; traditional Chinese falls back to
    /// English unless an explicit Simplified Chinese script is requested.
    pub fn from_language_tag(tag: &str) -> Self {
        let tag = tag.split(['.', '@']).next().unwrap_or(tag);
        let tag = tag.replace('_', "-").to_ascii_lowercase();
        let parts: Vec<_> = tag.split('-').collect();
        if parts.first() == Some(&"zh")
            && (parts.contains(&"hans")
                || (!parts.contains(&"hant")
                    && (parts.len() == 1 || parts.contains(&"cn") || parts.contains(&"sg"))))
        {
            Self::ZhCn
        } else {
            Self::En
        }
    }

    pub fn text(self, key: &'static str) -> &'static str {
        self.catalog()
            .get(key)
            .or_else(|| Self::En.catalog().get(key))
            .map(String::as_str)
            .unwrap_or(key)
    }

    fn catalog(self) -> &'static Catalog {
        static EN: OnceLock<Catalog> = OnceLock::new();
        static ZH_CN: OnceLock<Catalog> = OnceLock::new();
        let (catalog, json) = match self {
            Self::En => (&EN, include_str!("../locales/en.json")),
            Self::ZhCn => (&ZH_CN, include_str!("../locales/zh-CN.json")),
        };
        catalog.get_or_init(|| serde_json::from_str(json).expect("valid embedded locale catalog"))
    }

    /// Translate application validation errors while retaining upstream GIS,
    /// filesystem and network diagnostics verbatim for troubleshooting.
    pub fn diagnostic(self, diagnostic: &str) -> String {
        diagnostic
            .split(": ")
            .map(|part| {
                Self::En
                    .catalog()
                    .iter()
                    .find(|(key, value)| key.starts_with("error.") && value.as_str() == part)
                    .map_or(part, |(key, _)| self.text(key))
            })
            .collect::<Vec<_>>()
            .join(": ")
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Translated {
        key: &'static str,
        args: Vec<(&'static str, String)>,
    },
    Raw(String),
}

impl Message {
    pub fn new(key: &'static str) -> Self {
        Self::Translated { key, args: vec![] }
    }

    pub fn arg(mut self, name: &'static str, value: impl fmt::Display) -> Self {
        if let Self::Translated { args, .. } = &mut self {
            args.push((name, value.to_string()));
        }
        self
    }

    pub fn localized(&self, locale: Locale) -> String {
        let (key, args) = match self {
            Self::Translated { key, args } => (*key, args),
            Self::Raw(value) => return locale.diagnostic(value),
        };
        // Substitute in one pass so braces in a filename or city name are
        // always literal, even when they resemble another placeholder.
        let mut rest = locale.text(key);
        let mut output = String::with_capacity(rest.len());
        while let Some(start) = rest.find('{') {
            output.push_str(&rest[..start]);
            let Some(end) = rest[start..].find('}').map(|end| start + end) else {
                output.push_str(&rest[start..]);
                return output;
            };
            let name = &rest[start + 1..end];
            if let Some((_, value)) = args.iter().find(|(arg, _)| *arg == name) {
                output.push_str(value);
            } else {
                output.push_str(&rest[start..=end]);
            }
            rest = &rest[end + 1..];
        }
        output.push_str(rest);
        output
    }
}

impl From<String> for Message {
    fn from(value: String) -> Self {
        Self::Raw(value)
    }
}

impl From<&str> for Message {
    fn from(value: &str) -> Self {
        Self::Raw(value.into())
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.localized(Locale::En))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(value: &str) -> Vec<&str> {
        let mut names: Vec<_> = value
            .split('{')
            .skip(1)
            .map(|part| part.split_once('}').expect("closed placeholder").0)
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn catalogs_have_matching_keys_and_placeholders() {
        let en = Locale::En.catalog();
        let zh = Locale::ZhCn.catalog();
        assert_eq!(en.keys().collect::<Vec<_>>(), zh.keys().collect::<Vec<_>>());
        for (key, value) in en {
            assert!(!value.is_empty(), "empty English translation: {key}");
            assert!(!zh[key].is_empty(), "empty Chinese translation: {key}");
            assert_eq!(placeholders(value), placeholders(&zh[key]), "{key}");
        }
    }

    #[test]
    fn language_tags_and_unsupported_locales() {
        for tag in [
            "zh",
            "zh-CN",
            "zh_CN.UTF-8",
            "ZH-cn",
            "zh-SG",
            "zh-Hans",
            "zh-Hans-HK",
        ] {
            assert_eq!(Locale::from_language_tag(tag), Locale::ZhCn, "{tag}");
        }
        for tag in ["en-US", "de-DE", "C", "", "zh-TW", "zh-Hant-CN", "zh-HK"] {
            assert_eq!(Locale::from_language_tag(tag), Locale::En, "{tag}");
        }
        assert_eq!(Locale::ZhCn.text("unknown.key"), "unknown.key");
    }

    #[test]
    fn messages_retranslate_without_replacing_argument_contents() {
        let message = Message::new("progress.downloading")
            .arg("filename", "地图{attempt}.zip")
            .arg("attempt", 2);
        assert_eq!(
            message.to_string(),
            "Downloading 地图{attempt}.zip (attempt 2)"
        );
        assert_eq!(
            message.localized(Locale::ZhCn),
            "正在下载 地图{attempt}.zip（第 2 次尝试）"
        );
        assert_eq!(
            message.to_string(),
            "Downloading 地图{attempt}.zip (attempt 2)"
        );
        assert_eq!(
            Locale::ZhCn.diagnostic("Region coordinates must be finite"),
            "区域坐标必须为有限值"
        );
        assert_eq!(
            Locale::ZhCn.diagnostic("GDAL: file not found"),
            "GDAL: file not found"
        );
    }
}
