#!/usr/bin/env python3
"""Import the curated CC0 art set. Requires Pillow 12.1.0; no GIS inputs change.

Downloads are pinned in assets/art/sources.json. --verify checks the committed
runtime files without network access or Pillow. Binary outputs use Git LFS.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import struct
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
ART = ROOT / "assets/art"
CACHE = ROOT / "target/art-source"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def download(url, destination, expected, algorithm="sha256"):
    if not destination.exists():
        request = urllib.request.Request(url, headers={"User-Agent": "HexCellMap-ArtImport/0.1"})
        with urllib.request.urlopen(request, timeout=60) as response:
            data = response.read()
        if hashlib.new(algorithm, data).hexdigest() != expected:
            raise RuntimeError(f"Download checksum mismatch: {url}")
        destination.write_bytes(data)
    data = destination.read_bytes()
    if hashlib.new(algorithm, data).hexdigest() != expected:
        raise RuntimeError(f"Cached download checksum mismatch: {destination}")
    return data


def verify():
    manifest = json.loads((ART / "manifest.json").read_text())
    for file in manifest["files"]:
        path = ROOT / "assets" / file["path"]
        if not path.exists() or digest(path.read_bytes()) != file["sha256"]:
            raise RuntimeError(f"Missing or changed art: {path}. Run git lfs pull, then retry.")
    print(f"Verified {len(manifest['files'])} runtime art files, including {len(manifest['models'])} GLB models")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    if args.verify:
        verify()
        return
    from PIL import Image

    CACHE.mkdir(parents=True, exist_ok=True)
    sources = json.loads((ART / "sources.json").read_text())
    manifest = {"version": 1, "license": "CC0-1.0", "models": [], "files": [], "texture_layers": []}

    def write(relative, data, source):
        path = ART / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        manifest["files"].append({"path": "art/" + relative, "sha256": digest(data), "source": source})

    for pack in sources["packs"]:
        data = download(pack["url"], CACHE / (pack["id"] + ".zip"), pack["sha256"])
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            (ART / (pack["id"] + "-LICENSE.txt")).write_bytes(archive.read("License.txt"))
            for kind, names in pack["models"].items():
                for name in names:
                    original = pack["folder"] + name + ".glb"
                    content = archive.read(original)
                    length, = struct.unpack_from("<I", content, 12)
                    gltf = json.loads(content[20:20 + length])
                    assert gltf["asset"]["version"] == "2.0"
                    # Preserve source geometry. Bevy applies source node transforms
                    # and normalizes each model once when preparing shared meshes.
                    relative = f"kenney_{pack['id']}/{name}.glb"
                    write(relative, content, {"pack": pack["id"], "entry": original})
                    triangles = sum(gltf["accessors"][p["indices"]]["count"] // 3
                                    for m in gltf["meshes"] for p in m["primitives"])
                    manifest["models"].append({"path": "art/" + relative, "kind": kind, "triangles": triangles})
                    # GLB files may reference external textures. Bundle exactly
                    # the relative paths expected by the original glTF loader.
                    for image in gltf.get("images", []):
                        if "uri" in image:
                            uri = image["uri"]
                            if ":" in uri or ".." in uri.split("/"):
                                raise RuntimeError(f"Unexpected image URI: {uri}")
                            target = f"kenney_{pack['id']}/{uri}"
                            if not any(f["path"] == "art/" + target for f in manifest["files"]):
                                write(target, archive.read(pack["folder"] + uri), {"pack": pack["id"], "entry": pack["folder"] + uri})

    # Four layers share two array textures. Detail channels are OpenGL normal X/Y,
    # perceptual roughness and occlusion. No displacement alters the DEM meshes.
    color = Image.new("RGBA", (1024, 4096))
    detail = Image.new("RGBA", (1024, 4096))
    for layer, source in enumerate(sources["textures"]):
        maps = {}
        downloaded = []
        for name, file in source["files"].items():
            data = download(file["url"], CACHE / (source["id"] + "-" + name + ".jpg"), file["md5"], "md5")
            maps[name] = Image.open(io.BytesIO(data)).convert("RGB")
            assert maps[name].size == (1024, 1024)
            downloaded.append({"url": file["url"], "sha256": digest(data)})
        color.paste(maps["Diffuse"].convert("RGBA"), (0, layer * 1024))
        nx, ny, _ = maps["nor_gl"].split()
        ao, rough, _ = maps["arm"].split()
        detail.paste(Image.merge("RGBA", (nx, ny, rough, ao)), (0, layer * 1024))
        manifest["texture_layers"].append({"id": source["id"], "layer": layer, "downloads": downloaded})
    for name, image in [("ground-color.png", color), ("ground-detail.png", detail)]:
        buffer = io.BytesIO()
        image.save(buffer, format="PNG", optimize=True)
        write("polyhaven/" + name, buffer.getvalue(), {"layers": [s["id"] for s in sources["textures"]], "processing": "Pillow 12.1.0, RGBA vertical array packing"})
    (ART / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify()


if __name__ == "__main__":
    main()
