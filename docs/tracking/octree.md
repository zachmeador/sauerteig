# Octree world

State: active. The maintainer chose to rebuild the core around one octree
before shared-sandbox stage 2's network and save formats (2026-11-13). O1a, O3
and O1b merged on 2026-11-14; O2 is next, in wave 2.

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
    movement and shots across its whole cube. The water and glass variants act
    as water and glass, and will choose a look later (design lead,
    2026-11-14). Lava, clip, noclip, death and alpha are stored but inert until
    later stages give each its own rule; noclip, for one, will make solid
    geometry passable. Format 3 saves only water and glass, so the others
    survive saves from shared-sandbox 2e on.
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
  - **Known gap after session 1:** children that together make one shape,
    such as a half-height slab of full eighths, don't merge into one shaped
    leaf, so the same geometry can have two trees. Worlds stay exact, and an
    edit followed by its undo still restores the identical tree. Sauerbraten's
    remip makes this merge, and O2's first session adds it.
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
    (core's `MAX_WORLD_PIECES`, the server's `MAX_WORLD_FRAGMENTS`) plays that
    part, and the 16 MiB JSON message ceiling applies. 2d sends larger edits in
    parts.
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
- Format 3 has no place for the root's origin, so the tree derives it on load
  (session 2's record says how), and a save and reload can still move it.
  Shared-sandbox 2e's binary saves store it.

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
  save format, and 2d replaces the JSON byte caps, the piece guard
  (`MAX_WORLD_PIECES`, `MAX_WORLD_FRAGMENTS`), `MAX_OBJECTS` and the conversion
  guard (`MAX_CONVERSION_LEAVES`, 4,194,304
  leaves) with the memory budget.

## O1a: one octree in the core

State: merged on 2026-11-14, both sessions with the fixes from session 2's
review. The checks in the app await the maintainer.

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
  - **From session 1's review** (design lead, 2026-11-14): water and glass
    variants act (`volume_material_of` ignores the variant bits); re-record
    Arena's goldens once as decided, since stairs, ramps, shape_ramps,
    wall_slide and compact_world change on the tree; find out why foundry under
    the Sauerbraten profile drifts 2.3 cm from its recording at tick 379 on the
    tree (the ignored `sauerbraten_on_the_tree_stays_within_tolerance_of_the_recording`),
    then fix the tree or record why and re-record that golden; and replay
    session 1's recorded query answers once `World` runs on the tree.
  - **Today's writes on the tree:** `apply_edits`, `apply_root_edits` (1 m
    boxes written as nodes, keeping cube material), `canonical_root_fragments`,
    `validate_root_edits`, and the cube-material writes `prepare_volume_edits`,
    `apply_volume_edits` and `replace_volumes`. `replace_volumes` writes only
    the boxes that differ from the current ones and reports what changed.
    Cube-material writes count the pieces they would create before building
    them, and are refused if the world would pass the 786,432-piece guard, so
    a thin pane can't use up memory on the server or a client. (After review:
    edits only; loads and patches count against the conversion guard, and the
    server checks every edit's exact leaf count.)
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
  - [ ] Convert nub1 with legacy-import I1's reader and host it. Only the
    region cap refused it before; record its counted size against the 63 MiB
    guard, where it had 3.8 MB to spare.
- **Tests:**
  - [x] Every existing test passes, except those that assert removed internals,
    which assert content instead. `compact_world.rs`'s seam after undo becomes
    "an edit and its undo restore the identical tree". Session 2, below.
  - [x] Arena's goldens pass, bit for bit or re-recorded once as decided.
    Session 2 re-recorded both golden files once, below.
  - [x] Copies of every library world, `.local/the_cube1m.json`, the arena and
    synthetic worlds covering every fragment, override and volume case convert
    to the tree, to format 3 and back with equal digests. Session 1, below.
  - [x] Random edit and undo sequences, and random conversions, restore
    identical trees, including edits beside groups that could merge. Session
    1, on the tree's own writes; session 2 repeats it through `World`.
  - [x] Old and new code agree on thousands of random queries over copies of
    the_cube1m and two smaller library worlds, recorded at the start: point
    material, box overlap, and support and ceiling heights exactly; ray first
    hits within 4 units in the last place of their coordinates, either cube
    where two lie at exactly the same distance; rays that start inside solid,
    as today. Session 2 replayed session 1's recordings against `World`;
    every difference falls in a class session 1 recorded, below.
  - [x] The format 3 output of every library copy loads in the loader from
    before O1a. Session 1, below.
  - [x] the_cube1m's world-mesh triangles, water and glass excluded, fall from
    3,595,084 to at most 1.5 million. Captures of every pose in
    `scripts/capture-poses.json` match a baseline taken at the session's base
    commit, within the capture tool's tolerance; differences the session
    expects, such as shadows from faces that are now left out, are listed in
    the record. Session 2: 717,378 triangles; captures below.
- **Record:** a standalone server's memory and load time for the_cube1m before
  and after (978–981 MiB and 2.49–2.55 s on the M5 Pro today); leaves, internal
  nodes and bytes per converted world; mesh units and triangles before and
  after; client join time and frame times on the imported poses; the query
  comparison counts; nub1's counted size; any re-recorded golden and why; and
  anything that couldn't stay identical.

### O1a session 1 record (2026-11-14)

Code, automated checks and measurements only. Nothing here was observed in the
running app, because `World` doesn't use the tree yet.

- **Built beside today's structures,** as the design lead asked: `World` keeps
  its storage until session 2 switches it, so its queries, its physics and
  Arena's goldens are unchanged, and the old-versus-new comparisons call both
  implementations directly. The tree's queries take `World`'s arguments and
  give `World`'s results, ready for session 2 to route `World` through them.
- **Files.**
  - New `crates/core/src/octree/`: `mod.rs` (the tree, leaves, canonical form,
    copy-on-write nodes, digest, `TreeStats`, the public `OctreeBuilder`, and
    crate-private writes with parts that return what an undo writes back),
    `lattice.rs` (exact integer geometry on the 0–8 lattice), `format3.rs`
    (`Octree::from_format3`, `from_world`, `to_format3` and the format 3
    views), `query.rs` (the queries listed above), `digest.rs` (SipHash-2-4
    with a 128-bit output and fixed keys; stable, not a cryptographic
    commitment), `tests.rs`, and `private_worlds.rs` (ignored tests on copies
    in `.local/`).
  - `physics.rs` runs the movement step on a crate-private `Space`. `World`'s
    implementation calls today's functions unchanged; the tree's is the single
    path, and `Octree::simulate_player_with` exposes it. `lib.rs` exports the
    new types.
  - `tests/arena_golden.rs` gains a test that runs all 19 scenarios on the
    tree against the unchanged recording.
- **How the tree answers as today.** Plain leaves replay today's region
  arithmetic on the same triangles: `lattice::triangles` gives
  `shape_triangles`'s triangles, in its order, from integer arithmetic, so an
  imported leaf answers as its region did. Containment is exact: signed
  crossings of a vertical probe with a fixed symbolic perturbation, in `i128`,
  instead of the solid angle and its `atan2`. Pieces use their source's
  triangles limited to the leaf, plus the leaf's own faces where the source's
  solid crosses them. Only `+ - * /` and correctly rounded square roots are
  used, so movement on the tree gives the same bits in dev and release builds;
  the two `trig` scenarios differ in release exactly as today's code does,
  from `sin` and `cos` in steering, which is platforms stage 2's.
- **Choices where the order left room:**
  1. **The root from format 3.** Format 3 can't store the root, so conversion
     derives it: at the bounds' minimum corner when the largest whole region
     lies on that grid, otherwise on that region's grid just below. Every
     library world gets the bounds' corner. Limit: in a world whose bounds
     are off its cubes' grid, such as a cropped import, edits that cut every
     one of the largest whole regions can move the root at the next reload.
     The geometry stays the same, but digests and the editor's grid would
     shift. Shared-sandbox 2e's binary saves should store the root.
  2. **Fixed diagonals.** A face's diagonal means nothing when flipping it
     leaves every other face's triangles alone and the face has no triangles
     or is a strictly convex planar quad. Its fixed value is the importer's
     choice for planar faces (bits 0–3 set, 4–5 clear), so imported shapes
     keep their triangles; with 0 as the fixed value, 5 of 1,033 ceiling
     heights on the_cube1m changed in the last bit. A planar quad with a
     collinear corner keeps its diagonal, since the other choice leaves a
     T-junction that today's shape rules refuse.
  3. **Faces without a surface** (collapsed, or cancelled as zero-thickness
     sheets) take the material of the first face that has one, so a uniform
     cube stays uniform and its cut faces keep its material.
  4. **Cube material.** Water and glass act only without a variant (index bits
     0); clip, death and the other flags are kept but inert. Format 3 writes
     water and glass leaves as boxes merged with today's
     `coalesce_material_volumes`.
  5. **Pieces never grow past their source's cube.** A merged piece's cube may
     cross the world bounds while its solid stays inside; the writer clips the
     written region to the bounds.
  6. **Unions of whole eighths.** A shaped cube whose solid is a union of
     whole eighths, a half-height slab for one, splits into full and empty
     children that no rule merges back, so two histories can give different
     trees for equal geometry. A write returns the leaf it split as what an
     undo writes back, so an edit and its undo still restore the identical
     tree, and a write that changes nothing leaves the tree as it was.
  7. **Queries that knowingly differ from today:**
     - The support interval is closed. Today's region path misses a floor
       lying exactly at the bottom of the interval it scans; today's cell path
       and the tree find it.
     - `volume_material_at` tests the point itself. Today's terrain test, a
       box shrunk by its contact tolerance to nothing, misses solids within
       about 63 m of the origin and reports water inside them.
     - A ray's internal-join samples start from the unrounded hit, in `f64`.
       Today's `f32` samples round back onto the face at grazing angles or a
       few hundred metres out, which loses real faces or reports internal
       joins as hits.
     - Glass floors and ceilings count only where more glass doesn't cover
       them. Today's count the tops of its stored boxes, internal ones
       included.
  8. **Hits.** A solid hit's `bounds` is the leaf's box within the hit's metre,
     as for today's regions, so it is larger where leaves merged. A water or
     glass hit's `bounds` is the leaf around it, and its face is the one the
     ray crossed.
  9. **Format 3 views.** `regions()` and `cells()` are the writer's split.
     `root_overrides()` is empty and `override_count()` is 0, since the tree
     has no masks. `detailed_roots()` lists metres with detail below a metre.
     `get()` gives a one-metre shaped leaf on whole metres, or any metre of a
     full leaf. `root_fragments()` applies `canonical_root_fragments`, then
     merges fragments of one source into boxes.
  10. **Guard.** Conversion refuses inputs that would expand past
      `MAX_CONVERSION_LEAVES` (4,194,304 leaves), counting each volume box's
      cubes before filling it, so a hostile file can't exhaust memory first; a
      test converts an 8 km glass sheet off the grid and is refused.
- **Checks,** all passing:
  - `cargo fmt --all`,
    `cargo clippy --workspace --all-targets --locked -- -D warnings` and
    `cargo test --workspace --locked`.
  - Unit and property tests in `octree/tests.rs` and `octree/lattice.rs`:
    leaves given in any order, or written one by one, build the same tree;
    leaves in octree order skip the sort; canonical merging (equal full and
    empty leaves, pieces into a piece and back into their shape, never equal
    shapes); normalized fields and canonical diagonals over random shapes;
    exact containment against today's solid angle away from faces; clones
    that share untouched nodes; 60 random sequences of 20 writes of every
    part, with groups that merge, then undo and redo, restore identical trees;
    80 random format 3 worlds (whole, partial and misaligned regions, cells,
    empty cells, roots with fragments, empty override roots, water and glass)
    convert, write and reload with equal digests, open in today's loader with
    the same content, and give the same digest in any input order and from
    today's `World`; the arena and hand-made cases of every part do the same;
    the height-field and shape forms of a cube build the same tree; root
    fragments, roots in a box and fragments in a box hold today's content;
    on the arena, 4,000 queries of each kind agree with today's exactly; on 40
    random worlds of 300 queries each, 8 point materials (water inside solid,
    as in choice 7, or a point on a face), 9 rays (another cube or face at the same
    distance, a grazing contact, or a zero-thickness sheet in today's regions)
    and 26 support heights within 1e-4 m (today's cell paths round
    differently) differ; movement on the tree stays within 2e-3 of today's in
    ten runs, 1,450 of 1,669 ticks bit for bit; the arena's digest is
    pinned.
  - `cargo test -p sauerteig-core --test arena_golden`: the recording passes
    bit for bit on `World` as before, and the new test runs it on the tree.
  - On copies in `.local/` (`the_cube1m.json` and `library/<name>.json`):
    `cargo test --release -p sauerteig-core --lib private_ -- --ignored
    --nocapture --test-threads 1`.
- **Conversion,** release build on the M5 Pro, all 24 copies passing. Leaves
  are full, shaped, piece and water or glass; bytes are the tree's estimate.
  Format 3 pieces and MiB are the server's counts for the written file, with
  the file read before conversion in brackets.

  | World | JSON | Convert | Full | Shaped | Water, glass | Internal | Bytes | Pieces | MiB |
  | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
  | the_cube1m | 171 ms | 237 ms | 167,562 | 58,879 | 112,672 | 80,332 | 14.5 M | 226,441 (306,094) | 34.73 (42.76) |
  | nub | 157 ms | 167 ms | 225,219 | 41,804 | 80,625 | 75,571 | 13.0 M | 267,023 (284,824) | 40.88 (40.57) |
  | tubes1 | 133 ms | 104 ms | 163,276 | 29,368 | 77,023 | 62,668 | 10.5 M | 192,644 (269,084) | 27.34 (35.70) |
  | complex | 29 ms | 64 ms | 12,838 | 14,302 | 0 | 6,929 | 1.7 M | 27,140 (37,927) | 4.73 (5.85) |
  | ralith1 | 20 ms | 18 ms | 30,762 | 3,552 | 21,321 | 11,413 | 1.8 M | 34,314 (41,958) | 4.75 (5.49) |
  | My world | 3 ms | 2 ms | 7,065 | 132 | 0 | 2,086 | 0.3 M | 7,197 (7,463) | 1.07 (1.10) |

  No library world has exact pieces; Sauerbraten keeps many equal children
  unmerged, which is why full leaves fall (the_cube1m's 247,215 full regions
  become 167,562 leaves). the_cube1m writes 172,178 regions, 54,263 cells and
  4,212 water and glass boxes (4,227 before). Writing takes 35 ms; the loader
  from before the octree opens the result in 0.8–0.9 s. nub's written file is
  0.3 MiB larger, because its one-metre leaves are written as cells, which
  the server counts as roots; it stays 22 MiB under the 63 MiB guard.
- **Query comparison,** today's `World` against the tree, 5,000 queries of
  each kind near geometry per world. The table counts differences. Rays with
  the same first hit, within 4 units in the last place, may name another face
  or a larger cube; any other difference outside the listed classes fails the
  test.

  | Queries | the_cube1m | complex | ralith1 | Class |
  | --- | --- | --- | --- | --- |
  | Box overlap, solid and glass | 0 | 0 | 0 | |
  | Point material | 100 | 0 | 301 | today reports water or glass inside solid, at most 62.7 m out |
  | Support where a body stands | 1 of 2,210 | 0 of 2,324 | 0 of 1,596 | a face the body only touches, internal to a merged leaf |
  | Ceilings | 0 of 1,033 | 0 of 613 | 0 of 459 | |
  | Rays, first hit | 5 | 1 | 18 | today's samples round onto the face |
  | Rays, same hit, other face | 0 | 1 | 0 | a tie on an edge |
  | Rays, same hit, larger cube | 239 | 236 | 37 | the hit's leaf merged today's regions |
  | Water and glass rays | 0 of 10,000 | none | 0 of 10,000 | |
  | Step-up probes inside solid | 99 of 4,711 | 103 of 4,517 | 69 of 4,867 | internal faces between today's regions or glass boxes |

  Rays hit in 4,453, 4,413 and 3,869 of 5,000 cases, and 2,459, 2,143 and
  3,407 of them start inside solid. The tree is faster: 5,000 box queries take
  4.0 ms against today's 16.6 ms on the_cube1m and 5,000 rays 23 ms against
  153 ms; support queries cost the same. Today's answers are recorded in
  `.local/query-recordings/` (ignored), and
  `private_query_recordings_replay` checks the tree against them, for use
  after session 2 removes today's code.
- **Movement on the tree** against Arena's recording, every tick within the
  2e-3 re-recording tolerance: 14 scenarios bit for bit; `stairs`, `ramps`
  and `shape_ramps` at most 1.5e-4 m apart, with 145, 145 and 139 of 230
  ticks exact; `wall_slide` 2.3e-5 m, 107 of 170 exact; `compact_world` with
  identical positions and velocities but 65 of 273 grounded flags changed by
  the closed support interval. `ramps` and `shape_ramps` build the same tree,
  so they no longer differ from each other.
- **Left for session 2:**
  - Switch `World` to the tree and route its queries and `simulate_player*`
    through `Space for Octree`; delete today's paths and the internals listed
    under Order.
  - Record Arena's goldens once more: `stairs`, `ramps`, `shape_ramps`,
    `wall_slide` and `compact_world`, all within tolerance today.
  - The writes, `volumes()` cache, mesher, size accounting, `/health` (from
    `TreeStats`), the importer's region list, the client's error path, the
    tests that assert internals, and the docs, as listed for session 2. The
    tree's crate-private `write`, `clear_geometry`, `insert_source` and
    `set_cube_material` are the primitives to build them on.
  - Replay the recordings after the switch, and measure the standalone
    server's memory and load time, which can't change until `World` stores
    the tree.

### O1a session 2 record (2026-11-14)

Code, automated checks and measurements. Nothing here was observed in the
running app; the acceptance boxes in the app stay for the maintainer.

- **`World` runs on the tree.** It holds the octree and the water and glass
  boxes; cloning takes constant time. Its queries, `simulate_player*` and the
  server and clients all go through the tree, and today's paths are gone:
  `compact.rs` (regions, BVH, masks, overrides, batches), chunk and fine-root
  storage, compiled triangles, `has_regions()`, `cell_chunk`, `chunk_bounds`,
  `CHUNK_SIZE`, `MAX_COMPACT_REGIONS`, `MAX_COMPACT_BATCH_TRIANGLES`,
  `MAX_REGION_PIECES`, `MAX_ROOT_FRAGMENTS` (disjoint fragments in a metre
  number at most 512 anyway), and core's own `World` serde format. The
  server's `RootCost` went too: leaf-based accounting replaced its only use,
  ahead of O2.
- **Files.** New `octree/mesh.rs` (the mesher and mesh units) and
  `octree/diff.rs` (what a write changed: leaves removed and added, and where
  geometry changed). `world.rs` (the new `World`, `WorldSnapshot`,
  `WorldBounds`), `fine.rs` (root writes), `volumes.rs` (the box facade),
  `geometry.rs` (now only shape rules), `physics.rs`, `octree/mod.rs` (remerge,
  write counting, a full-cube fast path), `format3.rs` (borrowed parts,
  `Format3Piece`), `query.rs` (one support rule, below). Server: `lib.rs`,
  `edits.rs`, `persistence.rs`, `simulation.rs`, `network.rs` (opening a world
  once). Client: `render.rs` (mesh keys only) and `game.rs` (the patch path).
  Importer: `convert.rs`. Docs: `crates/core/CONTRACT.md` and the development
  guide.
- **Writes.** `apply_root_edits` validates everything, then clears each
  metre's geometry and writes its fragments' sources, keeping cube material. A
  write that changes no geometry restores the tree it started from, so it
  leaves no split leaves behind. Writing a fragment of a larger shaped source
  also merges the nodes around it back into that source where every leaf
  under them is the source restricted: eight children wholly inside or outside
  a shape (the slab gap in Decided) carry no piece to show which shape they
  belong to, and without this an undo would leave such a shape split. Every
  write's mesh units to build again come from the difference between the
  trees before and after (after review, below). Clipping failures no longer
  refuse an edit, since nothing is compiled at write time; the mesher falls
  back to the clipped source without caps if a cap cannot be triangulated,
  and counts each time it does.
- **Water and glass.** `volumes()` keeps the loaded boxes. `apply_volume_edits`
  computes the new list with today's cut, add and coalesce, which changes only
  boxes the edit touches, and writes the edits' own boxes as cube material.
  `replace_volumes` writes only where the new list differs from the current
  one: plain space where a box is gone, then each new box's material where the
  current boxes held another, a box being written whole when its difference
  would take more than 64 pieces; it returns the boxes it wrote. Edits'
  cube-material writes (`apply_volume_edits`, and `prepare_volume_edits`, so
  the editor learns of a refusal before sending) count the leaves they would
  add before building them, against `MAX_WORLD_PIECES` (786,432, now a core
  constant the server uses), and are refused past it; an eighth-thick pane
  across the whole 8 km test world is refused, a one-metre floor 64 m across
  is 4,096 leaves. `replace_volumes`, which loading, patches and the server's
  undo and redo use, counts against the conversion guard instead (after
  review, below). The variants of water and glass act as water and glass.
- **Meshes.** As decided, behind `mesh_chunk` and `chunk_keys`. Keys are mesh
  units' minimum corners in eighths of a metre; the renderer turns them into
  positions with `FINE_UNITS` and sizes them with `MESH_UNIT_SIZE`, and the
  batch ids above x = 1,000,000 are gone. Coverage is exact integer arithmetic
  in 64ths of a metre for full and shaped leaves: a face is covered when, over
  each neighbour's footprint, the neighbour is full, or its faces on the
  shared plane (one triangle, or triangles that tile their convex hull) hold
  the face's part there. Pieces are judged conservatively: covered by full
  neighbours, by pieces of their own source where they are cut faces, by
  empty neighbours their source lies outside, or by any piece whose source
  fills the slab beyond. Most faces are decided with one descent to the leaf
  beyond. **Unit size:** 16 m. On the_cube1m, units of 16, 32 and 64 m give
  749, 335 and 218 units against the 2,392 batches they replace, so the
  smallest size allowed is the nearest; 383 of the 749 hold triangles.
- **Size accounting.** `World::piece_changes` lists the format 3 pieces a
  change removed and added, from every leaf it split, merged or replaced, and
  the server adds their pieces and JSON bytes, each cell by its own JSON. A
  room starts from its world's written pieces and bytes and refuses to open a
  world whose next save would not load again. An edit that adds nothing is
  never refused for size. `snapshot_geometry_bytes` counts cells by their own
  JSON too, so `load_world` and the room agree. The room also checks the
  tree's exact leaf count against the world's guard (after review, below).
- **Other changes.** `/health` has a `world` object: leaves by kind, internal
  nodes, and bytes by part. The server builds a saved world once at startup
  instead of twice. A missing world file is the arena's format 3 parts,
  regions included. The client applies a transaction patch with
  `World::apply_patch`: the boxes, then the metres, as the server applied the
  edit, on a copy kept only if both apply, so a refused patch changes nothing
  and reconnects as other bad patches do. The importer writes the regions it
  reads, with no cells or roots, as before.
- **Choices where the order left room:**
  1. **Support beside walls (the foundry drift).** On the tree, foundry under
     the Sauerbraten profile fell beside the plinth at x = 20 with its body
     2.9e-5 m inside the plinth's side, which the box test allows (it clips the
     body to a full leaf, shrinks it by 1e-5 m on each side and needs more than
     1e-5 m of what is left: 3e-5 m in all). The support query, like the old
     region path, counted any leaf the footprint touched, so at tick 379 the
     player landed on the face between the plinth's two materials at y = 4,
     inside the wall, while the recording kept falling, 2.3 cm lower at that
     tick. The old cell path ignored overlaps under 1e-5 m. The fix: support and
     ceiling queries skip a leaf the footprint overlaps by no more than 3e-5 m,
     the box test's own tolerance (`SIDE_CONTACT` in `query.rs`). Foundry then
     stays within 2.1e-5 m of its recording, and no Arena scenario moved.
  2. **Stored forms.** `get()`, `cells()`, `root_fragments` and patches give a
     planar face's diagonal its fixed value (session 1's choice 2), and a cell
     whose six faces all name a material comes back with its first face's
     material as its fallback, since the tree keeps the six only. Tests that
     compared cells compare them as a world stores them.
  3. **Saved files** are the tree's writer output: leaves as regions and cells,
     no roots. Patches carry each metre's content from `root_fragments`.
  4. **Thin detail costs leaves.** A fragment a quarter metre wide and a metre
     tall is eight leaves, where format 3 counted one piece. The server's
     largest test blueprint (16,384 roots of four strips two eighths wide)
     would be a million leaves and is refused by the piece guard, so it now
     uses four half-metre columns per metre: still 65,536 request fragments,
     131,072 leaves. The crossing-strip test of the face-contact limit uses
     whole-metre strips for the same reason.
  5. **Tests of removed internals** assert content and digests: region counts
     after painting, `cells()` and `detailed_roots()` against files that
     omitted regions, override counts and empty mask roots, compiled triangle
     counts, and core's `World` JSON. Session 1's comparisons with today's
     `World` are gone with it; random worlds now check against the tree's own
     leaves, the old loader's limits are checked by a test helper that
     restates them, and the solid-angle containment stays as a test oracle.
- **After review** (design lead, 2026-11-14), three fixes:
  1. **The world's guard counts every leaf.** Only cube-material writes
     counted leaves before, so geometry written inside a large water box could
     take a world past the guard unseen: the water leaves it splits are not
     format 3 pieces. The server now refuses an edit when the prepared world's
     exact `stats().leaves()` is over `MAX_WORLD_PIECES` and above the room's
     current count. An edit that adds no leaves is never refused, format 3's
     piece and byte checks stay as they were, and edits' cube-material writes
     still count before building. Loading never refuses for the guard:
     `replace_volumes` counts against the conversion guard
     (`MAX_CONVERSION_LEAVES`) as a memory limit, and every path that builds a
     world from a file or a snapshot goes through it: the server's
     `build_world`, the client's Welcome, the capture tool's loads, the
     importer's check of its output, `measure_join` and the test clients. A
     client applies a transaction patch with `apply_patch`, the boxes then the
     metres, as the server applied the edit, within the same memory limit, so
     it never refuses a patch for the guard.
  2. **Every write rebuilds the units it changed.** `World::changed_chunks`
     gives the units to build again from the difference between two trees,
     skipping shared subtrees, whatever wrote the change; `apply_root_edits`
     and `apply_patch` return the same. The client so rebuilds the units a
     patch's water and glass split or merged. The rule also rebuilds units
     beside a removed or added shaped leaf or piece: a neighbour's coverage
     by one depends on its kind and box, so splitting a shaped leaf into
     pieces with the same surface used to leave a neighbour's face out until
     that unit was built again (fewer faces than a fresh build, never a
     hole). The client writes boxes only from patches; the capture tool's
     edit storms write none.
  3. **The reload root keeps pieces whole.** Loading anchors the root on the
     largest region whose box is a power-of-two cube on the largest whole
     region's grid, pieces included, instead of on the largest whole region.
     The tree writes every leaf as such a region, so a world whose largest
     whole region was cut, leaving pieces larger than any whole region left,
     keeps its pieces on reload: 35 pieces and 4,315 bytes in the test below,
     where the old rule made 44 and 5,506. Clipped regions off that grid are
     ignored, so old saves behave as before; no private world has a clipped
     region, so none roots differently. The root can still move at a reload,
     keeping that region's grid, until shared-sandbox 2e stores it.
  - **Pieces without caps.** The mesher counts the pieces it draws without
    caps because a cap could not be built (`cap_fallbacks`, per thread, for
    tests). It has not happened: 0 of the 19,095 and 5,043 pieces meshed by
    the two random dirty-unit tests, which now assert it; 0 in the private
    worlds as loaded, which hold no pieces; and 0 of 582,224 pieces cut from
    the eight children of every shaped leaf of the_cube1m, complex and
    ralith1.
  - **Choices.** The server's undo and redo replace boxes through
    `replace_volumes`, so they count against the conversion guard and then
    meet the exact check, which refuses an undo that adds leaves past the
    guard. A world without whole regions keeps the old root, at its bounds'
    minimum corner, since clipped regions alone give no grid to trust.
  - **Tests:** a cell inside water that would pass the guard is refused, one
    that removes leaves is accepted, and a world saved just under the guard
    reopens with the same digest; a world saved 1,000 leaves over the guard
    opens on the server and through a Welcome with the same digest, refuses
    an edit that adds leaves and accepts one that removes them; near the
    guard, a transaction whose cell alone would pass it, a refused one and
    the undo of the first, whose boxes alone pass it, each leave the client's
    world equal to the server's; the dirty-unit test mixes box edits,
    `replace_volumes` and patches with root writes over ten worlds holding 69
    leaves of 32 or 64 m, 68 steps that change the tree, with incremental
    meshes equal to a fresh build after each; water that splits a shaped leaf
    into pieces rebuilds the unit beside it; and a cut 64 m slab's pieces and
    bytes don't grow on reload. The last two and the three server tests fail
    without their fixes.
- **nub1 opens** (design lead, 2026-11-14, M5 Pro, release builds of the
  merged branch). The stock importer converts a copy of nub1 in 0.56 s: 434,233
  regions, 1,552 boxes, 155 lights, a 62 MB file. The standalone server opens
  it in 0.34–0.45 s with 177 MiB resident: 511,155 leaves (374,236 full,
  37,128 shaped, 99,791 water or glass), 99,863 internal nodes and 16.2 MB. A
  headless client joins in 0.56 s, reading 53.1 MB, with a peak footprint of
  128 MB. The capture tool loads it in 0.6 s and draws 755,996 world
  triangles, against 5,140,198 before hidden faces were left out. Nobody has
  looked at it in the app yet.
- **Checks,** all passing, and again after the review's fixes: `cargo fmt
  --all`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo test --workspace --locked`, the private tests in release (`cargo test
  --release -p sauerteig-core --lib private_ --locked -- --ignored
  --test-threads 1`) and the GPU tests (`cargo test -p sauerteig-client --lib
  gpu_ --locked -- --ignored --test-threads=1`). In the sun cascade test one
  block now redraws 46,762 of 3,145,728 texels, against 16,124 with the base
  commit's build: the fingerprint's cells span a quarter of a 16 m unit instead
  of an 8 m chunk, and the faces beside the block change too. New tests: World
  edits and their undo restore identical trees over 40 random worlds (12 writes
  each, root and water edits mixed, undone in reverse); rewriting, or carving
  and restoring, a metre inside a two-metre slab keeps its shaped leaf; only
  dirty units' meshes change after random edits, over 25 random worlds; hidden
  faces are left out and a partly covered face is drawn whole; faces on the
  root's floor are left out; wedges meeting side to side cover each other; cut
  faces between pieces of one carved ramp are left out; a leaf four units wide
  is drawn by the unit at its corner; `piece_changes` is the difference of the
  written parts; boxes are written only where they differ; a thin pane is
  refused before it is built; the variants act.
- **Goldens,** re-recorded once, both files, under platforms stage 2's rule,
  after checking every tick against the old recordings with the support rule
  in place. Arena: `stairs` and `ramps` 85 of 230 ticks changed, `shape_ramps`
  91, all at most 1.47e-4 m; `wall_slide` 63 of 170, at most 2.3e-5 m;
  `compact_world` 65 grounded flags of 273 ticks, positions and velocities
  identical (the closed support interval). Sauerbraten: `shape_ramps` 3 of 230
  ticks (2.4e-7 m), `wall_slide` 154 of 170 (3.9e-5 m), `foundry` 22 of 390
  (2.1e-5 m, the contact difference above) and `compact_world` 118 grounded
  flags. Every other scenario stays bit for bit, and release and dev builds
  give the same bits. The tests that ran movement on a converted tree beside
  `World` went, since `World` is the tree.
- **Query replay** (`private_query_recordings_replay`, against `World` loaded
  as the server loads it), queries agreeing / differing, and why:

  | Queries | the_cube1m | complex | ralith1 |
  | --- | --- | --- | --- |
  | Box overlap, solid and glass | 5,000 / 0 | 5,000 / 0 | 5,000 / 0 |
  | Point material | 4,900 / 100 | 5,000 / 0 | 4,699 / 301 |
  | Support | 2,209 / 1 | 2,324 / 0 | 1,596 / 0 |
  | Ceilings | 1,033 / 0 | 613 / 0 | 459 / 0 |
  | Rays, first hit | 4,995 / 5 | 4,999 / 1 | 4,982 / 18 |
  | Water and glass rays | 10,000 / 0 | none | 10,000 / 0 |

  Point material differs only where the old code reported water or glass
  inside solid; the support difference is a face between two old regions
  inside a leaf the tree merged, which the body only touches; of the rays,
  22 are hits the old `f32` samples rounded onto the face (lost faces or
  joins taken for faces) and 2 old hits lie between solids. These are session
  1's classes and counts; the test now fails on any other difference.
- **Measurements,** M5 Pro, release builds:
  - **Standalone server on the_cube1m** (start to first `/health` answer,
    resident memory after loading, three runs each): before 2.51–2.64 s and
    980 MiB; after 0.36–0.47 s and 134–135 MiB. `/health` reports 339,113
    leaves (167,562 full, 58,879 shaped, 112,672 water or glass), 80,332
    internal nodes and 14.5 MB.
  - **Leaves, nodes and bytes** are session 1's table; conversion is
    unchanged. the_cube1m's counted size is 226,441 pieces and 29.48 MiB with
    cells counted by their own JSON (34.73 MiB counted as roots).
  - **Meshes:** the_cube1m from 2,392 batches and 3,595,924 triangles (the
    capture tool's count; the audit counted 3,595,084 another way) to 749
    units, 383 of them drawing, and 717,378 triangles, 80% fewer. complex from
    297 batches and 450,596 triangles to 134 units (100 drawing) and 79,469;
    ralith1 from 328 batches and 500,676 triangles to 1,057 units (603
    drawing) and 74,256. The arena from 68 chunk meshes and 28,158 triangles
    to 16 units and 23,220. Building every unit of the_cube1m takes about
    330 ms on one thread.
  - **Client join on the_cube1m** (`measure_join`, connect to world built,
    three runs, alternating): before 1.29–1.31 s, 44.9 MB read and a peak
    memory footprint of 1,016 MB; after 0.47–0.48 s, 30.9 MB read and 83 MB.
    The capture tool's load (file to meshes on the GPU): 1,539–1,655 ms before,
    663–676 ms after.
  - **Frame times** on the imported poses at 3840 × 2160, three rounds
    alternating the two builds (medians of the rounds; serial frame median,
    then GPU throughput in ms a frame):

    | Pose | Frame before | Frame after | Throughput before | Throughput after |
    | --- | ---: | ---: | ---: | ---: |
    | imported-spawn | 9.08 | 8.33 | 5.36 | 2.86 |
    | imported-overlook | 12.65 | 10.89 | 8.63 | 6.82 |
    | imported-water | 9.60 | 8.05 | 5.93 | 4.94 |
    | imported-tower | 13.91 | 14.55 | 11.40 | 10.83 |
    | imported-tower-rooms | 12.64 | 10.99 | 9.59 | 8.76 |
    | imported-flight | 10.35 | 8.47 | 6.30 | 3.12 |
    | imported-teleports | 13.90 | 12.33 | 11.40 | 10.83 |
    | imported-storm | 21.08 | 14.66 | 18.27 | 12.39 |

    Throughput varied by at most 0.6 ms between rounds. Serial frame times
    include a fence wait and are noisier; imported-tower's serial median rose
    while its throughput fell.
  - **Frame times on the built-in poses** (the design lead's measurement with
    the same two builds): 17 poses, every pose that isn't imported except the
    storms, waterglass-low-bare, lightshow-dynamic-moved,
    materials-underwater and lightroom-actors-4, in three alternating rounds
    at 3840 × 2160 with `--throughput 120 --runs 5`. Throughput changed by
    −1.0% (arena-yard) to +4.5% (corridor-wall, 6.47 → 6.76 ms), median
    +1.4%; lightshow-dynamic 4.44 → 4.50 ms, lightroom-center 9.42 → 9.69 ms.
    Small scenes now fit in one or a few units, so nearly every triangle
    counts as visible (lamphall-actor 4,940 → 7,344 of 7,344). The serial
    rise of up to 1.5 ms that one unalternated run of the full capture showed
    (lightshow-dynamic) was not a GPU cost.

- **Captures.** All 30 poses of `scripts/capture-poses.json` at 3840 × 2160,
  baseline from the base commit's build taken before any change. Share of
  pixels whose largest channel differs by more than 16/255: 0.055% for
  arena-yard, at most 0.001% for every other pose; 0.268% of arena-yard's
  pixels differ by more than 2/255, at most 0.03% elsewhere (imported-overlook,
  scattered edge and shadow-texel noise). arena-yard's eye, at [16, 9, 19],
  sits on top of the east bay's roof (`fill([10, 8, -24], [24, 9, 24])`);
  below y = 9 it is inside that roof. The lintel over the yard's portal,
  [10, 8, -4] to [13, 10, 4], rises a metre above the roof, so its side faces
  are partly covered and drawn whole, and only a camera on or inside the roof
  sees the covered part, as in Sauerbraten. The design lead's captures at
  1920 × 1080 show it: with the eye at y = 8.5, 8.95 and 9 the builds differ
  (0.049–0.053% of pixels by more than 16/255), and at y = 9.05 they match
  exactly. This is the decided rule; nothing else differs visibly.
- **Not verified:** anything in the running app, including joins, edits,
  undo, save and reopen, water painting and glass edge bands; nub1 (not in
  this machine's library); Windows and Linux.
- **Left for later:** the slab gap in the canonical form (O2's remip); an edit
  whose unchanged metres split a union-of-eighths shape without carrying its
  source still leaves that shape split (geometry identical, digest not);
  pieces' coverage is conservative, so some covered cut faces of pieces of
  different sources are drawn; support and ceiling queries skip a leaf the
  body overlaps by up to 3e-5 m on a side, while the box test treats a shaped
  leaf as touching only above 2e-5 m, so a body can't rest in that band; the
  root can still move at a reload until shared-sandbox 2e stores it; the
  mesher runs on one thread.

## O1b: water and glass from the tree in the renderer

State: merged on 2026-11-14. The checks in the app await the maintainer.
Renderer N2 rebases onto it, since both edit `volume.wgsl` and `render.rs`.

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
  - [x] Captures of materials-water, materials-underwater, materials-glass,
    imported-water, waterglass-view and waterglass-low match today's, apart
    from glass edge bands, which now follow surfaces; the record shows before
    and after. Session record, below.
  - [x] CPU frame time for waterglass-low against waterglass-low-bare, and for
    imported-water, is no worse than today, and neither is GPU throughput.
    Within the noise of this machine, which another session's GPU captures
    disturbed for part of the run; the record gives the numbers.
  - [ ] In the app, painting water over a large area and undoing it re-meshes
    only the touched units, as F3 shows. Covered by tests (below); not looked
    at in the running app.

### O1b session record (2026-11-14)

Code, automated checks and measurements, on the M5 Pro. Nothing here was
observed in the running app, so the box above stays open.

- **What changed.**
  - **Core.** New `octree/transparent.rs` meshes a unit's water and glass
    (`World::transparent_keys`, `World::transparent_chunk`). A leaf that is
    not solid all through draws a face of its box where the cube beyond has
    another medium and solid geometry does not cover it. Faces against the same
    medium or fully against solid are left out, a leaf's own solid filling a
    face leaves it out, a water-glass interface keeps both faces, and faces on
    the root's boundary are drawn, as the old boxes drew them. Within a unit,
    coplanar faces of one medium merge into rectangles by joining whole edges.
    `TransparentVertex` is the format: position, normal, `uv` and `size` of the
    quad, material, and `edges`, four flags for the quad's edges where its
    surface ends. A neighbour query is one descent to the leaf beyond for most
    faces. `octree/diff.rs` also lists leaves with water or glass that were
    removed, added or changed, and the boxes where solid shape (not surface
    materials) differs. `World::mesh_changes(before)` gives `MeshChanges`:
    `opaque` units (what `changed_chunks` gives), `transparent` units, and
    `shadows`, boxes in eighths around the changed solid shapes. `apply_patch`
    returns it, and `apply_root_edits_changes` and `apply_edits_changes` are the
    same for the other writes; `apply_root_edits` and `changed_chunks` keep
    their signatures.
  - **Renderer.** `render/volumes.rs` holds one pool of quad slots with a
    free-run allocator, a mirror of the vertices on the CPU and one GPU buffer
    that grows by doubling. Each unit keeps its slots and a hierarchy over its
    quads' bounds; rebuilding a unit frees its slots, takes new ones and
    writes only that range. Each frame the units in view are found, their
    visible quads are sorted back to front together, and one indexed draw
    follows. The old box-boundary code (`boundary_vertices`, the contact
    rectangles) and `rebuild_volumes` are gone, and the renderer no longer
    calls `volumes()`. `Renderer::rebuild` takes a `MeshChanges`;
    `Renderer::remeshed` counts the units the last rebuild made again, F3
    shows them next to the triangle count, and the capture report gives
    `transparent_units`, `transparent_quads` and `transparent_mesh_ms`.
    `volume.wgsl`: a flat `edges` input, and the edge band measures only the
    flagged edges. Nothing in its lighting loop changed.
  - **Docs.** `crates/core/CONTRACT.md` and the development guide.
- **Choice: where shadows refresh.** Core reports the boxes where solid shape
  changed (`shadows`), and the renderer redraws cached shadows only there,
  instead of comparing the meshes it rebuilt. Why: when a water edit splits a
  large solid leaf, its faces move to the units that now own the pieces, and
  one 64 m quad becomes many, so the rebuilt meshes and their fingerprints
  differ everywhere the leaf was, though no surface moved. The tree diff
  already tells shape from owner exactly (a leaf that splits into leaves of
  the same shape compares equal), costs nothing more, and puts no guesswork in
  the renderer. The units are still rebuilt, since their faces moved. The
  fingerprint stays, to narrow a redraw inside a box, and it runs for every
  rebuilt unit so that its stored prints stay current. A change of surface
  materials alone also leaves shadows as they are, which paint did not before
  for the point lights. Boxes are joined per owning unit and grown by one eighth
  of a metre, to cover faces that a change covers or uncovers beside it.
- **Choice: glass bands against solid.** An edge of a glass surface carries no
  band where the surface goes on under solid geometry, so a pane set into a
  wall (the_cube1m's windows) has none where it enters the wall, as the old
  boxes, whose edges lay hidden inside the wall, showed none. A pane resting
  on a floor still has one at the floor. A partial continuation flags the
  whole edge.
- **Tests.** Core: water draws once, merged and not against solid; a
  water-glass interface keeps both faces and every quad faces outward; edge
  flags follow the surface and not the merge's seams, none across a unit
  boundary, none where a pane enters a wall; a shaped or solid leaf with water;
  random worlds with random root writes, box writes and patches, where only the
  transparent units named differ (25 worlds, 8 steps each, at least 100 that
  change the tree); a water edit over a 64 m solid leaf rebuilds its pieces'
  units, lists no shadow box, keeps the opaque surface area exactly, and so
  does its undo; paint lists no shadow box and a carve lists a box around the
  metre; the private test `private_water_and_glass_meshes` checks incremental
  against fresh meshes for a 64 m water edit on the private worlds. Client:
  the slot allocator reuses and joins runs and never overlaps under random
  use; quads sort back to front; a quad is 48 bytes; and the GPU test
  `gpu_water_and_glass_edits_redraw_no_shadows_and_match_fresh_renders`: with
  `materials`, a glass pane and water through part of the floor redraw 0
  sun texels and 0 shadow maps, the picture equals a fresh renderer's, and the
  undo returns the first picture exactly. With the gating switched off, that
  test fails with 152,290 sun texels redrawn. The sun cascade test's one-block
  edit still redraws 46,762 of 3,145,728 texels, as after O1a.
- **Checks,** all passing: `cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace
  --locked`, the private tests in release (4 passed, including the new one) and
  the GPU tests (9 passed, including the new one).
- **Captures.** All 30 poses of `scripts/capture-poses.json` at 3840 × 2160,
  baseline from the build at `208f63f` taken before any change. Share of
  pixels whose largest channel differs by more than 16/255 and by more than
  2/255:

  | Pose | > 16/255 | > 2/255 |
  | --- | ---: | ---: |
  | materials-water, materials-glass | 0 | 0 |
  | materials-underwater | 0.005% | 0.059% |
  | imported-water | 0 | 0.015% |
  | waterglass-view | 0.001% | 0.665% |
  | waterglass-low | 0 | 3.818% |
  | imported-spawn, arena, lightroom, corridor, lightshow, shadowtest and lamphall poses | 0 | 0 |
  | imported-overlook | 1.054% | 4.840% |
  | imported-tower, imported-storm | 0.134%, 0.135% | 2.112%, 2.110% |
  | imported-flight | 0.050% | 2.755% |
  | imported-teleports, imported-tower-rooms | 0.005% | 0.054% |

  materials-underwater's difference is run-to-run noise: the base build
  compared with a second run of itself gives 0.005% and 0.059%. Expected
  differences: glass edge bands now follow surfaces, so the faint seam lines
  across the_cube1m's glass, which showed where authored boxes met, are gone
  (imported-overlook, tower, storm, flight); window glass has no band where it
  enters the wall; and waterglass-view and -low differ faintly (0.001% of
  waterglass-view above 16/255, none in -low), along the water box and the
  glass walls, which I did not trace further. Nothing else changed. A first
  version of the edge rule drew bands at every wall contact; its captures
  differed more (imported-flight 1.29%, imported-tower 1.06% above 16/255),
  which is why the continuation under solid exists.
- **Measurements** (M5 Pro, release builds of `208f63f` and this branch's
  working tree, alternating base and branch five rounds at 3840 × 2160,
  `--frames 60 --throughput 120 --runs 5`; GPU throughput in ms a frame: the
  best run of rounds 3 to 5, and the median of those rounds' medians). The
  machine was shared. The renderer N2 session ran GPU captures during rounds 1
  and 2 and at the start of round 3, and other work ran throughout, so rounds
  1 and 2 are discarded and a median of three takes out the one round in which
  the materials poses ran into it (the base build's 6.3, 9.7 and 6.1 ms).

  | Pose | Base best | Branch best | Base median | Branch median |
  | --- | ---: | ---: | ---: | ---: |
  | materials-water | 2.52 | 2.51 | 2.54 | 2.53 |
  | materials-underwater | 3.93 | 3.93 | 4.04 | 3.96 |
  | materials-glass | 3.02 | 3.02 | 3.13 | 3.08 |
  | imported-water | 5.13 | 5.02 | 5.18 | 5.07 |
  | waterglass-view | 4.72 | 4.75 | 4.77 | 4.81 |
  | waterglass-low | 8.26 | 8.35 | 8.33 | 8.38 |
  | waterglass-low-bare | 4.16 | 4.15 | 4.23 | 4.22 |

  GPU throughput is the same within 1%. waterglass-low is 1% slower on the best
  run and 0.6% on the median, and one of its branch rounds read 8.69: the
  branch draws 94 quads where the boxes drew 60, since leaves that the tree
  splits unevenly merge into more rectangles than the boxes' six faces, and
  visible triangles rise from 17,266 to 17,328. CPU frame time (submit,
  median of each round, rounds 3 to 5): waterglass-low 0.36–0.38 ms on the base
  and 0.40–0.41 on the branch; waterglass-low-bare, whose code path did not
  change, 0.32–0.38 and 0.34–0.39, so a difference of 0.03 to 0.04 ms is inside
  this machine's noise. imported-water: 0.70–1.27 ms on the base and 0.64–1.20
  on the branch. A longer rerun of waterglass-low and its bare twin (300
  frames, six rounds) was disturbed by GPU contention (throughput 10 to 16 ms
  on both builds) and settles nothing more; the criteria are met within noise,
  and a rerun on an idle machine would tighten the waterglass-low pair.
- **Transparent quads and re-mesh times, before and after** (release):

  | World | Boxes | Quads before | Time before | Units now | Quads now | Time now |
  | --- | ---: | ---: | ---: | ---: | ---: | ---: |
  | materials | 2 | 12 | 0.1 ms | 2 | 10 | 0.5 ms |
  | waterglass | 10 | 60 | 0.1 ms | 12 | 94 | 22 ms |
  | the_cube1m | 4,227 | 19,430 | 6.8 ms | 109 | 8,180 | 101 ms |

  "Before" is the old box-boundary code run on the world's box list; "now"
  meshes every unit from the tree in one thread, which for the_cube1m means
  112,672 water and glass leaves. A load that meshes the units therefore costs
  about 95 ms more on the_cube1m and 22 ms more on waterglass (thousands of
  eighth-thick glass leaves); ralith1 takes 12 ms for 866 quads. The old path
  rebuilt every box after any water or glass edit. Now a 64 m cube of water
  through the middle of the_cube1m names 67 water and glass units and 9 opaque
  units; the diff takes 31 ms, the opaque units mesh in 23 ms and the water and
  glass units in 32 ms (complex: 36 water and glass and 32 opaque units,
  ralith1: 83 and 47). No such edit lists a shadow box, and undoing it names
  the same units back.
- **Observed behaviour:** none in the running app. The capture images, and the
  GPU test with a settled renderer, show what the six acceptance poses draw.
- **Remaining gaps:** the checks in the app (painting water over a large area
  and undoing it, with F3 open; glass in the_cube1m's windows); the mesher
  runs on one thread, so a load pays the figures above; greedy merging gives
  more rectangles than boxes did on irregular leaves; a partly continued edge
  flags the whole edge; the quads' centroid sort is as before, with the same
  ordering artifacts for overlapping large faces; lighting in `volume.wgsl`
  stays at N2's hands, so water and glass still use at most 8 lights until it
  merges.

## O2: grid-sized edits

State: waits for [lighting](lighting.md) L1, which changes light objects, since
O2 reworks how objects travel in edits (design lead, 2026-11-14). Wave 2. Large:
two sessions in order on one branch, merged once. The design lead commits each session's work to the
branch. Between the sessions the branch builds core and the server; the client
builds again after session 2.

- **Decided:** Edits, above.
- **Session 1, core, server and protocol:**
  - Writes with their parts, conflicts and undo by comparing recent roots,
    one edit request and one patch, and objects as a map.
  - No size limit per edit, as decided: the world guard is checked before an
    edit is applied, undo history stays within a share of memory, and rate
    limits count requests.
  - **Loading files the tree wrote.** A format 3 file with both regions and
    cells takes the loader's override path: after the regions are built, each
    cell is written into the finished tree on its own, clearing and inserting
    its metre with two copy-on-write passes from the root (`from_parts` in
    `octree/format3.rs`). The tree's own writer emits one-metre leaves as
    cells, so every world the server or the importer has written since O1a
    loads this way: the_cube1m's re-import, 172,226 regions and 54,257 cells,
    takes about twice as long to open as the older file of 306,094 regions
    (0.89–1.85 s against 0.41–0.84 s, alternating, on a busy M5 Pro,
    2026-11-14), though it uses 105 MiB against 135. Make files whose cells no
    region covers load in one pass, as regions-only files do, and keep older
    files' rule that cells and roots replace the regions in their metre.
    Record both loads at idle before and after.
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

State: merged on 2026-11-14. The library holds the re-imports since the same
day; the maintainer's look at them remains.

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
  - **Done 2026-11-14.** The design lead re-imported all 22 with main at
    `afc0358`, the defaults plus `--skip-unsupported` for the four recovered
    copies; all 22 equal their library copies reflected. At the maintainer's
    word the design lead swapped them in, keeping IDs and names, with the old
    files backed up outside the repository. The server opens the swapped
    the_cube1m, nub and complex.
- **Acceptance:**
  - [x] Every library map, re-imported, matches its current copy reflected:
    the loaded trees' digests are equal once the current copy is reflected.
    Checked on copies in the worktree, all 22 library worlds, with the options
    the record lists (the library itself untouched).
  - [ ] **Maintainer:** a few re-imported maps look the right way round, as
    the maintainer remembers them from Sauerbraten.
  - [x] nub1 imports with the stock importer (0.37 s, 152 MiB).
- **Record:** import times and memory, leaf counts, inert-material counts, and
  the orientation check. See the session record below.

### O3 session record (2026-11-14)

Code, automated checks and measurements, on the M5 Pro with release builds.
Nothing was opened in the Sauerteig app. What did run: the capture tool on a
re-imported world (the orientation check), and the standalone server on
re-imports of nub1, the_cube1m and complex.

- **What changed.** All in `crates/importer`:
  - `convert.rs` builds the octree through O1a's `OctreeBuilder` and writes it
    as format 3 (`Octree::to_format3`), so the box path and `World`
    validation are gone. `convert_to_tree` also returns the tree.
  - `transform.rs` reflects; `lib.rs` reads level-of-detail nodes' materials
    into a new `lod_nodes` count in the report; `main.rs` prints the tree's
    leaves.
  - New `reflect.rs` (`reflect_x`, a tree's mirror image across X = 0) and
    `examples/compare_reflected.rs`, which compares a re-import with an older
    copy reflected, for the library swap. A private test
    (`private_reimports_equal_the_library_copies_reflected`, in the importer's
    `private_` tests, `-- --ignored`) does it for complex and ralith1 from
    `.local/originals/` and `.local/library/`.
  - `crates/importer/README.md`'s conversion sections and
    [legacy-import](legacy-import.md).
- **The fix: reflect X.** Sauerbraten's world is Z-up and left-handed, so
  `(x, y, z) -> (x, z, -y)`, a rotation, mirrored every map. It is now
  `(x, y, z) -> (-x, z, -y)`: up stays up, source north (+y) stays
  Sauerteig's -Z, and only east and west trade places. Any other reflection
  would be as correct; this one keeps the old placement on two axes and makes
  a later spawn-heading import a plain turn. Changes: `ImportTransform`
  (cube bounds, positions, the crop region, bounds), the edge-to-corner
  mapping (the lattice slot for each source corner), the face order
  (Sauerteig's -X, +X, -Y, +Y, -Z, +Z take source faces +x, -x, -z, +z, +y,
  -y) and the diagonals, which are chosen in the source's frame by the
  original rule (`SOURCE_FACES`) and mapped to slots. A test mirrors the old
  mapping, which it restates, and compares 20,000 random cubes' vertices and
  diagonals with the new mapping. The tests that pinned the old mapping
  (water and glass boxes' places, bounds, the ramp's raycasts, the face
  slots, entity bounds, crop volumes) now pin the new one.
- **Orientation check.** The archive holds `complex.png`, the screenshot the
  original game saved with the map, so it is the engine's own picture and has
  the right handedness. From a pose I found by eye in the imported complex
  (eye [-19, 28.4, -4], looking at [-28, 29.2, 12]), the layout matches it:
  a window opening at the left of the back wall, a stair with railings
  left of centre, and the loading-bay docks along the wall on the right. The
  library's old copy of complex, viewed from the same pose mirrored across X,
  gives the exact mirror image of that picture, with the docks on the left,
  so the old copy was the mirror image and the new import is not. The map's
  `sign_loadingbay` mapmodel (source [760, 424, 563], now [-31, 30.4, 11]) is
  at the dock wall. Limits: the screenshot's own pose isn't known, so this
  compares layout, not pixels; imported surfaces are neutral, models aren't
  imported and the sign's text isn't geometry. My reasoning that the engine's
  axes are left-handed (a comment in Sauerbraten's `rendergl.cpp`, as I
  remember it) wasn't checked against the source here; the screenshot is the
  evidence. The pictures are `.local/o3/cap/b2.png` (new) and
  `old-b2-mirrored-pose.png` in the worktree, not tracked.
- **The root.** The root is the smallest power-of-two cube holding the bounds
  among three candidates: one on the source cube's own grid at the root's
  size; the source cube's ancestors, placed as the bounds need; and, when
  bounds reach past the source cube on both sides of a grid line (a centre
  off whole metres), a smaller cube whose origin steps down on the grid of the
  largest leaf. A whole import at its default centre has the source cube as
  its root. Leaves off the root's grid or larger than it (a crop) become the
  part of themselves inside each root node, a shaped one as a `Piece` of its
  source. An entity that would take the bounds past 8,192 m, or the root past
  8,192 m where the map itself fits, is left out and listed in the report's
  `entities_omitted_beyond_world` (none of the 23 maps has one; ralith and
  ralith1 each still list their one orphan teleport destination under I1's
  `entities_omitted_out_of_range`).
- **Crop.** A crop's edges are whole metres, and the tree cuts a big leaf only
  where its grid allows, so an edge off the one-metre grid costs leaves down to
  an eighth of a metre: nub1 cropped around its spawn made 713,983 regions
  (past the 64 MiB file limit; the old importer made 42,132). So a crop moves
  its centre to the nearest whole metre, at most half a metre, and says so in
  the report's notes (`center_source_xyz` gives the centre used). With it nub1
  cropped around its spawn is 75,003 leaves and 5.8 MB, the_cube1m 32,524
  leaves (old: 8,319 regions) and complex 45,464 leaves, 37,586 written (old: 37,188 regions). This
  moves today's placement for a cropped import with a centre off the metre
  grid; whole imports are unaffected. Decide if you'd rather keep the exact
  centre and refuse such crops.
- **Materials.** Every leaf keeps its full 16-bit value in the tree, including
  empty leaves and leaves inside solid geometry; the file keeps water and
  glass as boxes from the tree's leaves, without variants or flags. A skipped
  unsupported leaf in water keeps its water. The report's counts are as they
  were (`unsupported_volume_material_leaves`, `material_flags_not_imported`,
  `volume_variants_not_imported`, `unmapped_texture_slot_faces`, and the
  inventory's raw slots). A zero-volume shape is still refused or, with
  `--skip-unsupported`, omitted and listed: 23 sheets in each of testcube,
  the_cube, the_cube1m and thecubesunlit, none elsewhere.
- **Level-of-detail nodes.** The reader takes a node's children and ignores
  its own shape. Their own material does differ from what is below them: of
  83,679 nodes in the archive, 1,932 carry a material of their own, and 23,004
  have some leaf below with another material than theirs (counts per map in the
  table below). In the first eight examples each report keeps, the node is
  air over water, glass or lava (cube4, nub1, ralith, ralith1, tubes), or
  water over a mix of air and water (tubes6). The importer follows the leaves,
  as the game's own material lookups do: they descend to the leaf at a point
  (`lookupmaterial` in Sauerbraten's source, from memory, not checked here).
  Nothing in the maps' look has been compared.
- **Triangular faces' diagonals.** The tree treats a triangular face's diagonal as
  meaningful, though both choices draw the same triangle (the canonical form's
  known gap, with O2's remip). Old imports lost the side diagonals of
  height-field leaves through `Cell`, so the importer gives a shape whose top
  is a height field the diagonals `Cell::as_shape` gives, mirrored. That keeps
  digests equal to the old copies reflected and can go when the canonical
  form normalizes triangular faces.
- **Validation.** The importer no longer loads its own file again, which cost
  300 ms on megacube. It checks the boxes against the format's limits, and the
  builder checks the tree. The comparison tool and the tests load the files.
  In review the design lead added a check that the written pieces stay within
  the server's 786,432-piece limit, which the old 400,000-region cap used to
  imply, so the importer still refuses a world the server wouldn't open.
- **Library acceptance,** on worktree copies of the originals and of each
  library world (deleted afterwards), re-imported with the stock options (the
  four recovered copies with `--skip-unsupported`; ralith, ralith1 and ralith2
  read as OCTA 27 directly), loaded as the server loads them, the old copy
  reflected onto the new one's root: **all 22 library maps equal in digest,
  objects (bit for bit, X negated) and bounds**: complex, cube4, megacube,
  megacube1, megacube2, megacube3, nub, ralith, ralith1, ralith2, temp,
  test_weapon, testcube (recovered), the_cube (recovered), the_cube1m
  (recovered), thecubesunlit (recovered), tubes1 to tubes6. nub1 isn't in the
  library. Off-grid centres: `--center spawn` on complex, nub1 and cube4, against
  the old binary's output, are equal reflected too. The design lead redoes the
  swap with `compare_reflected`; the options the library needs are those
  above.
- **Import times and memory** (best of three, release, `/usr/bin/time -l`,
  including reading, writing the file and the report; the old importer is
  main's at `208f63f`):

  | Map | Old time, memory, file | New time, memory, file |
  | --- | --- | --- |
  | complex | 0.14 s, 23 MiB, 6.1 MB | 0.09 s, 19 MiB, 4.4 MB |
  | cube4 | 0.11 s, 53 MiB, 15.0 MB | 0.11 s, 49 MiB, 9.8 MB |
  | megacube | 0.22 s, 94 MiB, 18.8 MB | 0.17 s, 89 MiB, 11.0 MB |
  | nub | 0.43 s, 114 MiB, 42.6 MB | 0.31 s, 132 MiB, 35.1 MB |
  | nub1 | 0.48 s, 178 MiB, 62.2 MB | 0.37 s, 152 MiB, 53.5 MB |
  | ralith | 0.06 s, 66 MiB, 15.8 MB | 0.05 s, 57 MiB, 4.6 MB |
  | temp | 0.14 s, 66 MiB, 17.9 MB | 0.10 s, 59 MiB, 10.5 MB |
  | the_cube1m (recovered) | 0.57 s, 121 MiB, 44.9 MB | 0.44 s, 139 MiB, 30.5 MB |
  | tubes1 | 0.30 s, 135 MiB, 37.5 MB | 0.26 s, 120 MiB, 25.2 MB |

  The other maps are in between: no import is slower (cube4 and test_weapon
  tie), and every file is 14% to 71% smaller except test_weapon's, which grew
  by 165 bytes. Peak memory is up to 18 MiB higher on nub and the four 8 km
  Cube maps (the_cube1m 121 to 139 MiB) and lower on the rest. nub1's
  result is 434,233 source leaves, 155 lights, 1,549 water and glass boxes
  (the old importer's 1,552: merged leaves join a few boxes the other way)
  and 411,560 written geometry leaves (325,361 regions and 86,199 cells), as
  against 434,233 regions. The standalone server opens the re-imports of nub1,
  the_cube1m and complex: nub1 in 0.73 s from launch to the first `/health`
  answer, 164 MiB and 511,155 leaves (374,236 full, 37,128 shaped, 99,791
  water or glass); the_cube1m in 0.55 s, 104 MiB and 339,113 leaves, the
  numbers O1a recorded for the library's copies.
- **Leaf counts** (source leaves are the file's non-empty leaves; the tree's
  count includes leaves that hold only a material, water and glass among them,
  and merges eight equal siblings, so it can be higher or lower; the written
  count is what the file holds):

  | Map | Root (m) | Source | Tree | Full | Shaped | Pieces | Material only | Written | Boxes |
  | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
  | complex | 128 | 37,927 | 35,018 | 12,838 | 14,302 | 0 | 7,878 | 27,140 | 0 |
  | cube4 | 4,096 | 108,062 | 102,284 | 64,363 | 12,073 | 0 | 25,848 | 76,436 | 713 |
  | megacube | 8,192 | 133,946 | 200,661 | 84,876 | 18,704 | 0 | 97,081 | 103,580 | 1,153 |
  | megacube1 | 8,192 | 141,719 | 205,166 | 89,852 | 18,505 | 0 | 96,809 | 108,357 | 1,137 |
  | megacube2 | 8,192 | 150,282 | 206,604 | 92,646 | 17,722 | 0 | 96,236 | 110,368 | 1,002 |
  | megacube3 | 8,192 | 133,893 | 200,339 | 84,784 | 18,750 | 0 | 96,805 | 103,534 | 1,165 |
  | nub | 2,048 | 284,824 | 348,375 | 225,408 | 41,804 | 0 | 81,163 | 267,212 | 2,323 |
  | nub1 | 2,048 | 434,233 | 511,889 | 374,432 | 37,128 | 0 | 100,329 | 411,560 | 1,549 |
  | ralith | 2,048 | 114,753 | 61,365 | 42,158 | 866 | 0 | 18,341 | 43,024 | 234 |
  | ralith1 | 2,048 | 41,958 | 56,986 | 31,840 | 3,552 | 0 | 21,594 | 35,392 | 294 |
  | ralith2 | 8,192 | 43,937 | 71,728 | 35,702 | 4,798 | 0 | 31,228 | 40,500 | 51 |
  | temp | 8,192 | 133,691 | 151,916 | 107,362 | 10,026 | 0 | 34,528 | 117,388 | 110 |
  | test_weapon | 128 | 108 | 120 | 88 | 20 | 0 | 12 | 108 | 0 |
  | testcube | 8,192 | 303,379 | 338,709 | 167,101 | 58,982 | 0 | 112,626 | 226,083 | 4,204 |
  | the_cube | 8,192 | 306,047 | 339,457 | 167,604 | 58,879 | 0 | 112,974 | 226,483 | 4,212 |
  | the_cube1m | 8,192 | 306,117 | 339,457 | 167,604 | 58,879 | 0 | 112,974 | 226,483 | 4,212 |
  | thecubesunlit | 8,192 | 306,047 | 339,457 | 167,604 | 58,879 | 0 | 112,974 | 226,483 | 4,212 |
  | tubes1 | 4,096 | 269,084 | 272,307 | 163,416 | 29,368 | 0 | 79,523 | 192,784 | 1,709 |
  | tubes2 | 4,096 | 216,532 | 237,372 | 128,239 | 29,598 | 0 | 79,535 | 157,837 | 1,835 |
  | tubes3 | 4,096 | 210,100 | 199,939 | 125,766 | 28,047 | 0 | 46,126 | 153,813 | 1,693 |
  | tubes4 | 4,096 | 214,188 | 203,154 | 127,988 | 28,240 | 0 | 46,926 | 156,228 | 1,695 |
  | tubes5 | 4,096 | 195,679 | 185,971 | 117,038 | 26,106 | 0 | 42,827 | 143,144 | 1,576 |
  | tubes6 | 4,096 | 209,747 | 191,940 | 123,081 | 26,032 | 0 | 42,827 | 149,113 | 1,587 |

  No import has a piece: every source leaf is on the root's grid.
- **Inert materials, raw slots and level-of-detail nodes** (the report's
  counts; flag values are Sauerbraten's version 33 bits, 32, 64 and 96 for
  the clip family, 256 for death, 1,024 for alpha; lava is volume value 8, and
  no map has a variant of water or glass or glass without a clip flag):

  | Map | Lava leaves | Leaves with flags | Flag values (leaves) | Raw slots (faces, distinct) | LOD nodes, own material, differing |
  | --- | ---: | ---: | --- | --- | --- |
  | complex | 0 | 18,374 | 32: 3,318; 64: 11,390; 1,056: 170; 1,088: 3,496 | 227,562, 159 | 0, 0, 0 |
  | cube4 | 0 | 1,937 | 32: 96; 64: 1,841 | 648,372, 56 | 40, 0, 38 |
  | megacube (the four) | 0 | 8,868 to 10,176 | 64: 8,148 to 9,456; 256: 720 | 803,676 to 901,692, 38–39 | 11, 11, 0 (megacube); 2, 0, 0; 3, 0, 0; 0 |
  | nub | 137 | 764 | 32: 148; 64: 176; 256: 440 | 1,708,944, 10 | 0, 0, 0 |
  | nub1 | 150 | 797 | 32: 148; 64: 176; 256: 473 | 2,605,398, 10 | 2,264, 100, 1,169 |
  | ralith | 1,824 | 1,824 | 256: 1,824 | 688,518, 36 | 54,554, 164, 5,885 |
  | ralith1 | 872 | 872 | 256: 872 | 251,748, 51 | 9,422, 64, 6,956 |
  | ralith2 | 0 | 0 | none | 263,622, 13 | 160, 0, 0 |
  | temp | 0 | 10 | 64: 10 | 802,146, 37 | 0, 0, 0 |
  | test_weapon | 0 | 12 | 32: 12 | 648, 1 | 0, 0, 0 |
  | testcube, the_cube, the_cube1m, thecubesunlit | 0 | 855 | 32: 855 | 1.82 to 1.84 million, 143 | 0, 0, 0 |
  | tubes1 to tubes6 | 1,610 each | 3,100 to 3,243 | 32: 96; 64: 1,351 to 1,438; 256: 1,653 to 1,709 | 1.17 to 1.61 million, 105–107 | tubes1: 11,504, 15, 7,090; tubes4: 1,391, 0, 288; tubes6: 4,328, 1,578, 1,578; the others 0 |

  The raw slots are all unmapped, as the importer has no default
  `--material-map`.
- **Checks,** all passing: `cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace
  --locked` (the importer has 40 tests and one private), and the private tests
  in release (`cargo test --release -p sauerteig-core --lib private_ --locked
  -- --ignored --test-threads 1`, 3 tests) plus the importer's private test.
  New importer tests: the mapping against the old rotation mirrored (20,000
  random cubes); the transform is a reflection keeping north and up; the face
  slots; the root equal to the source cube and growing for an entity; far
  entities left out and reported; an off-metre centre keeps every leaf on the
  grid; full material values in the tree and water and glass in the file; a
  skipped leaf in water keeps its water; a crop with leaves larger than its
  root; a crop's centre moves to a whole metre; a round trip through the file
  keeps the tree; the slot table; level-of-detail nodes; and `reflect_x`
  restoring a tree when applied twice.
- **Not observed:** any import in the Sauerteig app (the in-app import path
  copies the converted JSON, so it is unchanged); the maintainer's look at the
  re-imported maps; Windows and Linux; maps other than the 23 in the archive.
- **Left as is:** shared-sandbox 2e's compact saves keep inert materials and
  raw slots in the file, and the library is imported again after it (see
  above); original textures, models and lights wait on the world format and
  renderer; the tree's gap over triangular faces' diagonals stays O2's; the
  importer still loads whole maps into one `Vec` of leaves before building,
  which `nub1` and the 8 km maps do within 210 MiB. The `inspect` command is
  unchanged.

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
