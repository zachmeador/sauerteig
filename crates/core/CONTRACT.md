# Geometry and multiplayer invariants

The Rust types and constants in [`src`](src/) define the API. This guide explains
behavior that must remain consistent across editing, rendering, and simulation.

## Geometry

Coordinates are right-handed, Y-up, in metres. Player positions are feet
positions. Yaw zero faces −Z, positive yaw turns toward +X, and positive pitch
looks up. [`lib.rs`](src/lib.rs) defines world bounds and simulation constants.

The world stores sparse one-metre roots in eight-metre chunks. Simple roots hold
one `Cell`; refined roots hold disjoint `CellFragment`s on an integer eighth-metre
grid. `FineBox` bounds are half-open. Empty space is omitted from storage.

A cell uses either a legacy height field or an explicit eight-vertex `Shape`.
Its face diagonals are part of the authored geometry. Shapes must have an
oriented, closed boundary and positive volume; folded and non-manifold shapes
are rejected. Legacy height-field pinches keep their original representation.
Material IDs are stable save-file indices into the catalog in
[`world.rs`](src/world.rs).

A fragment clips an authored source cell without resampling its surface. Source
sizes are powers of two, up to 8,192 metres for imported octree leaves. A root
contains at most 512 disjoint fragments. Exact clip boundaries drive meshes,
rays, collision, and support queries. Clipped warped surfaces retain their
source corners and diagonals.

Root replacements validate and compile in full before changing the world.
Compatible solid fragments coalesce; a full-root cell normalizes to coarse
storage. Coarse and detailed storage must never overlap. `get()` and `cells()`
expose coarse storage only; use the root APIs for complete geometry.

Edits invalidate affected chunks and their face neighbors. Rendering, aiming,
and collision share the same cell surfaces. Rays reject internal union
boundaries; meshing can retain partially occluded triangles hidden by depth
buffering. Corner-deforming a clipped source is rejected when the result cannot
be represented by one editable cell.

## Shared edits and history

The server owns world revisions and validates every edit. Edits require edit
mode and cannot add geometry through an active fighter. Each operation is
atomic and bounded by changed roots and fragment complexity.

`EditRoots.base_revision` rejects replacements if a touched root changed after
that revision. Unrelated newer edits are allowed. This change clock never
rewinds, including when another player clears a root.

Undo has a separate ownership history. It succeeds only when every touched root
still belongs to that operation. Undo restores prior ownership; redo requires
that restored lineage and reinstates the original operation token. Public world
revisions always increase. A changed edit clears the player's redo stack.
History is bounded by both operation count and geometry complexity.

Every parsed `Edit` or `EditRoots` receives one `EditResult` with the matching
request ID. Changed edits broadcast their patch before the successful result;
unchanged edits succeed without a patch. Clients finish pending edits from that
acknowledgment, never by comparing another player's patch with their geometry.

## Simulation and transport

[`protocol.rs`](src/protocol.rs) defines JSON messages, with snake-case `type`
tags and `[x,y,z]` vectors. The server owns combat results and fixed-step time.
Each `InputFrame` describes one simulation step; the server consumes at most one
queued frame per tick and acknowledges only consumed sequence numbers.

Duplicate or stale input is ignored. Queue overflow disconnects the client.
Missing input applies neutral movement while gravity continues; it does not
advance the acknowledged sequence. Held fire expires after 300 ms without a
consumed command.

Mode changes drain old-mode inputs and acknowledge the highest received
sequence. Success and denial both return the actual mode and request ID.
Snapshots carry that ID so clients can reject stale mode state.

The server keeps only the latest movement snapshot while preserving ordered
world events. Request, history, world, and connection limits live with their
validation code in [`crates/server`](../server/src/).

## Persistence and objects

World saves contain `revision`, coarse `cells`, detailed `roots`, and `objects`.
Older saves can omit `roots`, cell `shape`, and cell `diagonal`. Loading validates
geometry, limits, and unique positions/object IDs before starting a session.

Spawn objects affect respawn locations. Lamps provide dynamic lighting. Other
props are currently decorative and have no gameplay collision. Object changes
are shared and saved, but do not enter geometry undo or blueprints.
