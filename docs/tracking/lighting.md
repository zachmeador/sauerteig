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
  [legacy map restoration](legacy-import.md). Lights' range and colour come
  first, in L1 below; ambient, sky, sun and fog wait for world settings.
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

## L1: lights keep their range and colour

State: on branch `lighting-l1`, held (design lead, 2026-11-14). Its light
objects, editing and import are done and pass every check. Its falloff and
brightness unit, chosen to imitate Sauerbraten's lightmaps, wait for the
[lighting architecture](lighting-architecture.md), since imported lights at
their original ranges made some views three to five times slower at 4K and the
maintainer asked for lighting designed from first principles. Octree O2 waits
for it, because both change objects in edits.

- **Why** (maintainer, 2026-11-14, after playtesting nub): light distance is one
  of the two first concerns. A big room in nub that the original lit with one
  large light is dark. Every imported light has the same 12 m reach and colour
  today, while nub's 155 lights reach 11 to 44 m in the original (median 25 m);
  154 of them reach further than here.
- **Decided** (design lead, 2026-11-14):
  - A light object carries its colour, range and brightness in the world. Format
    3 gains them as optional fields of a light object, and a file without them
    gets today's values (the warm colour, 12 m and 16). Shared-sandbox 2e's
    format carries them, and the [world format](world-format.md)'s later
    object-properties stage keeps them as typed properties.
  - They travel with the light wherever objects go: the join, placing, moving
    and deleting, undo and redo, copy and paste, blueprints and saves. Changing
    them in the editor is an edit like moving the light.
  - The importer copies each Sauerbraten light's radius and colour, scaled by
    `--units-per-metre`. Sauerbraten's lightmaps fall off linearly to a light's
    radius. Choose how radius and colour map to range, colour and brightness so
    imported rooms are lit about as far and as brightly as in the original. If
    that needs another falloff, it applies to every light, under the lighting
    rule, and the captures show the change on the built-in scenes.
  - Out of scope: spotlights (nub has none; they need a direction and cone on
    the light object), and the map's ambient, sky, sun and fog, which wait for
    world settings. The import report keeps listing them.
- **Scope:** core's object types and their validation (`CONTRACT.md`), the
  messages that carry objects, the server's object edits, the client's light
  descriptions (`lighting.rs`) and the editor's panel for a selected light, the
  importer's light mapping and its README, [editor controls](../editor.md) and
  this document.
- **Parallel work:** large-worlds stage 2 edits `render.rs`, the clusters, the
  fog and the sun cascades; if both edit the Settings or editor panels, the
  second to merge rebases. Octree O2 waits for this order.
- **Acceptance:**
  - [ ] nub imports with its lights' ranges and colours. Captures of nub's large
    halls before and after, and complex against the original game's
    `complex.png` by eye.
  - [ ] Captures of the built-in scenes match, unless the falloff changed; then
    the record lists the differences.
  - [ ] A light's colour, range and brightness survive save and reopen, a join,
    undo and redo, copy and paste, and a blueprint round trip. Files without
    them load with today's values. Tests cover each.
  - [ ] **Maintainer:** select a lamp, change its colour and range, undo it; nub's
    big rooms are lit as you remember them, once the library is re-imported.
- **Afterwards:** the design lead re-imports the library with the merged
  importer and, with the maintainer's word, swaps it in, as after octree O3.

## Evidence

On 2026-10-24, native and browser checks showed editor-only markers,
illumination without a fixture, and an independently visible lamp prop. Browser
marker picking selected the Light node at its stored coordinates. A newly placed
native node survived switching worlds and reopening. Regression tests cover no
physical Light mesh, non-shadowing guides, placement/picking/emitter alignment,
and decorative lamp meshes. These checks do not establish original-map lighting
fidelity.
