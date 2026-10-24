# Project tracking

This is the current work list, not a development diary. Sauerteig is a playable
prototype; the entries below describe specific completed scopes and remaining
work, not a finished game.

## Current work

| Work | State | What remains |
| --- | --- | --- |
| [Lighting](lighting.md) | Partial | Independent nodes are verified; configurable point/spot lights and imported properties remain incomplete. |
| [Legacy map restoration](legacy-import.md) | Partial | Geometry recovery works; original media, light properties, several entity types, and thin surfaces are missing. |
| [Large worlds and performance](large-worlds.md) | Partial | Compact storage works; viewing distance, traversal, and measured laptop GPU performance need work. |
| Combat and multiplayer playtesting | Planned | Exercise real fights during collaborative editing, including internet latency and reconnects; basic FPS mechanics alone do not establish game quality. |

**Active** means work is underway. **Partial** means useful behavior exists but
the stated outcome is unfinished. **Planned** means work has not started.
**Complete** means the bounded outcome has been verified, with any limits stated.
A blocker belongs in its work document with the next practical step.

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
