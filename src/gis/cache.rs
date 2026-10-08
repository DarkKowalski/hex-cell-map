use crate::{jobs::JobContext, map_core::SourceRecord};
use anyhow::{Context, Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Cache {
    pub root: PathBuf,
    agent: ureq::Agent,
}

impl Cache {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(4)))
            .timeout_recv_response(Some(Duration::from_secs(5)))
            .build();
        Ok(Self {
            root,
            agent: config.into(),
        })
    }

    pub fn default_path() -> PathBuf {
        std::env::var_os("HEX_MAP_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::cache_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("hex-cell-map")
            })
    }

    pub fn key<T: Serialize>(value: &T) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
    }

    pub fn read_json<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.root.join(name);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_reader(File::open(&path)?).with_context(|| {
                format!(
                    "Read cache {}; remove this file to reacquire its inputs",
                    path.display()
                )
            })?,
        ))
    }

    pub fn write_json<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(file.as_file_mut(), value)?;
        file.as_file_mut().sync_all()?;
        file.persist(self.root.join(name))
            .context("Publish cached input")?;
        Ok(())
    }

    pub fn download(
        &self,
        url: &str,
        filename: &str,
        context: &JobContext,
        progress: f32,
    ) -> Result<(PathBuf, Option<String>)> {
        context.check()?;
        let path = self.root.join(filename);
        if path.is_file() && fs::metadata(&path)?.len() > 0 {
            return Ok((path, None));
        }
        let mut last_error = None;
        for attempt in 0..3 {
            context.report(
                progress,
                format!("Downloading {filename} (attempt {})", attempt + 1),
            )?;
            match self.download_once(url, &path, context, progress) {
                Ok(etag) => return Ok((path, etag)),
                Err(error) => {
                    context.check()?;
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap()).with_context(|| format!("Could not acquire {url}"))
    }

    fn download_once(
        &self,
        url: &str,
        destination: &Path,
        context: &JobContext,
        progress: f32,
    ) -> Result<Option<String>> {
        let response = self.agent.head(url).call()?;
        let etag = response
            .headers()
            .get("etag")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let expected: Option<u64> = response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|v| v.parse().ok());
        ensure!(
            expected.unwrap_or(0) <= 1_000_000_000,
            "Individual GIS download exceeds the 1 GB limit"
        );
        // Ureq's body deadline covers the complete download. Small range requests bound
        // stalled reads and let cancellation run between chunks of large archives.
        drop(response);
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        let mut written = 0_u64;
        let block_size = 2 * 1024 * 1024_u64;
        if let Some(size) = expected {
            while written < size {
                context.check()?;
                let end = (written + block_size - 1).min(size - 1);
                let response = self
                    .agent
                    .get(url)
                    .header("Range", &format!("bytes={written}-{end}"))
                    .config()
                    .timeout_recv_body(Some(Duration::from_secs(8)))
                    .build()
                    .call()?;
                if response.status().as_u16() == 200 {
                    ensure!(written == 0, "Server ignored a resumed range request");
                    let mut reader = response.into_body().into_reader();
                    self.copy_checked(&mut reader, &mut temp, context, &mut written)?;
                    break;
                }
                ensure!(
                    response.status().as_u16() == 206,
                    "Unexpected range response"
                );
                let mut reader = response.into_body().into_reader();
                let before = written;
                self.copy_checked(&mut reader, &mut temp, context, &mut written)?;
                ensure!(
                    written - before == end - before + 1,
                    "Truncated range response"
                );
                context.report(
                    progress,
                    format!(
                        "Downloading {}: {:.1} / {:.1} MB",
                        destination
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        written as f64 / 1e6,
                        size as f64 / 1e6
                    ),
                )?;
            }
            ensure!(written == size, "Truncated GIS download");
        } else {
            let response = self
                .agent
                .get(url)
                .config()
                .timeout_recv_body(Some(Duration::from_secs(8)))
                .build()
                .call()?;
            self.copy_checked(
                &mut response.into_body().into_reader(),
                &mut temp,
                context,
                &mut written,
            )?;
        }
        context.check()?;
        ensure!(written > 0, "Empty GIS download");
        temp.as_file_mut().sync_all()?;
        temp.persist(destination)?;
        Ok(etag)
    }

    fn copy_checked(
        &self,
        reader: &mut impl Read,
        writer: &mut impl Write,
        context: &JobContext,
        written: &mut u64,
    ) -> Result<()> {
        let mut buffer = [0_u8; 65536];
        loop {
            context.check()?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            *written += count as u64;
            ensure!(
                *written <= 1_000_000_000,
                "GIS download exceeds the 1 GB limit"
            );
            writer.write_all(&buffer[..count])?;
        }
        Ok(())
    }
}

pub fn source_record(
    name: &str,
    version: &str,
    url: &str,
    license: &str,
    attribution: &str,
    etag: Option<String>,
    path: &Path,
) -> Result<SourceRecord> {
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buffer = [0_u8; 65536];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    Ok(SourceRecord {
        name: name.into(),
        version: version.into(),
        url: url.into(),
        license: license.into(),
        attribution: attribution.into(),
        acquired_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        etag,
        sha256: format!("{:x}", hasher.finalize()),
    })
}
