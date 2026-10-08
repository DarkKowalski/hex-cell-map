//! Repeatable real-data validation without opening a GPU window.
use anyhow::{Context, Result, bail, ensure};
use geo::Intersects;
use hex_cell_map::{generation, gis::cache::Cache, jobs::JobContext, map_core::*};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::PathBuf, sync::mpsc, time::Instant};

fn main() -> Result<()> {
    let mut settings = GenerationSettings::default();
    let mut cache_path = Cache::default_path();
    let mut output = None;
    let mut repeat = false;
    let mut audit = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--region" => {
                settings.region = match args.next().context("Missing region name")?.as_str() {
                    "alps" => Region {
                        west: 7.6,
                        south: 46.3,
                        east: 8.6,
                        north: 47.1,
                    },
                    "hudson" => Region {
                        west: -74.5,
                        south: 40.4,
                        east: -73.4,
                        north: 41.4,
                    },
                    "yangtze" => Region {
                        west: 118.2,
                        south: 30.8,
                        east: 120.0,
                        north: 32.4,
                    },
                    name => bail!("Unknown region {name}; use alps, hudson, or yangtze"),
                }
            }
            "--bounds" => {
                let mut next = || {
                    args.next()
                        .context("Expected west south east north")?
                        .parse::<f64>()
                        .context("Invalid bound")
                };
                settings.region = Region {
                    west: next()?,
                    south: next()?,
                    east: next()?,
                    north: next()?,
                };
            }
            "--spacing" => settings.spacing_km = args.next().context("Missing spacing")?.parse()?,
            "--cache" => cache_path = PathBuf::from(args.next().context("Missing cache path")?),
            "--output" => output = Some(PathBuf::from(args.next().context("Missing output path")?)),
            "--repeat" => repeat = true,
            "--audit-raster" => audit = true,
            "--check-environment" => {
                check_environment()?;
                return Ok(());
            }
            "--help" => {
                println!(
                    "gis-probe [--region alps|hudson|yangtze] [--bounds W S E N] [--spacing KM] [--cache DIR] [--output FILE] [--repeat]\n  --check-environment checks bundled drivers and PROJ without network access."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument {arg}"),
        }
    }
    let cache = Cache::new(cache_path)?;
    let (tx, rx) = mpsc::channel();
    let context = JobContext::with_progress(tx);
    let logger = std::thread::spawn(move || {
        for p in rx {
            eprintln!("{:3.0}% {}", p.fraction * 100., p.message);
        }
    });
    let start = Instant::now();
    let document = generation::generate(&settings, &cache, &context)?;
    validate(&document, &cache, &context)?;
    if audit {
        println!(
            "{} raster windows match independent GDAL RasterIO within 0.001",
            hex_cell_map::gis::raster::audit_references(&cache, &document.sources, &context)?
        );
    }
    let digest = cell_digest(&document)?;
    println!(
        "{} cells, {} chunks, {:?}; {:.2}s; canonical SHA-256 {digest}",
        document.cells.len(),
        document.chunk_ids().len(),
        generation::summary(&document),
        start.elapsed().as_secs_f64()
    );
    if repeat {
        let start = Instant::now();
        let second = generation::generate(&settings, &cache, &context)?;
        ensure!(
            digest == cell_digest(&second)?,
            "Cached regeneration changed canonical GIS data"
        );
        println!(
            "Cached repeat identical in {:.2}s",
            start.elapsed().as_secs_f64()
        );
    }
    if let Some(path) = output {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        serde_json::to_writer(std::fs::File::create(&path)?, &document)?;
        println!("Validation artifact: {}", path.display());
    }
    drop(context);
    logger.join().unwrap();
    Ok(())
}

fn cell_digest(document: &MapDocument) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            &document.cells,
            &document.height_field,
            &document.river_paths,
            &document.heights
        ))?)
    ))
}

fn check_environment() -> Result<()> {
    hex_cell_map::gis::initialize()?;
    for name in ["GTiff", "ESRI Shapefile", "GeoJSON", "MEM"] {
        gdal::DriverManager::get_driver_by_name(name)
            .with_context(|| format!("Missing bundled {name} driver"))?;
    }
    let projection = Projection::new(GenerationSettings::default().region)?;
    let mut point = [[8.1, 46.7]];
    projection.project(&mut point)?;
    projection.unproject(&mut point)?;
    ensure!(
        (point[0][0] - 8.1).abs() < 1e-8 && (point[0][1] - 46.7).abs() < 1e-8,
        "Projection round trip failed"
    );
    println!(
        "GDAL {}, required drivers, and PROJ round trip OK",
        gdal::version_info("RELEASE_NAME")
    );
    Ok(())
}

fn validate(document: &MapDocument, cache: &Cache, context: &JobContext) -> Result<()> {
    let spacing = document.settings.spacing_km * 1000.;
    let projection = Projection::new(document.settings.region)?;
    for cell in &document.cells {
        ensure!(cell.elevation_m.is_finite(), "Non-finite elevation");
        let mut positions: Vec<_> = cell.cities.iter().map(|c| [c.lon, c.lat]).collect();
        projection.project(&mut positions)?;
        for position in positions {
            ensure!(
                point_hex(position, spacing) == cell.hex
                    && hex_polygon(cell.hex, spacing)
                        .intersects(&geo::Point::new(position[0], position[1])),
                "City outside assigned hex"
            );
        }
    }
    let mut checked = 0;
    let mut clipped = 0;
    let region = document.settings.region;
    let padding = spacing
        / 100_000.
        / region
            .north
            .abs()
            .max(region.south.abs())
            .to_radians()
            .cos();
    let (geometries, _) = hex_cell_map::gis::vectors::rivers(
        cache,
        region.expanded(padding),
        &projection,
        document.settings.min_river_discharge,
        context,
    )?;
    for reach in &document.river_network {
        let hexes: HashSet<_> = document
            .cells
            .iter()
            .filter(|c| c.river_ids.contains(&reach.id))
            .map(|c| c.hex)
            .collect();
        if hexes.is_empty() {
            continue;
        }
        // A reach can leave and re-enter the rectangle; report these clipped sets.
        let mut seen = HashSet::new();
        let mut pending = vec![*hexes.iter().next().unwrap()];
        while let Some(h) = pending.pop() {
            if !seen.insert(h) {
                continue;
            }
            pending.extend(
                h.all_neighbors()
                    .into_iter()
                    .filter(|n| hexes.contains(n) && !seen.contains(n)),
            );
        }
        if seen.len() == hexes.len() {
            checked += 1;
        } else {
            let outside = geometries.iter().filter(|r| r.id == reach.id).any(|r| {
                r.points_m
                    .iter()
                    .any(|p| !document.index.contains_key(&point_hex(*p, spacing)))
            });
            ensure!(
                outside,
                "Fully covered river reach {} has a gap in its cell chain",
                reach.id
            );
            clipped += 1;
        }
    }
    println!(
        "Validated city containment; {checked} connected river reaches, {clipped} clipped multi-component reaches"
    );
    Ok(())
}
