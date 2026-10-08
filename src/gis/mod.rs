pub mod cache;
pub mod raster;
pub mod vectors;

use anyhow::{Context, Result};
use std::{path::PathBuf, sync::OnceLock};

/// Configure GDAL once, before any dataset or coordinate system is created.
pub fn initialize() -> Result<()> {
    static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();
    INITIALIZED
        .get_or_init(|| configure().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn configure() -> Result<()> {
    let exe = std::env::current_exe().context("Find application directory")?;
    let parent = exe
        .parent()
        .context("Application has no parent directory")?;
    let candidates = [
        parent.join("gis-data/proj"),
        parent.join("../Resources/gis-data/proj"),
    ];
    let proj = std::env::var_os("HEX_MAP_PROJ_DATA")
        .map(PathBuf::from)
        .or_else(|| candidates.into_iter().find(|p| p.join("proj.db").is_file()));
    if let Some(path) = proj {
        gdal::config::set_config_option("PROJ_DATA", &path.to_string_lossy())?;
    }
    // Static OpenSSL does not inherit the system certificate store. Keep verified
    // HTTPS enabled with the bundled Mozilla CA roots on every installation.
    let certificate_directory = cache::Cache::default_path();
    std::fs::create_dir_all(&certificate_directory)?;
    let certificates = include_bytes!("../../assets/cacert.pem");
    let certificate_path = certificate_directory.join("mozilla-cacert.pem");
    if std::fs::read(&certificate_path).ok().as_deref() != Some(certificates) {
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(&certificate_directory)?;
        temporary.write_all(certificates)?;
        temporary.persist(&certificate_path)?;
    }
    gdal::config::set_config_option("CURL_CA_BUNDLE", &certificate_path.to_string_lossy())?;
    for (key, value) in [
        ("GDAL_HTTP_TIMEOUT", "4"),
        ("GDAL_HTTP_CONNECTTIMEOUT", "4"),
        ("GDAL_HTTP_MAX_RETRY", "1"),
        ("GDAL_HTTP_RETRY_DELAY", "1"),
        ("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR"),
        ("CPL_VSIL_CURL_ALLOWED_EXTENSIONS", ".tif"),
        ("GDAL_CACHEMAX", "128"),
        ("CPL_VSIL_CURL_CACHE_SIZE", "33554432"),
    ] {
        gdal::config::set_config_option(key, value)?;
    }
    Ok(())
}
