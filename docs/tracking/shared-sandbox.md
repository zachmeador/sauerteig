# Shared sandbox

State: active.

## Outcome

Friends build together in one world over the internet, including large imported
maps. A builder's edits appear immediately on their own screen and promptly for
everyone else. Joining shows progress and succeeds on ordinary home connections.
Nobody's game hitches when someone else pastes, saves, or joins.

## Current behavior

- **The simulation has one owner.** It runs on its own thread, and saves, joins,
  and edit preparation work from shared snapshots off that thread (stage 1).
- **A joining player still receives the whole world as one JSON message,** about
  45 MB for the largest archive maps.
  - The server now waits through a 30 second stall rather than a fixed 5 second
    limit.
  - The client still builds and meshes the world on the main thread, which
    freezes the browser.
- **The wire format is still JSON** over one reliable WebSocket. Protocol types
  carry JSON-specific serde attributes, so a compact encoding needs a codec
  boundary.
- **Each editor action still waits** for the server's acknowledgment before the
  next is sent.
- **Browser players on another computer** need HTTPS and secure WebSockets, which
  the native host does not provide.

## Stage 1: the simulation never waits

State: merged on 2026-10-25. The maintainer has not yet checked it in the running
app.

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
    with a "busy, try again" result. Nobody is evicted, and other players' edits
    keep flowing.
  - **Exact reservations.** Publication reservations use the same sizes as the
    packets they reserve, plus room for the follow-up result.
  - **Nothing lost on close.** Commands a client sent before closing still apply,
    even without a Sync.
  - **Smaller fixes:**
    - failures publish before logging;
    - a full room says why it closes;
    - departing players' undo history is freed off the simulation thread.

Gaps:

- **Ticks are still not always fast.** One run recorded an 86.9 ms tick dominated
  by physics, and a 36.9 ms edit. Profile collision queries on large maps under
  the [large-worlds](large-worlds.md) track before claiming a consistent frame
  budget.
- **Not yet observed in the running app:**
  - smoothness while another client pastes and saves on a large hosted world;
  - saving, reopening through the home screen, and rejoining in the browser;
  - the host's error display after a simulation failure;
  - a builder seeing the busy message while a friend joins a large world.

## Later stages

- **Stage 2, compact streamed transfer:**
  - Separate wire types from game types behind a codec boundary, using a compact
    binary encoding with compression.
  - Put a link boundary with two lanes under it: reliable framed messages
    (welcome, chunks, edits, patches) and an unreliable lane for input, snapshots,
    and effects. Over WebSocket the unreliable lane is "latest wins", so a later
    datagram transport slots in without redesign. See
    [internet play](../../research/internet-play.md).
  - Send the world in independently decodable chunks with a progress display.
  - The client builds and meshes incrementally without freezing the browser.
  - **Design decided after [research](../../research/wire-encoding.md):**
    - postcard for messages;
    - a hand-packed delta format for bulk tables;
    - zstd per chunk (ruzstd to decode in wasm);
    - chunks ordered by distance from spawn;
    - a build time-sliced by clock.

    The largest library world shrinks from 44.9 MB of JSON to 165 KB. Building it
    on the client (about 1 s of CPU in wasm) becomes the main work.
- **Stages 3 and 4 are parked** (maintainer, 2026-10-25): internet multiplayer and
  multiplayer playtesting aren't current priorities, so don't spend effort on them
  until the maintainer asks.
  - **Stage 3, instant edits:** apply edits locally at once and reconcile with the
    server's result, with several edits in flight.
  - **Stage 4, joining from other networks:** the group played over the internet,
    so this is needed eventually. Paid services are ruled out. When it resumes,
    start with a free Cloudflare quick tunnel started by the host app, with
    Tailscale's free plan as the fallback. The constraints and options are in
    [internet play](../../research/internet-play.md).

## Evidence

Stage 1, 2026-10-25. Each measurement is a 30-second release run on this Mac's
largest library world (copied):
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
- **Checks.** fmt, Clippy, the workspace tests, and the native and browser builds
  passed for the implementer and again at merge.
