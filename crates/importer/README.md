# Legacy OGZ import

`sauerteig-import` reads Cube 2 **OCTA versions 29–33**, including older Tesseract maps saved in that format. It produces an editable Sauerteig world plus an import report. This is a command-line import path; it is not yet an in-game file picker. Newer Tesseract `MAPZ` files and earlier Cube formats are rejected explicitly.

```sh
. ./scripts/env.sh
mkdir -p .local/imported
cargo run -p sauerteig-import -- inspect my-map.ogz --report my-map.inventory.json
cargo run -p sauerteig-import -- convert my-map.ogz --output .local/imported/my-map.json --center spawn --crop
cargo run -p sauerteig-server -- --bind 127.0.0.1:3002 --world .local/imported/my-map.json
```

Input maps and existing output files are never overwritten. A companion `my-map.cfg`, or an explicit `--cfg FILE`, is inspected for literal asset references only. Scripts, aliases, includes, filesystem paths and shader commands are never executed or followed.

## Geometry and scale

The default is **8 legacy Cube units per metre**. Legacy Z-up coordinates become Y-up with the rotation `(x,y,z) → (x,z,-y)`. `--center X,Y,Z` puts that source point at Sauerteig `[0,24,0]`; `--center spawn` uses the first playerstart, rounding only the region's center onto the import grid. Without a center argument the original map center is used.

Sauerteig currently supports a **128 × 48 × 128 metre world**. A whole map that does not fit is rejected before expansion. `--crop` explicitly retains that region and records every excluded or boundary-crossing leaf count in the report. The original geometry remains in the untouched OGZ. Source leaf bounds are used conservatively for this check, so even an empty part of a deformed cube's bounds can require explicit crop.

`--units-per-metre N` selects an explicit power-of-two scale. It never shrinks the map automatically. Geometry finer than the 1/8 metre import grid is rejected instead of rounded. For example, `--units-per-metre 32` makes physical dimensions one quarter of the default scale.

Large original leaves retain their eight authored corners and triangle diagonals. They are clipped into one-metre roots without voxel resampling. The core accepts authored fragment sources up to 65,536 eighths (8,192 metres), with checked coordinates. Rendering, rays, collisions, copy/paste and blueprints use the same clipped geometry. Imported partial slopes can be carved, painted, copied and transformed; corner deformation of only a clipped part is still rejected by the editor rather than silently altering the original slope.

Unsupported degenerate or non-manifold solids cause an error. `--skip-unsupported` is an explicit optional recovery mode and records the source position, size and reason for each omitted leaf. A conversion cannot exceed the server's 786,432-fragment, 1,024-object or 256 MiB save-file budgets.

## Appearance and entities

OGZ contains **texture slot numbers, virtual-slot overrides and model entity indices**, not the texture images or model files. Their meaning comes from the companion map config and the original installation's default texture/model configs. Missing configs or a different game version can change the slot-to-file mapping.

The report retains texture usage, virtual-slot metadata, map variables, entity positions/attributes, and literal config references. `--material-map FILE` accepts a JSON object such as `{"7":0,"12":2}` to select current Sauerteig materials for legacy slots. Unmapped slots use neutral material 0. A cell currently supports one material, so a cube with different face textures uses its most frequent slot (lowest slot breaks a tie). **Original per-face texture appearance, UV overrides and external asset loading are not implemented.**

Light and playerstart positions are imported. Light radius/color, spotlight parameters and spawn yaw remain in the report; imported lights use the current defaults and does not reproduce those attributes. Mapmodels, sounds, pickups, triggers, water/lava/clip volumes, blend maps and skyboxes need separate adapters. Source geometry still imports when its texture or model is missing. Old lightmaps, merged render caches and PVS are discarded in favor of current geometry meshing and dynamic lighting.

## Asset licenses

The original engine's **zlib license does not license all of its bundled media**. The [canonical Sauerbraten README](https://svn.code.sf.net/p/sauerbraten/code/README.html) explicitly distinguishes engine code from individually licensed maps, models, textures and sounds. Each original asset would need its own permission and attribution review before shipping with Sauerteig; importing a map does not relicense its referenced assets under MIT. The current importer redistributes none of that media. Suitable permissively licensed assets can be added later with their original notices; unresolved assets can instead be remapped to the project's CC0 collection.

## Format references

The parser is an independent Rust implementation of the on-disk layout, with no linked or vendored Cube engine:

- [Canonical Cube 2 `worldio.cpp`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/worldio.cpp): header, variables, entities, vslots, octree and version-specific surface records.
- [Canonical Cube 2 `octa.h`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/octa.h) and [`octa.cpp`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/octa.cpp): edge nibbles, face winding and convexity-selected diagonals.
- [Tesseract `worldio.cpp`](https://github.com/lsalzman/tesseract/blob/master/src/engine/worldio.cpp) and [`ents.h`](https://github.com/lsalzman/tesseract/blob/master/src/shared/ents.h): legacy OCTA compatibility and common entity IDs.
