# Independent light nodes and lighting

State: partial. Node/model separation is complete within the scope below.

## Outcome

Place and edit a light independently of any model. Add a lamp prop only when the
builder wants one. Support intentional point/spot lighting and preserve imported
light settings rather than substituting a fixture with unrelated defaults.

## Current behavior

`Light` has no physical mesh in play, uses an editor-only selectable marker, and
emits from its stored position. Wall and hanging lamp models are separate
decorative props. Their glass is emissive, but a separate node supplies
illumination. Existing saves keep their object IDs; no map rewrite is needed to
remove forced fixtures from Light nodes. Worlds that relied on lamp props alone
for illumination now need separate Light nodes.

Every Light node in view lights the scene as a warm omnidirectional point light
with a 12 metre range and 16 units of intensity, through the clustered renderer
([renderer](renderer.md) stage 2 and N2), and every one of them is shadowed: the
shadow atlas grows with the lights and shrinks tiles, to 64² and 32², rather than
drop a light, so the_cube1m draws all 339 of its lights, up from 149. Each level
bounds the atlas's memory, 60 MiB at Low and 256 MiB at the others, and a light
that no tile can hold within the bound takes pages past it. There is no limit on
the lights in view or on the lights a fragment shades, and every shadowed light
shadows players and other moving actors, wherever it ranks. Spot lights exist in
the renderer but nothing places one yet. Color, intensity, radius, direction,
and cone are fixed defaults, not editable or imported, which is a substantial
limitation for imported lights: a dense imported map is lit at the same strength
everywhere. The sun is also a fixed environment light; no baked lighting or
global illumination is implemented.

Shading every light is not free: at 4K on an M5 Pro, the densest imported views
and the lightroom already miss the 6.06 ms budget at the default level, with or
without N2. The numbers are in [renderer](renderer.md) under N2.

## Remaining work

- [x] Verify model-free nodes, editor picking, independent props, and
  illumination from the stored node position in native and browser rendering.
- [ ] Define editable light properties and preserve them through multiplayer,
  save/reopen, copy/paste, blueprints, and undo. Planned as typed object
  properties in the [world format](world-format.md).
- [ ] Render point and spot lights with suitable shadows and a measured GPU
  budget. Implemented in [renderer](renderer.md) stage 2 and measured on an
  Apple M3 Pro; play checks in the running app are still to do.
- [ ] Import and apply source light properties and sun/environment settings,
  after the two items above, as part of
  [legacy map restoration](legacy-import.md).
- [ ] Compare representative indoor/outdoor lighting with original maps, noting
  deliberate renderer differences rather than claiming exact reproduction.
- [ ] Light and shadow every light the same way: every light in view lit, with
  no cap on the lights a fragment shades, every light shadowed (339 of 339 on
  the_cube1m instead of 149), and every shadowed light casting player shadows
  instead of the eight most important. Quality settings may vary only detail,
  never which lights are lit or shadowed (maintainer, 2026-11-11). Implemented
  in [renderer](renderer.md) N2 and checked on the capture tool and the GPU
  tests on an M5 Pro; the maintainer's look in the running app and a Vulkan and
  a DirectX 12 GPU are still to do.

## Evidence

On 2026-10-24, native and browser checks showed editor-only markers,
illumination without a fixture, and an independently visible lamp prop. Browser
marker picking selected the Light node at its stored coordinates. A newly placed
native node survived switching worlds and reopening. Regression tests cover no
physical Light mesh, non-shadowing guides, placement/picking/emitter alignment,
and decorative lamp meshes. These checks do not establish original-map lighting
fidelity.
