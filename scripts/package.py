#!/usr/bin/env python3
"""Create a relocatable development package with the native projection database.

Run after `cargo build --bins` or `cargo build --release --bins`.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def command(*args):
    return subprocess.check_output(args, text=True, cwd=ROOT)


def dependency_notices(destination):
    metadata = json.loads(command("cargo", "metadata", "--locked", "--format-version", "1"))
    notices = destination / "dependency-notices"
    notices.mkdir()
    inventory = []
    for package in metadata["packages"]:
        inventory.append(f'{package["name"]} {package["version"]}: {package.get("license") or "see bundled license files"}')
        source = Path(package["manifest_path"]).parent
        files = sorted({*source.glob("LICENSE*"), *source.glob("COPYING*"), *source.glob("NOTICE*"), *source.glob("source/LICENSE*"), *source.glob("source/COPYING*")})
        if files:
            target = notices / f'{package["name"]}-{package["version"]}'
            target.mkdir()
            for file in files:
                if file.is_file():
                    shutil.copy2(file, target / file.name)
    (notices / "inventory.txt").write_text("\n".join(inventory) + "\n")


def mac_dependencies(binary, frameworks):
    """Reject non-system dynamic libraries; native dependencies are static."""
    output = command("otool", "-L", str(binary))
    for line in output.splitlines()[1:]:
        library = line.strip().split(" (", 1)[0]
        if not library.startswith(("/usr/lib/", "/System/Library/")):
            raise RuntimeError(f"Unbundled dynamic dependency in {binary.name}: {library}")


def windows_runtime(destination):
    roots = []
    if os.environ.get("VCToolsRedistDir"):
        roots.append(Path(os.environ["VCToolsRedistDir"]))
    for base in [Path(os.environ.get("ProgramFiles", "C:/Program Files")), Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)"))]:
        roots.extend(base.glob("Microsoft Visual Studio/*/*/VC/Redist/MSVC/*"))
    candidates = sorted((p for root in roots for p in root.glob("x64/Microsoft.VC*.CRT")), reverse=True)
    if not candidates:
        raise RuntimeError("MSVC redistributable DLLs not found; run from a Visual Studio developer shell")
    for dll in candidates[0].glob("*.dll"):
        shutil.copy2(dll, destination / dll.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=["debug", "release"], default="debug")
    args = parser.parse_args()
    target = ROOT / "target" / args.profile
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    apple = sys.platform == "darwin"
    windows = sys.platform == "win32"
    if not apple and not windows:
        raise RuntimeError("MVP release packaging supports macOS and Windows")
    command(sys.executable, "scripts/import_art.py", "--verify")
    package = dist / ("Hex Cell Map.app" if apple else "hex-cell-map-windows-x64")
    if package.exists():
        shutil.rmtree(package)
    executable_dir = package / "Contents/MacOS" if apple else package
    data_dir = package / "Contents/Resources" if apple else package
    executable_dir.mkdir(parents=True)
    data_dir.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if windows else ""
    for name in ["hex-cell-map", "gis-probe"]:
        shutil.copy2(target / (name + suffix), executable_dir / (name + suffix))
    databases = sorted(target.glob("build/proj-sys-*/out/share/proj/proj.db"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not databases:
        raise RuntimeError("Bundled PROJ database not found in this Cargo profile")
    proj = data_dir / "gis-data/proj"
    proj.mkdir(parents=True)
    for file in databases[0].parent.iterdir():
        if file.is_file():
            shutil.copy2(file, proj / file.name)
    for name in ["NOTICE.md", "README.md"]:
        shutil.copy2(ROOT / name, data_dir / name)
    shutil.copytree(ROOT / "docs", data_dir / "docs")
    shutil.copytree(ROOT / "assets", data_dir / "assets")
    dependency_notices(data_dir)
    if apple:
        with (package / "Contents/Info.plist").open("wb") as stream:
            plistlib.dump({"CFBundleName": "Hex Cell Map", "CFBundleDisplayName": "Hex Cell Map", "CFBundleIdentifier": "org.hexcellmap.editor", "CFBundleExecutable": "hex-cell-map", "CFBundlePackageType": "APPL", "CFBundleShortVersionString": "0.1.0", "LSMinimumSystemVersion": "13.0", "NSHighResolutionCapable": True}, stream)
        for name in ["hex-cell-map", "gis-probe"]:
            mac_dependencies(executable_dir / name, None)
        command("codesign", "--force", "--deep", "--sign", "-", str(package))
    else:
        windows_runtime(executable_dir)
    # Exercise runtime discovery of projection data relative to the executable.
    env = os.environ.copy()
    env.pop("HEX_MAP_PROJ_DATA", None)
    env.pop("PROJ_DATA", None)
    env.pop("PROJ_LIB", None)
    subprocess.run([str(executable_dir / ("gis-probe" + suffix)), "--check-environment"], cwd=dist, env=env, check=True)
    print(f"Package and offline GIS environment check passed: {package}")


if __name__ == "__main__":
    main()
