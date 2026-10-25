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

The world stores compact authored `CellFragment` regions plus sparse one-metre
root replacements in eight-metre chunks. The immutable region index finds large
leaves without enumerating their metre cells. Editing a root masks the base in
that root, then supplies its replacement. An empty replacement is a persistent
deletion, not an absent edit. Simple roots hold one `Cell`; refined roots hold
disjoint fragments on an integer eighth-metre grid. `FineBox` bounds are half-open.

A cell uses either a legacy height field or an explicit eight-vertex `Shape`.
Its face diagonals are part of the authored geometry. Shapes must have an
oriented, closed boundary and positive volume; folded and non-manifold shapes
are rejected. Legacy height-field pinches keep their original representation.
Each cell has a fallback material and optional six-face overrides in the order
−X, +X, −Y, +Y, −Z, +Z. Clipped faces preserve their logical face material;
clipboard transforms remap it with the geometry. Material IDs are stable
save-file indices into the catalog in [`world.rs`](src/world.rs).

A fragment clips an authored source cell without resampling its surface. Source
sizes are powers of two, up to 8,192 metres for imported octree leaves. A root
contains at most 512 disjoint fragments. Exact clip boundaries drive meshes,
rays, collision, and support queries. Clipped warped surfaces retain their
source corners and diagonals.

Root replacements validate and compile in full before changing the world.
Compatible solid fragments coalesce; a full-root cell normalizes to coarse
storage. Coarse and detailed storage must never overlap. `get()` and `cells()`
expose coarse overrides only; use fragment queries for complete geometry and
`try_roots_in` for bounded editor expansion. `regions()` and `root_overrides()`
together with `cells()` preserve the full save state, including empty masks.

`World::snapshot()` retains authored regions, coarse cells, detailed roots,
empty override masks, and volumes through structural sharing. It contains no
triangles, mesh batches, or spatial query indexes. Expanding a snapshot into save
or protocol vectors is worker work. `World::clone()` is also cheap, but shares
both authored and derived data for off-thread edit preparation; mutations copy
only changed map paths/chunks/regions. A snapshot or edit view remains consistent
while the live world changes.

Edits invalidate affected compact mesh batches, local chunks, and face neighbors.
Compact batch keys are opaque identifiers, not world coordinates; use mesh bounds
for culling. Rendering, aiming, and collision share the same cell surfaces.
Rays reject internal union
boundaries; meshing can retain partially occluded triangles hidden by depth
buffering. Corner-deforming a clipped source is rejected when the result cannot
be represented by one editable cell.

## Material volumes

`World::volumes()` stores a separate sparse layer of disjoint `MaterialVolume`
boxes on the same eighth-metre grid. A region is either water or glass; absence
means normal space. Ordered `VolumeEdit` operations paint or clear that layer
without changing solid geometry. Exact adjacent boxes coalesce. Validation and
spatial indexing keep large regions compact. A world may store at most 16,384
material boxes; an edit may contain at most 32,768 changes, allowing a maximum
paste to clear its destination. `MAX_VOLUME_FACE_CONTACTS` limits equal-material
pairs sharing a positive-area face to 65,536, bounding the exterior mesh generated
by partially touching regions. Edge and point contacts do not count, and water/
glass interfaces remain visible without shared-face subtraction. Intermediate
splitting is bounded and rejects excessive detail before mutating the world.

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
mode and cannot add geometry through an active fighter. Each operation is
atomic and bounded by changed roots and fragment complexity.

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
The loader accepts previous versions; omitted `volumes` means normal space.
Older saves can
omit the version and new fields, as well as cell `shape`, `diagonal`, and
`face_materials`; their original fixed bounds and uniform material still apply.
Loading validates geometry, limits, and unique positions/object IDs before
starting a session.

Spawn objects affect respawn locations. Light nodes provide dynamic lighting,
independently of decorative lamp models. Other
props are currently decorative and have no gameplay collision. Object changes
are shared, saved, undoable, and included in volume copy/paste and blueprints.
