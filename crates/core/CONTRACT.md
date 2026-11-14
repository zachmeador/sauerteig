# Geometry and multiplayer invariants

The Rust types and constants in [`src`](src/) define the API. This guide explains
behavior that must remain consistent across editing, rendering, and simulation.

## Geometry

Coordinates are right-handed, Y-up, in metres. Player positions are feet
positions. Yaw zero faces −Z, positive yaw turns toward +X, and positive pitch
looks up. Each world has half-open `WorldBounds` in metres; the default arena is
128 × 48 × 128 metres. Bounds span at most 8,192 metres per axis and stay within
coordinates −8,192 through +8,192. [`lib.rs`](src/lib.rs) exports the shared types
and simulation constants.

The world is one octree of cubes on the eighth-metre grid (`Octree`, in
[`src/octree/`](src/octree/)). Node sizes are powers of two, from an eighth of a
metre up to the root, a power-of-two cube that contains the bounds. The root's
origin is set when a world is created or loaded and does not move while the
world runs. Format 3 cannot store it, so loading anchors it on the largest
region whose box is a power-of-two cube on the largest whole region's grid,
pieces included; a world without whole regions roots at its bounds' minimum
corner. Saving and reloading can still move the root, keeping that region's
grid, until shared-sandbox 2e stores it. A node is empty space, a leaf, or eight
children shared copy-on-write: cloning a world takes constant time, and a change
copies only the path from the root to what it changes. A leaf holds empty, full
or shaped geometry, or an exact piece of a larger shaped cube (that source's
position, size and shape) lying inside the leaf; six surface materials, one per
face in the order −X, +X, −Y, +Y, −Z, +Z; and a 16-bit cube material,
Sauerbraten's value. Nothing derived is stored.

Leaves follow one canonical form, so equal worlds have equal trees and equal
`World::digest()`s: eight equal full or empty children merge; pieces of one
source with equal materials merge back into it; a piece wholly inside or
outside its source's solid is a full or empty leaf; and fields without meaning
take fixed values, such as a planar face's diagonal and the materials of faces
without a surface. Shaped leaves never merge by equality. One gap remains:
children that together make one shape, such as a half-height slab of full
eighths, do not merge into a shaped leaf.

Format 3's types (`Cell`, `CellFragment`, `RootEdit`, `CellEdit`,
`MaterialVolume`) convert at the tree's boundary. A cell uses either a legacy
height field or an explicit eight-vertex `Shape`. Its face diagonals are part of
the authored geometry where they change it. Shapes must have an oriented,
closed boundary and positive volume; folded and non-manifold shapes are
rejected, while legacy height-field pinches stay accepted. A cell has a fallback
material and optional six-face overrides; the tree keeps the six face materials
only, so a cell read back names its first face's material as its fallback.
Clipped faces preserve their logical face material; clipboard transforms remap
it with the geometry. Material IDs are stable save-file indices into the catalog
in [`world.rs`](src/world.rs).

A fragment clips an authored source cell without resampling its surface. Source
sizes are powers of two, up to 8,192 metres for imported octree leaves. The tree
stores a clip as exact pieces at the nodes the clip box covers. A metre holds
disjoint fragments on the eighth grid, so at most 512.

`apply_root_edits` writes whole metres: their geometry and surface materials,
keeping cube material. Every edit validates before anything changes, and a
write that changes nothing leaves the tree as it was. Writing back the
fragments a metre held before an edit, as undo does, restores the identical
tree, including a larger shaped leaf the edit split. `get()` gives the coarse
cell filling a metre: a one-metre shaped leaf on whole metres, or any metre of a
full leaf. `cells()` and `regions()` are the format 3 writer's view: one-metre
plain leaves on whole metres as cells and every other leaf with geometry as a
region. `root_overrides()` is always empty, since the tree has no masks.
`root_fragments` gives a metre's content in as few fragments as stay exact;
`fragments_in` and `try_roots_in` walk the tree and skip empty space.

`World::snapshot()` keeps the tree and the water and glass boxes and writes
format 3's parts the first time they are asked for, which is worker work. A
snapshot or a clone stays consistent while the live world changes.

Meshes are built per mesh unit, a node of `MESH_UNIT_SIZE` (16 m) on the root's
grid; a unit's key is its minimum corner in eighths of a metre. Each leaf's
faces belong to the unit holding the leaf's minimum corner and are never split
at unit boundaries, so a unit's mesh can reach past its node; use mesh bounds
for culling. A face lying on its leaf's box is left out when the solids beyond
it cover it completely, judged as polygon coverage on the shared plane: exact
for full and shaped leaves, conservative for pieces. A partly covered face is
drawn whole, as Sauerbraten does, and depth testing hides the covered part.
Cut faces where pieces of one source meet cover each other and are left out.
Faces on the root's outer boundary are left out. `mesh_chunk` returns opaque
surfaces only; water and glass are drawn from `volumes()`.

Every write's units to build again come from the difference between the trees
before and after, skipping shared subtrees: units owning a leaf with solid
geometry that the write removed or added, and units owning a leaf beside
changed geometry or beside a removed or added shaped leaf or piece, since a
neighbour's coverage depends on that leaf's kind and box. `apply_root_edits`
and `apply_patch` return them, and `changed_chunks` gives them for any other
write, water and glass included. A client applies a `TransactionPatch` with
`apply_patch`: the boxes first, then the metres, as the server applied the
edit.

Rays, box overlap, containment, and support and ceiling queries walk the tree
and handle each leaf kind once. Containment is exact integer arithmetic, and
the rest uses only basic arithmetic and square roots, so every build gives the
same bits. Rays reject internal joins between solids, testing samples beside
the unrounded hit; a ray from inside solid hits its exit. A solid hit's `bounds`
is the struck leaf's box within the hit's metre. Support and ceiling queries
ignore a leaf the body overlaps by no more than the box test's contact
tolerance (3e-5 m), so a body falling beside a wall cannot land on the faces
between the wall's leaves. Corner-deforming a clipped source is rejected when
the result cannot be represented by one editable cell.

## Material volumes

Water and glass are cube material in the tree. A leaf's cube material fills the
cube's open space: solid geometry wins at a point, water never blocks shots, and
glass blocks movement and shots across its whole cube. The water and glass
variants act as water and glass; lava, clip, noclip, death and alpha are stored
but inert.

Format 3 writes water and glass as boxes, and `World::volumes()` keeps them as a
list of disjoint `MaterialVolume` boxes on the same eighth-metre grid: the
loaded boxes until a change, after which only boxes the change touches differ,
so unchanged glass keeps its boxes and their edge bands. Ordered `VolumeEdit`
operations paint or clear water and glass without changing solid geometry;
`apply_volume_edits` writes the edits' own boxes into the tree.
`replace_volumes` writes cube material only where the new list differs from the
current one and returns the boxes it wrote. `apply_volume_edits` and
`prepare_volume_edits` count the leaves their writes would add before building
them and refuse a change that would take the world past `MAX_WORLD_PIECES`
(786,432 leaves), so a thin pane cannot exhaust memory; a one-metre floor 64 m
across is 4,096 leaves, and an eighth-thick pane as wide 262,144.
`replace_volumes` serves loading, patches and the server's undo and redo, so it
counts against the memory limit conversion uses instead
(`MAX_CONVERSION_LEAVES`, 4,194,304 leaves): a world saved over the guard still
opens, and a client never refuses a patch for the guard. Exact adjacent boxes
coalesce. A world may hold at most 16,384 boxes; an edit may contain at most
32,768 changes, allowing a maximum paste to clear its destination.
`MAX_VOLUME_FACE_CONTACTS` limits equal-material pairs sharing a positive-area
face to 65,536. Edge and point contacts do not count, and water/glass interfaces
remain visible without shared-face subtraction.

Volume assignments may overlap solid terrain. Terrain takes precedence when
querying a point's material, so a wall inside water is not swimming space. Glass
has axis-aligned collision and blocks combat rays, rockets, and blast visibility.
Water is non-solid. The shared movement step queries the world at the profile's
water probe, the torso under Arena, to apply drag, reduced gravity, and upward
swimming while jump is held; Sauerbraten swims by view pitch instead. Rendering
queries the eye separately for underwater tint. Edit-mode flight ignores both
effects.

## Shared edits and history

The server owns world revisions and validates every edit. Edits require edit
mode and cannot add geometry through an active fighter. Each operation is atomic
and bounded by changed roots and fragment complexity. The server counts a
world's format 3 pieces and bytes from the leaves each edit changes, including
leaves split outside the metres it names, and refuses an edit that would take
the world past what it can load again. It also refuses an edit after which the
tree holds more than `MAX_WORLD_PIECES` leaves, water and glass included,
counted exactly, unless the edit adds no leaves. Loading never refuses for this
guard: a world saved over it opens, on the server and through a `Welcome`, and
accepts only edits that do not add leaves.

`EditRoots.base_revision` and `EditTransaction.base_revision` reject replacements
if a touched root or object changed after that revision. Unrelated newer edits
are allowed. This change clock never
rewinds, including when another player clears a root.

Undo has a separate ownership history. It succeeds only when every touched root
and object still belongs to that operation. Undo restores prior ownership; redo requires
that restored lineage and reinstates the original operation token. Public world
revisions always increase. A changed edit clears the player's redo stack.
History is bounded by both operation count and geometry complexity.

`EditTransaction` validates geometry, object mutations, and volume edits together
before applying any, and broadcasts one `TransactionPatch`. New glass cannot
intersect an active fighter. Volume history stores the before/after layer and
counts toward the history complexity limit. It currently uses one global
conflict/ownership clock: even a distant intervening volume edit rejects a stale
volume edit or undo. Ordinary geometry and object edits retain their existing
per-root/per-object conflict checks. `TransactionPatch.volumes` is the complete
replacement layer when changed, omitted otherwise; `Welcome` includes the full
layer. Paste receives fresh object IDs;
undo removes those objects, while redo restores the same identities. ID allocation
is persisted so deleted identities are not reused after a restart.

Every parsed `Edit`, `EditRoots`, or `EditTransaction` receives one `EditResult`
with the matching request ID. Changed edits broadcast their patch before the successful result;
unchanged edits succeed without a patch. Clients finish pending edits from that
acknowledgment, never by comparing another player's patch with their geometry.

## Movement profiles

`simulate_player_with` steps one player by one fixed tick under a
`MovementProfile`: a style, its numbers (speeds, jump, gravity, easing), and the
dimensions the player moves with. The server and every client's prediction must
use identical profiles. `simulate_player` is the same step under the Arena
profile, whose trajectories are pinned bit for bit by `tests/golden/`.

Arena is the original model: ground friction, then acceleration along the
requested direction up to the walking speed. Sauerbraten is a port of the
velocity model in the original engine's `physics.cpp`: velocity eases toward
the steering target by `1 - 1/ease` of the gap per 20 ms, gravity accumulates in a
separate falling component, straight forward or back and airborne movement each
speed the target up by 1.3, and a jump fires once per press. Each fixed tick runs
the model in steps close to the original 5 ms physics frame. Both styles share
the box collision, step-up and support queries, which take the profile's radius,
height and step height. Hit detection, spawning, rendering and the camera still
use `PLAYER_RADIUS`, `PLAYER_HEIGHT` and `EYE_HEIGHT`.

A style that needs more than position and velocity keeps it in `Player::movement`:
Sauerbraten's falling velocity, its jump latch, and whether the last tick ended in
water. Arena clears it, and idle state is not serialized, so Arena snapshots are
unchanged. Whatever resets a player's velocity, such as a respawn or a mode
change, must reset `movement` with it.

Movement gives the same bits in every build (dev and release) and on every
system, with no tolerance. The server and each client's prediction compute the
same step, and a last-bit difference can grow in collision into a visible
correction. Code that movement depends on therefore calls the `libm` crate
(`libm::sinf`, `libm::cosf`, `libm::pow`, `libm::atan2`, and so on) and never
the system's math library through `f32::sin`, `powf`, `atan2` and the like,
whose last bits differ between systems and between optimization levels. It
writes powers as multiplications instead of `powi`. Square roots, `abs`,
`floor`, `%` and basic arithmetic are exact in IEEE 754 and stay allowed.
`crates/core/clippy.toml` lists the forbidden methods, and Clippy rejects them
in every target of the crate. Two golden files pin the result: `tests/golden/`
holds the Arena and Sauerbraten trajectories, compared bit for bit by
`tests/arena_golden.rs`. Any later movement golden follows the same rule.

## Simulation and transport

[`protocol.rs`](src/protocol.rs) defines JSON messages, with snake-case `type`
tags and `[x,y,z]` vectors. The server owns combat results and fixed-step time.
Each `InputFrame` describes one simulation step; the server consumes at most one
queued frame per tick and acknowledges only consumed sequence numbers.
Clients cap outstanding input at 24 frames and discard excess accumulated time
while full. They resume when acknowledgments free space, preventing server stalls
from growing a permanent input backlog.

Duplicate or stale input is ignored. Queue overflow disconnects the client.
Missing input applies neutral movement while gravity continues; it does not
advance the acknowledged sequence. Held fire expires after 300 ms without a
consumed command.

Mode changes drain old-mode inputs and acknowledge the highest received
sequence. Success and denial both return the actual mode and request ID.
Snapshots carry that ID so clients can reject stale mode state.

The server keeps only the latest movement snapshot while preserving ordered
world events. A `Sync`/`Synced` barrier acknowledges all earlier commands before
a client leaves. Full world snapshots must fit the native transport's 64 MiB
message limit; geometry reserves 1 MiB for objects, players, and the envelope.
The owner captures Welcome at one revision before accepting later patches into
that peer's reliable queue. Welcome is serialized and sent first; queued patches
then retain revision order. Queue pressure delays an edit on the edit worker for up
to two seconds, then rejects it as busy, rather than dropping a joining peer. A
client that vanishes without `Sync` still has the commands it already sent
applied, in order. A progressing transfer has no whole-message time
limit; 30 seconds without a successful TCP write ends a stalled connection.
Request, history, world, and connection limits live with their
validation code in [`crates/server`](../server/src/).

## Persistence and objects

Version 3 world saves contain `revision`, `bounds`, compact `regions`, coarse
`cells`, detailed or empty `roots`, `volumes`, `objects`, and `next_object`.
The tree writes its leaves as `regions`, one-metre leaves on whole metres as
`cells`, height-field shapes as `heights` and `diagonal`, and water and glass as
boxes; it writes no roots. A build from before the octree opens such a save
within its own limits. The loader accepts previous versions, with cells and
roots replacing the regions in their metre; omitted `volumes` means normal
space.
Older saves can
omit the version and new fields, as well as cell `shape`, `diagonal`, and
`face_materials`; their original fixed bounds and uniform material still apply.
Loading validates geometry, limits, and unique positions/object IDs before
starting a session.

Spawn objects affect respawn locations. Light nodes provide dynamic lighting,
independently of decorative lamp models. Other
props are currently decorative and have no gameplay collision. Object changes
are shared, saved, undoable, and included in volume copy/paste and blueprints.
