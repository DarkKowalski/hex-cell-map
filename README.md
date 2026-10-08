# Hex Cell Map

Native Rust + Bevy GIS hex-map generator for Windows x64 and macOS Apple Silicon. Development follows [the roadmap](docs/roadmap.md); implementation is scoped through M2.

## Build prerequisites

Install Rust through rustup, CMake, and the platform C++ tools: Xcode command-line tools on macOS, or Visual Studio 2022 Build Tools (Desktop development with C++) and LLVM on Windows. The pinned toolchain is installed automatically. GDAL and PROJ are built from source; no GIS SDK installation or manually prepared datasets are required. The initial build takes several minutes.

## Validate real GIS generation

```sh
cargo run --no-default-features --bin gis-probe -- --region alps --repeat --output target/validation/alps.json
cargo run --no-default-features --bin gis-probe -- --region hudson --repeat --output target/validation/hudson.json
cargo run --no-default-features --bin gis-probe -- --region yangtze --repeat --output target/validation/yangtze.json
cargo test --no-default-features --lib
```

The probe automatically downloads Copernicus GLO-90, ESA WorldCover, HydroRIVERS, and GeoNames inputs. It checks city containment, reports river connectivity, and compares canonical data from cached regeneration. JSON output is a development validation artifact; portable user projects and save/load are scheduled for M3.

Use `--bounds WEST SOUTH EAST NORTH` and `--spacing KM` for other supported regions. Limits are 1–20 km spacing, 1,000 km extent, 100,000 hexes, and latitude ±60°, without antimeridian crossings. Boundary hexes intersecting the geographic selection are included. Elevation and cover statistics use 13 deterministic interior samples per hex; coarse aggregation describes operational terrain rather than exact local terrain.

Caches live in the platform cache directory under `hex-cell-map`. Set `HEX_MAP_CACHE_DIR` or pass `--cache DIR` to change acquisition storage. Generated maps contain dataset provenance and credits. Initial downloads need internet access; matching cached regional inputs can be reused offline.

See [data and dependency notices](NOTICE.md) before distributing generated data or packages.
