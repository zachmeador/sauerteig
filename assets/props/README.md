# Prop models

These five models and their derived textures are **CC0 1.0**, sourced from [Poly Haven](https://polyhaven.com/license). Sauerteig's code remains MIT licensed. The source authors, download URLs, source SHA-256 checksums, and cooked mesh measurements are recorded in [manifest.json](manifest.json).

| Editor entry | Source and author | Near triangles | Distant triangles |
| --- | --- | ---: | ---: |
| Wall lamp | [Industrial Wall Lamp](https://polyhaven.com/a/industrial_wall_lamp), Kuutti Siitonen | 2,598 | 698 |
| Hanging lamp | [Hanging Industrial Lamp](https://polyhaven.com/a/hanging_industrial_lamp), Kuutti Siitonen | 3,486 | 1,062 |
| Utility box | [Utility Box 01](https://polyhaven.com/a/utility_box_01), James Ray Cock | 2,000 | 465 |
| Pipe | [Modular Industrial Pipes 01](https://polyhaven.com/a/modular_industrial_pipes_01), Jorge Camacho | 648 | 220 |
| Rock | [Rock 07](https://polyhaven.com/a/rock_07), Jenelle van Heerden | 1,800 | 400 |

The pipe is the source kit's long straight piece, extracted from its display arrangement. The rock is enlarged eight times to approximately 1.15 m high and 2.56 m deep. Utility box, pipe and rock have ground-center pivots. The wall lamp keeps its back-center mounting pivot, facing +Z; the hanging lamp's pivot is its ceiling mount and the fixture extends down 1.34 m. All meshes use glTF's right-handed Y-up convention.

Preparation preserves source UVs and normals while simplifying geometry, with an additional distant LOD. Tiny chain and cage details disappear at distance. Lamp glass is opaque frosted glass with a warm emissive surface; it does not need transparency sorting. The light entity supplies actual illumination separately. Meshes are static: no imported source scripts, rigs or shader programs run in the game.

Seven material layers use 512×512 textures: base-color RGB plus ambient occlusion alpha, and OpenGL tangent-space normal RGB plus roughness alpha. Metallic is approximated by a scalar per material. Normal vectors are renormalized after downsampling. Source baked material AO adds surface crevices; it does not replace dynamic scene lighting or bake the map's shadows.

The shipped meshes total **710,724 bytes** and the PNG textures **5,953,790 bytes**. Uncompressed GPU texture storage for all seven layers and complete mip chains is **19,573,400 bytes** (about 18.67 MiB), excluding mesh/instance buffers and driver overhead.

## Rebuild

Ordinary Rust builds embed the cooked files and require no downloads or asset tools. To regenerate from cached sources:

```sh
python3 -m venv .local/prop-tools/venv
.local/prop-tools/venv/bin/pip install pillow==13.0.0 numpy==2.5.4
npm install --prefix .local/prop-tools meshoptimizer@1.0.1 --ignore-scripts
.local/prop-tools/venv/bin/python scripts/prepare-props.py --offline
```

Omit `--offline` to retrieve missing 1K glTF sources. Downloads are cached in ignored `.local/prop-sources`; every file is checked against the official API's source checksum before conversion. The output manifest retains SHA-256 hashes. The mesh format is documented beside its bounded decoder in [`props.rs`](../../crates/client/src/props.rs).

The download helper is **Powered by Poly Haven** and identifies itself to the [public API](https://github.com/Poly-Haven/Public-API/blob/master/ToS.md). The game uses embedded files and makes no Poly Haven API calls. No website logos or preview renders are included.
