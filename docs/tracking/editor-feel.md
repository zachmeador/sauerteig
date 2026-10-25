# Editor feel

State: planned.

## Outcome

Building feels as direct as Sauerbraten's editor for people with Sauerbraten
muscle memory, and the editor's small rough edges are gone.

## Known gaps

- Scrolling on an aimed face with no selection extrudes whichever face is under
  the cursor each time, so new faces turn the result into a staircase. In
  Sauerbraten the aimed cube becomes the selection and follows the extrusion,
  growing a pillar. Observed on 2026-10-25 in the browser client.
- Number keys 1 and 2 set both the weapon and the building material.
- Missing volume kinds: lava, clip, no-clip, and death.
- No heightmap brush.

Work happens in small pieces between larger stages, in files that no active stage
owns.
