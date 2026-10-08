# Native terrain validation

Validated on macOS Apple Silicon with an Apple M2 GPU using Bevy's Metal backend. These are development checks; Windows runtime verification, clean-machine installation and the 100,000-hex release performance gate remain pending.

## Build and regression checks

- `cargo build --locked --bins` passes.
- `cargo test --locked --all-targets` passes all 24 tests.
- `cargo clippy --locked --all-targets -- -D warnings` passes.

Regression coverage includes complete shared hex boundary subdivisions, matching chunk vertices/normals after elevation edits, single water coverage at bends and lake crossings, downstream propagation through complete water bodies, preserved GIS elevations, adaptive relief, live display-height alignment, urban edits, undo/redo, offline snapshots, cancellation, stale-job rejection and UI input capture. An outline test toggles cached assets 120 times without rebuilding lines or increasing the asset count, and checks distant-zoom hiding. Art checks verify hydrated model/texture hashes, independent texture-array mip chains, dry-ground model anchors, urban building counts and tree placement restricted to forest land cells even when other cells retain high GIS forest fractions.

`python3 scripts/import_art.py --verify` validates all 20 runtime files, including 17 original GLB models. `git lfs ls-files` lists every binary model and image, the Git index contains LFS pointers, and `git lfs fsck` passes. The committed source inventory includes CC0 notices, original download URLs and checksums.

## Automatically acquired real GIS maps

All regions use 2 km between neighboring hex centers, Copernicus GLO-90 elevation, ESA WorldCover 2021, HydroRIVERS and GeoNames. Generation uses the application's acquisition/cache pipeline. No manual GIS preparation is required.

| Region | Cells | Chunks | Land triangles | Water triangles | Matching cross-chunk samples | Minimum water vertex up-normal |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Alps, 7.6–8.6° E / 46.3–47.1° N | 2,089 | 5 | 327,083 | 75,924 | 1,282 | 0.97282 |
| Hudson, 74.5–73.4° W / 40.4–41.4° N | 3,087 | 9 | 433,404 | 139,926 | 2,005 | 0.98859 |
| Yangtze, 118.2–120.0° E / 30.8–32.4° N | 8,917 | 16 | 1,369,763 | 352,305 | 5,309 | 0.98498 |

Each region passes settlement containment, river cell-chain connectivity, complete terrain edge connectivity, finite geometry and exact shared position/normal checks. Repeated cached generation produces identical canonical cells, source fields, river paths and display settings. Independent GDAL RasterIO comparisons also pass within 0.001 for five Alps windows, five Hudson windows and twelve Yangtze windows.

## Native render checks

Each region completes a native screenshot capture and 32 terrain picks on chunk-boundary slopes. Every native run loads 17 imported GLB prefabs and the two four-layer Poly Haven arrays; the smoke check confirms that all environmental mesh handles belong to imported prefabs. Full-region Hudson and Yangtze checks include cached outlines. Focused Alps and Hudson captures inspect foliage, rocks, ground detail and building clusters at 16 km and 12 km camera distances respectively. The current placement produces 13,038 model parts in the Alps, 61,490 in Hudson and 58,946 in Yangtze; parts sharing mesh/material handles can batch together.

Captures were visually inspected for model appearance, water overlaps, stretched shore triangles and open hex seams. The native logs contain no shader compilation errors or panics. Frame-rate targets across all supported map sizes still require release profiling.

To reproduce after building:

```sh
./target/debug/gis-probe --region alps --repeat --audit-raster --audit-terrain --output target/validation/alps.json
./target/debug/gis-probe --region hudson --repeat --audit-raster --audit-terrain --output target/validation/hudson.json
./target/debug/gis-probe --region yangtze --repeat --audit-raster --audit-terrain --output target/validation/yangtze.json
./target/debug/hex-cell-map --preview target/validation/alps.json --smoke --screenshot target/validation/alps.png
./target/debug/hex-cell-map --preview target/validation/hudson.json --smoke --outlines --screenshot target/validation/hudson.png
./target/debug/hex-cell-map --preview target/validation/yangtze.json --smoke --outlines --screenshot target/validation/yangtze.png
./target/debug/hex-cell-map --preview target/validation/alps.json --smoke --focus -7 8 --distance 16 --screenshot target/validation/alps-art-close.png
./target/debug/hex-cell-map --preview target/validation/hudson.json --smoke --focus 4 -12 --distance 12 --screenshot target/validation/hudson-art-close.png
```

Local logs and screenshots are generated under `target/validation/` and are not committed. A successful smoke run must include both `SMOKE: native frame captured` in its log and the resulting PNG; closing the window early does not satisfy the check.

## Development package

`python3 scripts/package.py` verifies art hashes and produces an ad-hoc signed macOS bundle with both binaries, all 20 runtime art files, PROJ data, documentation and dependency notices. The package check passes with projection-data environment overrides removed: bundled GDAL 3.12.1 supplies the required GTiff, Shapefile, GeoJSON and MEM drivers, and the PROJ round trip passes. The packaged desktop executable also completes the focused Hudson art capture and 32 terrain picks when launched from `dist/`, loading bundled models and textures from its Resources directory. The binary dependency scan finds only system dynamic libraries. Release signing/notarization and installation on a separate clean machine remain pending.
