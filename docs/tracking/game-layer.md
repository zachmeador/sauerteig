# Game layer

State: planned, after the sandbox core.

## Outcome

Combat feels fair and immediate at internet latency, and game modes are data
rather than code: Sauerbraten's arsenal, items, and modes first, then a Tournament
family inspired by Unreal Tournament.

## Known gaps

- Hitscan traces targets' current positions, with no lag compensation.
- Remote players are smoothed toward the newest 20 Hz snapshot instead of
  interpolated from a buffer.
- Firing is not predicted: sound and tracers wait for the server's echo, and the
  shot sound uses the shooter's current weapon instead of the one that fired.
- Weapons, damage, cooldowns, splash, and scoring are literals in the server's
  `Session`. `Player` is both wire format and simulation state, and `InputFrame`
  carries only `fire` and `weapon`.
- There are no pickups, armour, ammo, teams, chat, respawn flow, kill feed, or modes.
- Combat has not been played over internet latency or reconnects, including
  fights during collaborative editing.

## Direction

- Each match is a mode plus a movement profile from the
  [world format](world-format.md), an arsenal, and optional mutators.
- **Classic first:** Sauerbraten's weapons and items; free-for-all, team
  deathmatch, instagib, efficiency, and capture the flag.
- **Tournament later:** dodging, double jump, alt-fire weapons, translocator,
  announcer, and objective modes such as Bombing Run.
- **Modes only a building game can have:** a timed build phase followed by a
  fight, and destructible siege.
