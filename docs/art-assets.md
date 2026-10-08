# CC0 art assets and import pipeline

The desktop renderer uses imported GLB models for every tree, shrub, grass cluster, rock and building. Continuous mountains and riverbeds remain derived from the GIS terrain meshes.

## Selected art

| Source | Included assets | License |
| --- | --- | --- |
| [Kenney Nature Kit 2.1](https://kenney.nl/assets/nature-kit) | Three broadleaf trees, two conifers, two bushes, three rocks and two grass clusters | CC0 1.0; original notice in `assets/art/nature-LICENSE.txt` |
| [Kenney City Kit Suburban 2.0](https://kenney.nl/assets/city-kit-suburban) | Building types a, h, k, r and u, plus their shared color atlas | CC0 1.0; original notice in `assets/art/city-LICENSE.txt` |
| [Poly Haven](https://polyhaven.com/license) | [Grass Ground](https://polyhaven.com/a/grass_ground), [Brown Mud Dry](https://polyhaven.com/a/brown_mud_dry), [Gravel Ground 01](https://polyhaven.com/a/gravel_ground_01), [Rock Face 03](https://polyhaven.com/a/rock_face_03); 1K diffuse, OpenGL normal and ARM maps | CC0 1.0 |

Kenney's nature and suburban models provide a consistent stylized palette and modest mesh complexity. Cities use one modern suburban theme. The Quaternius packs were checked as alternatives; the Ultimate Stylized Nature glTF folder and free Downtown Standard download page were accessible. Their textured foliage and larger modular kits remain available for expanding the art set. Medieval architecture is deferred to a separate future theme. No paid Pro or Source files are included.

## Reproduce and verify

Install Git LFS and hydrate the committed binary assets before building:

```sh
git lfs install
git lfs pull
python3 scripts/import_art.py --verify
```

Normal builds and packaged apps load the bundled assets offline. They do not download art at startup. Missing, altered or unhydrated files produce an explicit error; the renderer supplies no primitive replacements.

To recreate the curated set from its source downloads:

```sh
python3 -m venv target/art-venv
target/art-venv/bin/pip install Pillow==12.1.0
target/art-venv/bin/python scripts/import_art.py
```

On Windows, use `target/art-venv/Scripts/python.exe` and `pip.exe`. Downloaded archives and original texture maps are cached under `target/art-source/`. `sources.json` pins archive SHA-256 values and the provider's texture download checksums. `manifest.json` records each runtime file's SHA-256, original archive entry or texture sources, model category and triangle count. Model files and texture images use Git LFS; manifests, scripts and notices use normal Git.

## Rendering and placement

- Bevy's glTF loader reads the original GLB models and external atlas dependencies. Source node transforms are applied once, and meshes are centered and grounded at their base. Trees normalize by height; buildings and other props normalize by horizontal footprint.
- Prefabs retain the source mesh parts, UVs and texture transforms. Foliage and bark receive a subdued green/brown grade; the kit's grass-capped rocks receive muted stone colors. Nonmetallic nature and building materials use matte surfaces. Instances reuse mesh and material handles so Bevy can batch repeated geometry. No per-instance glTF scene hierarchy is required.
- Trees appear only in forest hexes: plains, mountains, urban cells, rivers and water cells are excluded. Local GIS forest coverage controls density and rejects nonforest samples within an eligible cell. Other land cover selects bushes, grass or rocks; source elevation and latitude provide a simple highland conifer preference. This preference is an art heuristic, not a species dataset. Wet footprints reject model placement. All anchors use the existing terrain triangles and update when display heights change.
- City cells retain their GIS settlements and urban edit state. Population and the existing density setting control building count and symbolic scale within the hex. Buildings use varied source models and right-angle orientations, with dry-ground checks and a regional density allowance.
- Vegetation allowances scale across the complete region with deterministic fractional placement, preserving distribution as map size grows. City clusters reduce their per-cell count for regions with many urban cells. Existing distance hiding and cached hex outlines remain active.
- Source land cover and uncompressed DEM slope produce continuous grass, soil, gravel and rock weights. Two four-layer texture arrays provide color and packed normal X/Y, roughness and occlusion. Eleven mip levels limit distant aliasing; triplanar projection and continuous world-space variation soften repetition and slope transitions. Snow and urban coverage retain their source-based palette.
- The material adds surface detail without displacement or extra terrain subdivision. Water retains its conditioned mesh and dedicated logical cells. Model placement and terrain materials are derived views; original GIS data, project schema, editing, picking and camera controls are preserved.

Native packages include the complete art directory and notices. CI hydrates LFS objects before building. Release performance at the full 100,000-hex limit and Windows runtime validation remain separate acceptance gates.
