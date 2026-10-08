# Data and dependency notices

Generated documents retain input URLs, versions, timestamps, hashes, credits, and links to applicable licenses. Raw archives remain in the local cache, outside documents. A generated document contains modified HydroRIVERS data; its distribution must preserve HydroSHEDS end-user terms and attribution. The MVP is not a stand-alone redistribution of raw GIS datasets.

- **Copernicus DEM GLO-90**: Produced using Copernicus WorldDEM-90 © DLR e.V. 2010–2014 and © Airbus Defence and Space GmbH 2014–2018, provided under COPERNICUS by the European Union and ESA; all rights reserved. The organisations in charge of the Copernicus programme by law or by delegation do not incur any liability for any use of the Copernicus WorldDEM-90. [Specific license, GLO-90 section](https://dataspace.copernicus.eu/sites/default/files/media/files/2025-06/copernicus_contributing_mission_data_access_v2_cop_dem_licenses.pdf).
- **ESA WorldCover 2021 v200**: © ESA WorldCover project 2021 / Contains modified Copernicus Sentinel data (2021) processed by ESA WorldCover consortium. [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
- **HydroRIVERS v1.0**: Lehner, B., Grill, G. (2013). Global river hydrography and network routing. Hydrological Processes 27(15), 2171–2186. [HydroSHEDS license appendix](https://data.hydrosheds.org/file/technical-documentation/HydroSHEDS_TechDoc_v1_4.pdf). Raw data must not be redistributed as a stand-alone product. Derived-data redistribution remains subject to this license.
- **GeoNames cities1000**: Contains modified [GeoNames](https://www.geonames.org) data, [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The downloaded daily snapshot is cached; regenerate from the same snapshot for reproducible results.
- **Natural Earth 1:110m land**: [Public domain](https://www.naturalearthdata.com/about/terms-of-use/). Bundled overview data from the [Natural Earth vector repository](https://github.com/nvkelso/natural-earth-vector).
- **Mozilla certificate roots**: `assets/cacert.pem`, obtained from [curl's Mozilla CA extract](https://curl.se/docs/caextract.html). Mozilla certificate-store data is distributed under [MPL 2.0](https://www.mozilla.org/MPL/2.0/).

Native dependencies include GDAL (MIT-style license), PROJ (MIT-style license), SQLite (public domain), libcurl (curl license), OpenSSL (Apache 2.0 on Unix builds), and zlib (zlib license). Rust dependencies retain their own licenses. Release packaging includes notices from the resolved source dependencies.

## Bundled art

- **Kenney Nature Kit 2.1**: Imported trees, bushes, rocks and grass from the [Nature Kit](https://kenney.nl/assets/nature-kit). [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/); the archive's original notice is retained in `assets/art/nature-LICENSE.txt`.
- **Kenney City Kit Suburban 2.0**: Five building variants and the shared color atlas from [City Kit (Suburban)](https://kenney.nl/assets/city-kit-suburban). CC0 1.0; the archive's original notice is retained in `assets/art/city-LICENSE.txt`.
- **Poly Haven**: Grass Ground, Brown Mud Dry, Gravel Ground 01 and Rock Face 03 diffuse, OpenGL normal and ARM maps, packed into terrain arrays. [CC0 license and redistribution terms](https://polyhaven.com/license). Asset pages, authors, download URLs and hashes are recorded in `assets/art/sources.json` and `assets/art/manifest.json`.

These art licenses permit commercial use and redistribution. The source meshes retain their original geometry. Runtime normalization and palette grading adapt them to the map; texture channel packing preserves the supplied material detail. See [the art pipeline](docs/art-assets.md) for the selected files and processing steps. Art files are stored using Git LFS and included in native packages.
