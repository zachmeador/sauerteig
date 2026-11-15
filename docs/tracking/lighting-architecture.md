# Lighting architecture

State: research. On 2026-11-14 the maintainer asked that lighting be designed
from first principles rather than tuned to particular maps, and approved two
prototypes, P1 and P2, before the architecture is written.

## Why

- Every frame, every pixel combines every light that reaches it and reads each
  light's cached shadow map ([renderer](renderer.md) stage 2 and N2). The cost
  is pixels times lights per pixel, and builders decide how many lights
  overlap, so no frame budget can hold. Imported lights at their original
  ranges took some views from 4–6 ms to 15–30 ms at 4K on the M5 Pro
  ([lighting](lighting.md) L1's record), and the 1,000-light scene takes 28 ms.
- Shadow lookups are about 70% of that cost in the slowest views; the rest is
  the per-light maths.
- Copying another engine is not the answer either. Tesseract, Sauerbraten's
  successor, lights live as we do, caching shadow maps, and its design notes
  say its speed was limited by the same per-light maths. Several of its choices
  worked around 2013 hardware: 8-bit buffers and gamma-space lighting, screen
  space ambient occlusion, and bounce light from the sun only.

## Requirements

- Players build worlds of any size, with any number of lights of any size, on
  hardware down to the [minimum specification](platforms.md).
- The world changes only through edits, which are local. The camera, players,
  projectiles and other actors move every frame.
- Every light lights and shadows the same way: no caps, no rank or distance
  cutoffs, no near/far hybrids. The rule keeps cheap tricks out (maintainer,
  2026-11-14).
- Direct light and shadows follow an edit at once; bounce light may lag by
  about half a second (maintainer, 2026-11-11).
- A frame's cost must not grow with how many lights or cubes a builder places.
  The work after an edit grows with what the edit changed, as the octree's
  meshing already does.
- Vulkan, DirectX 12 and Metal, with optional hardware features used only where
  an adapter has them.

## Directions to weigh

- **Live per-pixel lighting,** as now: cost grows with lights per pixel.
- **A cache of lighting on surfaces,** updated near each change: Sauerbraten's
  lightmaps computed at runtime, or Unreal's Lumen surface cache. Moving things
  need their own light and shadows on top.
- **Sampled lighting:** each pixel picks a few lights at random, weighted by how
  much each would add, and reuses good picks from neighbours and the previous
  frame (ReSTIR, 2020). Cost per pixel stays flat however many lights there
  are; the price is noise, smoothed over frames.
- **Rays through the world's own octree on the GPU,** without hardware ray
  tracing, which wgpu offers only experimentally. The world is already a
  sparse octree of cubes, the structure ray tracing needs; Teardown lit a voxel
  world by stepping rays through its data in compute shaders. Rays could give
  shadows, bounce light and occlusion from one structure.
- **Temporal upscaling** (AMD's open-source FSR 2 and 3, Apple's MetalFX):
  render below 4K and reconstruct it, cutting every per-pixel cost.

## P1: rays through the octree on the GPU

State: measured on 2026-11-14 on branch `proto-octree-rays`; verdict below. A
prototype: recorded here, not merged, and the default renderer is unchanged.

- **Question:** can the GPU step rays through the world's octree fast enough to
  give every pixel a few shadow or bounce rays each frame, at 1080p and 4K on
  the M5 Pro, and plausibly at the minimum specification?
- **Build:** a compact GPU copy of the octree in storage buffers (branches, and
  full, shaped and piece leaves with their corners), and compute shaders that
  trace rays through it: any-hit for shadows and closest-hit for checking.
  Updating the copy after an edit touches only the nodes the edit changed.
- **Measure:**
  - rays per second for primary rays, shadow rays toward random lights and
    random hemisphere rays, on the_cube1m, nub and the benchmark world's
    presets;
  - what one to four rays a pixel cost a frame at 1080p and 4K;
  - the GPU copy's memory and upload time, and the update time after a one-metre
    and a 64 m edit;
  - agreement with the CPU's `World::raycast` over thousands of random rays,
    shaped and piece leaves included, and what exact shaped-leaf tests cost
    against an approximation.
- **Minimum specification:** nobody has the hardware here, so estimate it from
  published GPU throughput and say that it is an estimate.

### P1 results

**Machine and build.** Apple M5 Pro (Metal, wgpu 24), release build, GPU
timestamps around each dispatch (median of 3–60 dispatches), cross-checked
against wall time of the same dispatches with one wait; the two agree within
5%. Another job (P2's capture tool) shares this GPU, so every timing ran only
while no `capture` process existed before or after it, and an earlier full run
that overlapped with P2 was thrown away. Worlds: the_cube1m (8,192 m root),
nub converted from `nub.ogz` (2,048 m root, 155 lights) and the benchmark
world's nub1 preset (2,048 m root, 155 lights). nub has no capture poses; its
six poses stand 1 m under six lights, facing the horizontal direction with
a wall about 10 m away. The poses of the_cube1m are six of the `imported-*`
poses, and nub1's are six of its generated poses.

**What was built** (all in the worktree, nothing wired into the renderer):
- `crates/client/src/octree_rays.rs` and `octree_rays/gpu.rs`: the GPU copy
  and the compute harness. `octree_rays.wgsl`: the shaders.
  `crates/client/examples/octree_rays.rs`: every measurement here
  (`cargo run --release -p sauerteig-client --example octree_rays -- all --worlds cube,nub,nub1`
  from the repository root, with the worlds in `.local/`).
  `crates/core/src/octree/export.rs` and `World::octree()` give read-only
  handles on the tree's nodes (`TreeNodeRef`) and a shape's lattice triangles.
- **The copy:** two storage buffers of 32-bit words. `nodes` holds one
  8-word block per branch; a word is a 3-bit kind (empty, full, shaped, piece,
  branch) and a 29-bit index. `tables` holds shape records (up to 12 triangles
  as 4-bit lattice coordinates, plus the shape's box) deduplicated by shape, and
  piece records (source origin, size, shape). Cube and surface materials are
  left out, so water and glass leaves read as empty, and a branch whose eight
  children are the same empty or full word becomes that word. Shared
  copy-on-write subtrees are encoded once.
- **The tracer:** a stack traversal in the root's local frame in eighths of a
  metre. It mirrors axes so the ray runs in +x, +y, +z, descends to the leaf
  holding the ray's position, tests the leaf, then steps to the neighbour cell
  through the exit face and climbs only as far as that face needs, with an
  ancestor-block stack of 20 words per ray. Any-hit and closest-hit are one
  function chosen by a pipeline constant, as are the two shaped-leaf modes.
  Eight storage buffers per shader stage are used, which is wgpu's default
  limit and above the renderer's current 3.
- **Leaf tests.** Full leaves: exact slab test. Shaped leaves, exact: the
  leaf's own boundary triangles (Moller-Trumbore, barycentric tolerance
  2e-5). Pieces, exact: the source's triangles kept where the hit lies in the
  leaf's box, plus the cut face where the ray enters the box inside the
  source's solid, found by counting crossings of a skew ray. Approximation: the
  ray against the shape's bounding box (for a piece, the source's box clipped
  to the leaf's).

**Size and upload.** One upload of the whole copy: the_cube1m 1.92 MiB
(1.8 ms once warm, 3.5 ms first), nub 1.91 MiB (1.8 ms), nub1 3.34 MiB (1.9 ms),
against 12–20 MiB for the CPU tree. Building the copy from scratch takes
10–16 ms on one CPU thread. Without merging empty and full words the copy was
2.50, 2.34 and 4.00 MiB.

**Edits** (through `World::apply_root_edits`, then re-encoding only what
the edit changed: a branch found in the copy by the address of its shared
children is skipped with its subtree). Old blocks the edit replaced stay in the
buffer as garbage until a rebuild; the 64 m block left 2,345 on nub.

| edit | `apply_root_edits` | re-encode | bytes written | full re-encode |
|---|---|---|---|---|
| 1 m cube, the_cube1m / nub / nub1 | 0.01 ms | 0.003 / 0.003 / 0.004 ms | 416 / 352 / 352 B | 11.6 / 8.8 / 15.2 ms |
| 64 m x 1 m x 64 m slab (4,096 roots) | 8.1 / 8.8 / 6.5 ms | 0.13 / 0.20 / 0.16 ms | 45 / 39 / 47 KB | 11.4 / 8.7 / 15.2 ms |
| 64 m x 8 m x 64 m block (32,768 roots) | 52 / 55 / 47 ms | 0.10 / 0.37 / 0.28 ms | 24 / 88 / 101 KB | 11.2 / 9.2 / 15.3 ms |

The upload after each edit took 1.4–4.9 ms measured as queue write, submit and
wait, which is submission latency and not the 0.4–100 KB; inside a frame the
write rides on that frame's submit. Rays over the updated copy agree with the
CPU as well as over a fresh one (20,000 rays per world after the edits).

**Agreement with `World::raycast`** (100,000 rays per world, half from
surface points, half aimed into shaped leaves, 32 m limit, origins outside
solid, tolerance 5 mm). The pieces row is a synthetic world, because none of
the three real worlds holds a piece: 144 random 8 m shaped sources with 40
metres cleared in each, which leaves 4,522 pieces and 644 full leaves.

| world | rays hitting | disagree beyond 5 mm | hit/miss disagree | of those, errors over 1 cm |
|---|---|---|---|---|
| the_cube1m, full leaves | 35,524 | 102 (0.29%) | 0 | few |
| the_cube1m, shaped leaves | 52,360 | 104 (0.20%) | 0 | few |
| nub, full / shaped | 46,556 / 45,550 | 7 / 12 | 0 | |
| nub1, full / shaped | 38,329 / 39,658 | 8 / 2 | 0 / 1 | |
| pieces world, piece leaves | 59,481 | 1 | 1 | |
| pieces world, full leaves | 811 | 0 | 0 | |

Median error is 0.2 µm to 0.2 mm, and the 99th percentile 0.2–2 mm. Every
disagreement I examined was a ray within about 0.5 mm of a cell edge, a
triangle edge, or a surface it grazes at under 3 degrees. The cause is `f32`:
positions in the root's frame carry 0.5 mm of rounding at the_cube1m's 8 km
root (0.12 mm at 2 km), which is why the_cube1m, whose root is four times
wider, has ten times the disagreements of nub. Origins nearer than 1 mm to a
surface cannot be told from the right side of it. One fix is to split a ray's
origin into a whole-unit part and a fraction, as camera-relative rendering does;
it was not built. The shadow-ray mismatch, the share of random-length rays on
which the two disagree about hitting anything, is 0.000–0.003% for the exact
test. Rays starting inside solid are not compared: the tracer reports a hit at
distance 0, the CPU the exit.

**Exact against approximate shaped leaves.** The bounding-box approximation
costs less and is wrong often:

| | exact | box approximation |
|---|---|---|
| primary rays, median Mrays/s (the_cube1m / nub / nub1) | 1241 / 1081 / 1003 | 1764 / 1636 / 1079 |
| shadow rays, median Mrays/s | 149 / 198 / 603 | 255 / 337 / 703 |
| hemisphere rays, median Mrays/s | 269 / 230 / 422 | 475 / 412 / 471 |
| shadow-ray mismatch, rays hitting shaped leaves | 0.001% / 0.000% / 0.001% | 4.1% / 1.7% / 10.1% |
| the same, rays hitting full leaves | 0.003% / 0.000% / 0.000% | 1.1% / 1.2% / 0.6% |
| pieces world: primary / shadow / hemisphere rays | 339–1052 / 66–70 / 120–154 | 2257–2800 / 244–1407 / 793–2837 |
| pieces world: rays hitting pieces, mismatch | 0.002% | 45% |

So the exact test costs 7–80% more on the real worlds (3 to 20 times more on
the piece world, where nearly every leaf is a piece with 12 triangles and a
containment test), and the approximation wrongly occludes or frees about one
shadow ray in ten near shaped leaves. The approximation is not worth using.

**Throughput** (exact, 1920x1080 grids, the median over six poses with the
range; Mrays/s is rays traced in the dispatch over its GPU time, shadow and
hemisphere dispatches include generating the rays and skipping the ones that
face away or have no light in range):

| rays | the_cube1m | nub | nub1 |
|---|---|---|---|
| coherent primary rays, closest hit | 1241 [449–2901] | 1081 [830–1632] | 1003 [413–1886] |
| shadow rays toward a random light within 24 m, any hit | 149 [23–829] | 198 [133–422] | 603 [0–1701] |
| cosine-hemisphere rays limited to 16 m, any hit | 269 [173–3159] | 230 [158–384] | 422 [337–477] |
| the same, closest hit | 270 [172–3135] | 230 [158–384] | 428 [337–478] |

Any-hit saves nothing on 16 m hemisphere rays, because most either hit
nearby or travel the whole 16 m. The slowest poses are those looking across
dense shaped geometry (the_cube1m's `imported-water` and the tower, nub's
rooms). Fixed cost of a dispatch over 2 million mostly idle threads is about
0.5 ms.

**What 1, 2 and 4 rays a pixel cost** (GPU ms for the dispatch, exact test,
median over six poses [fastest–slowest]; a shadow ray per sample goes to a
random light within 24 m, skipping surfaces facing away, so shadow samples
traced are 0.4–0.7 per pixel per sample on nub and the_cube1m and 0.46
on nub1):

| | rays a pixel | the_cube1m | nub | nub1 |
|---|---|---|---|---|
| 1920x1080, hemisphere | 1 / 2 / 4 | 7.9 / 15.7 / 31.5 | 9.1 / 18.1 / 36.3 | 4.5 / 8.8 / 17.7 |
| 1920x1080, shadow | 1 / 2 / 4 | 4.8 / 9.6 / 19.2 | 7.7 / 15.4 / 30.9 | 1.0 / 1.7 / 3.6 |
| 3840x2160, hemisphere | 1 / 2 / 4 | 31 / 63 / 126 | 36 / 73 / 144 | 17.7 / 35 / 70 |
| 3840x2160, shadow | 1 / 2 / 4 | 18.9 / 37.5 / 74.6 | 30.3 / 60.6 / 121 | 3.9 / 6.7 / 12.8 |

The slowest single poses at 4K, 1 ray a pixel: hemisphere 47 ms (the_cube1m
`imported-water`), 52 ms (nub), 22 ms (nub1); the 4K budget at 165 Hz is
6.06 ms for the whole frame. Cost is close to linear in rays.

**Minimum specification (an estimate, not a measurement).** The Steam Deck's
GPU is 8 RDNA2 compute units at up to 1.6 GHz, about 1.6 TFLOPS of FP32, with
about 88 GB/s of memory shared with the CPU, and about 1 MB of L2 where the
whole 2–3 MiB copy sits in the M5 Pro's far larger caches. The M5 Pro's 20-core
GPU is reported at 8.3–10.3 TFLOPS (Apple publishes no figure) and 307 GB/s
([Wikipedia](https://en.wikipedia.org/wiki/Apple_M5),
[CPU-Monkey](https://www.cpu-monkey.com/en/cpu-apple_m5_pro_18_cpu_20_gpu),
[Flopper](https://flopper.io/gpu/apple-m5-pro)), and the machine here may have
the 16-core part. Tracing is bound by dependent memory reads and divergence as much as
arithmetic, so I take 4–8 times slower than the M5 Pro. One hemisphere ray a
pixel at the Deck's own 1280x800 (1 Mpixel, half of 1080p) would then cost
about 9–36 ms in dense scenes (the M5 Pro's 2.3–4.5 ms for that pixel count, times 4–8),
against a 16.7 ms frame that also has to draw the world. Nothing here was run on such a GPU, and
a GPU without the 8 storage buffers or 128 MiB binding size this used would
need a different layout (the copy fits in 4 MiB).

**Verdict.**
- **As the way to light every pixel each frame: not feasible.** Rays are exact
  where it counts (99.7–100% of rays within 5 mm of `World::raycast`, and hit/miss disagreements
  only at grazing and edge cases at `f32` resolution) and the copy is
  small, cheap to build and cheap to keep current: a one-metre edit re-encodes
  in 3-9 µs and writes under half a kilobyte, a 64 m edit re-encodes in
  0.1-0.4 ms and writes under 0.1 MB. But a ray a pixel costs 4.5–9 ms at 1080p and
  18–36 ms at 4K on the M5 Pro in typical views (up to 52 ms), three to eight
  times the 4K frame budget for one ray, before shading. The measured
  throughput, 0.2–0.6 Grays/s for secondary rays, is what traversal in
  compute shaders without hardware tracing gives for incoherent rays through a tree
  this size; I did not profile where the time goes.
  I did not tune it beyond merging empty and full words (which took the water
  pose from 6.3 to 4.6 ms for primary rays); starting rays below the root and
  sorting rays by direction could plausibly give 1.5–2 times, not 10.
- **As a budgeted tool: plausible.** About 0.25 Mrays a frame (one ray per 4x4
  pixels at 1080p, or per 6x6 at 4K) costs about 0.5–1.2 ms on the M5 Pro. That
  fits visibility for a surface cache or probes, for bounce light that may lag
  half a second, and for a few shadow rays per frame, but not per-pixel shadows
  or ambient occlusion at full resolution. On the minimum specification it is
  an estimate of 4-8 times more.
- **Remaining uncertainty:** the Deck estimate; `f32` precision at roots wider
  than 8 km and the unbuilt split-origin fix; no piece-heavy real world (the
  figures for pieces come from a synthetic one, where the exact test is 5
  times or more slower than the approximation, and a real world edited heavily would
  fall between); the tracer's own tuning headroom; dynamic actors (not
  traced); the copy keeps garbage blocks after edits and has no compaction;
  materials, glass and water are not in the copy, so coloured bounce or
  glass would need more; and the light-range and pose choices here, in
  particular nub's six invented poses.
- **Rough edges noted:** the incremental update keeps every old branch alive
  through its handle (memory grows until a rebuild); the pieces world's
  disagreements (2 in 100,000: a ray through a triangle edge, and one at a
  tolerance of 0.1 mm on a box face) were not reduced further; GPU tests were not
  run because no renderer code changed.

## P2: sampled lights per pixel

State: running on branch `proto-light-sampling` since 2026-11-14, from lighting
L1's branch so imported lights have their original ranges. A prototype:
measured, recorded here, and not merged.

- **Question:** does picking a few lights per pixel, weighted by contribution
  and reused across neighbours and frames, give images close to shading every
  light, at a cost that stays flat as lights are added?
- **Build,** behind a capture flag in today's renderer: candidates from each
  pixel's cluster list, weighted reservoir sampling, temporal reuse with
  reprojection and depth and normal checks, spatial reuse from neighbours, and
  shading of the chosen lights with visibility from the cached shadow maps,
  then accumulation over frames.
- **Measure:**
  - frame cost against lights per pixel on the 1,000-light scene, nub with its
    imported lights, the light room and the_cube1m, beside today's exact
    shading;
  - error against the exact render in stills, and noise;
  - stability in motion: the flight and teleport poses, the edit storm and
    moving actors, looking for smearing and lag.

## What the results decide

- Where a frame's light comes from: sampled lights with shadow-map or ray
  visibility, a surface cache, or a combination.
- How bounce light and sky occlusion are done, and whether rays through the
  octree carry them.
- The light model, in physical units, and exposure that adapts like a camera.
  Lighting L1's light objects, editing and import stay; its falloff and
  brightness unit, chosen to imitate Sauerbraten's lightmaps, wait for this.
- The work orders that follow, and what they replace in the renderer.
