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

State: running on branch `proto-octree-rays` since 2026-11-14. A prototype:
measured, recorded here, and not merged.

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
