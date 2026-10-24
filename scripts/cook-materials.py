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
        if len(data) != record["bytes"]:
            raise SystemExit(f"Unexpected download size: {record['file']}")
        if hashlib.md5(data).hexdigest() != record["md5"]:
            raise SystemExit(f"Source checksum mismatch: {record['file']}")
        path.write_bytes(data)
    data = path.read_bytes()
    if len(data) != record["bytes"] or hashlib.md5(data).hexdigest() != record["md5"]:
        raise SystemExit(f"Cached source checksum mismatch: {path}")
    return path


def scaled(path, size):
    with Image.open(path) as source:
        return source.convert("RGB").resize((size, size), Image.Resampling.LANCZOS)


def normalized(image):
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--download", action="store_true", help="fetch missing pinned CC0 sources")
    args = parser.parse_args()
    manifest_path = ASSETS / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    size = manifest["texture_size"]
    CACHE.mkdir(parents=True, exist_ok=True)
    sources = {}
    print("Starter surface assets from Poly Haven (https://polyhaven.com), CC0 1.0")
    for source in manifest["sources"]:
        maps = {
            role: scaled(checked_source(record, args.download), size)
            for role, record in source["maps"].items()
        }
        maps["normal"] = normalized(maps["normal"])
        sources[source["id"]] = maps
    outputs = []
    for material in manifest["materials"]:
        if material["key"] == "amber":
            albedo = Image.new("RGBA", (size, size), (255, 188, 105, 255))
            normal = Image.new("RGBA", (size, size), (128, 128, 255, 153))
        else:
            maps = sources[material["source"]]
            albedo = maps["albedo"].convert("RGBA")
            albedo.putalpha(maps["arm"].getchannel("R"))
            normal = maps["normal"].convert("RGBA")
            normal.putalpha(maps["arm"].getchannel("G"))
            if material["key"] in ("gunmetal", "worn_concrete"):
                # Retain surface detail while keeping the industrial palette
                # coherent with the original saved-map material identities.
                mix = 0.8 if material["key"] == "gunmetal" else 0.35
                tint = (0.72, 0.79, 0.84) if mix == 0.8 else (1.0, 1.0, 1.0)
                pixels = []
                for r, g, b, a in albedo.getdata():
                    gray = 0.2126 * r + 0.7152 * g + 0.0722 * b
                    rgb = tuple(round(((1.0 - mix) * c + mix * gray) * t) for c, t in zip((r, g, b), tint))
                    pixels.append((*rgb, a))
                albedo.putdata(pixels)
            if material["key"] == "hazard":
                # A tile contains two complete black/yellow stripe pairs.
                # Photographic plate detail modulates the authored paint.
                grain = maps["albedo"].convert("L")
                pixels = []
                for i, lum in enumerate(grain.getdata()):
                    x, y = i % size, i // size
                    bright = ((x + y) // (size // 4)) % 2 == 0
                    color = (187, 143, 34) if bright else (31, 34, 34)
                    variation = 0.75 + lum / 510.0
                    pixels.append(tuple(min(255, round(c * variation)) for c in color))
                paint = Image.new("RGB", (size, size))
                paint.putdata(pixels)
                albedo = paint.convert("RGBA")
                albedo.putalpha(maps["arm"].getchannel("R"))
        for field, image in [("albedo", albedo), ("normal_roughness", normal)]:
            path = ASSETS / material[field]
            path.parent.mkdir(parents=True, exist_ok=True)
            image.save(path, optimize=True, compress_level=9)
            data = path.read_bytes()
            outputs.append({"file": material[field], "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
        print(f"{material['id']}: {material['name']} ({size} x {size})")
    manifest["cooked_files"] = outputs
    manifest["cooked_bytes"] = sum(entry["bytes"] for entry in outputs)
    # Two RGBA8 arrays, all levels from 512 down to 1 inclusive.
    manifest["gpu_bytes_with_mips"] = len(manifest["materials"]) * 2 * 4 * sum((size >> i) ** 2 for i in range(size.bit_length()))
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Cooked PNG bytes: {manifest['cooked_bytes']:,}; GPU bytes including mips: {manifest['gpu_bytes_with_mips']:,}")


if __name__ == "__main__":
    main()
