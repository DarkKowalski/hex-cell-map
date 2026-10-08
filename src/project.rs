//! Self-contained project snapshots. Render meshes and GIS caches are not needed.
use crate::map_core::MapDocument;
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};
pub fn save(document: &MapDocument, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("Create project temporary file")?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(&mut writer, document)?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    if path.exists() {
        std::fs::copy(path, path.with_extension("hexmap.bak"))?;
    }
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .context("Replace project file")?;
    Ok(())
}
pub fn load(path: &Path) -> Result<MapDocument> {
    let file = File::open(path).context("Open project file")?;
    anyhow::ensure!(
        file.metadata()?.len() <= 1_000_000_000,
        "Project exceeds 1 GB limit"
    );
    let mut document: MapDocument =
        serde_json::from_reader(BufReader::new(file)).context("Read project JSON")?;
    document.rebuild_index()?;
    Ok(document)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_core::*;
    #[test]
    fn snapshot_preserves_heights_sources_cities_and_edits_without_cache() -> Result<()> {
        let mut d = crate::terrain::fixture()?;
        d.heights = HeightSettings {
            scale: 0.7,
            compression_m: 350.,
            hill_boost: 0.6,
        };
        d.cells[0].urban = Some(UrbanTerrain {
            population: 12345,
            style: UrbanStyle::Dense,
        });
        d.cells[0].edited = true;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("test.hexmap.json");
        save(&d, &path)?;
        let reopened = load(&path)?;
        assert!(
            serde_json::to_value(&d)? == serde_json::to_value(&reopened)?,
            "Snapshot changed canonical fields"
        );
        save(&d, &path)?;
        assert!(path.with_extension("hexmap.bak").exists());
        std::fs::write(&path, b"broken")?;
        assert!(load(&path).is_err());
        assert!(load(&path.with_extension("hexmap.bak")).is_ok());
        Ok(())
    }
}
