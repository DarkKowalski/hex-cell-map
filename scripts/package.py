#!/usr/bin/env python3
"""Create a relocatable native package with the projection database and assets.

Run after `cargo build --bins` or `cargo build --release --bins`.
Use --archive to create and verify a versioned ZIP and its SHA-256 checksum.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def command(*args):
    return subprocess.check_output(args, text=True, encoding="utf-8", cwd=ROOT)


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
    (notices / "inventory.txt").write_text("\n".join(inventory) + "\n", encoding="utf-8")


def offline_environment():
    env = os.environ.copy()
    for name in ["HEX_MAP_PROJ_DATA", "PROJ_DATA", "PROJ_LIB"]:
        env.pop(name, None)
    return env


def verify_archive(archive, package_name, apple):
    """Test extracted executables and assets away from the build directory."""
    with tempfile.TemporaryDirectory(prefix="hex-cell-map-package-") as temporary:
        destination = Path(temporary)
        if apple:
            command("ditto", "-x", "-k", str(archive), str(destination))
        else:
            with zipfile.ZipFile(archive) as stream:
                stream.extractall(destination)
        package = destination / package_name
        executables = package / "Contents/MacOS" if apple else package
        data = package / "Contents/Resources" if apple else package
        for name in ["gis-data/proj/proj.db", "NOTICE.md", "README.md",
                     "dependency-notices/inventory.txt", "docs/data-pipeline.md",
                     "docs/data-pipeline.zh-CN.md", "assets/fonts/OFL.txt"]:
            if not (data / name).is_file():
                raise RuntimeError(f"Archive is missing {name}")
        manifest = json.loads((data / "assets/art/manifest.json").read_text(encoding="utf-8"))
        for entry in manifest["files"]:
            asset = data / "assets" / entry["path"]
            if hashlib.sha256(asset.read_bytes()).hexdigest() != entry["sha256"]:
                raise RuntimeError(f"Archived asset checksum mismatch: {asset}")
        suffix = "" if apple else ".exe"
        if apple:
            command("codesign", "--verify", "--deep", "--strict", str(package))
        else:
            for name in ["vcruntime140.dll", "msvcp140.dll"]:
                if not (executables / name).is_file():
                    raise RuntimeError(f"Archive is missing MSVC runtime {name}")
        subprocess.run([str(executables / ("gis-probe" + suffix)), "--check-environment"],
                       cwd=destination, env=offline_environment(), check=True)
        for language in ["en", "zh-CN"]:
            subprocess.run([str(executables / ("hex-cell-map" + suffix)), "--lang", language, "--help"],
                           cwd=destination, env=offline_environment(), check=True, capture_output=True)


def write_zip(package, archive):
    # Dependency notices can retain dates before ZIP's 1980 lower bound.
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED,
                         strict_timestamps=False) as stream:
        stream.write(package, package.name)
        for path in sorted(package.rglob("*")):
            stream.write(path, path.relative_to(package.parent))


def create_archive(package, version, apple):
    platform_name = "macos-arm64" if apple else "windows-x64"
    archive = package.parent / f"hex-cell-map-v{version}-{platform_name}.zip"
    if apple:
        command("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(package), str(archive))
    else:
        write_zip(package, archive)
    verify_archive(archive, package.name, apple)
    with archive.open("rb") as stream:
        checksum = hashlib.file_digest(stream, "sha256").hexdigest()
    archive.with_suffix(".zip.sha256").write_text(f"{checksum}  {archive.name}\n", encoding="utf-8")
    print(f"Verified release archive: {archive}")


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
    parser.add_argument("--archive", action="store_true", help="Create and test a ZIP with a SHA-256 checksum")
    parser.add_argument("--tag", help="Require a release tag matching the Cargo package version")
    args = parser.parse_args()
    metadata = json.loads(command("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
    version = next(p["version"] for p in metadata["packages"] if Path(p["manifest_path"]) == ROOT / "Cargo.toml")
    if args.tag and args.tag != f"v{version}":
        parser.error(f"Release tag must match Cargo.toml version: v{version}")
    target = ROOT / "target" / args.profile
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    apple = sys.platform == "darwin"
    windows = sys.platform == "win32"
    if not apple and not windows:
        raise RuntimeError("MVP release packaging supports macOS and Windows")
    if apple and platform.machine().lower() not in ["arm64", "aarch64"]:
        raise RuntimeError("macOS packages must be built on Apple Silicon")
    if windows and platform.machine().lower() not in ["amd64", "x86_64"]:
        raise RuntimeError("Windows packages must be built on x64")
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
            plistlib.dump({"CFBundleName": "Hex Cell Map", "CFBundleDisplayName": "Hex Cell Map", "CFBundleIdentifier": "org.hexcellmap.editor", "CFBundleExecutable": "hex-cell-map", "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version, "CFBundleVersion": version, "LSMinimumSystemVersion": "13.0", "NSHighResolutionCapable": True}, stream)
        for name in ["hex-cell-map", "gis-probe"]:
            if command("lipo", "-archs", str(executable_dir / name)).strip() != "arm64":
                raise RuntimeError(f"Expected an arm64 macOS executable: {name}")
            mac_dependencies(executable_dir / name, None)
        command("codesign", "--force", "--deep", "--sign", "-", str(package))
    else:
        windows_runtime(executable_dir)
    # Exercise runtime discovery of projection data relative to the executable.
    subprocess.run([str(executable_dir / ("gis-probe" + suffix)), "--check-environment"], cwd=dist, env=offline_environment(), check=True)
    print(f"Package and offline GIS environment check passed: {package}")
    if args.archive:
        create_archive(package, version, apple)


if __name__ == "__main__":
    main()
