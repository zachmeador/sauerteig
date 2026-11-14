# Shared sandbox

State: active.

## Outcome

Players build together in one world over the internet, including large imported
maps. A builder's edits appear immediately on their own screen and promptly for
everyone else. Joining shows progress and succeeds on ordinary home connections.
Nobody's game hitches when someone else pastes, saves, or joins.

## Current behavior

- **Desktop only.** The maintainer removed the browser client on 2026-11-12
  until browsers match native.
- **The world lives in four structures:** 1 m cells, 1 m roots of pieces,
  imported leaves that edits can only mask, and a separate layer of water and
  glass boxes. The [octree world](octree.md) replaces them with one tree before
  stage 2's formats carry them.
- **The simulation has one owner.** It runs on its own thread, and saves, joins,
  and edit preparation work from shared snapshots off that thread (stage 1).
- **A joining player still receives the whole world as one JSON message,** about
  45 MB for the largest archive maps.
  - The server now waits through a 30 second stall rather than a fixed 5 second
    limit.
  - The client still builds and meshes the world on the main thread, which
    freezes the game.
- **The region cap keeps the largest archive map out.** nub1 (434,233 regions)
  is refused only by the 400,000-region cap (`MAX_COMPACT_REGIONS`), which
  octree O1a removes. It fits every other limit, with 5.8% of the 63 MiB
  geometry budget left for building (measured 2026-11-12 on the M5 Pro). nub1
  is an OCTA 27 map, which the importer can't read yet
  ([legacy import](legacy-import.md) I1), so that measurement converted it
  through a hand-made header shim.
- **The wire format is still JSON** over one reliable WebSocket. Protocol types
  carry JSON-specific serde attributes, so a compact encoding needs a codec
  boundary.
- **Each editor action still waits** for the server's acknowledgment before the
  next is sent.
- **Secure addresses connect** since 2b's first session (2026-11-14), but only
  to servers whose certificate a public authority signed: the client trusts the
  bundled web roots alone.

## Stage 1: the simulation never waits

State: merged on 2026-10-25. The maintainer has not yet checked it in the
running app.

- **One owner.**
  - `simulation.rs` owns `Session` on a dedicated 60 Hz thread.
  - Network tasks send typed commands, and health and admission read published
    status.
  - A panic publishes a failure, closes connections with an error, reaches the
    embedded host's existing error path, and skips the final save.
- **Cheap snapshots.**
  - `WorldSnapshot` shares immutable region and volume arrays, and keeps coarse
    chunks, authored roots, and empty masks in `im` persistent maps.
  - Objects share one array.
  - No triangles, query indexes, or meshes are copied.
- **Edits.**
  - One ordered worker validates, canonicalizes, and compiles each request once.
    It is a single worker so that concurrently prepared edits can't cut the same
    compact region inconsistently.
  - The owner keeps the conflict, budget, fighter-overlap, and commit checks.
- **Queues and transfers.**
  - Reliable queues are bounded at 128 MiB per peer. At capacity, the worker
    defers commits instead of dropping a joiner's revisions.
  - Movement snapshots stay replaceable.
  - Writes fail only after 30 seconds without progress.
- **TCP_NODELAY** is on for server sockets. The native client's tungstenite
  already sets it.
- **Telemetry.** `/health` and ten-second logs report tick work, lateness, the
  largest tick's cause, and per-operation p50/p99/max.
- **Testing aid.** Scripted WebSocket clients (`bots.rs`, test-only) drive the
  acceptance tests. The design lead removed a standalone stress tool at merge.

- **Fixes from an independent review at merge.** The review found no ordering,
  undo, snapshot, or persistence bugs, but these were fixed:
  - **Busy instead of stuck.** An edit that can't be published because a
    joining or slow player's queue is full waits at most 2 seconds, then fails
    with a "busy, try again" result. Nobody is evicted. The room has one edit
    worker, so while any player's queue lacks room for a patch, every builder's
    edits queue behind the waiting one, and each that still doesn't fit after
    2 seconds fails as busy, one at a time (`Session::peers_have_room`,
    `run_edit`). Stage 2f removes the wait.
  - **Exact reservations.** Publication reservations use the same sizes as the
    packets they reserve, plus room for the follow-up result.
  - **Nothing lost on close.** Commands a client sent before closing still
    apply, even without a Sync.
  - **Smaller fixes:**
    - failures publish before logging;
    - a full room says why it closes;
    - departing players' undo history is freed off the simulation thread.

Gaps:

- **Ticks are still not always fast.** One run recorded an 86.9 ms tick
  dominated by physics, and a 36.9 ms edit. Profile collision queries on large
  maps under the [large-worlds](large-worlds.md) track before claiming a
  consistent frame budget.
- **Not yet observed in the running app:**
  - smoothness while another client pastes and saves on a large hosted world;
  - saving, reopening through the home screen, and rejoining;
  - the host's error display after a simulation failure;
  - a builder seeing the busy message while another player joins a large world.

## Stage 2: big worlds open and join quickly

State: planned. The maintainer approved the decisions below on 2026-11-13, with
an 8 GB minimum specification, and then chose to rebuild the core as one
[octree](octree.md) first. Stage 2's chunks and saves carry octree subtrees.

What players get:

- Big maps, nub1 included, can be opened, hosted and joined. One memory budget
  limits world size, and a computer that may not hold a world says so before
  downloading it.
- Hosting opens a big world in under a second and uses far less memory
  (octree O1a).
- Joining never freezes the window and shows progress. The largest library maps
  download about 0.2 MB instead of 45 MB and are playable in about a second on a
  LAN (estimate).
- No more "busy, try again": commits never wait for a slow or joining player,
  and a player who falls behind catches up.
- A dropped player reconnects as themselves for 10 minutes, with their undo
  history.
- A player on another release sees a message naming both versions, instead of
  reconnecting silently every 3 seconds.
- Saves are small binary files, written from a world's first edit, and
  `world.json` stays untouched.
- `wss://` addresses connect.

Why: every byte cap that keeps big maps out comes from one prototype choice.
Each joiner receives the whole world as one JSON message (`connection` in the
server's `network.rs`). The client's WebSocket library limits a message to
64 MiB by default, and ewebsock 0.8 can't change that. The snapshot budget, the
save cap, the importer cap, the write buffer and the 400,000-region cap, raised
"within snapshot limits" in a622226, all follow from it. None is a browser rule
or a maintainer decision, so stage 2 removes the cause instead of raising the
numbers.

### Decisions

Approved by the maintainer on 2026-11-13:

1. **Transport.** One WebSocket over TCP per player for now: axum on the server,
   and tokio-tungstenite on the client in place of ewebsock. Every binary frame
   starts with a lane byte, so a later QUIC link can carry the same lanes. QUIC
   waits for stage 4.
2. **Joining.** The client speaks first, with a JSON `Hello` carrying a protocol
   number. The world follows in binary chunks of at most 2,048 regions, then an
   end marker, then the edits made meanwhile. The client builds everything on
   worker threads when the download ends. This reverses the earlier plan's
   spawn-ordered chunks and time-sliced build, which existed because browsers
   lacked threads.
3. **One memory budget** replaces every size cap; see World size below.
4. **Encoding.** postcard plus deflate: no zstd, no hand-packed bit format, no
   32 KiB chunk rule and no ruzstd, reversing the
   [research](../../research/wire-encoding.md)'s choices. That costs about 50 KB
   more per join on the largest maps: nub1's regions take 187 KB instead of
   127 KB, and the_cube's 199 KB instead of 147 KB (measured).
5. **Nobody waits.** Commits never wait for a peer. Recent patches sit in one
   bounded list for the room, and a player who falls behind is resynced and sees
   "Catching up".
6. **Reconnect as yourself** for 10 minutes, with a resume token.
7. **Compact binary saves** start at a world's first edit, and `world.json` is
   never touched. Each conversion is read back and compared; on a mismatch the
   edits go to a sibling JSON file with a visible error. A dry run on edited
   copies of the 23 library worlds plus nub1 comes first.
8. **The host joins its own server** like any other player, holding two world
   copies, until 2g.
9. **Native GPU features within the lighting rule:** every light shadowed (339
   in the_cube1m, up from 149), every light in view lit (the 256-light cap
   goes), 2× and 8× MSAA where the adapter supports them, quality that varies
   detail only, and lighter meshes. This is [renderer](renderer.md) work: N1,
   stage 2b, N2 and N3.
10. **Internet play stays parked.** The server refuses requests that carry an
    `Origin` header. An optional server password protects only on encrypted
    paths, such as `wss://` or a VPN, until stage 4.

### Minimum specification

The maintainer approved it on 2026-11-13, and
[platforms](platforms.md#minimum-specification) holds it: 8 GB of memory and
graphics about as fast as a Steam Deck's, with Vulkan, DirectX 12 or Metal.
Nobody has checked Sauerteig on such hardware yet, so checks that need it are
marked below. Here it sets the default world budget. It also made renderer N3,
2h and 2g required; the octree replaces 2h, and whether the minimum still needs
N3 and 2g is checked against the octree's measurements.

### World size

- **One memory budget replaces every remaining size cap.** The octree removes
  the region cap (`MAX_COMPACT_REGIONS`) and the water and glass box caps. 2d
  removes the 786,432-fragment cap (`MAX_WORLD_FRAGMENTS`), the 1,024-object
  cap (`MAX_OBJECTS`, repeated in the importer), and every 64 MiB cap: the
  snapshot budget (`MAX_WORLD_SNAPSHOT_BYTES`, `MAX_GEOMETRY_BYTES`), the save
  cap (`MAX_SAVE_BYTES`), the importer's `MAX_WORLD_BYTES` and the write buffer
  (`OUTBOUND_WRITE_BYTES`). Nor is the piece cap of an earlier draft
  (`MAX_WORLD_PIECES`) added.
- **The budget counts estimated memory.** One core function estimates what a
  joining client needs: the tree's leaves and nodes, the triangles of its
  meshes, objects and lights, plus a fixed term for the renderer. A cube count
  alone doesn't track memory, because shaped cubes mesh to more triangles than
  full ones. 2d sets the per-item costs from measurements of the octree, and
  renderer N3 lowers them.
- **A server setting.** The default is 4 GiB, half of a minimum-specification
  computer's memory. How many cubes that holds is measured after the octree.
  Today's costs, about 5.5 KB per imported piece at a joiner's peak, come from
  the structures the octree removes, and its culling alone leaves out at least
  61% of an imported map's triangles.
- **Only growth is refused.** The server checks the budget on commits and
  nowhere else. It refuses an edit that would leave the estimate above the
  budget and larger than before: "This world has reached its size limit".
  Opening and joining never fail for size, so a world saved under one budget
  or estimate opens under any other. Only JSON saves have a size limit: the
  JSON loader accepts at least 1 GiB, and the writer refuses a save the loader
  would refuse, shows the error and keeps the previous file (2d). Library
  worlds save in binary from 2e, without that limit, but worlds opened by an
  explicit `.json` path stay JSON.
- **The estimate comes first.** `Accept` announces the world's totals before
  the download. The client computes its own estimate from them, reads the
  computer's memory on every system, and when the estimate passes half of it
  asks: "This world needs about X GB; this computer has Y GB. Join anyway?" A
  host's own client asks the same when it joins, before the download, once the
  embedded server has loaded the world. Its estimate counts two world copies
  until 2g removes one. With a discrete GPU, meshes live in video memory, so
  the client counts them against the GPU's video memory instead (2d).
- **No size limit per edit** (octree O2): an edit is refused only when the
  world would pass its budget, and edits larger than one message travel in
  parts (2d). Undo history stays within a share of the budget, and rate limits
  count requests.
- **Kept:** the 8,192 m world size (`MAX_WORLD_COORDINATE`) and the importer's
  broken-file guards (64 MiB of OGZ, 256 MiB inflated, 2,000,000 octree
  nodes).
- **Later.** Once streaming ([large worlds](large-worlds.md)) bounds a client's
  memory by what is near, the budget becomes the server's own memory.

### Rules for a public game

Stage 2 treats Sauerteig as a public open-source game (maintainer, 2026-11-13):

- **The client treats every server as hostile.** Before downloading, it compares
  the world size the server announces with its own memory. It stops the
  download if the server sends more than it announced, and it decodes every
  message within a per-message budget (2d).
- **The server treats every client as hostile:** limits before admission, frame
  and decode budgets, and a server-wide inbound budget (2d).
- **Fuzz tests** cover the network decoder (2d) and the save loader (2e), under
  a counting allocator that checks every declared cap. Imported worlds are
  untrusted too, so import validates a world before adding it (2e).
- **Version policy.** Mismatched releases refuse each other with a message that
  names both versions. New releases open old worlds; old releases refuse newer
  worlds clearly. Every wire change bumps the protocol number or fills a
  reserved field, checked by golden byte fixtures. The first message stays JSON,
  so every release can read a refusal.
- **Server settings, not constants:** the world budget, the player count
  (default 16, `MAX_PLAYERS` today) and an optional password, set through the
  standalone server's options and the home screen's hosting options (2d).
- **No web pages.** The server refuses requests that carry an `Origin` header,
  which browsers add to every WebSocket upgrade and to cross-origin requests
  whose answers a script could read; the native client never sends one. A
  typed address, a link or an image carries none, so the server's one-line
  page still opens in a browser.
- **Reproducible numbers.** Performance checks run on the generated benchmark
  world ([large worlds](large-worlds.md) stage 1), which contributors can
  rebuild. The maintainer's library and nub1 are extra coverage.

### Design evidence

Measured on 2026-11-12 on the maintainer's M5 Pro, on copies, with release
builds. These describe today's structures, which the octree replaces:

- **nub1** converts to 434,233 regions and 62,225,398 B of JSON once the region
  cap is lifted. The server then loads it in 3.23–3.30 s and delivers its
  Welcome over loopback in 79 ms.
- **Load time.** 90% of a server's load is two builds of the same world, and the
  first only validates. About half of each build is the region BVH, which grows
  faster than the world. Scratch code with one shared empty mask set and a
  flat-array BVH built nub1 in 0.16 s and the_cube in 0.14 s on 12 threads.
- **Memory per region,** today and then with one shared empty mask set:
  - a standalone server: 3.3 KB, then 1.7 KB;
  - a joiner at its join peak: 9.1 KB, then 7.5 KB;
  - a joiner once settled: 5.8 KB, then 4.2 KB;
  - a host at its own join peak: 12.3 KB, then 9.0 KB.

  A client's fixed memory is about 0.53 GiB. Upload staging adds about 2.0 KB
  per region at a join peak; submitting every 128 chunks cut nub1's peak by
  827 MiB, at a cost of 64 ms. The client figures come from the capture tool
  standing in for the game, on Apple's unified memory.
- **Memory readings.** Activity Monitor's default Memory column undercounts
  memory that is freed and reused: it showed 366 MiB for the nub1 server while
  1.30 GB was live. On macOS use the Real Memory column; `/health` (after
  octree O1a) and F3 (after 2b) will show memory by part on every system.
- **Not measured:** the game client and hosting app themselves, Windows, Linux,
  discrete GPUs, minimum-specification hardware, and edited worlds, whose cut
  pieces carry more triangles.

### Wave 1

Wave 1 holds the save and quit fixes and 2b's first session here, and in other
tracks [octree](octree.md) O1a, [renderer](renderer.md) N1,
[legacy import](legacy-import.md) I1 (the importer reads OCTA version 27) and
[platforms](platforms.md) stage 2 (identical movement in every build), side by
side; platforms stage 1, CI on Windows, macOS and Linux, is parked until
releases are near. As those merge, octree O1b and O3,
[large worlds](large-worlds.md) stage 1 (a generated benchmark world) and
renderer stage 2b follow. Here the order is:

- The save and quit fixes go first, because 2b's first session edits the same
  parts of the client: the application handler in `lib.rs`, and `hosting.rs`.
  They also merge before octree O1a, which edits some of the same server files.
- 2b's first session follows.

2a (faster, lighter world builds) and 2h (lean pristine regions) are gone: they
tuned the structures the octree removes. 2a's `/health` memory by part moved to
octree O1a.

#### Save and quit fixes

State: merged on 2026-11-14. The in-app checks below wait for the maintainer.

- **Decided:** close known ways of losing edits or rewriting a world for
  nothing, ahead of 2e's compact saves.
- **Server:**
  - Skip the final save when nothing changed since the last one. Today
    `Command::Stop` always returns a snapshot and `serve_inner` writes it, so
    every hosted session rewrites `world.json` when it ends. A world without a
    file yet, such as a new one (`WorldSaves::create` leaves the file to the
    server), is saved once when the server starts.
  - Autosave at most 10 s after the first unsaved change, even while editing
    continues. Today a save waits for 750 ms without edits (`SAVE_DELAY`), so
    steady editing postpones it indefinitely.
  - Make the rename durable: fsync the world's folder after it on macOS and
    Linux, where `write_guarded` stops at the rename today. Windows has no
    folder fsync, so rename there with `MoveFileExW` and
    `MOVEFILE_WRITE_THROUGH`, which returns only once the move is on disk.
- **Client:**
  - Every way of quitting waits for the embedded host's final save, on every
    system. Today only closing the window (`WindowEvent::CloseRequested`) takes
    the save path, and the app has no `exiting` handler, so quitting from the
    menu (Cmd+Q on macOS) may end the process first.
  - The per-frame updates (`poll_host`, `Game::update` and `poll_change`) move
    from `Runtime::render` into a tick that `about_to_wait` runs, so they go on
    while the window is minimized, which may get no redraws. Without that, a
    minimized window closed from the taskbar could never finish closing,
    because its save path runs only while drawing. When nothing draws, the loop
    waits for the next tick instead of spinning.
  - Session preferences save through the existing storage worker instead of
    writing and syncing on the UI thread (`SessionPreferences::save`, which the
    home screen calls).
  - Startup failures, such as a graphics adapter the renderer can't use, show a
    message and write a log file. Today `report_error` only logs, which nobody
    sees when the app wasn't started from a terminal. The message is a native
    dialog, `MessageBoxW` on Windows and `NSAlert` on macOS, through the
    windows-sys and objc2-app-kit crates that winit already uses; on Linux,
    `zenity` or `kdialog` where one is installed. It also goes to stderr, and
    the log file goes in the data folder (`data_directory`).
- **New `unsafe` code.** The Windows rename and the dialogs are the first
  `unsafe` calls outside `shaders::trusted`. Each gets a `SAFETY` comment, and
  the development guide's paragraph about `unsafe` names them.
- **Touches:** the server's `simulation.rs` (`Command::Save`, `Command::Stop`),
  `lib.rs` (save timing), `persistence.rs` (`write_guarded`), `network.rs`
  (`serve_inner`'s final save) and `Cargo.toml`; the client's `lib.rs` (the
  application handler and `Runtime`), `hosting.rs` and its tests, `launcher.rs`
  (the caller of `SessionPreferences::save`), `user_data.rs` and `Cargo.toml`;
  and `docs/development.md`. Runs beside the other tracks' wave 1 work;
  renderer N1 also edits the guide's rendering section, where the paragraph
  about `unsafe` is, and the second to merge rebases.
- **Acceptance in the app** (a copy of the library, through
  `SAUERTEIG_DATA_DIR`):
  - [ ] Open a world and quit without editing: `world.json`'s modified time
    doesn't change. A new world has its file as soon as it opens.
  - [ ] Edit without stopping for 30 s: the modified time advances at least
    every 10 s.
  - [ ] While hosting, edit and quit at once, by closing the window and by the
    menu's Quit (Cmd+Q), then reopen: the edit is there.
  - [ ] Edit while hosting, minimize the window and quit from the Dock, then
    reopen: the edit is there.
  - [ ] The same on Windows and Linux, including closing a minimized window
    from the taskbar. Needs those systems.
  - [ ] A forced startup failure shows a message, and the log file names the
    error.
- **Tests:**
  - [x] Server tests for the skipped final save, a new world's first save and
    the 10 s limit.
  - [x] Hosting tests that relied on a save after every session follow the new
    rule: `final_save_failure_is_returned_to_the_session` makes an edit so
    that a final save runs, and
    `repeated_host_sessions_save_and_release_the_listener` and
    `dropping_host_waits_for_save_and_lan_requires_explicit_opt_in` now rely on
    a new world's first save.
  - [x] Session preferences still save and load.
- **Record:** which quit paths were checked on which systems, and whether the
  Windows code was compiled, for example with
  `cargo check --target x86_64-pc-windows-msvc`, or waits for
  [platforms](platforms.md) stage 1's Windows job.

##### Result

What changed:

- **Server.** `Session` tracks `dirty_since`, the age of the oldest unsaved
  change, next to the quiet timer. A save is due after 750 ms without edits or
  once a change is 9 s old (`AUTOSAVE_LIMIT`; the save worker polls once a
  second, so a save starts within 10 s). A save that finishes while newer
  edits exist leaves them dirty with the age of the save's snapshot.
  `Command::Stop` returns a snapshot only when something is unsaved. A world
  whose file doesn't exist when the server starts is marked for saving at
  once, so it has its file within a second, or at the final save if the
  server stops first. `write_guarded` ends in `durable_rename`: rename then
  fsync of the folder on macOS and Linux, and `MoveFileExW` with
  `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` on Windows.
- **Client.** The per-frame updates moved into `Runtime::tick`, which
  `about_to_wait` runs once per drawn frame, or every 50 ms while the window
  isn't being redrawn (minimized, occluded, or no redraw for 250 ms; the last
  case covers Wayland, which reports neither state). Then the control flow
  waits for the next tick instead of polling, and each tick still asks for a
  redraw so that drawing resumes by itself. `App::exiting` runs
  `Runtime::finish_for_exit`, which starts the normal close and ticks until the
  host's final save is done (waiting at most 20 s for the server to confirm
  edits first; `Drop` then still waits for the host's save with no limit). A
  failed final save at that point shows the dialog. Session preferences are
  queued on the storage worker (`Work::Session`), counted in
  `pending_writes`, and failures come back through `take_write_error`.
  Startup failures go through the new `failure` module: stderr, an appended
  `sauerteig.log` in the data folder (restarted once it passes 256 KiB), and a
  native dialog. The window-system and `run_app` errors that used to `expect`
  now report the same way.
- **New `unsafe`.** `MoveFileExW` (server, Windows), `MessageBoxW` (client,
  Windows) and `NSAlert` calls (client, macOS), each with a `SAFETY` comment,
  all named in `docs/development.md`. New dependencies: `windows-sys 0.52` in
  both crates on Windows, and `objc2 0.5`, `objc2-app-kit 0.2` and
  `objc2-foundation 0.2` on macOS, the versions winit already builds.
  `Cargo.lock` gained only those dependency edges, with no new packages.

Checks, run in the worktree on the M5 Pro (macOS):

- `cargo fmt --all`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings` and `cargo test --workspace --locked`: clean and passing (client 173
  passed and 6 ignored, server 66).
- New server tests: `a_save_waits_for_quiet_but_never_longer_than_the_autosave_limit`,
  `changes_made_during_a_save_stay_unsaved_and_keep_their_age`,
  `replacing_a_world_file_leaves_only_the_new_world`,
  `a_session_without_changes_leaves_the_world_file_untouched`,
  `an_edit_is_saved_by_the_final_save_when_the_server_stops_at_once` and
  `a_new_world_is_saved_once_when_the_server_starts` (stopping at once and
  waiting for the first save). New client tests:
  `a_session_without_changes_does_not_rewrite_the_world`,
  `session_choices_save_on_the_storage_worker_and_failures_are_reported`, and
  the log tests in `failure.rs`. `final_save_failure_is_returned_to_the_session`
  now places a light after the first save.
- Windows: `cargo check --target x86_64-pc-windows-msvc -p sauerteig-client -p
  sauerteig-server --locked` (also `--all-targets` for the client) compiles,
  after the target was added with rustup. It is a check, not a link or a run,
  so `MoveFileExW` and `MessageBoxW` have not run.
- Linux: `cargo check --target x86_64-unknown-linux-gnu -p sauerteig-server`
  compiles. The client was not checked for Linux, whose audio and window
  libraries are missing here, so the `zenity` and `kdialog` code was compiled
  alone with `rustc` and run once with neither tool installed, which does
  nothing, as intended.
- Observed on macOS: with the renderer forced to fail temporarily (a patch that
  was removed again), the app wrote the error to stderr and appended it to
  `sauerteig.log` in the data folder, then stayed up, as it does while a modal
  alert is open. A first attempt showed that winit's run-loop observer panics
  if the alert opens after the event loop ended, so the dialog is shown inside
  the loop's callback. I could not see the alert itself.

Not observed, for the maintainer (the acceptance boxes above stay unticked):

- Whether `world.json`'s time stays put when a world is opened and quit, and
  advances every 10 s during steady editing. The server tests cover the logic
  with the hosted server, not the app.
- Closing the window and the menu's Quit or Cmd+Q, including `exiting`
  running on Cmd+Q (winit's source calls it from `applicationWillTerminate`),
  while hosting with a fresh edit, and the same from a minimized window.
- That the alert shows on macOS, and everything on Windows and Linux.

Choices and gaps:

- The 9 s limit, not 10 s, so that the one-second poll keeps saves within 10 s.
- The tick is once per drawn frame, so the on-screen fps figure still follows
  frames, and mouse events between frames don't tick extra. While idle it is
  20 Hz, which also keeps the connection alive in a minimized window.
- A session-preferences write that fails now surfaces when a session change
  or the quit drain next takes write errors, not on the home screen at once.
- Client documents other than the world (`settings.json`, `blueprints.json`
  and the session file) still use a plain rename without a folder sync.
- The quit drain gives up on waiting for the server to confirm edits after
  20 s, then closes anyway; the host's final save is always waited for.

#### 2b. Joining without freezing

State: session 1 (the network thread) merged on 2026-11-14; its in-app checks
wait for the maintainer. Session 2 waits for octree O1b and large-worlds stage
1. Two sessions, each merged on its own: the network thread, then building on
worker threads. Client only; the protocol doesn't change.

- **Decided:** the client half of decision 1, and decision 2's worker-thread
  build, still fed by today's JSON Welcome.

##### 2b, session 1: the network thread

- **Network thread.** A thread running a current-thread tokio runtime owns a
  tokio-tungstenite 0.29 socket in place of ewebsock (`Game::connect`,
  `Game::send` and the event loop in `Game::update`). tokio-tungstenite is
  already in `Cargo.lock` through axum.
  - It sends at once (ewebsock sends one queued message per 10 ms read cycle),
    counts the bytes it receives, and decodes messages off the frame thread.
  - It decodes the Welcome into a plain structure instead of the tagged
    `ServerMessage`, which briefly needs 465 MiB more for the_cube and 600 MiB
    more for nub1 (measured on the M5 Pro).
  - `max_message_size` and `max_frame_size` stay at 64 MiB. The library's
    default 16 MiB frame limit would refuse today's one-frame Welcome.
  - Messages reach the frame thread through a bounded channel that keeps only
    the newest snapshot, so a stalled frame loop backs up into the connection,
    where 2f can see it, instead of into memory. The frame thread drains it in
    the per-frame update, which the save and quit fixes keep running while the
    window is minimized.
  - About 5 s without any bytes from the server ends the connection and
    reconnects, counted from the end of the WebSocket handshake. Snapshots
    arrive at 20 Hz, so silence means the server is gone; today the client
    waits forever when a host vanishes without a reset. Connecting and the
    handshake get a longer limit of their own, such as 30 s: a host's embedded
    server answers the upgrade only after loading the world, which takes
    2.49–2.55 s for the_cube1m on the M5 Pro today and longer on slower
    computers.
  - Dropping `Game` still closes the connection before `HostedServer` waits for
    its final save, the order the quit path relies on (`impl Drop for
    Runtime`).
- **`wss://` works.** Add rustls with its `ring`, `std` and `tls12` features to
  the client, or install a crypto provider at startup. Today rustls 0.23.46 is
  built with only `std`, through ewebsock's `tls` feature and tungstenite
  0.24's `rustls-tls-webpki-roots`. With no provider, the first `wss://`
  connection panics in the connection thread with "Could not automatically
  determine the process-level CryptoProvider", and the client stays at
  "Connecting…". tokio-tungstenite's rustls feature has the same gap. Adding the
  three features removed the panic in the design critics' test.
- **TCP_NODELAY stays on.** ewebsock's blocking connector turns Nagle's
  algorithm off; tokio-tungstenite's `connect_async` doesn't. Open the
  `TcpStream` on the network thread with `set_nodelay(true)`, which is also
  where the byte counter goes, or pass `disable_nagle = true`. Loopback hides
  the difference (input p99 0.14–0.39 ms in all four on and off combinations,
  measured by the critics on the M5 Pro), so a test checks the socket option.
- **ewebsock goes.** The tests that drive a hosted server through it
  (`an_edit_survives_stopping_and_reopening_the_host` in `hosting.rs`, and
  `Wire` in `editor.rs`) use the network module instead.
- **Local network.** The macOS app's Info.plist gets
  `NSLocalNetworkUsageDescription` (`scripts/build-macos-app.sh`).
- **Touches:** a new client network module, `game.rs` (connection and
  Welcome), the client's `Cargo.toml`, the tests in `hosting.rs` and
  `editor.rs`, and `scripts/build-macos-app.sh`. Runs beside the other tracks'
  wave 1 work.
- **Acceptance in the app** (a copy of the library):
  - [ ] Minimize a client during a join and while others edit. Restored, it is
    current and still connected.
  - [ ] Kill and restart the host: clients reconnect as before, and edits
    behave as before. Freeze a host with `kill -STOP`: other clients notice
    within about 5 s and show that they are reconnecting.
  - [ ] A `wss://` address connects or shows an error, and never stays at
    "Connecting…". One pointed at a plain server shows the error path.
  - [ ] Join over the LAN from a second computer, ideally another system.
    Record whether macOS asks for Local Network access and whether a firewall
    prompts. Needs a second computer.
- **Tests:**
  - [x] Network-thread tests against a local server, in the style of
    `crates/server/src/bots.rs`: connect, Welcome, patches in order, sending
    without waiting on reads, and reconnecting after a close and after 5 s of
    silence, while a slow upgrade doesn't count as silence.
  - [x] A `wss://` handshake against a local rustls server with a test root
    certificate.
  - [x] TCP_NODELAY is on for the client's socket and the server's accepted
    socket.
  - [x] Existing client tests pass, including the moved ones.
- **Record:** the peak memory of a the_cube1m join before and after (the
  capture tool measured about 0.23 GiB less with the plain decode), how long
  other clients took to notice a frozen host, and the Local Network and
  firewall prompts.

##### Result (session 1)

What changed:

- **Network module.** `crates/client/src/network.rs` holds `Connection`. Each
  connection runs one thread with a current-thread tokio runtime and a
  tokio-tungstenite 0.29 socket; `Game::connect`, `Game::send`, the edit
  send in `process_edit_queue` and the event loop in `Game::update` use it.
  ewebsock is gone from `Cargo.toml` and `Cargo.lock`. The thread sends each
  message as soon as the frame thread hands it over (an unbounded channel;
  the 5 s silence limit ends a connection whose server stopped reading and
  talking). A byte counter wraps the TCP stream under the encryption.
  `max_message_size` and `max_frame_size` are 64 MiB.
- **Welcome.** The thread decodes it into `network::Welcome`, a plain
  structure, after a check of the message's first bytes; any other message,
  and a Welcome whose keys come in another order, goes through
  `ServerMessage` (a Welcome found that way is converted). `Game` gets the
  new `receive_welcome`; `ServerMessage::Welcome` in `Game::receive` is
  ignored.
- **Queue.** Up to 64 messages wait in order. Only the newest snapshot is
  kept, in its place in the order. When 64 messages wait, the thread stops
  reading, so a stalled frame thread backs up into TCP. `Game::update` drains
  it once per tick, with no 256-event cap.
- **Limits.** `SILENCE_LIMIT` is 5 s, counted from the end of the handshake,
  and only while the thread waits to read, so decoding a large Welcome or a
  full queue never counts as silence. `CONNECT_LIMIT` is 30 s for the TCP
  connection, the encryption and the upgrade. Either ends the connection with
  an `Event::Closed` error, and `Game` reconnects 3 s later as before.
- **Drop.** Dropping a `Connection` signals the thread, which sends a Close
  frame (at most 250 ms), closes the socket and is joined, so the connection
  is closed before `Drop for Runtime` goes on to `HostedServer`'s final save.
- **`wss://`.** rustls 0.23 with `ring`, `std` and `tls12` features, and the
  `webpki-roots` certificates. The client builds its `ClientConfig` with the
  `ring` provider explicitly, so no process-wide provider is needed.
- **TCP_NODELAY.** Set on the client's socket and read back; the server's
  accept loop already set it.
- **Tests moved.** `an_edit_survives_stopping_and_reopening_the_host`, the
  `place_light` helper and `Wire` in `editor.rs` now use
  `network::testing::Client`, a blocking player built on `Connection`.
- **Other.** `NSLocalNetworkUsageDescription` is in the macOS app's
  Info.plist. `docs/development.md` describes the client connection.
  New dev-dependencies of the client: `rcgen` and `tokio-rustls`
  (for the `wss` test). New dependencies: `tokio-tungstenite`,
  `futures-util` (already built for the server), `rustls`, `webpki-roots`.

Checks, run in the worktree on the M5 Pro (macOS):

- `cargo fmt --all`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings` and `cargo test --workspace --locked`: clean and passing (client
  190 passed and 7 ignored, server 67).
- New client tests in `network.rs`:
  `connecting_delivers_opened_then_messages_in_order`,
  `a_welcome_is_decoded_into_plain_data_whatever_its_key_order`,
  `only_the_newest_snapshot_waits_and_it_keeps_its_place`,
  `a_stalled_frame_thread_backs_up_into_the_connection_without_losing_messages`
  (3,000 messages of 8 KiB; also sends while nothing is read),
  `messages_are_sent_at_once_while_nothing_is_read`,
  `a_closed_connection_reports_it_and_a_new_one_reaches_the_server_again`,
  `a_server_that_goes_quiet_ends_the_connection`,
  `a_server_that_keeps_talking_is_not_silent`, `a_slow_upgrade_is_not_silence`,
  `an_upgrade_that_never_comes_ends_at_the_connect_limit`,
  `a_refused_connection_and_a_bad_address_say_why`,
  `dropping_the_connection_closes_the_socket_before_it_returns`,
  `dropping_a_connection_that_is_still_connecting_does_not_wait_for_the_limit`,
  `a_message_beyond_the_old_frame_limit_arrives_and_its_bytes_are_counted`
  (20 MiB), `the_clients_socket_has_nagles_algorithm_off`,
  `a_wss_handshake_succeeds_against_a_server_with_a_trusted_certificate` (a
  rustls server with a certificate from a test authority made by `rcgen`) and
  `a_wss_address_never_stays_connecting_when_it_cannot_be_trusted_or_is_plain`.
  The server has `accepted_sockets_have_nagles_algorithm_off`. The silence
  tests use a 400 ms limit; the 5 s and 30 s values are the constants.
- Peak memory of a the_cube1m join, release build, a standalone release
  server on a copy of the world, a headless client that joins and builds the
  `World` the way `Game` does (`/usr/bin/time -l`, three runs each, peak
  memory footprint): before (ewebsock, tagged `ServerMessage`) 1.18 GiB
  (1,265 MB), maximum resident size 1.42 GiB; after (this branch) 0.95 GiB
  (1,016 MB), resident 0.95 GiB, so 0.23 GiB less. The join took 1.5 s before
  and 1.3 s after, in the same process, counting the world build. The "after"
  measurement stays as the ignored test `network::tests::measure_join`; its
  doc comment says how to run it. The "before" harness was a temporary test
  and was not kept.

Not observed, for the maintainer (the acceptance boxes above stay unticked):

- Everything in the running app: minimizing during a join, restarting or
  `kill -STOP` of a host (how long other clients take to notice and what
  they show), a `wss://` address in the join box, and a join from a second
  computer, including whether macOS asks for Local Network access and whether
  a firewall prompts. The tests cover the network thread, not `Game`'s
  reconnect timer (unchanged: 3 s after a loss).
- The Info.plist key was added to the script, but the app was not rebuilt.
- Windows and Linux were not built.

Choices and gaps:

- Reconnecting stays in `Game`: the thread ends a connection and `Game` opens
  a new one after 3 s. The tests show a closed or silent connection ending
  and a new `Connection` working, not `Game`'s timer.
- The network thread decodes on its own thread, so a large Welcome delays
  its writes and its silence check while it decodes (about 0.1 s for
  the_cube1m). Nothing is sent before the Welcome anyway.
- Outgoing messages queue without a bound. The 16 MiB edit check in
  `process_edit_queue` is unchanged.
- The connection counts bytes (`Connection::bytes_received`) but nothing shows
  them yet; the join progress display of 2d can.
- Trusted certificates are the bundled web roots only, not the system's.
  A server with a private certificate authority cannot be joined over
  `wss://` yet.

##### 2b, session 2: building on worker threads

State: waits for session 1, octree O1b and large-worlds stage 1. Wave 2.

- **Building.**
  - On Welcome, worker threads build the World's tree, mesh every chunk, and
    create GPU buffers through a cloned `wgpu::Device` and `Queue`, which are
    `Send` and `Sync` in wgpu 24.
  - Each chunk is meshed, converted, uploaded and dropped in turn, with a
    bounded number in flight, and uploads are submitted every 128 chunks.
  - The frame thread installs the world and its chunk table on one frame, so
    world queries and visible meshes still change together, then applies the
    patches that arrived meanwhile. `Renderer::rebuild_all` splits into a
    prepare step for workers and an install step for the frame thread.
- **Releasing the old world.** The client releases the old world and its
  meshes when the connection drops; `Game::connection_lost` keeps them today.
  Until a new world installs, it draws an empty world (`World::empty`) under
  the session panel's status line.
- **Progress, memory and frame time.** The session panel shows the bytes
  received ("Downloading 23 MB"; 2d adds the total), then "Building world". F3
  shows the world's memory by part (leaves, triangles and vertex-buffer bytes)
  and the longest frame of the last 5 s beside the average.
- **Touches:** `game.rs` (Welcome handling, status and F3), the client's
  `lib.rs` (`session_ui`), `render.rs` (`rebuild_all` and buffer creation) and
  the network module. Runs beside octree O2 and renderer N2. Octree O2's second
  session also edits `game.rs` (patch handling), and renderer stage 2b also
  changes `rebuild_all`; when two of these are open at once, the second to
  merge rebases onto the first.
- **Acceptance in the app** (a copy of the library, on the M5 Pro):
  - [ ] Host or join the_cube1m and the benchmark world's smaller preset: the
    window keeps drawing, Esc opens the menu during the join, the progress
    line counts up, and the joiner's longest frame in F3 stays under 50 ms.
  - [ ] While someone joins the_cube1m, the host's and another connected
    client's longest frames stay under 50 ms.
  - [ ] The peak memory of a the_cube1m join, process plus GPU, is at least
    0.4 GiB below session 1's. Submitting uploads every 128 chunks cut nub1's
    peak by 0.81 GiB in the capture tool, about 0.6 GiB at the_cube's size
    (estimate). Measure as the design did: the larger of the peak footprint
    and the peak resident memory plus vertex buffers.
  - [ ] Drop a client's connection: its old world disappears under the status
    line, and the world it rejoins draws normally.
- **Tests:**
  - [ ] A world prepared on workers and installed matches one built on the
    frame thread: the same chunks, triangles and query results.
  - [ ] Existing client tests pass.
- **Record:** join times and peak memory for the_cube1m and the benchmark
  world, before and after; the longest frames seen; the time spent creating
  GPU buffers on worker threads.

### Wave 2

[Octree](octree.md) O2, grid-sized edits, runs with 2b's second session (above)
and renderer N2. Stage 2's own orders resume in wave 3, once O2 has settled the
edit messages that 2d encodes.

### Wave 3

#### 2d. Compact joins with a version handshake and a memory budget

State: waits for octree O2, both sessions of 2b and the save and quit fixes.
Large: three sessions in order on one branch, merged once. The design lead
commits each session's work to the branch. 2d absorbs 2c, the wire codec, which
had nothing to check in the running app on its own. It merges only once
movement is identical across builds and machines: [platforms](platforms.md)
stage 2 made it so on macOS ARM64 and x86-64, and before 2d merges the
maintainer checks that a client on their Windows computer joining a Mac host
doesn't snap.

- **Decided:** decisions 1–4 and 10, and the world-size and public-game rules
  above.

##### 2d, session 1: the codec

- **Codec** (a new `core::wire` module; postcard is the only new dependency,
  and flate2 and crc32fast are already in the build):
  - Mirror types used only on the wire and in saves, with conversions both ways
    to the game types. No internal tags (`ClientMessage` and `ServerMessage`
    use `#[serde(tag = "type")]`, which makes serde buffer whole messages), no
    `skip_serializing_if` or defaults, and append-only enums.
  - Protocol number 2; today's JSON counts as 1. Reserved fields hold what 2f
    adds to `Accept`: a resume token, and the last completed request id with
    its result.
  - Octree chunks: subtrees in pre-order, at most 256 KiB raw each, with a
    palette of leaf contents (geometry, surface materials and cube material)
    and a child mask per internal node. An exact piece stores its source
    relative to the leaf.
  - Objects travel in chunks of at most 256 KiB raw as well, so no part of a
    world depends on one message's size. With `MAX_OBJECTS` gone, nothing else
    would bound a single objects message.
  - Deflate through flate2 above 512 B. Inflating stops at the declared raw
    length, which must be within the cap.
  - Frame header `[lane u8][flags u8]`. Messages over 64 KiB split into parts
    that reassemble up to 8 MiB. An edit larger than that travels as several
    messages and commits as one once all have arrived, so message size never
    limits an edit.
  - Decoding returns errors and never panics. A sequence whose declared length
    passes its cap fails before allocating, and indices, sizes, coordinates and
    totals are validated. Each message decodes through a `DeserializeSeed` that
    carries one budget for the whole message (nodes, leaves and objects):
    per-sequence caps alone would let an 8 MiB message of nested subtrees
    decode to many times its size.
- **Fuzzing** runs on stable Rust as ordinary tests, so CI runs it on every
  system. A seeded generator (rand is already in `Cargo.lock`) makes random
  input and mutates valid encodings: truncated, bit-flipped, with counts and
  lengths raised, indices out of range and parts reordered. A fixed set of
  seeds keeps it under a minute in CI. The session also runs each decoder for
  at least 10 minutes on fresh seeds, records how many cases ran, and keeps
  the seed of any failure as a test. A test-only counting allocator checks
  every decode against its declared cap. Coverage-guided fuzzing (cargo-fuzz,
  which needs nightly Rust) is a later option.
- **Touches:** core's new `wire` module and tests, a module line in its
  `lib.rs`, and its `Cargo.toml`. It runs beside everything else.
- **Tests** (those that need no private world run on the Mac and in the Linux
  CI job):
  - [ ] Copies of every library world, nub1, the benchmark world and a
    synthetic world using every leaf kind and material round-trip through the
    codec with equal digests.
  - [ ] Golden byte fixtures for every protocol 2 message.
  - [ ] Fuzzing stays within the declared caps for every decoder, including
    deep subtrees and an edit split across messages.
  - [ ] Malformed input ends in an error, without a panic and within the caps:
    truncated data, oversized counts, a bad palette index, inflating past the
    declared size, and parts that reassemble past 8 MiB.
  - [ ] An edit larger than 8 MiB travels in several messages, commits as one,
    and each message decodes within the budget.

##### 2d, session 2: the server

- **Admission:**
  - The server waits at most 5 s for a JSON text `Hello` of at most 4 KiB, with
    at most 32 connections waiting, and does no world work before it passes.
  - Requests that carry an `Origin` header are refused.
  - The optional password is checked here; a wrong or missing one gets a JSON
    `ServerMessage::Error` saying so, then a close. It travels in plain text
    over `ws://`, so the docs say it protects only on an encrypted path
    (`wss://` or a VPN) and don't suggest it for a forwarded port.
  - The player count is checked after `Hello`, so a full room answers with a
    message the client shows (an HTTP 503 at the upgrade today), and 2f can
    admit a resuming player.
  - A `Hello` without protocol 2 gets a JSON `ServerMessage::Error` naming both
    versions, then a close. Builds from before 2d show it and retry every 3 s.
  - Join on the simulation thread as today (`Command::Join`), then send a
    binary `Accept`: the player id; a manifest with the world id, revision R,
    bounds, the totals the estimate needs (leaves, nodes, triangles, lights and
    objects), the chunk count and the compressed bytes; the players;
    `editing_allowed`; and the reserved fields.
- **Transfer:**
  - Object and octree chunks and `WorldEnd { R }`, then the patches queued
    since R, in order. A chunk whose subtree hasn't changed since it was last
    encoded is reused for later joins.
  - The writer alternates: after each reliable frame of at most 64 KiB it sends
    the newest snapshot, if one waits. Today the writer in `connection` prefers
    reliable messages (`biased`), so snapshots wait until those drain.
  - Shots are coalesced into a bounded list sent with the next frame. Making
    them replaceable like snapshots would drop shot sounds, hit flashes and shot
    effects, which come only from `Shots`.
  - TCP_NODELAY stays on when the progress wrapper goes, through axum's
    `ListenerExt::tap_io`; 2b's test keeps passing.
- **Settings.** The world budget (default 4 GiB), the player count (default 16)
  and the password become `Config` fields and standalone server options with
  environment variables, as `--bind` has. The password can also come from a
  file, so it needn't show in process lists.
- **Limits:**
  - The budget check runs on commits only (see World size). The importer
    reports the estimate and refuses nothing for size.
  - Delete `MAX_WORLD_FRAGMENTS`, `MAX_OBJECTS` with the importer's
    1,024-object check, `MAX_WORLD_SNAPSHOT_BYTES`, `SNAPSHOT_ENVELOPE_BYTES`
    and `MAX_GEOMETRY_BYTES` with the JSON accounting (`json_bytes`,
    `snapshot_geometry_bytes` and their callers), the importer's
    `MAX_WORLD_BYTES`, the write-progress wrapper
    (`network/transport.rs`) and the Welcome read-timeout exemption (`welcoming`
    in `read_commands`), and the limit of 2,048 object changes per edit
    (`MAX_OBJECTS * 2` in `edits.rs`); objects count in the estimate. Keep the
    60 s read timeout and the 15 s pings.
  - JSON saves: the loader accepts files up to at least 1 GiB, because a world
    within the budget can pass 512 MiB of JSON at about 300–350 B per shaped
    piece (computed). The writer refuses any save the loader would refuse,
    shows the error and keeps the previous file. Worlds that stay JSON after 2e
    keep this limit, even under a raised budget.
  - 64 KiB per WebSocket message both ways, checked before decoding; 8 MiB per
    reassembled message after inflating; and the per-message decode budget,
    sized for an 8 MiB message. Set axum's
    `read_buffer_size` explicitly; it defaults to 128 KiB per socket.
  - Commands read while that player's edit is pending are stored as received,
    still compressed, and decoded at dispatch. Small ones, up to 64 KiB, get a
    generous cap, such as 64; large ones count separately, at most 2. Overflow
    pauses reading from that player instead of ending the connection, as it
    does today, so pressing Undo repeatedly behind another player's edit never
    disconnects.
  - A server-wide inbound budget of 64 MiB covers reassembly buffers, stored
    deferred commands, and messages being inflated or decoded. Without it, the
    caps above let each player hold about 36 MiB: 20 MiB of deferred commands
    (two large, 64 small) and 16 MiB for a message being reassembled and
    inflated. That is about 0.56 GiB at 16 players, and more with a larger
    player count (computed).
  - Each player's backlog is 16 MiB, charged at binary worst-case item costs,
    until 2f replaces it; the write buffer is 128 KiB.
  - When a player's input backlog (`MAX_INPUT_BACKLOG`) overflows, the oldest
    input is dropped instead of the player. Inputs carry the mode request id,
    and the server drops old-mode inputs by tag.
- **Docs:** `crates/core/CONTRACT.md` (transport, persistence and objects),
  `docs/editor.md` (the 16 MiB message ceiling), `crates/importer/README.md`
  (the region and 64 MiB limits) and `docs/development.md` (server options and
  transfer limits).
- **Touches:** core `protocol.rs` (`Hello`) and the estimate; the server's
  `network.rs`, `network/transport.rs`, `lib.rs`, `edits.rs`, `outbound.rs`,
  `persistence.rs`, `simulation.rs`, `main.rs`, `bots.rs` and `tests.rs`, and
  the tests in `crates/server/tests/`; the importer's `main.rs` and
  `convert.rs`; and the docs above. Runs beside renderer N2.
- **Tests:**
  - [ ] A frame over 64 KiB, a `Hello` over 4 KiB and no `Hello` within 5 s
    each end in an error, without a panic and within the caps.
  - [ ] Origin refusal, the password, a full room, and the version refusal.
  - [ ] The 80-edit throttled-transfer test, with the edits after `WorldEnd`.
  - [ ] A world with several chunks of objects joins.
  - [ ] Input overflow, deferred Undo commands, TCP_NODELAY without the
    wrapper, a refused oversized JSON save (with a lowered cap), and a world
    over its budget that opens and accepts edits that shrink it.

##### 2d, session 3: the client

- **Client:**
  - Sends `Hello` with protocol 2, the player's name and the password, if one
    was entered, and decodes binary messages on the network thread within the
    per-message budget. The network thread alternates like the server's
    writer, so a builder's large upload doesn't hold up their own inputs.
  - Before downloading, computes the estimate from the announced totals and
    asks as described under World size, with "Join anyway". The host's own
    client asks the same with the host's estimate, and declining ends the
    hosted session. A test setting overrides the memory the client assumes, so
    the question can be checked on any machine.
  - Stops the download with a message if the server sends more chunks, bytes or
    leaves than it announced.
  - Builds at `WorldEnd` with 2b's pipeline, then applies the held patches in
    order.
  - Updates remote players from every snapshot, and reconciles the local player
    only when the snapshot's revision equals its own. Today `Game::receive`
    drops the whole snapshot when the revisions differ.
  - Reports "This host runs an older Sauerteig" when the first reply is a JSON
    `Welcome` or a frame over 64 KiB. After any refusal (another version, a
    wrong password, a full room) it shows the server's message and stops
    reconnecting on its own.
- **Reading memory** (a new client module):
  - The computer's total memory on every system: `GlobalMemoryStatusEx` on
    Windows (windows-sys), the `hw.memsize` sysctl on macOS (libc) and
    `/proc/meminfo` on Linux. Both crates are already in `Cargo.lock`.
  - On a discrete GPU (`AdapterInfo::device_type`), its video memory, read
    through the backend with wgpu's `Adapter::as_hal`, since wgpu 24 has no
    query of its own: DXGI's dedicated video memory on DirectX 12 and the
    device-local heaps on Vulkan. Meshes then count against video memory, not
    system memory. Where no answer comes, as for a discrete GPU under Metal,
    the question says the GPU's memory is unknown.
  - `Renderer::create` reads the GPU's memory once and keeps it. Each `unsafe`
    call gets a `SAFETY` comment, and the development guide's paragraph about
    `unsafe` names them.
- **Home screen** (`launcher.rs`):
  - The join form gets an optional password field.
  - Hosting options beside the LAN checkbox set the player count, the world
    budget and a password, and `HostedServer::start` passes them to `Config`.
    The player count and budget are remembered with the other home-screen
    choices; passwords aren't written to disk.
- **Touches:** the client's network module, `game.rs`, `lib.rs` (the session
  panel), `launcher.rs`, `hosting.rs`, `user_data.rs` (the remembered
  settings), `render.rs` (`Renderer::create` only), `Cargo.toml` and a new
  memory module; and `docs/development.md`. Runs beside renderer N2, which
  edits other functions in `render.rs`; the second to merge rebases.
- **Acceptance in the app** (a copy of the library):
  - [ ] Host nub1 from a copy of the library and join it from a second client:
    fly to its far corners, then paste and undo.
  - [ ] The benchmark world at nub1 scale hosts and joins.
  - [ ] Join the_cube1m from a second computer on the LAN: the progress line
    reads about 0.2 MB, and play starts in about a second (estimate). Needs a
    second computer.
  - [ ] A build from before 2d that joins a 2d host shows "This host runs a
    newer Sauerteig", and a 2d client that joins an older host says the host is
    older. The design lead builds `main` from before 2d into a scratch folder
    and names its path in the session's instructions.
  - [ ] While one client joins, the others see remote players keep moving and
    never see "busy".
  - [ ] A paste bigger than one message goes through. Today a maximal shaped
    paste of 16,384 roots is 17.08 MB of JSON (measured), which the 16 MiB
    message ceiling refuses.
  - [ ] With the test memory setting, joining a world above half the assumed
    memory asks first, and "Join anyway" joins. Hosting such a world asks the
    same, and declining returns to the home screen.
  - [ ] Host with a small world budget set on the home screen: an edit that
    would pass it is refused with the size-limit message, and deleting
    geometry still works.
  - [ ] Host with a player count of 2 (the host counts as one) and a password.
    A client with a wrong password sees the refusal and stops retrying, one
    with the right password joins, and a further client is told the room is
    full.
  - [ ] Pressing Undo repeatedly while another player's large edit runs doesn't
    disconnect.
  - [ ] Opening the server's address in a browser still shows its one-line
    page.
  - [ ] **Needs a tester:** on Windows or Linux with a discrete GPU, the
    question names that GPU's video memory. The project has no such computer.
  - [ ] On a minimum-specification computer, join a world near the default
    budget and record memory and frame times. Needs a tester with such a
    computer ([platforms](platforms.md) stage 3); the project has none yet.
- **Tests:**
  - [ ] The client stops a download that passes the announced totals, and
    follows the snapshot revision rule.
  - [ ] The estimate and the question under the test memory setting, for a
    joiner and a host, with and without a GPU's own video memory.
  - [ ] Reading the computer's memory gives a plausible total on macOS and in
    the Linux CI job; the maintainer's Windows computer checks Windows.
  - [ ] A movement golden over the benchmark world's test preset, received
    through the codec and built as a joining client builds it, is exact in dev
    and release builds on the Mac, under Rosetta and in the Linux CI job
    ([platforms](platforms.md) stage 2's rule).
- **Record:** wire sizes for every library world, nub1 and the benchmark world
  (a region encoding measured about 0.2 MB for the_cube1m and nub1); LAN join
  times for the_cube1m and nub1; joiner and host memory, peak and settled,
  measured in the game and hosting app rather than the capture tool, for
  the_cube1m, nub1, the benchmark world with and without its edited layer, and
  an edited copy of an imported map (pasted imported slopes, rooms carved in
  large imported cubes, scattered small blocks); the per-item costs set from
  these; the fuzzing runs; and the memory each system tried reported.

### Wave 4

2e and 2f start once 2d is merged, and renderer N3 runs beside them. They share
two server files: `network.rs` (2e's save worker, 2f's connection code) and
`lib.rs` (2e adds a `Config` field).

#### 2e. Compact saves with safe conversion

State: waits for 2d. Medium.

- **Decided:** decision 7, and the version policy for world files.
- **Format 4, `world.sauerteig`:**
  - A magic number, a header (format, world id, revision, bounds, the octree
    root's origin, `next_object`, the estimate's totals including triangles,
    and the digest, size and modified time of the `world.json` it came from),
    and the wire chunks, each with a CRC32.
  - Written whole: temporary file, fsync, rename, folder fsync. Chunk bytes
    come from 2d's encoder, reusing unchanged chunks. No journal.
  - One previous generation is kept. A corrupt file is moved aside before a new
    one is written. The writer refuses anything its loader would refuse.
- **Loading:**
  - JSON v1–3 and format 4 are recognized by content. Library worlds prefer
    `world.sauerteig`.
  - An explicit library flag in `Config` says which worlds convert; today
    hosting passes `.../world.json` either way (`HostedServer::start`). Worlds
    opened by an explicit `.json` path stay JSON, such as the standalone
    server's `--world` and `.local/the_cube1m.json`. The capture tool reads its
    own JSON (`load_world` in `capture.rs`) and is unaffected.
  - A world saved by a newer release is refused with a message naming both
    format versions.
  - The save loader gets fuzz targets under a counting allocator, like 2d's
    decoders.
- **Conversion,** on the first save after an edit:
  - Write `world.sauerteig`, read it back, and compare it with the snapshot
    being saved. When a world opens, separately check JSON to binary to JSON on
    the unedited world, in memory.
  - If either check fails, write the edited world as JSON to a new sibling file
    (never `world.json`), show a visible error, and stop converting that world.
  - Detect a changed `world.json` by content. Record a canonical digest of its
    parsed content (revision, regions, cells, roots, volumes and objects, with
    `next_object` normalized). On open, hash `world.json` only when its size or
    modified time changed, and only when the digest differs, keep both and offer
    the JSON as a separate world. Builds before the save and quit fixes rewrite
    `world.json` whenever a hosted session ends, and copies and backups change
    modified times, so size and time alone would raise false prompts.
  - A world without its own `world.json` (new worlds, whose first save is now
    binary, binary imports, and duplicates made by this build) gets a stub
    `world.json` with `format_version` 4, `revision` 0 and empty `cells` and
    `objects`, the fields older builds require. They parse it and refuse it
    ("Invalid saved world limits.") instead of opening a default arena, which
    `load_world` does when `world.json` is missing, and saving that arena over
    the world.
  - The host lock keeps its `world.json`-derived name (`lock_world`), so an old
    and a new build can't host one world at once.
- **Dry run first.** Before any real world converts, convert edited copies of
  the 23 library worlds, nub1, `.local/the_cube1m.json` and a synthetic world
  using every leaf kind and material. Each copy first gets a scripted edit set
  at several grid sizes: paste copied imported slopes with face materials,
  carve rooms inside large imported cubes, place small blocks, paint water and
  glass, edit objects, and undo once. The library holds unedited imports and
  one small world made in Sauerteig, so without these edits the dry run would
  test none of what real conversions contain. Then convert, read back, reopen
  and compare digests, and repeat with a second edit. The report lists, per
  world, the leaves, JSON bytes, binary bytes, digest result and save time.
- **Library:**
  - Import parses and validates a JSON or binary world before adding it; today
    `WorldSaves::import` copies bytes unparsed.
  - Duplicate mints a new world id.
  - Add Export as JSON, which writes version 3 so builds from before 2e open
    it. The library has no export today.
  - The importer picks its output format from the file extension (`.json` or
    `.sauerteig`), so scripts that read its JSON keep working.
- **Autosave** within 1 s of the first unsaved change for worlds saved as
  `world.sauerteig`, down from the save and quit fixes' 10 s, now that those
  saves take milliseconds. Worlds that stay JSON keep the 10 s limit: each of
  their saves rewrites the whole file, 45 MB for the_cube1m and 62 MB for
  nub1.
- **Touches:** core `wire` (format 4), the server's `persistence.rs`,
  `network.rs` (the save worker) and `lib.rs` (`Config`), the client's
  `world_saves.rs`, `hosting.rs` and library screen (`launcher.rs`), and the
  importer's `main.rs`. Runs beside 2f (shared files above) and renderer N3.
- **Acceptance in the app** (a copy of the library; the maintainer checks the
  real library after merge):
  - [ ] The dry-run report passes for every world before anything converts.
  - [ ] Editing and quitting creates `world.sauerteig` beside `world.json`,
    about 0.2 MB for the_cube1m plus the edits, and `world.json` keeps its
    digest and modified time.
  - [ ] Reopening the largest worlds spends well under a second loading.
  - [ ] A build from before 2e (the design lead's scratch build of `main`)
    opens an exported JSON and a converted world's untouched `world.json`, and
    refuses a binary-only world without creating an arena. After it edits a
    converted world, this build offers that `world.json` as a separate world;
    after it only opens and closes one, no prompt appears.
  - [ ] Duplicate works, and importing a broken file fails at import with a
    message.
  - [ ] The capture poses still render from `.local/the_cube1m.json`.
- **Tests:**
  - [ ] Digest equality after conversion for every library copy, nub1, the
    benchmark world and the synthetic world.
  - [ ] Truncated or corrupt files are refused clearly and moved aside, and
    `world.json` still opens.
  - [ ] An injected read-back failure writes the sibling JSON and shows the
    error.
  - [ ] A crash between the temporary file and the rename loses nothing.
  - [ ] JSON v1–3 fixtures load, and the importer's binary output loads in the
    server.
  - [ ] Fuzz targets for the save loader stay within their caps.
- **Record:** the dry-run report, save and reopen times, and file sizes.

#### 2f. Nobody waits for a slow player; reconnect as yourself

State: waits for 2d. Medium.

- **Decided:** decisions 5 and 6. "Catching up" is a progress line over a blank
  world, because the client releases the old world first (2b). Keeping the last
  picture on screen would cost about 1.4 GiB more on nub1 (estimate).
- **Resume:**
  - `Accept` carries a resume token in the field 2d reserved, and `Hello`
    carries it back.
  - A departed player's id, name, score, undo and redo history and edit tokens
    are kept for 10 minutes, at most one entry per player slot, and freed on
    the edit worker.
  - A `Hello` with a valid token restores them, closing any stale connection
    first. A valid token is admitted even when the room is full: a dropped
    player's old connection still counts until the 60 s read timeout.
  - On resume, `Accept` carries the last completed request id and its result,
    so the player knows whether their last edit landed.
- **No waiting:**
  - Delete `Session::peers_have_room`, `COMMIT_WAIT`, `COMMIT_POLL`, `BUSY` and
    `CommitError::Busy`, with the retry loop in `run_edit`.
  - One 16 MiB list of recent patches serves the room, each patch encoded once,
    with a read position per player. It replaces per-player reservations and
    the cost model in `outbound.rs`.
  - Per-player messages (`EditResult`, `ModeChanged`, `Synced`, notices and
    errors) keep their order relative to the list. Either they enter the list
    tagged with their recipient, or each is stamped with the list position when
    queued and released only after that player's reader passes it. The contract
    requires a changed edit's patch to arrive before its successful result, and
    the client sends its next edit, based on its current revision, as soon as
    that result arrives (`crates/core/CONTRACT.md`, "Shared edits and history").
- **Resync:**
  - A player who falls off the end of the list is closed with a resync code,
    rejoins at once and sees "Catching up".
  - Three resyncs in a minute show "Your connection can't keep up" and stop
    automatic reconnect.
  - A test setting shrinks the list. Real pastes compress to about 8 KB
    (measured), so a 16 MiB list rarely overflows and the path needs help to be
    seen.
- **Docs:** correct the queue paragraphs of `docs/development.md` and
  `crates/core/CONTRACT.md`.
- **Touches:** the server's `network.rs` (connections and the edit worker),
  `lib.rs`, `edits.rs`, `outbound.rs` and `simulation.rs`; the client's network
  module and `game.rs`. Runs beside 2e (shared files above) and renderer N3.
- **Acceptance in the app:**
  - [ ] Pause a client with `kill -STOP` (macOS, Linux) while the host pastes
    repeatedly with the shrunken list, then resume it with `kill -CONT`. Nobody
    sees a busy message, and the paused client shows "Catching up" and returns
    as the same player, with Undo still undoing its last edit.
  - [ ] Drop a client's connection with a debug key, or a local proxy that
    drops or throttles the socket: the client shows "Reconnecting", then
    returns with the same name and score.
  - [ ] Turn Wi-Fi off for about 5 s, then on. Maintainer only: implementation
    sessions don't change system settings, and a short drop often leaves the
    TCP connection intact.
- **Tests:**
  - [ ] Resume restores the id and history; tokens expire; the departed list
    stays bounded; a full room admits a valid token.
  - [ ] Overflow leads to a resync close and an immediate resume.
  - [ ] Commits proceed while a peer is wedged (today's stall test, inverted).
  - [ ] A player whose reader lags the list makes two quick edits on the same
    cube, and the second isn't rejected as a conflict.
  - [ ] The three-resync limit, and the last result in `Accept` on resume.
- **Record:** resync and resume times for the_cube1m, nub1 and the benchmark
  world, and the memory the departed list holds.

### After wave 4

The 8 GB minimum made renderer N3, 2h and 2g required (maintainer,
2026-11-13). The octree replaces 2h, since it stores no triangles. N3 runs in
wave 4, and 2g comes after 2f. Whether the minimum still needs N3 and 2g is
checked against the octree's measurements.

#### 2g. The host shares its world

State: starts after 2f. Medium.

- **Decided:** this ends decision 8. A test setting still sends the host
  through the socket, so the remote path stays exercised.
- An in-process link for the embedded host: typed messages over channels, with
  no encoding. The host's client starts from `World::clone()` at the join
  revision, sharing structure, and applies the same typed patches.
- The server's connection code becomes generic over a small link type, with a
  socket implementation and a local one. A later QUIC link would use the same
  boundary.
- The host's memory estimate drops to one world copy.
- **Touches:** the server's `network.rs` and `lib.rs`, and the client's
  `hosting.rs` and network module. Runs beside renderer N3.
- **Acceptance:**
  - [ ] In the app, hosting the_cube1m shows one world copy less in F3 and the
    process's memory. The host's own join is instant.
  - [ ] Protocol tests run over both links.
- **Record:** host memory for the_cube1m, nub1 and the benchmark world, and the
  new host estimate.

### Optional: LAN games list

State: optional; after 2d, because it announces the protocol number and whether
a password is needed.

- A UDP broadcast responder in the embedded host, running only when the host
  chose LAN play, answers with the world name, player count, protocol, port,
  and whether a password is needed. It parses queries as untrusted input and
  rate-limits its replies. Cube 2 used port 28784.
- A "LAN games" list on the home screen joins with one click.
- **Touches:** the client's `hosting.rs` and home screen (`launcher.rs`). Runs
  beside any stage after 2d that leaves those alone.
- **Acceptance:**
  - [ ] With two computers on one network, ideally on different systems, the
    second lists the first's hosted world and joins it in one click. Needs a
    second computer.
  - [ ] Unit tests for the responder and the parser.
- **Record:** the firewall and Local Network prompts on each system tried.

## Later stages

- **Stages 3 and 4 are parked** (maintainer, 2026-10-25): internet multiplayer
  and multiplayer playtesting aren't current priorities, so don't spend effort
  on them until the maintainer asks.
  - **Stage 3, instant edits:** apply edits locally at once and reconcile them
    with the server's result, with several edits in flight. Stage 2 keeps every
    world change in one revision order on one lane, which this needs.
  - **Stage 4, joining from other networks:** a public multiplayer game needs
    this eventually. Paid services are ruled out, and Tailscale is not part of
    the plan (maintainer, 2026-11-13). Internet play is the game's own feature:
    dedicated servers that anyone can run and players can find, and games
    hosted on home computers behind routers. The earlier plan, a Cloudflare
    quick tunnel with Tailscale as the fallback, came from
    [internet play](../../research/internet-play.md), which assumed players
    joining one player's computer from browsers. Browsers need HTTPS pages and
    secure WebSockets and can only get through NAT with WebRTC, so redo the
    options for desktop clients when this stage resumes.
    - **Options then:** a direct connection (IPv6, router port mapping or a
      forwarded port), UDP hole punching (likely QUIC), or a tunnel the host app
      starts. Whether fighting or building comes first is decided then.
    - **What stage 2 leaves ready:** a protocol number in `Hello`, a lane byte
      in every frame, an optional password, Origin refusal, resume after a
      drop, and working `wss://`.
    - **Encryption.** The password and the resume token travel in plain text
      over `ws://`, so internet play needs encryption before either protects
      anything: for example TLS on the same WebSocket with the host's
      certificate pinned in a join link, or QUIC. Admission limits per address
      also need the real client address, which a tunnel hides.
    - **QUIC's datagrams help only on a direct UDP path.** iroh's relays carry
      traffic over WebSocket or HTTPS, and a quick tunnel can't carry UDP, so
      play through either keeps TCP's stalls. Keep WebSocket as the fallback for
      networks that block UDP (3–5%, from 2016 studies cited by RFC 9308).
      iroh's public relays are free but meant for development and hobby use,
      and support only the latest iroh release, which clashes with players on
      mixed releases.
    - **Lab evidence** (quinn 0.11.12 through an emulated 10 Mbit/s link with a
      100 ms router buffer, 2026-11-12, M5 Pro). TCP itself was not measured.
      - With 100 ms round trips and 1% loss, snapshot p99 was about 205 ms on
        one QUIC stream, against about 100 ms as datagrams once lost snapshots
        count as late (70 ms for those that arrived).
      - Behind a 4 MB transfer at 40 ms round trips, one QUIC stream with the
        default send window delayed snapshots by a median of 0.9 s; a 32 KB
        window kept their p99 at 38–67 ms.
      - The first datagram limit is 1,162 B, rising to 1,414 B after path-MTU
        discovery. A wrong pinned key is refused during the handshake.
    - **TCP tuning before QUIC.** If movement hitches behind bulk data, cap
      unsent data with `TCP_NOTSENT_LOWAT`. socket2 0.6.5 offers it only on
      Linux and Android, behind its `all` feature. macOS needs a raw
      `setsockopt` (0x201 in the SDK), and Windows has no such option. There,
      as through a tunnel, only the game's own pacing helps (inferred).
    - **A client chunk cache,** if one is added, keys chunks by a 128-bit hash
      of their uncompressed bytes, so the player's own library can serve them:
      the_cube and thecubesunlit share every chunk.
    - **First step when it resumes:** measure round trips, loss and router
      behavior on real home connections, then choose.

## Evidence

Stage 1, measured on 2026-10-25 on the maintainer's M3 Pro. Each measurement is
a 30-second release run on a copy of that Mac's largest library world:
- two moving fighters;
- one 16,384-root, 65,536-fragment paste every two seconds;
- forced saves;
- a bot joining repeatedly.

| Work (p50 / p99 / max ms) | Before | After, final run |
| --- | --- | --- |
| Active tick | 0.073 / 75.364 / 528.795 | 0.091 / 1.015 / 6.693 |
| Save snapshot | 25.316 / 75.320 / 81.360 | 0.002 / 0.012 / 0.012 |
| Join admission and snapshot | 6.261 / 10.598 / 13.731 | 0.012 / 0.048 / 0.317 |
| Owner edit work | 216.176 / 445.703 / 528.581 | 2.185 / 3.147 / 3.424 |

- **The earlier run was worse.** It reached a 2.231 ms p99 and an 86.911 ms max
  tick, and its edits peaked at 36.872 ms. The baseline lost a moving bot; both
  after-runs kept all bots connected and accepted every paste.
- **Tests** cover:
  - snapshot consistency across every authored layer;
  - ticks continuing through a slow save;
  - a throttled transfer longer than five seconds, with 80 later edits arriving
    in order;
  - byte-budget backpressure;
  - stall detection;
  - panic closure with the saved bytes unchanged.
- **Save and reopen.** The edited 55 MB world reopened, accepted four joins, and
  saved byte-identically.
- **Checks.** fmt, Clippy, the workspace tests, and the native and browser
  builds passed for the implementer and again at merge.
