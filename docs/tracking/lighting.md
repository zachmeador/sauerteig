# Independent light nodes and lighting

State: partial. Node/model separation is complete within the scope below.

## Outcome

Place and edit a light independently of any model. Add a lamp prop only when the
builder wants one. Support intentional point/spot lighting and preserve imported
light settings rather than substituting a fixture with unrelated defaults.

## Current behavior

`Light` has no physical mesh in play, uses an editor-only selectable marker, and
emits from its stored position. Wall
and hanging lamp models are separate decorative props. Their glass is emissive,
but a separate node supplies illumination. Existing saves keep their object IDs;
no map rewrite is needed to remove forced fixtures from Light nodes. Worlds that
relied on lamp props alone for illumination now need separate Light nodes.

Local lighting is still fixed: eight nearest nodes, warm color, downward 120°
spotlights, 12 metre range, and 512 px shadow maps. Color, intensity, radius,
direction, and cone are not editable. These are substantial limitations, especially
for imported point lights. The sun is also a fixed environment light; no baked
lighting or global illumination is implemented.

## Remaining work

- [x] Verify model-free nodes, editor picking, independent props, and illumination
  from the stored node position in native and browser rendering.
- [ ] Define editable light properties and preserve them through multiplayer,
  save/reopen, copy/paste, blueprints, and undo.
- [ ] Render point and spot lights with suitable shadows and a measured GPU budget.
- [ ] Import and apply source light properties and sun/environment settings.
- [ ] Compare representative indoor/outdoor lighting with original maps, noting
  deliberate renderer differences rather than claiming exact reproduction.

## Evidence

On 2026-10-24, native and browser checks showed editor-only markers, illumination
without a fixture, and an independently visible lamp prop. Browser marker picking
selected the Light node at its stored coordinates. A newly placed native node
survived switching worlds and reopening. Regression tests cover no physical Light
mesh, non-shadowing guides, placement/picking/emitter alignment, and decorative
lamp meshes. These checks do not establish original-map lighting fidelity.
