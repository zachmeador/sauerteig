# World format

State: planned.

## Outcome

A world carries everything that defines it: geometry, materials with per-face
alignment, objects with their properties, and world settings such as movement,
gravity, sun, sky, and fog. Old saves keep loading. Changing any of these later
does not require another format overhaul.

## Decisions so far

- **World settings block.** Movement is a world setting, not a code constant: a
  movement style, its numbers (speed, jump, gravity, air control, player size),
  and feature flags. Styles are Sauerbraten's model, where velocity eases toward
  the steering direction; today's Quake-style acceleration; and later a Tournament
  model with dodge and double jump. A game mode may override the block. Clients
  receive it on join, the server and every client's prediction use identical
  values, and changes go through the server like edits. Imported Sauerbraten maps
  get the Sauerbraten profile, and existing Sauerteig worlds keep today's
  movement. Sun, sky, fog, and ambient light join the same block.
- **Only format-defining values stay in code:** the eighth-metre grid, metre units,
  chunk sizes, the file format, and the 60 Hz simulation rate.
- **Objects carry typed properties** instead of a closed kind with a position and
  yaw. Lights come first (color, intensity, radius, cone, direction); the same
  mechanism later serves sounds, particles, jump pads, teleporters, and pickups.
- **Materials come from a library** rather than eight compiled-in entries, with
  per-face texture offset, rotation, and scale. Worlds reference materials by
  stable identifiers.
- **Saves move to the compact encoding** chosen in
  [shared-sandbox](shared-sandbox.md) stage 2, compressed; JSON stays available
  for export and debugging.

## Sauerbraten movement reference

From Sauerbraten's `physics.cpp` and `ents.h`, at the importer's 8 units per
metre: base speed 100 units/s (12.5 m/s), multiplied by 1.3 when moving straight
forward and by another 1.3 while airborne. Velocity approaches the wish velocity
by a factor of `1 - 1/f` per 20 ms, with `f` = 6 on the ground, 30 in the air, and
20 in water. Jump velocity is 125 units/s and gravity 200 units/s², accumulated
separately from the eased velocity. The jump peaks around twice as high as
Sauerteig's current 1.34 m. Player radius is 4.1 units and eye height 14.

## Remaining work

Break this into stages after shared-sandbox stage 1 lands, since both touch
protocol and server types. The movement-profile stage must measure speed, jump
height, and air control against the reference above and verify traversal on
imported maps.
