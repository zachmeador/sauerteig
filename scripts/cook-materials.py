#!/usr/bin/env python3
"""Rebuild the starter textures with Pillow; optionally fetch pinned CC0 sources.

Run from any directory: python3 scripts/cook-materials.py --download
Ordinary game builds use the checked-in cooked files and need no Python/network.
Source files stay in ignored .local/material-sources. The manifest pins their
download URLs and checksums, so cooking never depends on a live catalog lookup.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import urllib.request

from PIL import Image


ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets/materials"
CACHE = ROOT / ".local/material-sources"
# Desaturation amount and RGB tint, preserving detail in the source photograph.
PALETTE_ADJUSTMENTS = {
    "gunmetal": (0.8, (0.72, 0.79, 0.84)),
    "worn_concrete": (0.35, (1.0, 1.0, 1.0)),
}


def verify_source(data, record, path):
    if len(data) != record["bytes"] or hashlib.md5(data).hexdigest() != record["md5"]:
        raise SystemExit(f"Source size or checksum mismatch: {path}")


def checked_source(record, download):
    path = CACHE / record["file"]
    if not path.exists():
        if not download:
            raise SystemExit(f"Missing {path}; rerun with --download")
        request = urllib.request.Request(
            record["url"], headers={"User-Agent": "Sauerteig asset preparation"}
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            data = response.read(32 * 1024 * 1024)
        verify_source(data, record, path)
        path.write_bytes(data)
    verify_source(path.read_bytes(), record, path)
    return path


def scaled(path, size):
    with Image.open(path) as source:
        return source.convert("RGB").resize((size, size), Image.Resampling.LANCZOS)


def normalize_normals(image):
    pixels = []
    for r, g, b in image.getdata():
        x, y, z = r / 127.5 - 1.0, g / 127.5 - 1.0, b / 127.5 - 1.0
        length = math.sqrt(x * x + y * y + z * z)
        if length < 0.001:
            pixels.append((128, 128, 255))
        else:
            pixels.append(tuple(round((v / length + 1.0) * 127.5) for v in (x, y, z)))
    image.putdata(pixels)
    return image


def adjust_palette(albedo, mix, tint):
    pixels = []
    for r, g, b, alpha in albedo.getdata():
        gray = 0.2126 * r + 0.7152 * g + 0.0722 * b
        rgb = tuple(
            round(((1.0 - mix) * color + mix * gray) * channel_tint)
            for color, channel_tint in zip((r, g, b), tint)
        )
        pixels.append((*rgb, alpha))
    albedo.putdata(pixels)


def hazard_paint(source, size):
    # A tile contains two black/yellow stripe pairs, modulated by plate detail.
    pixels = []
    for index, luminance in enumerate(source.convert("L").getdata()):
        x, y = index % size, index // size
        bright = ((x + y) // (size // 4)) % 2 == 0
        color = (187, 143, 34) if bright else (31, 34, 34)
        variation = 0.75 + luminance / 510.0
        pixels.append(tuple(min(255, round(channel * variation)) for channel in color))
    paint = Image.new("RGB", (size, size))
    paint.putdata(pixels)
    return paint.convert("RGBA")


def cook_material(material, sources, size):
    if material["key"] == "amber":
        return (
            Image.new("RGBA", (size, size), (255, 188, 105, 255)),
            Image.new("RGBA", (size, size), (128, 128, 255, 153)),
        )

    maps = sources[material["source"]]
    albedo = maps["albedo"].convert("RGBA")
    albedo.putalpha(maps["arm"].getchannel("R"))
    normal_roughness = maps["normal"].convert("RGBA")
    normal_roughness.putalpha(maps["arm"].getchannel("G"))

    adjustment = PALETTE_ADJUSTMENTS.get(material["key"])
    if adjustment is not None:
        adjust_palette(albedo, *adjustment)
    if material["key"] == "hazard":
        albedo = hazard_paint(maps["albedo"], size)
        albedo.putalpha(maps["arm"].getchannel("R"))
    return albedo, normal_roughness


def write_texture(relative_path, image):
    path = ASSETS / relative_path
    path.parent.mkdir(parents=True, exist_ok=True)
    image.save(path, optimize=True, compress_level=9)
    data = path.read_bytes()
    return {
        "file": relative_path,
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--download", action="store_true", help="fetch missing pinned CC0 sources"
    )
    args = parser.parse_args()
    manifest_path = ASSETS / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    size = manifest["texture_size"]
    CACHE.mkdir(parents=True, exist_ok=True)

    print("Starter surface assets from Poly Haven (https://polyhaven.com), CC0 1.0")
    sources = {}
    for source in manifest["sources"]:
        maps = {
            role: scaled(checked_source(record, args.download), size)
            for role, record in source["maps"].items()
        }
        maps["normal"] = normalize_normals(maps["normal"])
        sources[source["id"]] = maps

    outputs = []
    for material in manifest["materials"]:
        albedo, normal_roughness = cook_material(material, sources, size)
        outputs.append(write_texture(material["albedo"], albedo))
        outputs.append(write_texture(material["normal_roughness"], normal_roughness))
        print(f"{material['id']}: {material['name']} ({size} x {size})")

    manifest["cooked_files"] = outputs
    manifest["cooked_bytes"] = sum(entry["bytes"] for entry in outputs)
    # Two RGBA8 arrays, including every mip level down to 1 x 1.
    pixels_per_layer = sum((size >> level) ** 2 for level in range(size.bit_length()))
    manifest["gpu_bytes_with_mips"] = len(manifest["materials"]) * 2 * 4 * pixels_per_layer
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"Cooked PNG bytes: {manifest['cooked_bytes']:,}; "
        f"GPU bytes including mips: {manifest['gpu_bytes_with_mips']:,}"
    )


if __name__ == "__main__":
    main()
