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

## Stage 1: movement styles in the simulation core

State: active on branch `movement-profiles`.

Decisions:

- **A movement profile replaces the movement constants** in
  `crates/core/src/physics.rs` and `lib.rs`: a style plus its numbers. There are
  two built-in profiles. **Arena** keeps today's exact values and Quake-style
  acceleration. **Sauerbraten** ports Sauerbraten's model per the reference above.
  Add `simulate_player_with(…, &MovementProfile)`, and keep `simulate_player` as a
  wrapper that uses Arena so server and client call sites need no changes.
- **Arena reproduces today's behavior exactly.** Before changing any code,
  record golden trajectories from the current implementation for scripted input
  sequences: flat ground, stairs, slopes, jumping, air strafing, water, and edit
  flight. Add a test that Arena reproduces them exactly.
- **The Sauerbraten style ports the velocity model, not the collision code.**
  Port `modifyvelocity`, `modifygravity`, and the velocity part of `moveplayer`
  from `src/engine/physics.cpp` in the Sauerbraten SVN at
  `https://svn.code.sf.net/p/sauerbraten/code/src/engine/physics.cpp`:
  - eased velocity with per-20 ms factors;
  - a separate falling component that accumulates gravity;
  - the 1.3 forward-only and 1.3 airborne multipliers;
  - jumps that fire once per press, including a press held while landing;
  - Sauerbraten's water handling, as far as Sauerteig's swimming allows.

  Sauerteig's box collision, step-up, and support queries stay as they are. Add
  simulation-state fields to `Player` (for example the falling component and the
  jump latch) with `#[serde(default)]` where the style needs them.
- **Player dimensions belong to the profile:** radius, height, eye height, and
  step height. The Sauerbraten profile uses Sauerbraten's player size at 8 units
  per metre. In this stage only movement uses them; hit detection, spawning,
  rendering, and the camera keep today's constants until the wiring stage.
- **Per-tick math stays the same on native and wasm where practical.** Compute
  per-tick coefficients once per profile instead of calling `powf` or `exp` every
  tick.
- **A reference oracle:** a test-only, literal port of Sauerbraten's velocity
  math, without collision, in Sauerbraten units. Compare its trajectories with
  the Sauerbraten profile at 60 Hz:
  - top speed straight forward, strafing, and airborne;
  - time to reach 90% of top speed;
  - jump apex height and air time;
  - stopping distance.

  Tolerance is 3%.
- **A measurement example**, `cargo run -p sauerteig-core --example
  movement_profiles`, prints those numbers for both profiles side by side.

Not in this stage: putting the profile into the world format, protocol, saves,
server, or client, and the Tournament style. Two other sessions are changing the
server and renderer on other branches, so leave `crates/server` and
`crates/client` untouched.

Acceptance: fmt, Clippy, all workspace tests, and native and browser builds pass.
The oracle comparison passes within tolerance, and Arena's golden trajectories
are unchanged. Record the measurement example's output under Evidence below.

## Remaining work

Later stages wire profiles into the world format, protocol, saves, server, and
client, and add object properties, the material library, and the compact save
encoding. Plan them after shared-sandbox stage 1 lands, since they touch protocol
and server types. When the Sauerbraten profile is wired in, verify traversal on
imported maps.

## Evidence

None yet for this outcome.
