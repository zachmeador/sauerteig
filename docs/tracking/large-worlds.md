# Large worlds and performance

State: partial.

## Outcome

Explore and edit large original-scale maps comfortably in the desktop client on
computers down to the
[minimum specification](platforms.md#minimum-specification), with multiple
players building and fighting together.

## Working now

Compact authored leaves, a spatial query index, local edit overrides, dynamic bounds,
and camera/shadow culling avoid expanding large cubes into metre cells. Storage
accepts worlds up to 8,192 metres per axis within its coordinate envelope.
Original-scale archive conversions and server loads have been exercised.

That storage support is not complete large-map usability. The camera far plane
is currently 180 metres and editor flight is fixed at 16 metres/second. Large maps
also lack a convenient way to locate interesting geometry or navigate between
distant areas. Existing profiles measure CPU work, not completed GPU frames.

World size is capped today. Each joining player receives the whole world as one
JSON message, and the 64 MiB limits on the join snapshot, the save file and the
importer's output follow from that. The server also caps a world at 786,432
pieces and 1,024 objects. nub1, at 434,233 regions, was refused only by the
400,000-region cap, which octree O1a removed; it now opens as 511,155 leaves,
with 275,000 to spare under the piece guard (measured 2026-11-14).
Every large-world measurement so far comes from the maintainer's private library
of imported maps, which the project can't redistribute, so contributors can't
reproduce them.

## Remaining work and acceptance

This work builds on the [octree world](octree.md), on
[shared-sandbox](shared-sandbox.md) stage 2 and on the [renderer](renderer.md)
stages for view distance, detail levels and lighter meshes. The octree makes
the world one tree whose cubes can be any size. Shared-sandbox stage 2 sends it
in compact chunks, builds it on worker threads, and replaces every remaining
size cap with one memory budget: a server setting whose default is what a
computer at the minimum specification can join using half its memory
(maintainer, 2026-11-13). How many cubes that holds is measured after the
octree ([world size](shared-sandbox.md#world-size)).

Streaming is the long-term answer. Once a player holds only the part of the
world near them, a world no longer has to fit in every player's memory, and the
budget becomes the server's own memory.

Stage 1 below is the first work order.

- [ ] Make viewing distance and editor traversal suitable for large maps, with
  deliberate performance limits rather than simply increasing every bound.
- [ ] Stream the world by distance, so that a player holds only what is near,
  and then size the default budget by the server's memory.
- [ ] Exercise rendering, collision, local edits, blueprints, and save/rejoin far
  from the origin on actual large maps.
- [ ] Measure frame times, memory, startup, and edit/remesh spikes on a
  reference machine at the minimum specification ([platforms](platforms.md)
  stage 3), recording map, resolution, client, and player count.
- [ ] Run two fighters plus a builder editing geometry during combat, and use
  the measurements to set a practical performance budget.
- [ ] Address measured bottlenecks and rerun the same scenarios before claiming
  the minimum specification is met.

## Stage 1: a generated benchmark world

State: running on branch `large-worlds-1` since 2026-11-14, on the tree that
[octree](octree.md) O1a merged. Wave 1, medium. Shared-sandbox 2b's second
session waits for it, because its records use the smaller preset.

- **Why.** Performance claims need evidence that contributors can reproduce
  ([direction](README.md#direction)), and today's come from a private library.
  A world generated from a seed gives every contributor, and CI, the same large
  world. The private library stays as extra coverage.
- **Decided:**
  - The generator is deterministic: the same seed and preset give byte-identical
    files on every system. It uses integer arithmetic and a fixed pseudo-random
    generator, not the system's floating-point math functions.
  - It makes worlds at nub1's scale with the parts that cost memory and time:
    regions, cut pieces, triangles and lights.
- **Scope:**
  - **Structure like an imported map:** an octree with cubes from one-eighth of
    a metre to many metres, solid and shaped, with slopes and corners; enclosed
    rooms, corridors and open ground, so that occlusion and shadows behave as
    in a real map; water and glass as cube material; lights spread through the
    rooms; and spawns.
  - **An edited layer** that can be switched off: small cubes cut into large
    ones, shaped cubes with mixed face materials, and scattered blocks, as real
    worlds carry after editing. With it on, the smaller preset must still fit
    today's JSON limits until shared-sandbox 2d removes them: 786,432 pieces
    (`MAX_WORLD_FRAGMENTS`), 63 MiB of geometry JSON (`MAX_GEOMETRY_BYTES`) and
    the importer's 64 MiB (`MAX_WORLD_BYTES`). At the_cube's size a world
    takes about 31 MB of JSON since octree O1a, so the edited layer has about
    30 MB (estimate).
  - **Presets.** A nub1 preset aims at nub1's measured conversion: 434,233
    imported leaves in a 2,048 m map, about 11.8 triangles per leaf before
    culling (5,140,198 in all), as much water and glass as nub1's 1,552 boxes
    hold, 155 lights and a spawn. A smaller preset aims at the_cube's 306,094
    leaves. A test preset of a few thousand leaves keeps the digest test fast.
    Both large presets open once octree O1a has removed the region cap, within
    the JSON limits until 2d; 2d's acceptance hosts and joins the nub1 preset.
  - **Output:** a world file that the server, the desktop client and the capture
    tool open, and a capture pose file for it.
  - **Where:** the generator in `crates/core`, so core tests can use it, and a
    command in `sauerteig-import`, which already writes world files, to write it
    to disk. Generated files stay out of the repository.
  - **Docs:** the command and how to reproduce each measurement, in the
    [performance checks](../development.md#performance-checks).
- **Out of scope:** the server, the client and every limit.
- **Parallel work:** new files in core and the importer, a module line in core's
  `lib.rs`, the importer's command parsing (`cli.rs`, `main.rs`),
  `crates/importer/README.md` and the development guide's performance checks.
  Runs after octree O1a, beside the rest of wave 1, sharing a few files:
  - Octree O3 rewrites the importer's reader and conversion (`lib.rs`,
    `convert.rs`, `transform.rs`) and its README, and
    [legacy-import](legacy-import.md) I1 edits the reader and `main.rs`'s help
    text; keep the generator's command apart from those parts of `main.rs` and
    the README.
  - [Renderer](renderer.md) N1 also edits the performance checks, deleting the
    capture tool's `--tier` flag there.

  The second to merge rebases onto the first, and `Cargo.lock` is regenerated
  at merge.
- **Acceptance:**
  - [ ] A test generates the test preset and checks its digest. It passes on the
    Mac and in the Linux CI job.
  - [ ] Import the smaller preset into a scratch library (`SAUERTEIG_DATA_DIR`),
    host it and fly through it: it shows rooms, slopes, water, glass, and lit
    and shadowed lights, not noise.
  - [ ] Unedited, the smaller preset lands within 15% of a copy of the_cube on
    the same machine and build in server memory per leaf, server load time and
    triangles per leaf.
  - [ ] With the edited layer on, the smaller preset opens. Record the same
    numbers, and frame times on the preset's capture poses at 1080p and 4K.
- **Record:** the command, the presets and their digests, and every measurement
  with its machine and build.

## Evidence

Geometry regression tests and CPU profiling tools exist; previous manual checks
covered a large imported map and native/browser shared sessions. They do not prove
sustained performance across this archive or the target hardware range.
Use the [performance checks](../development.md#performance-checks) for commands;
keep only the latest useful benchmark findings here when measurements are available.

Memory and load, measured 2026-11-12 on the maintainer's M5 Pro with release
builds, on copies of imported maps, through scratch measurement hooks. They
describe today's structures, which the [octree](octree.md) replaces:

- **Server memory.** Per region, the standalone server uses 3.3 KB of resident
  memory, or 1.7 KB with one shared empty mask set. The cost per region stays
  flat from 306,000 to 1.74 million regions.
- **Client memory.** The capture tool, standing in for a joining client at 4K,
  uses 9.1 KB per region at its join peak and 5.8 KB once settled (7.5 and
  4.2 KB with the shared set). A client also needs about 0.53 GiB whatever the
  world.
- **Load time.** 90% of a server's load is two World builds. Half of each build
  is the region BVH, which grows faster than the world: 0.71–0.77 s at 434,233
  regions and 3.96–4.03 s at 1.74 million.
- **Rendering.** nub1 renders at 4K within the 6.06 ms budget of a 165 Hz
  display; its heaviest pose takes 5.39–5.41 ms of GPU throughput.
- **Reading memory.** Activity Monitor's default Memory column undercounts
  memory that is freed and reused: it showed 366 MiB for the nub1 server while
  1.30 GB was live. On macOS use the Real Memory column. `/health` (after
  octree O1a) and F3 (after shared-sandbox 2b) will show memory by part on
  every system.
