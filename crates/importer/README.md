# Legacy OGZ import

`sauerteig-import` reads Cube 2 **OCTA versions 27 and 29–33**, including older Tesseract maps saved in that format. **Import is partial, not a faithful map restoration.** It converts the map's octree, with its solid geometry, materials, and light/spawn positions, into an editable Sauerteig world, plus a report of source data and omissions. Original textures, models, lighting properties, and gameplay entities are not restored. This is a command-line import path; it is not yet an in-game OGZ file picker. Version 28, newer Tesseract `MAPZ` files and earlier Cube formats are rejected explicitly.

```sh
. ./scripts/env.sh
mkdir -p .local/imported
cargo run -p sauerteig-import -- inspect my-map.ogz --report my-map.inventory.json
cargo run -p sauerteig-import -- convert my-map.ogz --output .local/imported/my-map.json --center spawn
cargo run -p sauerteig-server -- --bind 127.0.0.1:3002 --world .local/imported/my-map.json
```

Input maps and existing output files are never overwritten. A companion `my-map.cfg`, or an explicit `--cfg FILE`, is inspected for literal asset references only. Scripts, aliases, includes, filesystem paths and shader commands are never executed or followed.

To keep the converted map in the native app's **Saved worlds**, open **Import an existing world**, enter the generated JSON path and a world name, then choose **Import copy**. This creates a separate editable save; the OGZ and converted JSON remain unchanged.

## Version 27 maps

Version 27 has a fixed 196-byte header instead of map variables, and no virtual slots or blend map. The importer keeps the header's lighting fields (light precision, error and leaks, ambient, sky, water, lava and waterfall colours, lerp and bump settings) and the map title in the report under `header.legacy`, and reads the rest as version 29. Version 28 shares that header but can carry a blend map, so it stays refused until a version 28 map is available to test.

Version 27 files written by the original game store a gzip CRC-32 that covers only the data after byte 205, not the whole file. The importer accepts exactly that: the stored checksum matches the data after byte 205, the stored length is right and the deflate stream is complete. The report notes it (`source.notes` and `notes`). Any other checksum mismatch, in any version, is still an error.

## Entities without a position

An entity whose position is not a finite number, or lies beyond 1,048,576 metres after conversion, is left out of the world and of its bounds. The report lists it in `entities_omitted_out_of_range` with its index, kind and source position (a position that is not a number appears as null). Some old maps hold such entities, for example an orphan teleport destination far below the map. Other entities still extend the world bounds, and the octree's root grows to hold them, up to the world's limits: an entity that would take the bounds past 8,192 metres, or the root past 8,192 metres where the map itself fits, is left out and listed in `entities_omitted_beyond_world`.

## Geometry and scale

The default is **8 legacy Cube units per metre**. Sauerbraten's world is Z-up and left-handed, so the right-handed Y-up import is a reflection, `(x,y,z) → (-x,z,-y)`: up stays up, source north (+y) stays Sauerteig's -Z, and east and west trade places. Imports made before 2026-11-14 rotated, `(x,y,z) → (x,z,-y)`, and so came out as mirror images of the maps; they equal today's imports reflected across X = 0. `--center X,Y,Z` puts that source point at Sauerteig `[0,24,0]`; `--center spawn` uses the first playerstart, rounding only the region's center onto the import grid. Without a center argument the original map center is used.

The default conversion retains the whole map at its original scale as one octree. The tree's root follows the source octree: it is the source cube, centred as above, so every source leaf is a leaf of the tree and eight equal siblings merge as the tree's canonical form says. The root grows to the smallest power-of-two cube that also holds the bounds when entities lie outside the source cube, and is placed on the source's grid. World bounds are stored with the map, up to 8,192 metres per axis within coordinates −8,192 through +8,192. Unsupported bounds are rejected explicitly. `--crop` remains available to retain a **128 × 48 × 128 metre region** around the selected center, which it moves to the nearest whole metre (at most half a metre) so that the region's edges fall on the tree's one-metre grid, as an edge off it costs leaves down to an eighth of a metre; the report's `center_source_xyz` and notes give the centre used: the root is the smallest cube on the source's grid that holds the region, and source leaves that cross the region's edge or are larger than the root become exact pieces of themselves, which the tree stores as it stores edited shapes. The report counts excluded and boundary-crossing leaves, and `tree` gives the root and the leaves by kind. The original geometry remains in the untouched OGZ.

`--units-per-metre N` selects an explicit power-of-two scale. It never shrinks the map automatically. Geometry finer than the 1/8 metre import grid is rejected instead of rounded. For example, `--units-per-metre 32` makes physical dimensions one quarter of the default scale.

Large original leaves retain their eight authored corners and triangle diagonals, stored once rather than expanded into one-metre cells. The edges of a Cube 2 leaf become the corners of a shape on the cube's 0–8 lattice, reflected with the axes; the face order and the diagonal each face's convexity picks follow the reflection, so the triangles are the source's. A shape whose top is a height field keeps only the top's diagonal, as imports always did. Local edits split a large leaf into exact pieces of it where they cut it. The core accepts authored fragment sources up to 65,536 eighths (8,192 metres), with checked coordinates. Rendering, rays, collisions, copy/paste and blueprints use the same geometry. Imported partial slopes can be carved, painted, copied and transformed; corner deformation of only a clipped part is still rejected by the editor rather than silently altering the original slope. Individual editing operations remain bounded; supporting a large world does not make a whole-world copy unlimited.

The importer reads level-of-detail nodes' children and ignores the node's own shape. The report's `source.lod_nodes` counts them and the ones whose own material differs from some leaf below; many old maps hold such nodes, and their children are what the world gets.

Unsupported degenerate or non-manifold solids (zero-volume shapes) cause an error. `--skip-unsupported` is an explicit optional recovery mode and records the source position, size and reason for each omitted leaf; a skipped leaf in water keeps its water. A conversion cannot exceed the tree's 4,194,304-leaf conversion limit or the server's 1,024-object and 64 MiB save-file budgets, and a world needs at most 16,384 water and glass boxes. The server also enforces a 64 MiB encoded snapshot budget when opening or editing a world, so the desktop client can join it.

Zero-thickness sheets are one unsupported case. They can be visible in the source engine, so skipping them can remove surfaces even though they enclose no volume. Recovery output is lossy; keep its report and label the saved copy accordingly. A successful server open/save check validates loadability, not a comparison with the original map.

## Appearance and entities

Every source leaf keeps its full 16-bit material value in the imported octree, empty leaves and leaves inside solid geometry included. The importer decodes the older packed material byte used before OCTA 33 and the newer 16-bit layout. The world file is format 3, which holds only water and glass: they are written as boxes from the tree's leaves, with adjacent equal-material boxes joined without rounding their extent, and lava, clip, noclip, death, alpha and the variants of water and glass are in the tree but not in the file until compact saves arrive (shared-sandbox 2e). Material assignments inside solid leaves are retained so later carving can expose them. Solid terrain takes precedence for rendering and immersion. The file is limited to 16,384 disjoint boxes; an oversized layer is rejected instead of silently dropped.

Glass blocks movement and shots; water supports swimming. They use Sauerteig's
current transparent shaders, not the original appearance. Alternate water/glass
styles, lava, and clip/noclip/gameclip/death/alpha flags are kept in the tree and
counted in the conversion report as not acted on. In particular, source glass without a collision flag
becomes solid glass here. This is a reported behavior difference. Original
water colors, reflection/refraction settings, and liquid lighting are not restored.

OGZ contains **texture slot numbers, virtual-slot overrides and model entity indices**, not the texture images or model files. Their meaning comes from the companion map config and the original installation's default texture/model configs. Missing configs or a different game version can change the slot-to-file mapping.

The report retains texture usage, virtual-slot metadata, map variables, entity positions/attributes, and literal config references. `--material-map FILE` accepts a JSON object such as `{"7":0,"12":2}` to select current Sauerteig materials for legacy slots. Each face is mapped independently and reflected into Sauerteig's coordinate system: Sauerteig's faces -X, +X, -Y, +Y, -Z, +Z take the slots of the source's +x, -x, -z, +z, +y, -y faces. The report counts the raw slots no mapping covers. Unmapped slots use neutral material 0. **Original texture images, UV overrides and external asset loading are not implemented.**

Light and playerstart positions are imported. Light radius/color, spotlight parameters and spawn yaw remain in the report; imported lights do not reproduce those attributes. They become independent light nodes with no fixture model, rendered like lights placed in the editor: warm point lights with a 12 metre range, of which about 150 in view can shine at once. This changes the original lighting substantially. Sun and environment settings are not reproduced. Mapmodels, sounds, pickups, triggers, lava/clip volumes, blend maps and skyboxes need separate adapters. Source geometry still imports when its texture or model is missing. Old lightmaps, merged render caches and PVS are discarded in favor of current geometry meshing and dynamic lighting.

## Comparing with an older import

A world imported before the importer reflected its maps is the new import's mirror image across X = 0. `compare_reflected` loads both files as the server loads them, reflects the old tree onto the new one's root, and compares the trees' digests and the objects, listing the first differing leaves if they differ. Run it on a re-import and the copy it replaces:

```sh
cargo run --release -p sauerteig-import --example compare_reflected -- NEW.json OLD.json
```

It exits with failure when they differ. Re-import with the options the old copy was made with: the defaults, plus `--skip-unsupported` for the four recovered copies (testcube, the_cube, the_cube1m and thecubesunlit).

## Asset licenses

The original engine's **zlib license does not license all of its bundled media**. The [canonical Sauerbraten README](https://svn.code.sf.net/p/sauerbraten/code/README.html) explicitly distinguishes engine code from individually licensed maps, models, textures and sounds. Each original asset would need its own permission and attribution review before shipping with Sauerteig; importing a map does not relicense its referenced assets under MIT. The current importer redistributes none of that media. Suitable permissively licensed assets can be added later with their original notices; unresolved assets can instead be remapped to the project's CC0 collection.

## Format references

The parser is an independent Rust implementation of the on-disk layout, with no linked or vendored Cube engine:

- [Canonical Cube 2 `worldio.cpp`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/worldio.cpp): header, variables, entities, vslots, octree and version-specific surface records.
- [Canonical Cube 2 `octa.h`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/octa.h) and [`octa.cpp`](https://svn.code.sf.net/p/sauerbraten/code/src/engine/octa.cpp): edge nibbles, face winding and convexity-selected diagonals.
- [Tesseract `worldio.cpp`](https://github.com/lsalzman/tesseract/blob/master/src/engine/worldio.cpp) and [`ents.h`](https://github.com/lsalzman/tesseract/blob/master/src/shared/ents.h): legacy OCTA compatibility and common entity IDs.

## Generated benchmark worlds

`sauerteig-import generate PRESET --output WORLD.json` writes a generated world
instead of converting a map, so that a performance measurement can be repeated
without any imported map. It also writes `WORLD.poses.json` (or `--poses FILE`),
a pose list for the capture tool that names the world and has cameras in
rooms, by water and glass, on the stairs and along a flight path. Existing files
are never overwritten.

```sh
cargo run --release -p sauerteig-import -- generate small --output .local/generated/small.json
```

The presets are `test`, `small` (the size of an imported map of about 340,000
leaves) and `nub1` (about 490,000). `--seed N` and `--edits on|off` change the
world; the edited layer of small cubes cut into large ones, shaped cubes with
mixed face materials and scattered blocks is on for `test` and `small` and off
for `nub1`. The files are the same on every system for the same preset, seed and
edits. The development guide's [performance checks](../../docs/development.md#generated-benchmark-worlds)
say how to measure them.

The file is format 3 with regions only, as `convert` writes, so the server opens
it the way it opens an imported map. Like a converted map it writes most shaped
cubes with explicit shapes rather than height fields. The command refuses a
world that the server would not open: over 786,432 leaves or 63 MiB.
