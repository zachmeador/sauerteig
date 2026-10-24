#!/usr/bin/env python3
"""Prepare Sauerteig's five static CC0 props; never needed for ordinary builds.

python3 -m venv .local/prop-tools/venv
.local/prop-tools/venv/bin/pip install pillow==13.0.0 numpy==2.5.4
npm install --prefix .local/prop-tools meshoptimizer@1.0.1 --ignore-scripts
.local/prop-tools/venv/bin/python scripts/prepare-props.py [--offline]

Powered by Poly Haven (https://polyhaven.com); source assets are CC0.
API terms: https://github.com/Poly-Haven/Public-API/blob/master/ToS.md
"""

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import time
import urllib.request

import numpy as np
from PIL import Image


ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / ".local/prop-sources"
OUT = ROOT / "assets/props"
TEXTURE_SIZE = 512
HEADERS = {
    "User-Agent": (
        "Sauerteig-asset-preparation/0.1 "
        "(https://github.com/zachmeador/sauerteig)"
    )
}
NEAR_ERROR = 0.006
DISTANT_ERROR = 0.035


@dataclass(frozen=True)
class PropSpec:
    key: str
    source: str
    node: int
    scale: float
    near_triangles: int
    distant_triangles: int
    material_slots: tuple[int, ...]
    ground_pivot: bool = False


SPECS = (
    PropSpec("wall_lamp", "industrial_wall_lamp", 0, 1.0, 2600, 700, (8, 9)),
    PropSpec(
        "hanging_lamp", "hanging_industrial_lamp", 0, 1.0, 3000, 850, (10, 11)
    ),
    PropSpec("utility_box", "utility_box_01", 0, 1.0, 2000, 500, (12,), True),
    PropSpec("pipe", "modular_industrial_pipes_01", 1, 1.0, 648, 220, (13,), True),
    PropSpec("rock", "rock_07", 0, 8.0, 1800, 400, (14,), True),
)


def download(url):
    for attempt in range(4):
        try:
            request = urllib.request.Request(url, headers=HEADERS)
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except OSError:
            if attempt == 3:
                raise
            time.sleep(attempt + 1)


def cached_file(path, url, offline):
    if not path.exists():
        if offline:
            raise ValueError(f"missing cached {path}")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(download(url))
    return path.read_bytes()


def fetch_source(asset, offline):
    directory = CACHE / asset
    for endpoint in ("files", "info"):
        cached_file(
            directory / f"{endpoint}.json",
            f"https://api.polyhaven.com/{endpoint}/{asset}",
            offline,
        )

    files = json.loads((directory / "files.json").read_text())
    source = files["gltf"]["1k"]["gltf"]
    downloads = {f"{asset}.gltf": source, **source["include"]}
    provenance = []
    for name, record in downloads.items():
        path = directory / name
        if directory not in path.resolve().parents:
            raise ValueError("unsafe source filename")
        data = cached_file(path, record["url"], offline)
        if hashlib.md5(data).hexdigest() != record["md5"]:
            raise ValueError(f"source checksum mismatch: {path}")
        provenance.append(
            {
                "file": name,
                "url": record["url"],
                "sha256": hashlib.sha256(data).hexdigest(),
                "bytes": len(data),
            }
        )
    document = json.loads((directory / f"{asset}.gltf").read_text())
    return directory, document, provenance


def read_accessor(document, directory, index):
    accessor = document["accessors"][index]
    view = document["bufferViews"][accessor["bufferView"]]
    if accessor.get("sparse") or accessor.get("normalized"):
        raise ValueError("unexpected glTF accessor")

    components = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}[accessor["type"]]
    dtype = np.dtype(
        {5121: "u1", 5123: "<u2", 5125: "<u4", 5126: "<f4"}[
            accessor["componentType"]
        ]
    )
    buffer_path = directory / document["buffers"][view["buffer"]]["uri"]
    return np.ndarray(
        (accessor["count"], components),
        dtype=dtype,
        buffer=buffer_path.read_bytes(),
        offset=view.get("byteOffset", 0) + accessor.get("byteOffset", 0),
        strides=(view.get("byteStride", dtype.itemsize * components), dtype.itemsize),
    ).copy()


def read_texture(document, directory, reference, default):
    size = (TEXTURE_SIZE, TEXTURE_SIZE)
    if reference is None:
        return Image.new("RGB", size, default)
    texture = document["textures"][reference["index"]]
    image_path = directory / document["images"][texture["source"]]["uri"]
    with Image.open(image_path) as image:
        return image.convert("RGB").resize(size, Image.Resampling.LANCZOS)


def cook_texture(document, directory, material_id, slot, asset_key):
    material = document["materials"][material_id]
    pbr = material.get("pbrMetallicRoughness", {})
    color = read_texture(
        document, directory, pbr.get("baseColorTexture"), (255, 255, 255)
    )
    rgb = np.asarray(color, dtype=np.float32) / 255
    factor = np.array(pbr.get("baseColorFactor", [1, 1, 1, 1])[:3])
    rgb = np.clip(rgb * factor, 0, 1)

    arm = read_texture(
        document, directory, pbr.get("metallicRoughnessTexture"), (255, 255, 255)
    )
    channels = np.asarray(arm)
    roughness = np.asarray(channels[:, :, 1], dtype=float) * pbr.get(
        "roughnessFactor", 1
    )
    metallic = float(channels[:, :, 2].mean() / 255 * pbr.get("metallicFactor", 1))

    normal = np.asarray(
        read_texture(document, directory, material.get("normalTexture"), (128, 128, 255))
    ).astype(float)
    # Resampling changes normal lengths; restore unit vectors before packing.
    normal = normal / 127.5 - 1
    normal /= np.maximum(np.linalg.norm(normal, axis=2, keepdims=True), 1e-8)
    normal = np.clip(np.rint((normal + 1) * 127.5), 0, 255).astype("uint8")

    glass = "glass" in material.get("name", "")
    if glass:
        # Opaque frosted glass avoids transparency sorting in the game.
        rgb = np.clip(
            rgb * np.array([1.0, 0.88, 0.67]) * 0.7 + np.array([0.30, 0.23, 0.12]),
            0,
            1,
        )
        roughness = np.full((TEXTURE_SIZE, TEXTURE_SIZE), 170)

    albedo_ao = np.empty((TEXTURE_SIZE, TEXTURE_SIZE, 4), dtype="uint8")
    albedo_ao[:, :, :3] = np.rint(rgb * 255).astype("uint8")
    albedo_ao[:, :, 3] = channels[:, :, 0]
    normal_roughness = np.empty_like(albedo_ao)
    normal_roughness[:, :, :3] = normal
    normal_roughness[:, :, 3] = np.clip(np.rint(roughness), 0, 255).astype("uint8")

    name = f"{slot:02d}_{asset_key}" + ("_glass" if glass else "")
    folder = OUT / name
    folder.mkdir(parents=True, exist_ok=True)
    Image.fromarray(albedo_ao).save(folder / "albedo.png", optimize=True)
    Image.fromarray(normal_roughness).save(folder / "normal_roughness.png", optimize=True)
    return {
        "slot": slot,
        "key": name,
        "source_material": material.get("name"),
        "metallic": round(metallic, 4),
        "emission": 0.8 if glass else 0.0,
    }


def simplify(vertices, indices, target_triangles, error):
    request = json.dumps(
        {
            "vertices": vertices.tolist(),
            "indices": indices.tolist(),
            "target": target_triangles,
            "error": error,
        }
    )
    result = subprocess.run(
        ["node", str(ROOT / "scripts/simplify-prop.mjs")],
        input=request,
        text=True,
        capture_output=True,
        check=True,
    )
    return json.loads(result.stdout)


def write_mesh(path, parts):
    vertices = []
    indices = []
    for mesh, slot in parts:
        offset = len(vertices)
        vertices.extend((vertex, slot) for vertex in mesh["vertices"])
        indices.extend(index + offset for index in mesh["indices"])

    # SPM1: counts, position/normal/UV + material/emission vertices, then u32 indices.
    result = bytearray(struct.pack("<4sII", b"SPM1", len(vertices), len(indices)))
    for vertex, slot in vertices:
        result.extend(struct.pack("<8fIf", *vertex, slot, 0.0))
    result.extend(struct.pack("<" + "I" * len(indices), *indices))
    path.write_bytes(result)
    return {
        "vertices": len(vertices),
        "triangles": len(indices) // 3,
        "bytes": len(result),
        "sha256": hashlib.sha256(result).hexdigest(),
    }


def primitive_geometry(document, directory, primitive, spec):
    if primitive.get("mode", 4) != 4:
        raise ValueError("only triangle lists supported")
    attributes = primitive["attributes"]
    positions = read_accessor(document, directory, attributes["POSITION"]) * spec.scale
    normals = read_accessor(document, directory, attributes["NORMAL"])
    uv = read_accessor(document, directory, attributes["TEXCOORD_0"])
    if spec.ground_pivot:
        # Lamps keep their authored wall/ceiling pivots; floor props sit at y=0.
        positions[:, 0] -= (positions[:, 0].min() + positions[:, 0].max()) / 2
        positions[:, 1] -= positions[:, 1].min()
        positions[:, 2] -= (positions[:, 2].min() + positions[:, 2].max()) / 2
    vertices = np.concatenate((positions, normals, uv), axis=1)
    indices = read_accessor(document, directory, primitive["indices"]).flatten()
    if not np.isfinite(vertices).all():
        raise ValueError("nonfinite vertices")
    return vertices, indices


def cook_prop(spec, offline):
    directory, document, source_files = fetch_source(spec.source, offline)
    node = document["nodes"][spec.node]
    if any(key in node for key in ("matrix", "rotation", "scale", "children")):
        raise ValueError("unexpected node transform")
    # Kit layout translations are removed so each extracted prop has its own pivot.
    mesh = document["meshes"][node["mesh"]]
    total_triangles = sum(
        document["accessors"][primitive["indices"]]["count"] // 3
        for primitive in mesh["primitives"]
    )
    near_parts = []
    distant_parts = []
    source_vertices = []
    textures = []
    for primitive, slot in zip(mesh["primitives"], spec.material_slots, strict=True):
        vertices, indices = primitive_geometry(document, directory, primitive, spec)
        proportion = (len(indices) // 3) / total_triangles
        for parts, target, error in (
            (near_parts, spec.near_triangles, NEAR_ERROR),
            (distant_parts, spec.distant_triangles, DISTANT_ERROR),
        ):
            simplified = simplify(vertices, indices, max(12, int(target * proportion)), error)
            parts.append((simplified, slot))
        source_vertices.append(vertices)
        textures.append(
            cook_texture(document, directory, primitive["material"], slot, spec.key)
        )

    positions = np.concatenate(source_vertices)[:, :3]
    info = json.loads((directory / "info.json").read_text())
    asset = {
        "key": spec.key,
        "source": spec.source,
        "url": f"https://polyhaven.com/a/{spec.source}",
        "authors": info.get("authors"),
        "license": "CC0-1.0",
        "selected_node": node["name"],
        "scale": spec.scale,
        "source_triangles": total_triangles,
        "bounds_min": positions.min(axis=0).tolist(),
        "bounds_max": positions.max(axis=0).tolist(),
        "near": write_mesh(OUT / f"{spec.key}.spm", near_parts),
        "distant": write_mesh(OUT / f"{spec.key}_distant.spm", distant_parts),
        "source_files": source_files,
    }
    return asset, textures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true", help="use cached source files only")
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    assets = []
    textures = []
    for spec in SPECS:
        asset, prop_textures = cook_prop(spec, args.offline)
        assets.append(asset)
        textures.extend(prop_textures)
        print(spec.key, asset["near"]["triangles"], asset["distant"]["triangles"], flush=True)

    manifest = {
        "schema": 1,
        "license_url": "https://polyhaven.com/license",
        "texture_size": TEXTURE_SIZE,
        "texture_packing": {
            "albedo": "sRGB RGB + linear ambient occlusion alpha",
            "normal_roughness": "OpenGL tangent-space XYZ encoded RGB + linear roughness alpha",
        },
        "assets": assets,
        "textures": textures,
    }
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
