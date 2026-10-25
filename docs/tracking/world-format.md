# World format

State: active.

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
  per-face texture offset, rotation (Sauerbraten's eight orientations), and scale.
  Worlds reference materials by stable identifiers. Unlike Sauerbraten, alignment
  survives paste, rotate, and mirror: those transforms compose the rotation and
  shift the offset. One choice is still open: Sauerbraten projects along a face's
  nominal axis, which matches imported maps but stretches on slopes, while
  Sauerteig projects along the actual normal. See the
  [Sauerbraten editor reference](../../research/sauerbraten-editor.md).
- **Saves move to the compact encoding** chosen in
  [shared-sandbox](shared-sandbox.md) stage 2, compressed; JSON stays available
  for export and debugging.

## Sauerbraten movement reference

From Sauerbraten's `physics.cpp` and `ents.h`, at the importer's 8 units per
metre: base speed 100 units/s (12.5 m/s), multiplied by 1.3 when moving straight
forward and by another 1.3 while airborne. Velocity approaches the wish velocity
by a factor of `1 - 1/f` per 20 ms, with `f` = 6 on the ground, 30 in the air, and
20 in water. Jump velocity is 125 units/s and gravity 200 units/s², accumulated
separately from the eased velocity. Player radius is 4.1 units and eye height 14.
Measured in the port, the jump peaks at 2.44 m, against 1.28 m for today's
movement.

## Stage 1: movement styles in the simulation core

State: complete on 2026-10-25 within its scope. No server or client uses the
Sauerbraten profile yet, so it has not been played.

- **What exists.**
  - `MovementProfile` in `crates/core/src/movement.rs` holds a style and its
    numbers.
  - **Arena** reproduces the previous movement bit for bit; **Sauerbraten** ports
    the engine's velocity model.
  - `simulate_player_with` takes a profile, and `simulate_player` wraps it with
    Arena, so the server and client are unchanged.
  - Style state (`falling`, the jump latch, `in_water`) lives in
    `Player::movement` and is omitted from snapshots under Arena.
  - Details are in the
    [core contract](../../crates/core/CONTRACT.md#movement-profiles).
- **Choices made while implementing.**
  - The Sauerbraten model runs three 5.6 ms velocity steps per tick, because a
    single 16.7 ms step undershoots the jump apex by 12%.
  - Coefficients are computed once per profile, and collision still runs once
    per tick.
  - Jumps fire once per press, including a press held through a landing.
  - Swimmers are sampled at 0.75 of body height and stay swimming down to 0.5.
    The vertical keys also steer swimmers.
  - Edit flight keeps instant velocity at the profile's fly speed.
  - Sauerbraten's player is 0.5125 m in radius and 1.875 m tall, with eyes at
    1.75 m.
- **Gaps.**
  - There is no step-down, so descending stairs or ramps leaves both profiles
    airborne most of the way. Sauerbraten's airborne boost then reaches 13.8 m/s
    on the test stairs; the engine's `trystepdown` is not ported.
  - Slope sliding needs floor normals that the support queries don't return.
  - Two trig-heavy golden scenarios are exact only on the recording platform
    (within 2e-3 elsewhere).

## Remaining work

Later stages wire profiles into the world format, protocol, saves, server, and
client, and add object properties, the material library, and the compact save
encoding. Plan them after shared-sandbox stage 1 lands, since they touch protocol
and server types. When the Sauerbraten profile is wired in, verify traversal on
imported maps.

The wiring stage must also reset `Player::movement` wherever it resets `velocity`
(server respawn and mode changes, client mode changes), because a stale `falling`
would otherwise carry into the next life, and validate profile numbers that come
from a world file: sizes and speeds finite and positive, easing numbers at least 1.

## Evidence

Stage 1, 2026-10-25:

- **Checks.** fmt, Clippy, the 303 workspace tests (32 new), the native builds,
  and a wasm32 `cargo check` passed for the implementer and again in the design
  lead's review. The full browser build passed at merge.
- **Arena is unchanged.** Its golden test reproduces 3,778 recorded ticks from 19
  scripted scenarios bit for bit.
- **Oracle comparison.** `cargo run -p sauerteig-core --example
  movement_profiles` prints both profiles beside a literal port of Sauerbraten's
  velocity math at its 5 ms frames. The comparison test allows 3%; with one
  velocity step per tick instead of three, the apex would be 12.3% short.

  ```text
                                     Arena   Sauerbraten  Sauerbraten oracle  vs oracle
  top speed, forward (m/s)           9.000        16.250              16.250     +0.00%
  top speed, strafing (m/s)          9.000        12.500              12.500     +0.00%
  top speed, airborne (m/s)          9.000        21.124              21.124     +0.00%
  time to 90% of top speed (s)       0.081         0.253               0.253     +0.06%
  jump apex height (m)               1.277         2.435               2.451     -0.65%
  jump air time (s)                  0.639         0.703               0.705     -0.25%
  stopping distance (m)              0.978         1.722               1.726     -0.20%
  ```

- **Native and wasm agree.** A 1,200-tick Sauerbraten run produced identical
  digests natively and as wasm32 under Node. This used a scratch crate, not a
  repository test.
- **Not observed:** anything in a native or browser client, and the golden test
  on Linux CI.
