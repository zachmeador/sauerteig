# Project tracking

This is the current work list, not a development diary. Sauerteig is a playable
prototype; the entries below describe specific completed scopes and remaining
work, not a finished game.

## Direction

Sauerteig is first a shared building sandbox: players build together in one live
world and fight in it whenever they like. Work goes first to the layers that
everything built rests on: shared editing over the internet, the world format,
the renderer, and large worlds. Combat networking, weapons, and game modes come
later, because they are easier to change once the core is right. Movement,
gravity, sky, and similar values are per-world settings in the world format, not
code constants.

The maintainer removed the browser client on 2026-11-12 until browsers match
native, and asked for the constraints it brought, starting with WebSocket, to be
rethought from first principles.

The maintainer asked on 2026-11-13 for Sauerteig to be treated as a public
open-source game. So designs size their requirements against a published
minimum specification, treat every server, client, and downloaded world as
untrusted, expect players and servers on different releases, and back
performance claims with evidence that contributors can reproduce. The
[minimum specification](platforms.md#minimum-specification) is 8 GB of memory
and graphics about as fast as a Steam Deck's, with Vulkan, DirectX 12 or Metal
(maintainer, 2026-11-13). Nobody has checked Sauerteig on such hardware yet.

Sauerteig is for Windows, macOS, and Linux, and nothing should be Mac-exclusive,
even though most testing so far happens on a Mac (maintainer, 2026-11-13). Paid
services stay ruled out, including Apple's paid developer membership, so Mac
builds aren't notarized.

The maintainer chose on 2026-11-13 to rebuild the core around one octree of
cubes, as Sauerbraten does, before shared-sandbox stage 2's network and save
formats ([octree world](octree.md)).

## Current work

Tracks are listed in priority order. Stages run in parallel when they do not
touch the same code. Wave 1 work orders start side by side once these documents
are committed, except where an order's State names another order to wait for.
Later waves wait for the orders they build on.

| Work | State | Next |
| --- | --- | --- |
| [Octree world](octree.md) | Planned | The core keeps geometry in three structures and water and glass in a fourth; one octree replaces them, as in Sauerbraten (maintainer, 2026-11-13). Wave 1: O1a, the tree in the core (two sessions). Then O1b (water and glass drawn from the tree) and O3 (imports straight into the tree, fixing mirrored maps), and in wave 2 O2 (edits at any grid size). Shared-sandbox 2d waits for O2. |
| [Shared sandbox](shared-sandbox.md) | Active | Stage 1 (single-owner simulation, snapshot saves and joins) is merged and awaits the maintainer's check in the app. Stage 2 (big worlds open and join quickly, within a memory budget set by the minimum specification) builds on the octree. Wave 1: the save and quit fixes, then 2b's first session (the network thread). Wave 2: 2b's second session (building on worker threads). Wave 3: 2d (compact joins, the version handshake and the memory budget). Wave 4: 2e (compact saves) and 2f (nobody waits, reconnect as yourself). Then 2g. The octree replaced 2a and 2h. Stages 3 and 4 stay parked. |
| [Platforms](platforms.md) | Planned | The minimum specification (8 GB of memory, Steam Deck-class graphics) was approved on 2026-11-13 and hasn't been checked on such hardware. Wave 1: CI on Windows, macOS and Linux (stage 1) and the movement check, identical movement in every build (stage 2), which shared-sandbox 2d waits for. Stage 3, checking a reference machine, needs a tester the project doesn't have. Packaging without paid signing comes later. |
| [World format](world-format.md) | Active | Stage 1 (movement styles in the core) is complete. Geometry becomes one octree first ([octree world](octree.md)). Next, once shared-sandbox 2e has merged: world settings with the profiles wired in, then object properties and a material library with texture alignment. |
| [Renderer](renderer.md) | Active | Stages 1 (HDR, anti-aliasing, smooth motion) and 2 (clustered lights with a cached shadow atlas) are merged and await the maintainer's look. Next, one at a time: N1 (native GPU features, wave 1), then stage 2b (sun cascades and quality levels that vary detail only, with a lowest level that runs smoothly on the minimum specification; wave 1, after N1), N2 (every light shadowed, wave 2) and N3 (lighter world meshes, wave 4). N1b adds GPU tests on software drivers once platforms stage 1 merges. Octree O1a leaves hidden faces out of world meshes, at least 61% of an imported map's triangles. All of it must run on Vulkan and DirectX 12 as well as Metal. |
| [Large worlds and performance](large-worlds.md) | Partial | Next: stage 1, a generated benchmark world at nub1's scale for numbers anyone can reproduce (wave 1, after octree O1a). Then viewing distance, streaming, and traversal on top of the octree, shared-sandbox stage 2 and the renderer stages. |
| [Editor feel](editor-feel.md) | Active | Stage 1 (fill copies the faces behind; Delete empties the selection) is merged; play-testing is pending. Next, after octree O2 changes how the editor plans edits: in-place selection transforms, keymap parity, heightmap brush. |
| [Legacy map restoration](legacy-import.md) | Partial | I1 (reading OCTA version 27 maps directly) merged on 2026-11-14 and awaits the maintainer's look in the app. Next: octree O3 imports maps straight into the tree and fixes mirrored imports. Then original textures, models, and lights once the world format and renderer support them. |
| [Lighting](lighting.md) | Partial | Light nodes in view are shadowed point lights, up to what the shadow atlas holds; the rest fade out, so the_cube1m draws 149 of its 339 (renderer stage 2, awaiting the maintainer's look). Next: renderer N2 lights and shadows every light, then editable light properties from the world format. |
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
the checks, plays the build, then commits and merges it together with the
tracking update. The maintainer can veto any decision or merge.

A session starts from a commit that holds its work order, so the lead commits
the tracking documents, and any work they describe as done, before cutting the
worktrees. Orders that run side by side can still share a few files, such as
`Cargo.lock`, `docs/development.md` and the top-level README; each order's
parallel-work line names them and the part it changes. The second to merge
rebases onto the first, and `Cargo.lock` is regenerated at merge. A session
updates its own stage section; the lead updates this overview when merging.

## Complete scopes

- Basic water/glass material volumes: verified on 2026-10-24 with transparent
  rendering and underwater views, shared edits visible to another client,
  material undo, blueprint capture, and swimming. Glass collision and weapon
  blocking, sparse editing/transforms, stale conflicts, and save/rejoin have
  regression coverage. This uses simple alpha transparency; original liquid
  styles, lava/clip flags, scene reflection/refraction, and broad GPU
  performance validation remain outside this scope.
  [Controls](../editor.md#volumes-and-blueprints) and
  [rendering limits](../development.md#rendering-and-assets).
- Main menu and automatic session switching: verified on 2026-10-24. Opening the
  menu kept players connected; editing, switching worlds, and reopening retained
  the edit. Regression tests cover sync/save failure, cancelled retries, and
  closing during a switch. [Behavior](../development.md#hosting-and-saves).
- Independent light nodes: no forced fixture model, editor-only selectable
  guides, and separately placeable lamp props. Rendering was checked on
  2026-10-24; guide, picking, and emitter regression tests pass. This does not
  complete the lighting work above.
- Shared geometry and object editing: additive/subtractive selections, per-face
  materials, object transforms, combined copy/paste, blueprints, and transactional
  undo/redo. Regression tests cover these operations; shared editing has had
  manual checks. [Controls and limits](../editor.md).
- Managed local saves and embedded hosting: named worlds, duplicate/import of
  Sauerteig JSON, automatic local server startup, save/reopen, and explicit LAN
  opt-in. Hosting and persistence tests cover save/lock/error paths.
  [Storage and hosting](../development.md#hosting-and-saves).
- Loading the maintainer's private OGZ archive into the saved-world library, on
  the M3 Pro: 15 conversions without solid-geometry omissions and four
  explicitly recovered copies. Three OCTA version 27 maps have been in the
  library through a hand-made shim since 2026-11-11; since 2026-11-14 the stock
  importer reads them and produces byte-identical worlds
  ([legacy import](legacy-import.md) I1). On 2026-10-24 all 19 untouched library
  copies were refreshed with backups; 17 contain restored water/glass
  assignments. All 19 passed two server open/save cycles with unchanged terrain
  and objects. Original OGZ hashes are unchanged. This completed local setup
  does **not** complete map restoration.

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
   user workflow. State any remaining limitation next to the completion claim.
   Include the tracking update with the implementation commit.
5. When an outcome is complete, update the brief complete scope above, move usage
   details into the regular docs, and delete its work document. Prune old completed
   summaries once the canonical docs explain the feature. For superseded work,
   keep only the current decision and remaining tasks; git history holds old truth.

Update existing entries instead of adding status snapshots or changelogs.
Unrelated future ideas do not become commitments just because they were mentioned.
