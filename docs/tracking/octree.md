# Octree world

State: planned. The maintainer chose to rebuild the core around one octree
before shared-sandbox stage 2's network and save formats (2026-11-13).

## Outcome

The world is one octree of cubes, as in Sauerbraten. Every cube, from an eighth
of a metre up to the whole map, carries its shape, six surface materials and a
cube material such as water or glass. Players build at any grid size, imported
maps keep their own octree, and collision, rays, meshing, edits, undo, saves
and the network each have one path.

## Why

The maintainer asked why water and glass weren't "just another cube state"
(2026-11-13). A design audit of the core that day found the cause, and more:

- **Geometry lives in three structures:** a grid of 1 m cells in 8 m chunks,
  1 m roots of clipped pieces, and an immutable list of imported leaves with
  per-leaf masks, a BVH and mesh batches (`World` in
  `crates/core/src/world.rs`). Every query, the mesher and the save and message
  formats handle all three. Physics has two complete implementations, chosen
  by whether the world holds a single imported leaf, and nine places branch on
  `has_regions()`.
- **No cube can be empty, so water and glass became a fourth structure,** a list
  of up to 16,384 boxes (`volumes.rs`, added 2026-10-24). Every water edit
  validates, resends and stores before and after copies of the whole layer, one
  world-wide clock makes distant water edits conflict, and collision, rays and
  meshing have separate code for it. Every other edit also clones and compares
  the whole layer. The importer reads water and glass per cube, then merges
  the_cube1m's 155,160 water and glass leaves into 4,227 boxes.
- **Imported leaves are frozen; edits can only mask them,** permanently.
  Deleting an 8 m cube inside a 512 m imported block turns 1 piece of 12
  triangles into 146 pieces of 1,752. A few room-sized edits in one block are
  refused by the mesh-batch triangle cap. Undo leaves the cut in place (a test
  asserts the seam), and saves keep a deletion marker for every cubic metre
  removed.
- **Built cubes stop at 1 m.** Every edit is a list of 1 m roots, at most
  16,384, so about 25 m is the largest cube one action can fill or delete, and
  pushing a corner at the 8 m grid moves the corners of 64 separate 1 m tiles,
  which makes a row of wedges instead of one slope.
- **Most triangles are hidden.** Each piece is meshed alone, so faces buried
  between solid neighbours stay in the mesh: at least 2,195,482 of the_cube1m's
  3,595,084 triangles, 61%.
- **Duplicate encodings.** A cube's shape (`heights` and `diagonal`, or `shape`)
  and its surface materials (`material`, or `face_materials`) each have two live
  forms, so cubes that look the same can compare unequal. Five kinds of edit
  request and four kinds of patch describe one operation.

Sauerbraten's `.ogz` files are already octrees. The importer flattens them into
a list, and the core then builds a different tree over that list. Shared-sandbox
2d's codec and 2e's binary saves would lock the three structures into both
formats, so this work comes first.

## Decided

Design lead, 2026-11-14, after two reviews of the first draft against the code.

**The tree**

- One octree on the eighth-metre grid holds all geometry and cube material.
  Node sizes are powers of two, from one eighth-metre unit to the root. The
  root is a power-of-two cube containing the world bounds. Its origin is set
  when a world is created or converted, so that existing cubes, including an
  imported map's own octree, fall on its grid, and it never moves afterwards.
- **Everything is relative to the root.** The editor's grid, mesh units and
  every write snap to the root's grid, so a selection at any grid size is a set
  of whole nodes. the_cube1m's root, for one, sits 24 m off a multiple of 64 m
  on Y. A write never places anything outside the world bounds: a node that
  crosses them is written only inside.
- A node is empty space, a leaf, or eight children. Children are shared and
  copied on write: cloning a world takes constant time, an edit copies the path
  from the root to what it changes, and saves and joins share snapshots as they
  do today.
- A leaf holds:
  - **geometry:** empty, full, or shaped. A shape is eight corners on the
    cube's 0–8 lattice and a diagonal choice for each face, today's `Shape`.
    It accepts everything today's height-field and `Shape` rules accept, so the
    height-field form converts on load. Shapes with no volume stay refused and
    reported: they are the zero-thickness sheets that legacy import lists as
    open work.
  - **or an exact piece:** the part of a larger shaped cube (its position, size
    and shape) that lies inside this leaf. Splitting a shaped cube then never
    approximates it, as today's clipped fragments don't.
  - **six surface materials,** one per face, always. Today's `material` and
    optional `face_materials` become one array.
  - **a cube material:** Sauerbraten's 16-bit material value. The name keeps
    "material" alone meaning a surface material, as in `Cell.material`. Water
    and glass act as they do today: they fill the cube's open space, so solid
    geometry wins at a point, water never blocks shots, and glass blocks
    movement and shots across its whole cube. Lava, clip, noclip, death, alpha
    and the water and glass variants are stored but inert until later stages
    give each its own rule; noclip, for one, will make solid geometry passable.
    Format 3 saves only water and glass, so the others survive saves from
    shared-sandbox 2e on.
- **One canonical form,** so equal worlds have equal trees and digests, and an
  edit followed by its undo restores the identical tree:
  - Eight children that are equal full or empty leaves merge into one. Shaped
    leaves never merge by equality: eight equal wedges make a sawtooth, not a
    larger wedge.
  - Eight sibling pieces of one source, with equal surface and cube materials,
    merge into a piece at their parent, and into the plain shape at the
    source's own node.
  - A piece whose cube lies wholly inside its source's solid is stored as a
    full leaf, and one wholly outside as an empty leaf. Merging counts such
    children as pieces of a source that is full or empty across them.
  - Fields without meaning are normalized: a leaf without solid geometry has no
    surface materials, and a planar or collapsed face's diagonal and a
    collapsed face's material take fixed values. An empty leaf of plain air
    isn't stored.
- **Nothing derived is stored in the tree.** Queries work from leaf definitions,
  and the renderer caches meshes per mesh unit.
- **One unit.** Positions are integers in eighth-metre units in the core and
  the editor. Metres appear only as `f32` world coordinates.

**Queries and physics**

- Point, box and ray queries walk the tree and skip empty space. Support and
  ceiling heights, box overlap and point containment handle each leaf kind
  once, and a large selection no longer scans the whole world
  (`MAX_QUERY_ROOTS` in `fragments_in`).
- Exact pieces are queried analytically: the leaf's box intersected with the
  source shape, without building cap polygons, so no per-root triangle cache
  is needed. All of it uses deterministic arithmetic, following
  [platforms](platforms.md) stage 2's rule; the `atan2` in today's containment
  test goes.
- **Movement goldens.** With one shape path, the height-field and shape forms
  of a ramp can't differ, and Arena's `ramps` and `shape_ramps` scenarios
  already differ in the last bit of Y on 6 ticks. So Arena's goldens may be
  recorded once more, under platforms stage 2's rule: every tick within the
  old 2e-3 tolerance of the old recording, with the reason in the stage notes.
  Scenarios that don't change stay bit for bit.

**Meshing**

- One mesher builds meshes per mesh unit: nodes of one fixed size, chosen by
  measurement between 16 and 64 m so that the_cube1m needs about as many meshes
  as today's 2,392 batches.
- A face is left out when solid neighbours cover it completely, judged as
  polygon coverage on the shared plane, so that the coincident caps where
  pieces of one source meet cancel, as today's cap code cancels opposite edges
  (`fine.rs`). A partly covered face is drawn whole, as Sauerbraten does. Faces
  on the root's outer boundary are left out.
- Each leaf's faces belong to one mesh unit, the one holding the leaf's minimum
  corner, whose bounds then cover the whole leaf. Faces are never split at unit
  boundaries.
- Water and glass surfaces come out where the cube material changes. Coplanar
  faces of one material merge within a unit, and faces lying fully against
  solid geometry are left out. A water–glass interface keeps both faces, as
  today. Glass's edge band follows the edge of each merged glass surface,
  which needs edge flags in a transparent vertex format defined in core.
- Mesh keys are unit positions. The renderer converts them with the core's unit
  size instead of the 8 m chunk size, and the imported batch ids above
  x = 1,000,000, which it misreads as positions today, go.

**Edits**

- A write names a node by position and size and carries a subtree, together
  with the parts it replaces:
  - everything, for paste, undo and redo;
  - geometry and surface materials, keeping the cube material, for extrude,
    carve, delete and shape;
  - surface materials only, for paint;
  - the cube material only, for water and glass.
- The editor plans at its grid size, from an eighth of a metre to half the
  root:
  - **Extrude** writes grid-sized cubes, each copying the cube behind it,
    subtree included, so a multi-material wall keeps its faces at any grid, as
    editor-feel stage 1 requires.
  - **Carve and delete** empty grid-sized cubes and keep their water and glass,
    as today.
  - **Paint** sets surface materials on every leaf in the selection and keeps
    its geometry.
  - **Shape** (corner pushes) reshapes the grid-sized cube, so pushing a corner
    at the 8 m grid shapes one 8 m cube. Finer detail inside it gives way to
    the one shaped cube, as in Sauerbraten and Tesseract, and undo brings it
    back (maintainer, 2026-11-14). Each face takes the surface material that
    covered most of it before the push, and the cube material stays where it
    was.
  - **Water and glass** set the cube material of the selected cubes.
  - **Paste** writes the clipboard's subtree at the destination and splits it
    again where the destination isn't on the subtree's grid; a shaped cube
    that lands off its own grid becomes exact pieces.
- **No size limit per edit** (maintainer, 2026-11-14). In an octree an edit
  costs what it creates, not the space it covers: changing a 5 km cube is one
  node. The 16,384-root (`MAX_EDITS`) and 65,536-fragment
  (`MAX_EDIT_FRAGMENTS`) limits go, and nothing replaces them.
  - An edit is refused only when the world would pass its memory budget. The
    server works that out before applying the edit, so a thin pane across a
    whole map can't use up memory first.
  - Until shared-sandbox 2d builds the budget, the 786,432-piece world guard
    (`MAX_WORLD_FRAGMENTS`) plays that part, and the 16 MiB JSON message
    ceiling applies. 2d sends larger edits in parts.
  - Undo history stays within a share of the memory budget, oldest entries
    out first, and rate limits count requests, not cubes.
- **Conflicts and undo compare snapshots; the tree holds no clocks.** The
  server keeps the tree roots of recent revisions, which share structure, over
  a window that measurement sets.
  - An edit with a base revision is refused when, in the nodes it writes, the
    parts it replaces differ between its base and now. A base older than the
    window is refused.
  - Undo is allowed while those nodes still hold exactly the parts the edit
    wrote, and redo while they hold what it replaced.
  - Unchanged subtrees compare as equal pointers, so the usual check is cheap.
  - This replaces the per-root clocks and the world-wide water clock. Edits in
    separate places never conflict, and a water edit and a geometry edit in the
    same place don't block each other, as today. Objects keep their own clocks.
- **One request and one patch.** One edit request (request id, base revision,
  writes, object changes) and one patch (revision, writes, object changes)
  replace `Edit`, `EditRoots`, `EditTransaction`, `PlaceObject`,
  `RemoveObject`, `WorldPatch`, `RootPatch`, `TransactionPatch` and `Objects`.
  Undo and redo get request ids and results. Until shared-sandbox 2d they are
  JSON, and each write carries its subtree in format 3's shapes: fragments, and
  water and glass boxes.
- Objects live in a map keyed by id, shared like the tree, instead of a `Vec`
  that is searched one by one and sorted again after each change.

**Formats until shared-sandbox 2d and 2e**

- Files, `Welcome` and the other messages keep format 3. The world loads
  versions 1–3 into the tree. The writer emits leaves as `regions`, 1 m leaves
  on whole metres as `cells`, height-field shapes as `heights` and `diagonal`
  (as the importer does today), and water and glass as boxes. A build from
  before the octree opens the result within its own limits, and a test checks
  that with the old loader.
- Reading and writing format 3, box merging included, stays until 2e, and for
  JSON export after it. Blueprints keep their version 3 shape. No new JSON
  format is added.
- Builds from before O2 can't join builds after it. Until 2d's version
  handshake, that shows up as a failed join.

**Kept from today's design,** because it is sound: cheap shared snapshots, exact
pieces instead of resampled slopes, stored face diagonals, per-object clocks,
and full replacements checked against a base revision.

## Order

- O1a's two sessions come first. O1b and O3 follow them; O2 needs O1a and O1b.
  Shared-sandbox 2d starts after O2
  ([shared-sandbox](shared-sandbox.md#stage-2-big-worlds-open-and-join-quickly)).
- Wave 1 also runs [platforms](platforms.md) stages 1 and 2, shared-sandbox's
  save and quit fixes and 2b's first session, [renderer](renderer.md) N1 and
  [legacy-import](legacy-import.md) I1 beside O1a. The save and quit fixes merge
  before O1a, which edits some of the same server files.
- **What each order removes.** O1a removes internals only: compact regions with
  their BVH, masks, overrides and batches; coarse-cell chunks and fine roots as
  storage; compiled triangles; `has_regions()`; `MAX_COMPACT_REGIONS`,
  `MAX_REGION_PIECES`, `MAX_COMPACT_BATCH_TRIANGLES` and `MAX_ROOT_FRAGMENTS`;
  and core's separate `World` serde format. O2 removes the water and glass edit
  path with its box limits and clock, the per-root clocks, `RootCost` and the
  old messages. O3 removes the importer's box path. 2e retires format 3 as the
  save format, and 2d replaces the JSON byte caps, `MAX_WORLD_FRAGMENTS` and
  `MAX_OBJECTS` with the memory budget.

## O1a: one octree in the core

State: ready. Wave 1, large: two sessions in order on one branch, merged once.
The design lead commits each session's work to the branch.

- **Decided:** the tree, queries, physics and meshing above. Every public core
  item that the server, client, importer and capture tool use keeps its
  signature and behavior, except as listed here. The format 3 types (`Cell`,
  `CellFragment`, `RootEdit`, `CellEdit`, `MaterialVolume`, `VolumeEdit`,
  `VolumeMaterial`) keep their fields and serde; the tree converts at its
  boundary.
- **Session 1, the tree:**
  - The tree, its leaves, canonical form and copy-on-write, in new core
    modules, with a digest and a public builder that takes leaves in any order,
    with a fast path for octree order. O3 and large-worlds stage 1 build
    through it.
  - **Conversion.** Load versions 1–3: compact regions, cells, roots, empty
    override roots, and volume boxes as cube material on the cubes they cover.
    A fragment whose clip box is partial, or whose origin is off its own size's
    grid (pastes at finer grids make these), becomes exact pieces. Write
    format 3 as decided.
  - Queries and physics as decided, with the second physics path deleted and
    the goldens re-recorded once if needed, as decided.
  - **Today's queries on the tree:** `raycast`, `contains_point`,
    `intersects_box`, the support and ceiling queries, `fragments_in`,
    `try_roots_in`, `regions`, `cells`, `root_overrides`, `detailed_roots`,
    `get`, `region_count`, `override_count` and `volume_material_at`.
    `root_fragments` returns a root's content in as few fragments as stay
    exact. `raycast_volumes` and `raycast_solid_volumes` skip joins between
    equal materials, as today, and report the box around a hit as its
    `bounds`.
- **Session 2, writes and meshes:**
  - **Today's writes on the tree:** `apply_edits`, `apply_root_edits` (1 m
    boxes written as nodes, keeping cube material), `canonical_root_fragments`,
    `validate_root_edits`, and the cube-material writes `prepare_volume_edits`,
    `apply_volume_edits` and `replace_volumes`. `replace_volumes` writes only
    the boxes that differ from the current ones and reports what changed.
    Cube-material writes count the pieces they would create before building
    them, and are refused if the world would pass the 786,432-piece guard, so
    a thin pane can't use up memory on the server or a client.
  - **Water and glass boxes.** `volumes()` returns the loaded boxes, cached,
    until a write changes cube material; then only the boxes around the change
    are derived again, deterministically, so unchanged glass keeps its edge
    bands. `volume_regions`, `validate_material_volumes`,
    `coalesce_material_volumes` and `MAX_MATERIAL_VOLUMES` stay. The server
    keeps checking the box list against the 16,384-box and 65,536-contact
    limits until O2 removes the box messages, so no client receives a layer it
    refuses.
  - **The mesher** as decided, behind `mesh_chunk` and the dirty keys,
    returning opaque surfaces only; water and glass meshing is O1b's. The
    renderer changes only where it turns mesh keys into positions
    (`rebuild_meshes` in `render.rs`).
  - **Size accounting.** The server computes format 3 bytes and pieces from the
    leaves an edit changes, including leaves split outside the written box, and
    counts each cell by its own JSON, so a world never saves past what
    `load_world` reopens.
  - The client's patch path handles `replace_volumes` errors instead of
    panicking (`game.rs`).
  - The importer writes the region list it builds instead of reading it back
    from the `World`, so its output doesn't change until O3.
  - Tests that assert today's internal layout (region counts after painting,
    `cells()` and `detailed_roots()` comparisons, empty mask roots, override
    counts) assert content and digests instead.
  - `/health` reports the tree's leaves, internal nodes and bytes by part.
  - Docs: `crates/core/CONTRACT.md` and the world paragraphs of the
    [development guide](../development.md).
- **Out of scope:** new edit messages, the editor's planning, the importer's
  reader, and drawing water and glass (O1b).
- **Parallel work:**
  - Core throughout.
  - The server's `lib.rs`, `edits.rs`, `persistence.rs` and `simulation.rs`
    (size accounting and `/health`), `tests.rs` and
    `tests/object_transactions.rs`. The save and quit fixes edit the same files
    and merge first.
  - The client's `render.rs` (mesh keys), `game.rs` (the patch error path) and
    the editor's tests. Shared-sandbox 2b's first session also edits `game.rs`
    and the editor's tests, and renderer N1 edits other parts of `render.rs`.
  - The importer's `convert.rs` (its output list), which legacy-import I1 also
    edits.
  - Platforms stage 2 edits `forward` in core's `lib.rs`, the steering in
    `physics.rs` and `movement.rs`, while O1a rewrites collision in
    `physics.rs`, `geometry.rs` and `fine.rs`. O1a's code follows that stage's
    deterministic-math rule.
  - Wherever two orders share a file, the second to merge rebases onto the
    first.
- **Acceptance in the app** (a copy of the library, through
  `SAUERTEIG_DATA_DIR`):
  - [ ] Host the_cube1m and a small library world. Fly around, paste, delete,
    paint water, undo, save, quit and reopen. Nothing changes except speed,
    memory and hidden faces, and glass edge bands move only next to water and
    glass edits.
  - [ ] Once legacy-import I1 has merged, convert nub1 and host it. Only the
    region cap refused it before; record its counted size against the 63 MiB
    guard, where it had 3.8 MB to spare.
- **Tests:**
  - [ ] Every existing test passes, except those that assert removed internals,
    which assert content instead. `compact_world.rs`'s seam after undo becomes
    "an edit and its undo restore the identical tree".
  - [ ] Arena's goldens pass, bit for bit or re-recorded once as decided.
  - [ ] Copies of every library world, `.local/the_cube1m.json`, the arena and
    synthetic worlds covering every fragment, override and volume case convert
    to the tree, to format 3 and back with equal digests.
  - [ ] Random edit and undo sequences, and random conversions, restore
    identical trees, including edits beside groups that could merge.
  - [ ] Old and new code agree on thousands of random queries over copies of
    the_cube1m and two smaller library worlds, recorded at the start: point
    material, box overlap, and support and ceiling heights exactly; ray first
    hits within 4 units in the last place of their coordinates, either cube
    where two lie at exactly the same distance; rays that start inside solid,
    as today.
  - [ ] The format 3 output of every library copy loads in the loader from
    before O1a.
  - [ ] the_cube1m's world-mesh triangles, water and glass excluded, fall from
    3,595,084 to at most 1.5 million. Captures of every pose in
    `scripts/capture-poses.json` match a baseline taken at the session's base
    commit, within the capture tool's tolerance; differences the session
    expects, such as shadows from faces that are now left out, are listed in
    the record.
- **Record:** a standalone server's memory and load time for the_cube1m before
  and after (978–981 MiB and 2.49–2.55 s on the M5 Pro today); leaves, internal
  nodes and bytes per converted world; mesh units and triangles before and
  after; client join time and frame times on the imported poses; the query
  comparison counts; nub1's counted size; any re-recorded golden and why; and
  anything that couldn't stay identical.

## O1b: water and glass from the tree in the renderer

State: waits for O1a. Medium.

- **Scope:**
  - The core mesher emits water and glass surfaces per mesh unit, as decided,
    in the transparent vertex format with edge flags.
  - The renderer keeps one shared pool of transparent quads, sorted back to
    front across the whole view every frame, as today. Re-meshing a unit
    replaces its quads in the pool.
  - Dirty units come from cube-material writes, which report the changed area,
    and from the patches `game.rs` applies. Transparent dirty units stay apart
    from opaque ones, so a water edit never re-renders shadows, and water and
    glass stay out of the shadow passes.
  - F3 shows how many units were re-meshed.
  - The renderer stops calling `volumes()`.
- **Parallel work:** `render/volumes.rs`, `volume.wgsl`, the rebuild paths in
  `render.rs`, `game.rs`, and core's mesher and write functions. Renderer N1
  (the pipelines in `render/volumes.rs`), renderer stage 2b (the invalidation
  in `rebuild`, `rebuild_all` and `objects`) and shared-sandbox 2b's first
  session (`game.rs`) touch the same files; whichever starts second waits for
  the first to merge.
- **Acceptance:**
  - [ ] Captures of materials-water, materials-underwater, materials-glass,
    imported-water, waterglass-view and waterglass-low match today's, apart
    from glass edge bands, which now follow surfaces; the record shows before
    and after.
  - [ ] CPU frame time for waterglass-low against waterglass-low-bare, and for
    imported-water, is no worse than today, and neither is GPU throughput.
  - [ ] In the app, painting water over a large area and undoing it re-meshes
    only the touched units, as F3 shows.
- **Record:** transparent quads and re-mesh times before and after.

## O2: grid-sized edits

State: waits for O1a and O1b. Large: two sessions in order on one branch,
merged once. The design lead commits each session's work to the branch.
Between the sessions the branch builds core and the server; the client builds
again after session 2.

- **Decided:** Edits, above.
- **Session 1, core, server and protocol:**
  - Writes with their parts, conflicts and undo by comparing recent roots,
    one edit request and one patch, and objects as a map.
  - No size limit per edit, as decided: the world guard is checked before an
    edit is applied, undo history stays within a share of memory, and rate
    limits count requests.
  - Remove the water and glass edit path (`VolumeEdit`,
    `TransactionPatch.volumes`, the water clock, the box limits and the
    whole-layer undo copies), the per-root clocks and maps, `RootCost`,
    `apply_root_edits`, and the messages that the request and patch replace.
    `Welcome` keeps format 3 until 2d.
  - Files: core's tree and `protocol.rs`; the server's `lib.rs`, `edits.rs`,
    `outbound.rs`, `simulation.rs`, `network.rs`, `network/tests.rs`,
    `bots.rs`, `tests.rs` and `crates/server/tests/`.
  - Docs: `crates/core/CONTRACT.md` (shared edits, history and messages) and
    the queue paragraphs of the development guide.
- **Session 2, the client:**
  - The editor plans at grid size as decided: `check_grid`, `change_grid`, the
    1 m tile limits in `editor.rs`, the Selection window's sizes and the help
    text. Shaping no longer refuses clipped or subdivided cubes
    (`ClippedShape`).
  - Copy, paste, rotate and mirror carry subtrees with their cube material.
    Blueprints keep their version 3 shape.
  - Aiming at water and glass uses the tree.
  - Selections keep their box unions through one shared box module in core,
    replacing `FineBox::subtract`, `RegionBox::difference` and their
    converters, and `RegionSet` keeps its serde shape.
  - The game applies the new patch and results, undo and redo included:
    `finish_edit`, the sync barrier, and object changes.
  - Files: `editor.rs`, `regions.rs`, `blueprints.rs`, `game.rs`,
    `game_objects.rs`, `game_panels.rs`, `hosting.rs` (its test sends
    `PlaceObject`), `capture.rs`, `capture/scenes.rs`, and the core box module.
  - Docs: [editor controls](../editor.md).
- **Not in scope:** preparing edits in parallel. It needs commits that apply
  writes to the live tree instead of swapping in a world prepared from a
  snapshot.
- **Parallel work:** shared-sandbox 2b's second session also edits `game.rs`
  (Welcome handling); the second to merge rebases. Renderer N2 runs beside it.
  The importer's functions stay until O3.
- **Acceptance in the app:**
  - [ ] On a copy of the_cube1m at the 8 m grid, pushing a corner shapes one
    8 m slope.
  - [ ] On a copy of the_cube1m, or a test world at least 128 m high, at the
    64 m grid: extrude a 64 m cube and delete it, each in one action. Undo
    restores the world exactly.
  - [ ] On a flat test world at the 1 m grid, fill a 100 × 100 × 2 m pool with
    water in one action. Deleting geometry inside it leaves the water.
  - [ ] Carve a room in a large imported block: the block splits cleanly, undo
    restores it, and save and reopen keep the result.
  - [ ] Two clients editing separate spots at once never see a conflict;
    editing the same spot conflicts as today; a water edit and a geometry edit
    in the same place don't conflict.
  - [ ] At the 8 m grid, pushing a corner of a cube that holds finer detail
    shapes it as one cube, and undo brings the detail back.
  - [ ] Copy, paste and blueprints carry water and glass, blueprints saved
    before O2 load, and on a flat test world a 1/8 m glass pane 100 × 100 m
    wide goes in one action.
- **Tests:**
  - [ ] Conflicts and undo: separate writes never conflict, overlapping writes
    after the base are refused, undo and redo follow the rule above, and a
    write beside a group that merges keeps working.
  - [ ] An edit and its undo restore the identical tree, with digests.
  - [ ] An edit that would pass the world guard is refused before anything is
    applied; a 64 m write is one leaf; a blueprint round trip keeps water and
    glass.
- **Record:** preparation and commit times for small and large edits, the
  time a client takes to apply the largest ones, the memory of history and the
  revision window, and the rate limits chosen.

## O3: imports straight into the tree

State: waits for O1a. Medium. It runs beside O1b and O2, which keep the
functions the importer calls.

- **Scope:**
  - Read the OGZ octree into the tree through O1a's builder. Edges become
    corners, textures map through `--material-map` as today, and each leaf
    keeps its full material value in the tree; format 3 saves only water and
    glass until 2e.
  - **The root follows the source octree.** Maps keep today's placement,
    centred with today's vertical offset, and `--crop` keeps its bounds. The
    root is the smallest power-of-two cube on the source's grid that holds the
    map and its entities, up to the 8,192 m world size. Entities beyond that
    are left out and reported, as I1 does for far entities.
  - **Fix mirrored imports.** The 2026-11-11 bug audit found imported maps
    mirrored: Sauerbraten's left-handed axes need a reflection where the
    importer rotates. Change `transform.rs` and, in `convert.rs`, the
    edge-to-corner mapping, the diagonal choice and the face order, and update
    the tests that pin today's mapping. Check the result against a map with a
    landmark whose handedness is known.
  - The report keeps counting inert materials and raw texture slots. Check
    whether level-of-detail nodes' own material ever differs from their
    children's, and report it if so.
  - Zero-volume shapes stay omitted and reported.
  - If [legacy-import](legacy-import.md) I1 hasn't merged, this order absorbs
    it, since both edit the reader and `convert.rs`.
- **The library afterwards.** None of the library's imported worlds has edits,
  so the design lead re-imports each from its original into a scratch copy of
  the library with the options it was imported with, compares it, and the
  maintainer swaps it in. It happens again after 2e, so that inert materials
  and raw texture slots are kept. Implementation sessions never touch the
  library.
- **Acceptance:**
  - [ ] Every library map, re-imported, matches its current copy reflected:
    the loaded trees' digests are equal once the current copy is reflected.
  - [ ] **Maintainer:** a few re-imported maps look the right way round, as
    the maintainer remembers them from Sauerbraten.
  - [ ] nub1 imports with the stock importer.
- **Record:** import times and memory, leaf counts, inert-material counts, and
  the orientation check.

## Evidence

Measured on 2026-11-13 by the design audit, with read-only scripts over
`.local/the_cube1m.json` (306,094 imported leaves, 4,227 water and glass boxes)
and the source:

- 74% of the_cube1m's leaves are smaller than 1 m: 12,296 at 1/8 m, 34,308 at
  1/4 m and 180,451 at 1/2 m. All 2,544 leaves larger than 8 m are solid boxes.
- 247,215 leaves are full boxes. 1,097,741 of their 1,483,290 faces (74%) are
  fully covered by full-box neighbours, so at least 2,195,482 of the map's
  3,595,084 triangles are hidden. The design lead re-ran this count.
- Splitting the faces of leaves larger than 8 m at 8 m chunk boundaries would
  add about 2.1 million triangles for interior faces and 6.3 million for faces
  on the world boundary, which is why faces stay whole.
- The importer turns the_cube1m's 45,782 water leaves and 109,378 glass leaves
  into 4,227 boxes; about 32,800 of the water leaves and 79,900 of the glass
  leaves are empty cubes. Only 890 of the boxes lie on whole metres, and 91% of
  the glass boxes are two to four eighths thick.
- Replaying today's masking on a 512 m solid imported leaf: deleting an 8 m cube
  leaves 146 pieces and 1,752 triangles, and a 16 m cube 546 pieces and 6,552
  triangles. With 127 untouched neighbours in its mesh batch, a fifth 16 m
  room in the block is refused at 34,236 batch triangles.
- Arena's `ramps` and `shape_ramps` goldens differ in the last bit of Y on 6
  ticks, because the height-field and shape support paths round differently.
- Today's authored arenas gain little storage from an octree, since 1 m walls
  don't merge: the Foundry's 13,386 cells become about 7,744 leaves. The gains
  are large cubes and slopes, edits that scale with the nodes they touch, one
  code path, and hidden faces left out.
