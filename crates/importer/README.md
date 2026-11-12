# Legacy OGZ import

`sauerteig-import` reads Cube 2 **OCTA versions 29–33**, including older Tesseract maps saved in that format. **Import is partial, not a faithful map restoration.** It converts supported solid geometry, water/glass volumes, and light/spawn positions into an editable Sauerteig world, plus a report of source data and omissions. Original textures, models, lighting properties, and gameplay entities are not restored. This is a command-line import path; it is not yet an in-game OGZ file picker. Newer Tesseract `MAPZ` files and earlier Cube formats are rejected explicitly.

```sh
. ./scripts/env.sh
mkdir -p .local/imported
cargo run -p sauerteig-import -- inspect my-map.ogz --report my-map.inventory.json
cargo run -p sauerteig-import -- convert my-map.ogz --output .local/imported/my-map.json --center spawn
cargo run -p sauerteig-server -- --bind 127.0.0.1:3002 --world .local/imported/my-map.json
```

Input maps and existing output files are never overwritten. A companion `my-map.cfg`, or an explicit `--cfg FILE`, is inspected for literal asset references only. Scripts, aliases, includes, filesystem paths and shader commands are never executed or followed.

To keep the converted map in the native app's **Saved worlds**, open **Import an existing world**, enter the generated JSON path and a world name, then choose **Import copy**. This creates a separate editable save; the OGZ and converted JSON remain unchanged.

## Geometry and scale

The default is **8 legacy Cube units per metre**. Legacy Z-up coordinates become Y-up with the rotation `(x,y,z) → (x,z,-y)`. `--center X,Y,Z` puts that source point at Sauerteig `[0,24,0]`; `--center spawn` uses the first playerstart, rounding only the region's center onto the import grid. Without a center argument the original map center is used.

The default conversion retains the whole map at its original scale in compact authored regions. World bounds are stored with the map, up to 8,192 metres per axis within coordinates −8,192 through +8,192. Unsupported bounds are rejected explicitly. `--crop` remains available to retain a **128 × 48 × 128 metre region** around the selected center; the report counts excluded and boundary-crossing leaves. The original geometry remains in the untouched OGZ.

`--units-per-metre N` selects an explicit power-of-two scale. It never shrinks the map automatically. Geometry finer than the 1/8 metre import grid is rejected instead of rounded. For example, `--units-per-metre 32` makes physical dimensions one quarter of the default scale.

Large original leaves retain their eight authored corners and triangle diagonals, stored once rather than expanded into one-metre cells. Local edits mask the authored base and supply exact clipped replacements. The core accepts authored fragment sources up to 65,536 eighths (8,192 metres), with checked coordinates. Rendering, rays, collisions, copy/paste and blueprints use the same geometry. Imported partial slopes can be carved, painted, copied and transformed; corner deformation of only a clipped part is still rejected by the editor rather than silently altering the original slope. Individual editing operations remain bounded; supporting a large world does not make a whole-world copy unlimited.

Unsupported degenerate or non-manifold solids cause an error. `--skip-unsupported` is an explicit optional recovery mode and records the source position, size and reason for each omitted leaf. A conversion cannot exceed 400,000 authored regions or the server's 786,432-fragment, 1,024-object and 64 MiB save-file budgets. The server also enforces a 64 MiB encoded snapshot budget when opening or editing a world, so both native and browser clients can join it.

Zero-thickness sheets are one unsupported case. They can be visible in the source engine, so skipping them can remove surfaces even though they enclose no volume. Recovery output is lossy; keep its report and label the saved copy accordingly. A successful server open/save check validates loadability, not a comparison with the original map.

## Appearance and entities

Water and glass are separate volume assignments, including those on empty octree
leaves. The importer decodes the older packed material byte used before OCTA 33
and the newer 16-bit layout, transforms their bounds with the geometry, and joins
adjacent equal-material boxes without rounding their extent. Material assignments
inside solid leaves are retained so later carving can expose them. Solid terrain
takes precedence for rendering and immersion. The converted layer is limited to
16,384 disjoint boxes; an oversized layer is rejected instead of silently dropped.

Glass blocks movement and shots; water supports swimming. They use Sauerteig's
current transparent shaders, not the original appearance. Alternate water/glass
styles, lava, and unsupported clip/noclip/gameclip/death/alpha flags are recorded
in the conversion report. In particular, source glass without a collision flag
becomes solid glass here. This is a reported behavior difference. Original
water colors, reflection/refraction settings, and liquid lighting are not restored.

OGZ contains **texture slot numbers, virtual-slot overrides and model entity indices**, not the texture images or model files. Their meaning comes from the companion map config and the original installation's default texture/model configs. Missing configs or a different game version can change the slot-to-file mapping.

The report retains texture usage, virtual-slot metadata, map variables, entity positions/attributes, and literal config references. `--material-map FILE` accepts a JSON object such as `{"7":0,"12":2}` to select current Sauerteig materials for legacy slots. Each face is mapped independently and rotated into Sauerteig's coordinate system. Unmapped slots use neutral material 0. **Original texture images, UV overrides and external asset loading are not implemented.**

Light and playerstart positions are imported. Light radius/color, spotlight parameters and spawn yaw remain in the report; imported lights do not reproduce those attributes. They become independent light nodes with no fixture model, rendered like lights placed in the editor: warm point lights with a 12 metre range, of which about 150 in view can shine at once. This changes the original lighting substantially. Sun and environment settings are not reproduced. Mapmodels, sounds, pickups, triggers, lava/clip volumes, blend maps and skyboxes need separate adapters. Source geometry still imports when its texture or model is missing. Old lightmaps, merged render caches and PVS are discarded in favor of current geometry meshing and dynamic lighting.

## Asset licenses

The original engine's **zlib license does not license all of its bundled media**. The [canonical Sauerbraten README](https://svn.code.sf.net/p/sauerbraten/code/README.html) explicitly distinguishes engine code from individually licensed maps, models, textures and sounds. Each original asset would need its own permission and attribution review before shipping with Sauerteig; importing a map does not relicense its referenced assets under MIT. The current importer redistributes none of that media. Suitable permissively licensed assets can be added later with their original notices; unresolved assets can instead be remapped to the project's CC0 collection.

## Format references

The parser is an independent Rust implementation of the on-disk layout, with no linked or vendored Cube engine:

- [Canonical Cube 2 `worldio.cpp`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/worldio.cpp): header, variables, entities, vslots, octree and version-specific surface records.
- [Canonical Cube 2 `octa.h`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/octa.h) and [`octa.cpp`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/octa.cpp): edge nibbles, face winding and convexity-selected diagonals.
- [Tesseract `worldio.cpp`](https://github.com/lsalzman/tesseract/blob/master/src/engine/worldio.cpp) and [`ents.h`](https://github.com/lsalzman/tesseract/blob/master/src/shared/ents.h): legacy OCTA compatibility and common entity IDs.
