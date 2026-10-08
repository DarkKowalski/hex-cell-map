# Hex Cell Map

Native Rust + Bevy GIS hex-map generator for Windows x64 and macOS Apple Silicon. Development follows [the roadmap](docs/roadmap.md). The desktop view uses continuous DEM terrain, separate river channels and source-mask water bodies, blended GIS colors, urban hexes and configurable nonlinear height compression.

## Build and run

Install Rust through rustup, CMake and platform C++ tools: Xcode command-line tools on macOS, or Visual Studio 2022 Build Tools (Desktop development with C++) and LLVM on Windows. GDAL and PROJ build from source; no GIS SDK or manually prepared datasets are required. The first build can take substantially longer than subsequent builds.

```sh
cargo build --locked --bins
cargo run --locked --bin hex-cell-map
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

Select a region in the sidebar and generate its map. Middle drag or Shift + left drag pans, right drag or Q/E rotates, the wheel zooms, and WASD pans. Click to inspect a hex. Display height controls preserve original source elevations. Urban cell controls apply or remove complete cells, adjust population/style and brush radius, and provide undo/redo. River cells retain their river identity; buildings use dry ground. Source settlement records remain available after an urban edit.

The Project panel opens and saves self-contained JSON snapshots to the entered path, including height settings and urban edits. A previous valid snapshot is retained as `.hexmap.bak`. General terrain/elevation brushes, broader project workflow and release performance gates are in later milestones.

## Real GIS validation

```sh
cargo run --locked --bin gis-probe -- --region alps --repeat --audit-raster --output target/validation/alps.json
cargo run --locked --bin gis-probe -- --region hudson --repeat --audit-raster --output target/validation/hudson.json
cargo run --locked --bin gis-probe -- --region yangtze --repeat --audit-raster --output target/validation/yangtze.json
cargo run --locked --bin hex-cell-map -- --preview target/validation/alps.json --smoke --screenshot target/validation/alps.png
```

The probe automatically retrieves Copernicus GLO-90, ESA WorldCover 2021, HydroRIVERS and GeoNames. It validates settlement containment, river connectivity, independently decoded raster windows and deterministic source fields/paths/cells. Its JSON can also be opened in the desktop Project panel. Schema 2 retains source land cover and river geometry; older development artifacts must be regenerated from their cached inputs.

Use `--bounds WEST SOUTH EAST NORTH` and `--spacing KM` for other supported regions. Limits are 1–20 km between neighboring hex centers, 1,000 km extent, 100,000 hexes and latitude ±60°, without antimeridian crossings. All boundary hexes intersecting the selection are included. Cell statistics use 13 deterministic samples; a separate projected source field controls terrain and materials. Visible river widths, urban footprints and buildings are symbolic at operational scale.

Caches use the platform cache directory under `hex-cell-map`. Set `HEX_MAP_CACHE_DIR` or use probe `--cache DIR` to change acquisition storage. Initial generation needs internet access; complete snapshots reopen offline without caches. Failures preserve the currently open map.

## Development packaging

```sh
python3 scripts/package.py
```

The script packages built binaries, projection data, notices and platform runtime requirements into `dist/`. The macOS package is ad-hoc signed for development; release signing/notarization and Windows runtime verification remain release gates. See [data and dependency notices](NOTICE.md) before distributing packages or derived map data.
