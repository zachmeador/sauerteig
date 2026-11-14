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
Every large-world measurement before stage 1 came from the maintainer's private
library of imported maps, which the project can't redistribute, so contributors
can't reproduce them. Stage 1's generated worlds can be reproduced by anyone
([performance checks](../development.md#generated-benchmark-worlds)).

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
  Viewing distance is stage 2 below; editor traversal follows.
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

State: merged on 2026-11-14. The automated checks pass on the Mac, and the
digest tests also pass on x86-64 under Rosetta (design lead). Not yet observed
([record](#stage-1-record-2026-11-14)): the Linux CI run of the digest test,
which runs once main is pushed; the fly-through in the app and a client
hosting the presets, for the maintainer. The small preset's frame times were
rerun on a quiet machine after renderer N2 merged; nub1's weren't.
Shared-sandbox 2b's second session can use the smaller preset.

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
    Mac and in the Linux CI job. Not done: it passes on the Mac; the Linux job
    has not run it (nothing in the generator or its files depends on the
    system).
  - [ ] Import the smaller preset into a scratch library (`SAUERTEIG_DATA_DIR`),
    host it and fly through it: it shows rooms, slopes, water, glass, and lit
    and shadowed lights, not noise. Not done: the maintainer's check in the
    app. Headlessly, the server opens the preset and the capture tool renders
    its poses, and the captures show rooms with lit lights, a stairwell, a
    lake, an underwater view, glass windows and canopies and a glass-roofed
    pool.
  - [x] Unedited, the smaller preset lands within 15% of a copy of the_cube on
    the same machine and build in server memory per leaf (-5.9%), server load
    time (+2.4%) and triangles per leaf (-8.8%): see the record.
  - [x] With the edited layer on, the smaller preset opens. Record the same
    numbers, and frame times on the preset's capture poses at 1080p and 4K. It
    opens (429,435 leaves, 57.2 MB of JSON), and the design lead measured its
    frame times on a quiet machine after renderer N2 merged (record below).
- **Record:** the command, the presets and their digests, and every measurement
  with its machine and build, in the stage 1 record below.

### Stage 1 record (2026-11-14)

**Command.** `sauerteig-import generate PRESET --output WORLD.json [--poses FILE]
[--seed N] [--edits on|off]` writes the world file (format 3, regions only, as
`convert` writes) and a pose file for the capture tool, and refuses a world over
the 786,432-leaf guard or 63 MiB. The how-to is in the
[performance checks](../development.md#generated-benchmark-worlds); the importer's
README has a short section. The generator is `crates/core/src/benchmark_world.rs`.

**Presets and digests** (default seed 1,592,590,337, which is `0x5eed0001`). The
tree digest is `Octree::digest()`; "objects and poses" is a hash of the lights,
spawns and capture poses (the edited layer doesn't change them).

| Preset | Edits | Leaves | Full / shaped / water or glass | File | Tree digest | Objects and poses |
| --- | --- | ---: | --- | ---: | --- | --- |
| test | on | 14,718 | 11,674 / 991 / 2,053 | 1,983,256 B | `ab23c3eaa1b078f729cc5c000118565c` | `f3d0066172ee586a` |
| test | off | 12,809 | 9,775 / 981 / 2,053 | 1,689,329 B | `f3515db81f0f4158732c48ea922e553e` | `f3d0066172ee586a` |
| small | on | 429,435 | 291,629 / 62,478 / 75,328 | 57,154,958 B | `3bd866067541417f873016be80fda537` | `f71604f627021736` |
| small | off | 355,903 | 218,581 / 61,994 / 75,328 | 45,975,272 B | `cd93c89f55f1015383484f0bac788a72` | `f71604f627021736` |
| nub1 | off | 485,984 | 301,812 / 66,513 / 117,659 | 61,784,415 B | `58fe1b5367e571fbcc7d4020a34be498` | `91ca4e77e983ccb5` |

nub1 with the edited layer is refused (83.5 MB of JSON), so its default is off.
The test preset's digests and its files' byte count and hash are asserted by
core's `the_test_preset_has_its_recorded_digest` and the importer's
`the_test_preset_writes_the_recorded_files`. They change only when the
generator does; such a change updates them with the reason here.

**What the worlds hold.** Rock below the ground and a heightfield of earth
over the middle: one-metre cells, each in one metre band so that it is one
shaped cube, flat across the districts, with hills and rough-bedded lake
basins in the ring around them. The districts are plots on a 32 m grid: a
building (16 to 24 m, one to three storeys of 5 m) or a plaza. Buildings have
a 1 m outer wall, rooms split by 0.5 m partitions, doors joining them,
entrances, ribbon windows of quarter-metre glass, skylights, flat, hip or
gable roofs of sloping cubes, stairs as a one-in-two or one-in-one ramp through
a hole in the slab, pillars with capitals, eighth-metre trim and crates. A
plaza has a pool, a glass canopy on four pillars and ruined walls. Lights are a
shuffled share of the rooms' centres and the plazas' lamps, as many as the
map the preset follows (339 and 155), and sixteen spawns stand outside the
doors. Leaves run from an eighth of a metre to 64 m (small) or 128 m (nub1).
The edited layer cuts eighth to half-metre niches into walls and pits into
the ground, scatters quarter-metre to two-metre blocks and adds quarter and
one-metre shaped cubes (wedges, pulled corners, slanted walls, lifted corners)
with six different face materials.

**Checks,** all passing on the Mac (M5 Pro): `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings` (core's
`clippy.toml` bans system math, and the generator calls none) and `cargo test
--workspace --locked`. New tests: the test preset's digests with the edited layer
on and off; the same generation twice, and a different seed, edits setting or
preset gives different worlds; the test world has full, shaped, water and glass
leaves, sizes from one eighth, lights and spawns, rooms and poses; its file form
(regions only, explicit shapes) loads back to the same digest and a world that
the server's loader builds, and every light and spawn stands in open space; the
importer's files for the test preset byte for byte, their loading as a
converted map, the pose file's names, and the command's arguments. The ignored
`the_large_presets_build_within_the_world_guard` (run with `--release`) builds
small and nub1 and passes. A scan of 20 seeds for each of test, small and nub1
(edits on) found no generation error.

**Measurements,** Apple M5 Pro (Metal), release builds of this branch's working
tree on main `208f63f` (not committed), 2026-11-14.

Server, `sauerteig-server --world FILE`, on copies, from starting it to the
first `/health` answer, resident memory after loading, five alternating runs.
Other work was running on the computer, so the load times are the medians of
alternating runs; the runs' CPU times track their wall times, apart from one
the_cube1m run (0.368 s of CPU in 0.396 s).

| World | Leaves | Load time | Resident | KiB per leaf | Triangles | Per leaf |
| --- | ---: | --- | --- | ---: | ---: | ---: |
| the_cube1m (a copy) | 339,113 | 0.369 s (0.361–0.396) | 134.6 MiB (134.2–134.6) | 0.406 | 717,378 | 2.116 |
| small, edits off | 355,903 | 0.378 s (0.373–0.382) | 132.9 MiB (132.9–133.0) | 0.382 | 686,765 | 1.930 |
| small, edits on | 429,435 | 0.416 s (0.413–0.426) | 160.2 MiB (160.0–160.2) | 0.382 | 828,372 | 1.929 |
| nub1 preset | 485,984 | 0.448 s (0.446–0.451) | 172.1 MiB (172.0–172.1) | 0.363 | 945,873 | 1.946 |

Unedited small against the_cube: leaves +4.9%, load time +2.4%, memory per
leaf -5.9%, triangles per leaf -8.8%: inside the 15% the acceptance asks for.
The triangle counts are the capture tool's `triangles`. For comparison, the
real nub1 (octree O1a's record) has 511,155 leaves (the preset: -4.9%), a 62
MB file (61.8), 0.34–0.45 s to open, 177 MiB (172.1) and 755,996 triangles
(945,873, +25%): the preset has fewer full leaves (302,000 against 374,000)
and more shaped and water leaves, and its triangles per leaf run higher. The
limit on a file is 63 MiB (66.06 MB); nub1's is 61.8 MB.

Frame times on a quiet machine (design lead, 2026-11-14): the small preset with
edits on, generated by main at `5c9be91` (octree O1a, O1b and renderer N2 in),
the capture tool at High, `--frames 1 --warmup 10 --throughput 120 --runs 5`,
three rounds, median and range of the rounds' medians, GPU throughput in ms a
frame, load averages 3.0 to 6.4 with no other session running:

| Pose | 1080p | 4K |
| --- | ---: | ---: |
| spawn | 1.64 (1.63–1.66) | 4.45 (4.41–4.56) |
| overlook | 2.00 (1.98–2.08) | 4.30 (4.28–4.41) |
| room-1 | 1.93 (1.90–2.00) | 5.33 (5.25–5.51) |
| room-2 | 1.80 (1.77–1.80) | 5.04 (5.02–5.07) |
| upstairs | 2.92 (2.91–2.92) | 9.42 (9.42–9.75) |
| water | 1.05 (1.03–1.05) | 2.24 (2.24–2.25) |
| underwater | 0.96 (0.96–0.96) | 2.29 (2.28–2.30) |
| glass | 1.71 (1.70–1.72) | 5.24 (5.24–5.24) |
| stairs | 2.70 (2.69–2.72) | 8.88 (8.87–8.89) |
| flight | 1.42 (1.40–1.44) | 3.35 (3.31–3.38) |
| teleports | 1.99 (1.97–2.01) | 4.27 (4.25–4.31) |
| storm | 2.66 (2.63–2.71) | 6.03 (6.00–6.05) |

Every pose fits 6.06 ms at 1080p; at 4K the two stairwell poses don't, like
the imported tower and the light room.

The session's own frame times follow, from before N2 and on a GPU other
sessions were using; only nub1's have no quiet rerun. The capture tool at
1920x1080 and 3840x2160 with `--frames 1 --warmup 10 --throughput 120 --runs 5`,
three rounds, every pose of each preset's pose file, GPU throughput in milliseconds a frame (lowest of the 15 runs, and
median of the 15). **These are contended and not a baseline.** Other
sessions were running the capture tool on this GPU throughout: every run
started with another capture running, and all but one ended with one (I sampled
the process list at both moments). The same pose moved by up to three times
between rounds, and some poses' 1080p medians exceed their 4K medians. The
lowest runs are the nearest to an idle GPU and may still be contended. Rerun
them on a quiet machine before using any of them
([performance checks](../development.md#generated-benchmark-worlds)).

Small, edits on (828,372 world triangles):

| Pose | Visible triangles | 1080p lowest | 1080p median | 4K lowest | 4K median |
| --- | ---: | ---: | ---: | ---: | ---: |
| spawn | 616,973 | 2.31 | 2.60 | 4.04 | 7.21 |
| overlook | 573,481 | 2.54 | 4.15 | 5.66 | 6.70 |
| room-1 | 650,873 | 2.89 | 4.16 | 5.21 | 7.25 |
| room-2 | 582,321 | 2.82 | 4.13 | 6.28 | 8.31 |
| upstairs | 523,956 | 2.70 | 5.50 | 11.05 | 14.85 |
| water | 271,524 | 0.97 | 1.53 | 3.55 | 5.54 |
| underwater | 169,787 | 0.93 | 1.43 | 4.97 | 5.99 |
| glass | 370,518 | 1.86 | 2.68 | 6.19 | 9.55 |
| stairs | 400,418 | 3.93 | 4.10 | 8.30 | 13.88 |
| flight | 564,610 | 1.97 | 2.32 | 3.31 | 4.90 |
| teleports | 573,481 | 2.54 | 3.91 | 4.80 | 6.44 |
| storm | 650,873 | 3.93 | 5.86 | 8.70 | 11.90 |

nub1 (945,873 world triangles):

| Pose | Visible triangles | 1080p lowest | 1080p median | 4K lowest | 4K median |
| --- | ---: | ---: | ---: | ---: | ---: |
| spawn | 532,073 | 2.91 | 4.34 | 4.91 | 6.74 |
| overlook | 393,170 | 1.95 | 4.15 | 4.36 | 7.43 |
| room-1 | 492,287 | 1.91 | 4.21 | 4.18 | 9.36 |
| room-2 | 526,427 | 3.91 | 6.14 | 6.24 | 10.90 |
| upstairs | 392,657 | 4.37 | 5.55 | 4.24 | 5.73 |
| water | 30,120 | 0.57 | 3.00 | 2.55 | 2.75 |
| underwater | 19,120 | 2.82 | 3.78 | 3.10 | 3.22 |
| glass | 490,000 | 3.13 | 3.88 | 4.73 | 6.73 |
| stairs | 491,339 | 3.39 | 5.04 | 8.74 | 10.34 |
| flight | 368,769 | 1.72 | 3.22 | 2.68 | 7.31 |
| teleports | 393,170 | 2.09 | 3.58 | 3.89 | 6.10 |
| storm | 492,287 | 3.77 | 4.53 | 4.14 | 6.81 |

The visible triangles are the ones in the pose's view after culling. By their
lowest 4K times the heaviest small poses are upstairs (11.05 ms), the stairwell
(8.30), the stormed room (8.70) and the glass window (6.19); the cause is not
investigated.

**Finding for shared-sandbox 2e.** How a file is written changes how fast the
server opens it far more than its size. The same unedited small tree takes
0.86–0.95 s to open as the tree's own writer saves it (regions plus 172,000
one-metre cells, which the loader writes into the tree after building it) and
0.27–0.29 s as regions only; the_cube1m, a converted map, has regions only. The
loader's work on 62,000 shaped cubes written as explicit shapes is 4 µs each
(38 to 292 ms of the tree's build) against heights, so the preset writes nine in
twenty that way, which puts its load time next to the_cube's.

**Choices.**

1. The world is a list of ordered operations (boxes, heightfield surfaces,
   one-cube stamps and cube materials), walked once down the tree: a node that
   one operation settles becomes a leaf and a node that one only partly covers
   splits. Leaves then come at the sizes the scene needs. An operation that
   would cut into a sloped cell is an error, so generation fails loudly rather
   than approximating; the seed scan found none.
2. Slopes are heightfield cells on one-metre cubes whose four corners lie in one
   metre band (the terrain lowers corners until they do), roofs and stairs are
   the same kind, and the rest of the shaped cubes are stamps in the edited
   layer. The districts stay flat; the hills and lakes lie in the ring.
3. The files are regions only, in tree order, with explicit shapes for nine in
   twenty shaped cubes (see the finding). The server's own saves would carry
   cells and load more slowly.
4. The small preset is sized by the three measures the acceptance names (leaves,
   triangles and load time), not by the share of each kind: it has 61,994 shaped
   leaves against the_cube's 58,879 but 75,328 water and glass leaves against
   112,672 and more full ones (218,581 against 167,562). The trim and crates
   raise its triangles per leaf; lakes and windows lower them.
5. nub1's edited layer is off by default and refused when switched on, since
   its file would pass the 63 MiB limit; the small preset's is on, and its
   57.2 MB file is 9 MB under the limit.
6. Lights and spawns: 339 and 155 lights as in the two maps, and 16 spawns (the_cube
   has one) outside the doors.
7. Poses stand at positions found by the generator: in lit rooms, by the first
   lake's shore and under its water, in front of a window, at the foot of a
   stair, at a spawn and above the district, a flight path between two
   corners of the play area, teleports between poses and a stormed room.

**Remaining gaps.**

- Not observed in the running app: hosting the presets from the app, a client
  joining them (nub1's join at 53 MB is octree O1a's number for the real map;
  nothing here joined a generated world), walking through them (the stairs
  are slopes of 26.6 and 45 degrees and the doors 2 m by 3 m, none tried with a
  player) and every in-app acceptance box above.
- The Linux CI job has not run the digest tests; Windows is untried.
- nub1's frame times above are contended; the small preset's were rerun on a
  quiet machine.
- nub1's triangles are 25% above the real map, its shaped leaves 80% above and
  its full leaves 19% below: a real map's hidden solid leaves (mixed textures
  inside solid ground) cost nothing in triangles, and the generator makes few.
- The small preset's water and glass share (21% of its unedited leaves) is two
  thirds of the_cube's 33%, and it has more meshes (1,027 chunk meshes in the
  capture tool, against 383), because its detail is spread over a wider area.
- Only structure that resembles a map: materials are the project's eight, the
  lights have no properties and nothing is textured per cube.
- Pieces (a shaped cube cut by an edit) don't occur: the generated trees hold
  no piece leaves, so the cost of cut shapes isn't exercised until editing runs.

## Stage 2: see the whole world

State: running on branch `large-worlds-2` since 2026-11-14. Medium. It runs
beside [lighting](lighting.md) L1.

- **Why** (maintainer, 2026-11-14, after playtesting nub): render distance is
  one of the two first concerns. The camera stops drawing at 180 m
  (`FAR_PLANE` in `render.rs`, and the clusters' `far`), and fog reaches about
  40% at 100 m and its 85% ceiling past 180 m. Both were prototype values. nub
  is 2 km across and its own fog setting is 12.5 km, effectively none.
- **Decided** (design lead, 2026-11-14):
  - Every player sees the same distance. No quality level shortens it, since how
    far you can see matters in a fight, and the lighting rule already keeps
    quality to detail.
  - Until worlds carry their own fog (the [world format](world-format.md)'s
    settings stage, which also imports Sauerbraten's `fog` and `fogcolour`), you
    see to the far side of the world from anywhere in it, and fog thickens only
    near that distance, so nothing pops at the edge.
  - Lights stay lit at any distance: the cluster grid covers the whole view.
  - Depth must stay precise at kilometres. The scene uses a 32-bit float depth
    buffer with depth increasing away from the camera today (`LessEqual`), which
    loses that precision; reversing it is the usual answer, but measure and
    choose.
  - The whole of each library map draws at once on the M5 Pro (the_cube1m's
    world is 717,378 triangles, nub's 755,996), so no level of detail is planned.
    Record how far sun shadows reach against how far you see, and extend the
    cascades only if that is cheap.
- **Out of scope:** editor flight speed, the other half of large-map traversal
  (16 m/s today); streaming.
- **Scope:** `render.rs` (projection, depth, fog constants), `render/clusters.rs`,
  `render/sun.rs`, `common.wgsl` and the other shaders' fog, the capture tool,
  and the rendering section of the [development guide](../development.md).
- **Parallel work:** lighting L1 edits light objects, the importer and the
  editor's light panel; the second to merge rebases.
- **Acceptance:**
  - [ ] Captures of nub from the playtest's spot in the large hall show its far
    walls and the map beyond, and from high above, the_cube1m and the benchmark
    world's nub1 preset can be seen across.
  - [ ] No depth fighting at 1 to 3 km in captures; a light far from the camera
    is lit; captures of the existing poses differ only where more can now be
    seen or the fog is thinner.
  - [ ] GPU throughput at 4K on the M5 Pro, before and after, for the imported
    poses, the benchmark world's poses and a far view of nub, at High and Low,
    with triangles drawn. Within budget, or the record says where and why not.
  - [ ] **Maintainer:** fly around nub and the_cube1m; the distance and the fog
    feel right.

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
