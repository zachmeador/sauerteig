# Project tracking

This is the current work list, not a development diary. Sauerteig is a playable
prototype; the entries below describe specific completed scopes and remaining
work, not a finished game.

## Direction

Sauerteig is first a shared building sandbox: friends build together in one live
world and fight in it whenever they like. Work goes first to the layers that
everything built rests on: shared editing over the internet, the world format,
the renderer, and large worlds. Combat networking, weapons, and game modes come
later, because they are easier to change once the core is right. Movement,
gravity, sky, and similar values are per-world settings in the world format, not
code constants.

## Current work

Tracks are listed in priority order. Stages run in parallel when they do not
touch the same code.

| Work | State | Next |
| --- | --- | --- |
| [Shared sandbox](shared-sandbox.md) | Active | Stage 1 (single-owner simulation, snapshot saves and joins) is merged and awaits the maintainer's check in the app. Stages 3 and 4 are parked; stage 2 is next when big-map loading needs it. |
| [World format](world-format.md) | Active | Stage 1 (movement styles in the core) is complete. Next: world settings with the profiles wired in, then object properties and a material library with texture alignment. |
| [Renderer](renderer.md) | Active | Stages 1 (HDR, anti-aliasing, smooth motion) and 2 (every light in view, with a shadow atlas) are merged and await the maintainer's look. Next: stage 2b, sun cascades and quality tiers. |
| [Large worlds and performance](large-worlds.md) | Partial | Viewing distance, streaming, and traversal, once the first three tracks lay their foundations. |
| [Editor feel](editor-feel.md) | Active | Stage 1 (fill copies the faces behind; Delete empties the selection) is merged; play-testing is pending. Next: in-place selection transforms, keymap parity, heightmap brush. |
| [Legacy map restoration](legacy-import.md) | Partial | Original textures, models, and lights once the world format and renderer support them. |
| [Lighting](lighting.md) | Partial | Every light node is a real point light with shadows (renderer stage 2, awaiting review). Next: editable light properties from the world format. |
| [Game layer](game-layer.md) | Planned | Combat networking, weapons, items, and modes, after the sandbox core. |

**Active** means work is underway. **Partial** means useful behavior exists but
the stated outcome is unfinished. **Planned** means work has not started.
**Complete** means the bounded outcome has been verified, with any limits stated.
A blocker belongs in its work document with the next practical step.

## How work runs

The design lead sets direction and writes each stage as a work order in its
tracking document: decisions already made, scope, parallel-work boundaries, and
acceptance checks. An implementation session executes one stage on its own branch
in a separate git worktree and does not commit. The lead reviews the result, runs
the checks, plays the build natively and in the browser, then commits and merges
it together with the tracking update. The maintainer can veto any decision or
merge.

## Complete scopes

- Basic water/glass material volumes: verified on 2026-10-24 with transparent
  rendering and underwater views in native/browser, shared browser edits visible
  natively, material undo, blueprint capture, and swimming. Glass collision and
  weapon blocking, sparse editing/transforms, stale conflicts, and save/rejoin
  have regression coverage. All 271 workspace tests, Clippy, and both builds pass.
  This uses simple alpha transparency; original liquid styles, lava/clip flags,
  scene reflection/refraction, and broad GPU performance validation remain outside
  this scope. [Controls](../editor.md#volumes-and-blueprints) and
  [rendering limits](../development.md#rendering-and-assets).
- Main menu and automatic session switching: verified on 2026-10-24 in native
  and browser clients. Opening the menu kept players connected; native editing,
  switching worlds, and reopening retained the edit. Browser server switching
  also worked. Regression tests cover sync/save failure, cancelled retries, and
  closing during a switch. [Behavior](../development.md#hosting-and-saves).
- Independent light nodes: no forced fixture model, editor-only selectable
  guides, and separately placeable lamp props. Native/browser rendering was
  checked on 2026-10-24; guide, picking, and emitter regression tests pass.
  This does not complete the lighting work above.
- Shared geometry and object editing: additive/subtractive selections, per-face
  materials, object transforms, combined copy/paste, blueprints, and transactional
  undo/redo. Regression tests cover these operations; native/browser shared
  editing has had manual checks. [Controls and limits](../editor.md).
- Managed local saves and embedded hosting: named worlds, duplicate/import of
  Sauerteig JSON, automatic local server startup, save/reopen, and explicit LAN
  opt-in. Hosting and persistence tests cover save/lock/error paths.
  [Storage and hosting](../development.md#hosting-and-saves).
- Loading the available private OGZ archive into this Mac's saved-world library:
  15 conversions without solid-geometry omissions, four explicitly recovered
  copies, and four rejected corrupt inputs. On 2026-10-24 all 19 untouched library
  copies were refreshed with backups; 17 contain restored water/glass assignments.
  All 19 passed two server open/save cycles with unchanged terrain and objects.
  Original OGZ hashes are unchanged. This completed local setup does **not**
  complete map restoration.

## How to keep this useful

1. Before changing code, read this overview and the relevant work document. Add
   or update a short entry when scope changes; record newly found gaps immediately.
2. Keep one work document per unfinished outcome that needs more than a table
   row. State the outcome, current behavior, remaining work, and acceptance checks.
   Name files after the work, without ticket IDs, dated reports, or duplicate lists.
3. During implementation, distinguish code written, automated checks passed, and
   behavior observed in the running app. Record failures and untested paths as
   plainly as successes. Do not check off an acceptance test based on a build.
4. Before calling work complete, run the relevant checks and exercise the actual
   user workflow. Cover native and browser when both are affected. State any
   remaining limitation next to the completion claim. Include the tracking update
   with the implementation commit.
5. When an outcome is complete, update the brief complete scope above, move usage
   details into the regular docs, and delete its work document. Prune old completed
   summaries once the canonical docs explain the feature. For superseded work,
   keep only the current decision and remaining tasks; git history holds old truth.

Update existing entries instead of adding status snapshots or changelogs.
Unrelated future ideas do not become commitments just because they were mentioned.
