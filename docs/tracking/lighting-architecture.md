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

State: built and partly measured on 2026-11-14, on branch `proto-light-sampling`
(from lighting L1's branch, so imported lights have their original ranges and
colours). A prototype: not merged. The maintainer asked to stop for the night
before the cost runs were finished: the cost table has **one round, not three**
(forward has two), and the sweeps over candidates and samples at 4K, the pass
breakdown of the final settings and the 1080p and 1440p runs were **not run**.
Quality and motion numbers are complete for the settings named below.

- **Question:** does picking a few lights per pixel, weighted by contribution
  and reused across neighbours and frames, give images close to shading every
  light, at a cost that stays flat as lights are added?

### What was built

Behind `RendererOptions::sampling` (capture flag `--sampling`). Without it the
renderer is the forward renderer it was: no new passes, the same pipelines.

**Structure: a G-buffer and compute passes**, not sampling inside the forward
fragment shader. A pixel can merge its neighbours' picks of the same frame only
if those exist when it runs, which a fragment shader drawn in mesh order cannot
promise, and overdraw would write several reservoirs for one pixel. The price
is that the sampled path draws without multisampling (the scene targets are
single-sample whenever the option is on, for the forward comparison frames too),
so its edges are not anti-aliased.

- **Geometry pass** (`fs_gbuffer` in `world.wgsl`, three targets, 20 bytes a
  pixel): the sun, sky ambient and sun highlight already applied; albedo and
  roughness; metallic and emission packed together; the shading and geometric
  normals, octahedral. Position comes from the depth buffer, which the pass
  keeps. Water, glass, the sky, guides and the held weapon are drawn after the
  compute passes in a second pass, as forward fragments with exact light
  shading (water and glass only take highlights from lights), so they are not
  sampled.
- **Cluster lists** (`cs_compact`, a thread a cluster): the cull pass's masks
  become, for each cluster, a list of the lights that reach it, with the running
  sum of each light's largest possible contribution anywhere in the cluster
  (its colour times its falloff at the cluster's nearest point). At most 2,048
  lights a cluster.
- **Initial candidates** (`cs_initial`): `m` candidates for each of `s`
  reservoirs a pixel, each drawn from the pixel's cluster list in proportion to
  that weight (a binary search of the running sums), then weighted reservoir
  sampling on the target value: the light's contribution at this pixel as a
  luminance, unshadowed, or (`vt=2`) times its visibility from the cached shadow
  maps.
- **Temporal reuse** (same pass): the pixel's world position is reprojected with
  the last frame's view and projection; the old reservoir is used if the stored
  view depth agrees within 3% and the geometric normals within about 26 degrees.
  Light identities are remapped by their stable ids, so reservoirs survive
  lights entering and leaving the view. `cap` (16) bounds how many candidates a
  reused reservoir counts for.
- **Spatial reuse** (`cs_resolve`): `k` neighbours at random within 12 pixels,
  accepted by the same depth and normal tests. With `u=1` the merged reservoir is
  divided only by the candidate counts of the pixels whose own target could have
  picked the light (shadows included), the unbiased normalisation of Bitterli et
  al. (2020); without it, by all of them.
- **Shading and accumulation** (same pass): the chosen lights are shaded with
  their cached shadow maps and the same falloff, cone and faint-light test as
  `light_term`; the `s` reservoirs are averaged. Diffuse and specular light are
  kept apart from albedo and blended with the reprojected history (`a`: at least
  that share of the new frame, 1/(frames+1) at first), clamped to the range of
  the pixel's 3 x 3 neighbourhood inside its 8 x 8 tile, so texture detail is
  never blurred by reprojection. Then the pixel is composed, fogged and
  written. A history pixel is dropped by the same depth and normal tests.
- **Switches**, in the capture tool's `--sampling` spec, for example
  `sampled,m=4,s=1,t=1,k=2,a=0.1,vt=2,u=1`: `m` candidates, `s` samples a pixel,
  `t` temporal reuse, `k` spatial neighbours (0 is off), `a` accumulation (0 is
  off), `c` history clamp (0 none, 1 range, 2 variance), `vt` shadows in the
  sampling (0 none, 1 only the first pick is checked, 2 in every target value),
  `u` the unbiased normalisation, `tt=1` one shadow tap for target values
  (shading keeps its taps), `sp` specular in target values, `cap`, `r`,
  `depth`, `normal`. `exact` draws the same G-buffer and shades every light of
  the cluster, as a reference for the G-buffer path; `off` is the forward path.
- **Comparison tool**: `capture --eval label:spec` (repeatable, with
  `--eval-frames N` and `--eval-save on`) renders each pose forward and with each
  spec, after one frame and after N (64), and reports the error of the 8-bit
  final picture (after bloom and tone map) as mean, rmse, maximum, the 99th
  percentile pixel and the share of pixels more than 8 levels off, plus the
  flicker between the last two frames. Poses with a path, an edit storm or
  actors are drawn along their frames, each beside the forward frame of the same
  camera, with the actors walking in circles. The poses added for nub are in
  `scripts/p2-poses.json` (world `.local/nub.json`, converted from `nub.ogz`
  with `--center spawn`).
- **Changed outside the prototype's own files:** the requested
  `max_storage_buffers_per_shader_stage` goes from 3 to 8 (the compute passes
  bind 7; 8 is WebGPU's default); the
  camera and light bind groups are visible to compute; planned lights keep their
  ids outside tests; the scene target gets storage-texture use, and multisampling
  is dropped, only when the option is on. Not compared pixel for pixel with the
  base commit; the forward throughput below (3.4 to 34 ms) matches lighting L1's
  record, and the existing GPU tests pass.

### Measurements

Machine and build: Apple M5 Pro (Metal, macOS 27.2.1), release builds of this
branch, default quality. Cost is GPU throughput at 3840 x 2160, `--throughput
120 --runs 5`, the median of the five runs. Settings, all with the sampled path
drawing without multisampling:

- `fwd`: today's forward shading, default anti-aliasing (2x at 4K). `fwd1x`:
  forward with multisampling off, the fair comparison for the rest.
- `exact`: the G-buffer path shading every cluster light exactly.
- `careful`: `sampled` (m=4, s=1, t=1, k=2, a=0.1, vt=2, u=1, cap=16).
- `lean`: `sampled,m=2,k=1,tt=1`. `lean2`: lean with `s=2`.
- `cheap`: `sampled,vt=1,u=0` (targets before shadows, first pick checked,
  biased merge).

**Cost, 4K, milliseconds a frame** (median of runs; one round, in the order fwd,
fwd1x, exact, careful, lean, lean2, cheap, each its own process; `fwd` is the
median of two rounds, 8.47 to 34.47 individually):

| Pose | fwd | fwd1x | exact | careful | lean | lean2 | cheap |
| --- | --- | --- | --- | --- | --- | --- | --- |
| lightfield-center | 34.30 | 33.01 | 32.52 | 19.80 | 12.38 | 19.81 | 11.04 |
| lightfield-corner | 34.01 | 32.46 | 31.61 | 19.95 | 12.53 | 19.96 | 11.09 |
| nub-light3-pX | 17.06 | 16.57 | 18.17 | 19.06 | 12.57 | 19.48 | 11.35 |
| nub-light3-mZ | 17.46 | 16.95 | 18.73 | 19.10 | 12.50 | 19.47 | 11.23 |
| nub-light5-mX | 14.91 | 14.71 | 16.86 | 18.74 | 11.98 | 19.10 | 10.60 |
| nub-light5-pZ | 19.46 | 18.56 | 20.06 | 21.26 | 14.43 | 21.62 | 13.02 |
| nub-light23-pX | 5.33 | 5.00 | 6.78 | 10.16 | 8.73 | 10.97 | 9.08 |
| nub-light24-pX | 3.75 | 3.54 | 5.60 | 12.52 | 8.96 | 13.23 | 8.32 |
| nub-light24-mZ | 5.06 | 4.80 | 6.77 | 13.45 | 9.44 | 14.09 | 8.64 |
| lightroom-center | 11.02 | 10.53 | 12.54 | 19.24 | 12.08 | 19.26 | 10.68 |
| lightroom-corner | 8.63 | 8.07 | 10.04 | 18.99 | 12.05 | 19.11 | 10.58 |
| imported-spawn | 3.39 | 3.24 | 5.26 | 6.64 | 6.20 | 7.54 | 6.62 |
| imported-tower | 12.48 | 11.74 | 13.60 | 20.71 | 14.11 | 21.14 | 12.77 |
| imported-tower-rooms | 10.20 | 9.69 | 11.63 | 19.24 | 12.50 | 19.58 | 11.08 |

**Lights a cluster** (1920 x 1080 capture report, over clusters that have any):

| Pose | Lights a lit cluster, mean / 95th / max | Lights in view |
| --- | --- | --- |
| lightfield-center | 54.0 / 84 / 113 | 527 |
| lightfield-corner | 52.4 / 96 / 182 | 1000 |
| nub-light3-pX | 19.5 / 38 / 45 | 71 |
| nub-light3-mZ | 27.1 / 44 / 50 | 85 |
| nub-light5-mX | 12.8 / 22 / 23 | 29 |
| nub-light5-pZ | 18.8 / 34 / 43 | 50 |
| nub-light23-pX | 10.0 / 30 / 40 | 71 |
| nub-light24-pX | 4.2 / 7 / 10 | 27 |
| nub-light24-mZ | 11.2 / 28 / 39 | 64 |
| lightroom-center | 15.2 / 23 / 27 | 44 |
| lightroom-corner | 10.8 / 23 / 32 | 64 |
| imported-spawn | 12.1 / 61 / 150 | 333 |
| imported-tower | 17.0 / 29 / 41 | 128 |
| imported-tower-rooms | 18.7 / 33 / 43 | 169 |

What the cost numbers say:

- Forward costs 3.4 to 34 ms and follows the lights a cluster holds, as before.
  Sampled costs do not: lean is 12.0 to 14.4 ms on the lightfield (54 lights a
  cluster, 527 to 1,000 in view), the nub halls (19 to 27) and the light room
  (11 to 15), and careful 18.7 to 21.3 ms on all of them. They fall only where
  most pixels need no local light (imported-spawn 6.2 to 6.6 ms; nub 23 and 24
  8.7 to 13.5). So the cost is flat in the number of lights, and set by pixels
  and by how many shadow lookups each pixel makes.
- It is flat at a high level. Even the lean setting is above forward wherever
  forward is under about 12 ms (light room, imported-tower at 14.1 against 12.3,
  everything that was already within a few ms of the 6.06 ms budget), and none
  of the sampled settings is near 6.06 ms in a view with several lights. Where
  forward is worst it wins clearly: lightfield 34 to 12.4 ms (careful 19.8),
  nub halls 17 to 19.5 to 12.5 to 14.4 (careful is 19.1 to 21.3, no better than
  forward there), imported-tower-rooms 10.0 against 12.5 (no gain).
- The G-buffer path alone costs about 2 ms more than forward without
  multisampling at the low end (`exact`: nub 24 4.8 to 6.8 ms, spawn 3.2 to
  5.3). In an earlier build, with all reuse off and one candidate, the whole
  sampled frame was 5.1 to 5.7 ms on the light room and nub 24 (geometry 1.3 ms,
  initial pass 0.8 to 0.9, resolve 2.0 to 2.2): a floor not far under the budget
  before any quality. An earlier build's serial GPU timestamps for careful at 4K
  (median ms; geometry, initial, resolve): light room 1.3, 6.4, 10.6; lightfield
  1.3, 6.7, 11.1; nub 3 +X 1.4, 6.3, 9.7; nub 24 -Z 2.2, 3.7, 7.4. Shadow lookups
  are the cost: dropping from four taps to one for target values saved about 3
  ms, one fewer candidate or neighbour about 0.9 ms each, and specular in the
  target value made no difference (earlier build, 40 frames x 3 runs: careful
  19.5, 20.0, 19.2, 13.5 on light room, lightfield, nub 3, nub 24; with `tt=1`
  16.5, 17.0, 16.8, 12.1). Each target value with shadows is a lookup into a
  different tile for neighbouring pixels, so the cache coherence forward
  shading gets from every pixel walking the same lights is lost.
- Memory: the G-buffer, histories, reservoirs and lists were 0.64 to 0.68 GiB
  at 4K (capture report, careful, 1 sample) and about 90 to 106 MiB at 1280 x 720.
  `s` samples add 16 bytes a pixel each.

**Error against the forward render of the same frame** (1920 x 1080, 8-bit final
picture, 64 frames to converge; each cell is converged mean error in levels /
share of pixels more than 8 levels off / mean error of the first frame after a
history reset / flicker between the last two frames; `exact` is the G-buffer
reference):

| Pose | exact | careful | lean | lean2 | cheap |
| --- | --- | --- | --- | --- | --- |
| imported-spawn | 0.00 / 0.0% / 0.0 / 0.00 | 0.00 / 0.0% / 0.0 / 0.00 | 0.00 / 0.0% / 0.0 / 0.00 | 0.00 / 0.0% / 0.0 / 0.00 | 0.00 / 0.0% / 0.0 / 0.00 |
| imported-overlook | 0.01 / 0.0% / 0.0 / 0.00 | 0.12 / 0.1% / 1.1 / 0.04 | 0.16 / 0.2% / 1.9 / 0.04 | 0.12 / 0.1% / 1.3 / 0.03 | 0.28 / 1.0% / 1.3 / 0.04 |
| lightroom-center | 0.26 / 0.0% / 0.3 / 0.00 | 1.51 / 3.9% / 9.5 / 0.48 | 1.93 / 6.0% / 16.4 / 0.51 | 1.43 / 3.5% / 11.0 / 0.38 | 1.63 / 4.9% / 10.4 / 0.48 |
| lightroom-corner | 0.26 / 0.0% / 0.3 / 0.00 | 1.73 / 5.5% / 10.1 / 0.54 | 2.20 / 8.2% / 17.1 / 0.56 | 1.64 / 5.0% / 11.6 / 0.42 | 1.89 / 7.0% / 11.3 / 0.54 |
| corridor-view | 0.15 / 0.0% / 0.1 / 0.00 | 2.20 / 3.6% / 13.5 / 0.67 | 2.77 / 6.2% / 25.0 / 0.70 | 2.11 / 3.3% / 15.8 / 0.54 | 2.69 / 5.8% / 17.7 / 0.67 |
| imported-tower | 0.11 / 0.0% / 0.1 / 0.00 | 1.92 / 2.6% / 19.1 / 0.61 | 2.45 / 5.0% / 36.7 / 0.65 | 1.83 / 2.3% / 23.6 / 0.48 | 3.06 / 8.7% / 25.3 / 0.73 |
| imported-tower-rooms | 0.12 / 0.0% / 0.1 / 0.00 | 3.10 / 9.7% / 24.5 / 0.96 | 4.00 / 14.4% / 47.3 / 1.00 | 2.99 / 9.1% / 29.8 / 0.75 | 5.48 / 20.3% / 31.7 / 1.01 |
| lamphall-actor | (moving actor) | (moving actor) | (moving actor) | (moving actor) | (moving actor) |
| lightfield-center | 0.61 / 0.0% / 0.6 / 0.00 | 1.96 / 6.2% / 12.1 / 0.56 | 2.43 / 9.7% / 22.4 / 0.60 | 1.88 / 5.5% / 14.4 / 0.45 | 2.23 / 8.7% / 14.3 / 0.55 |
| lightfield-corner | 0.53 / 0.0% / 0.5 / 0.00 | 1.59 / 3.8% / 10.7 / 0.46 | 1.98 / 6.5% / 19.9 / 0.49 | 1.52 / 3.2% / 12.8 / 0.36 | 1.74 / 5.1% / 12.3 / 0.46 |
| nub-light3-pX | 0.12 / 0.0% / 0.1 / 0.00 | 2.33 / 3.0% / 32.9 / 0.75 | 3.29 / 7.6% / 67.4 / 0.81 | 2.52 / 3.5% / 41.8 / 0.58 | 4.61 / 15.7% / 53.0 / 1.14 |
| nub-light3-mZ | 0.12 / 0.0% / 0.1 / 0.00 | 2.77 / 5.1% / 38.4 / 0.89 | 3.80 / 10.8% / 76.0 / 0.95 | 2.88 / 5.1% / 48.1 / 0.69 | 5.22 / 18.2% / 56.3 / 1.28 |
| nub-light5-mX | 0.12 / 0.0% / 0.1 / 0.00 | 2.90 / 6.5% / 26.5 / 0.88 | 3.90 / 12.0% / 55.4 / 0.94 | 2.99 / 6.8% / 34.0 / 0.69 | 6.02 / 19.6% / 44.9 / 1.19 |
| nub-light5-pZ | 0.12 / 0.0% / 0.1 / 0.00 | 3.66 / 11.3% / 28.6 / 1.11 | 4.76 / 18.1% / 55.5 / 1.15 | 3.59 / 10.7% / 35.2 / 0.85 | 7.33 / 28.2% / 44.9 / 1.36 |
| nub-light23-pX | 0.01 / 0.0% / 0.0 / 0.00 | 0.08 / 0.1% / 0.9 / 0.02 | 0.10 / 0.1% / 1.7 / 0.03 | 0.08 / 0.1% / 1.1 / 0.02 | 0.09 / 0.1% / 0.9 / 0.02 |
| nub-light24-pX | 0.07 / 0.0% / 0.1 / 0.00 | 0.51 / 0.0% / 5.3 / 0.15 | 0.63 / 0.1% / 10.3 / 0.15 | 0.47 / 0.0% / 6.3 / 0.11 | 0.62 / 0.6% / 5.6 / 0.15 |
| nub-light24-mZ | 0.07 / 0.0% / 0.1 / 0.00 | 0.88 / 0.5% / 10.1 / 0.27 | 1.12 / 1.3% / 17.2 / 0.28 | 0.82 / 0.4% / 11.9 / 0.20 | 1.13 / 1.8% / 11.3 / 0.30 |


Pictures are in `.local/p2-review/` (listed in the report). What they show:

- Converged `careful` is close: in the nub hall (`nub-hall-3`) the five lights'
  crisp overlapping shadows on ceiling, floor and slabs match the exact render,
  with a little grain. The first frame (`nub-hall-2`, `lightfield-2`) is heavily
  speckled, and darker on average, because a pixel with one candidate often picks
  a light that adds nothing. `nub-hall-4` (`cheap`, converged) is grainier and
  loses contrast in the shadows.
- In the light room and the lightfield the lighting is saturated, which hides
  error; the same numbers are the least informative there.

**Motion** (1280 x 720, mean error over the path / mean after the first 40
frames / worst frame, 8-bit levels; actors walk in circles; edit storms
invalidate every shadow each frame):

| Pose (frames) | careful | lean | lean2 | cheap |
| --- | --- | --- | --- | --- |
| lightroom-storm (60) | 1.9 / 1.5 / 10.0 | 2.7 / 2.0 / 17.3 | 2.0 / 1.4 / 11.6 | 2.0 / 1.6 / 11.1 |
| imported-flight (788) | 1.3 / 1.4 / 29.0 | 1.9 / 2.0 / 51.5 | 1.5 / 1.6 / 34.4 | 1.6 / 1.7 / 39.2 |
| imported-teleports (450) | 2.0 / 1.9 / 26.4 | 2.8 / 2.6 / 50.0 | 2.1 / 1.9 / 31.5 | 2.9 / 2.8 / 34.4 |
| imported-storm (60) | 2.8 / 2.0 / 20.1 | 4.3 / 2.6 / 38.5 | 3.1 / 1.9 / 24.7 | 3.9 / 3.0 / 26.9 |
| lightroom-actors-16 (60) | 5.1 / 5.0 / 12.2 | 8.3 / 8.1 / 21.3 | 6.0 / 5.9 / 14.3 | 5.6 / 5.5 / 14.4 |
| lightfield-actors-16 (60) | 2.8 / 2.3 / 12.9 | 4.0 / 3.0 / 23.8 | 3.0 / 2.4 / 15.2 | 3.1 / 2.6 / 15.8 |
| nub-flight (350) | 5.5 / 5.4 / 31.3 | 7.8 / 7.5 / 63.2 | 6.2 / 6.0 / 39.5 | 7.7 / 7.4 / 51.0 |
| nub-teleports (240) | 2.3 / 1.9 / 40.2 | 3.8 / 3.1 / 77.5 | 2.7 / 2.2 / 49.8 | 3.9 / 3.1 / 58.5 |
| nub-storm (60) | 3.7 / 2.5 / 34.5 | 6.5 / 3.5 / 69.2 | 4.5 / 2.6 / 43.3 | 6.8 / 4.9 / 55.1 |

Error after a jump (frames after the jump, mean error), history rejected by the
depth and normal tests:

| Pose, setting | 0 | 1 | 2 | 4 | 8 | 16 | 32 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| imported-teleports, careful | 15.9 | 8.5 | 5.8 | 3.8 | 2.5 | 1.9 | 1.8 |
| imported-teleports, lean | 30.1 | 17.0 | 11.5 | 7.0 | 4.1 | 2.6 | 2.3 |
| imported-teleports, lean2 | 19.2 | 10.8 | 7.7 | 4.9 | 3.0 | 2.0 | 1.7 |
| imported-teleports, cheap | 20.9 | 11.0 | 7.4 | 4.8 | 3.3 | 2.7 | 2.6 |
| nub-teleports, careful | 18.5 | 9.0 | 5.8 | 3.6 | 2.4 | 1.7 | 1.6 |
| nub-teleports, lean | 36.7 | 20.7 | 13.3 | 7.3 | 4.0 | 2.5 | 2.2 |
| nub-teleports, lean2 | 23.1 | 12.5 | 8.5 | 5.1 | 3.0 | 1.9 | 1.7 |
| nub-teleports, cheap | 28.0 | 13.7 | 8.6 | 5.4 | 3.8 | 3.1 | 3.0 |

- Flights and teleports show no smeared or stale lighting: history is rejected
  where depth or normals disagree, and static lighting reprojects cleanly. A cut
  costs about 4 frames to reach 4 to 7 levels and about 16 to reach 2 to 3 for
  careful; lean needs about twice as long. Newly uncovered areas are as noisy as
  a first frame until their history builds.
- Flying through nub's halls stays at 5 to 8 levels (`motion-flight` pictures):
  visibly grainier than stills.
- **Moving actors are the weak case.** There are no motion vectors, so a moving
  actor's pixels find no history and are shaded at first-frame quality (visible
  speckle on their bodies, error 5 to 8 in the light room), and their floor
  shadows come out much fainter than in the forward render
  (`motion-actors` pictures). Turning accumulation off did not remove the error
  (6.8 against 5.0), so most of it is the actors' own pixels; raising samples to
  4 with no accumulation cut it to 3.7. I did not separate shadow lag from that.
- Edit storms (every shadow redrawn every frame) cost about 0.5 to 1 level more
  than stills and add no visible flicker in the numbers (1 level).
- Not tested: a real edit (a block placed or removed, a light moved or recoloured)
  and how long its change takes to show; bounce light.

**What the quality work found** (earlier builds of the same code, 1280 x 720,
64 frames; nub 3 +X is the hall with five 44 m lights):

- Reservoirs merged by dividing by every contributing pixel's count are biased
  where shadows differ between neighbours. Averaged over 400 frames the error
  stayed at 5.3 levels with bias -5.3 and 18.5% of pixels over 8 levels off (the
  flicker was 0.08, so it is bias, not noise); with the normalisation by the
  pixels that could have picked the light it was 1.7, 0.07% over 8; plain
  sampling without reuse, averaged the same way, 3.3.
- Without shadows in the target values, lights behind walls are picked and shade
  nothing: converged error 33 levels in the hall, against 5.4 with shadows in the
  targets (biased merge) and 2.5 with both. The candidates drawn in proportion to
  the cluster weights, not uniformly, cut the lightfield's first-frame error from
  29 to 13.
- Ablation of the reuse stages, converged / first-frame mean error (light room,
  lightfield, nub 3 +X), careful as the base: base 1.5/10.0, 2.0/12.8, 2.5/34.4;
  `k=4` 1.3/8.1, 1.7/10.2, 1.8/25.2; `m=8` 1.5/7.4, 1.9/9.1, 2.5/21.8; `m=16`
  1.3/5.6, 1.8/6.9, 2.4/14.6; `s=2` 1.1/7.0, 1.6/8.7, 1.8/21.8; `s=4` 0.9/4.9,
  1.3/6.1, 1.4/14.4; `a=0.05` 1.3/10.0, 1.7/12.8, 2.1/34.5; `t=0` 2.3/10.0,
  3.0/12.8, 7.0/34.4; `vt=0` 2.1/12.6, 3.2/20.2, 33.9/82.7. Temporal reuse and
  shadow-aware targets matter most; spatial neighbours and samples help the
  first frame.

### Verdict, as far as the numbers go

- **Image:** sampled lighting with shadows in the sampling and the unbiased
  merge gives converged stills within about 0.1 to 3.7 levels of the exact
  picture on average (up to 11% of pixels over 8 levels in rooms with several
  lights), and lights and their shadows come out in the right places. It is not
  the same picture frame to frame: a cut, a newly uncovered area or a moving actor
  shows grain for several frames. Unshadowed targets or a biased merge give a
  visibly worse hall.
- **Cost:** flat in the number of lights, as hoped, but at about 12 ms (lean) to
  20 ms (careful) at 4K on the M5 Pro, with 0.7 GiB extra at 4K and no
  multisampling. That beats forward only where forward is above about 12 ms and
  loses where forward is under it. The 6.06 ms budget at 4K is out of reach of
  this design on this machine: its own floor is about 5 ms with no reuse.
- So the numbers do not support replacing forward shading with this as the
  general method, and do not rule out sampling as part of a design at a lower
  internal resolution (for example with temporal upscaling) or with fewer
  shadow lookups. Whether a coarser, half-resolution sampling, cached visibility,
  or an overflow-only use above some lights a cluster would pay is untested; the
  last would be a hybrid by density, which the lighting rule would need the
  maintainer's word on.
- The minimum specification is not measured. Both costs scale with shadow lookups
  and memory bandwidth, and a Steam Deck has much less of both than this machine,
  so expect the 4K figures to be large; this is a guess from the hardware, not a
  measurement.

### Not finished or not measured

- Cost: rounds 2 and 3 of the main table, the sweeps of candidates, samples and
  reuse stages at 4K, a pass breakdown of the final settings, and 1080p and
  1440p throughput. The pass breakdown above is from earlier builds.
- Moving actors need motion vectors; actor shadow lag is not separated from
  actor noise. Real edits and moving lights are untested. Water and glass are not
  sampled. No anti-aliasing in the sampled path.
- Observed only in the capture tool, never in a live window. The minimum
  specification, other GPUs and other drivers are untested. Forward output was
  not compared pixel for pixel with the base commit.
- Prototype shortcuts: nearest-pixel history, a 2,048-light cluster list cap,
  reservoirs 8 bytes with 14 bits of count, the geometry pass duplicates the
  forward fragment code.

### Checks

Run after the last code change: `cargo fmt --all -- --check` clean; `cargo clippy
--workspace --all-targets --locked -- -D warnings` clean; `cargo test --workspace
--locked` passed (the private-world tests were not run); the GPU tests (13 passed,
12 existing and `gpu_sampled_lighting_modes_match_the_forward_render`). The
release capture binary used for the cost runs was built before two comment-only
shader edits.

## What the results decide

- Where a frame's light comes from: sampled lights with shadow-map or ray
  visibility, a surface cache, or a combination.
- How bounce light and sky occlusion are done, and whether rays through the
  octree carry them.
- The light model, in physical units, and exposure that adapts like a camera.
  Lighting L1's light objects, editing and import stay; its falloff and
  brightness unit, chosen to imitate Sauerbraten's lightmaps, wait for this.
- The work orders that follow, and what they replace in the renderer.
