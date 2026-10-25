# Shared sandbox

State: active.

## Outcome

Friends build together in one world over the internet, including large imported
maps. A builder's edits appear immediately on their own screen and promptly for
everyone else. Joining shows progress and succeeds on ordinary home connections.
Nobody's game hitches when someone else pastes, saves, or joins.

## Current behavior

These limits are invisible on one machine and decisive over the internet:

- One `Mutex<Session>` serializes the 60 Hz tick, every inbound message, save
  snapshots, and each joiner's copy of the world
  ([network.rs](../../crates/server/src/network.rs)). Edit validation and
  clipping run under that lock, so a large paste or save stalls every player,
  and a panic poisons the lock for the whole room. The embedded host runtime has
  two worker threads.
- A joining player receives the whole world as one JSON message, about 45 MB for
  the largest archive maps, under a fixed 5 second write timeout. The client then
  builds and meshes it on the main thread, freezing the browser.
- The wire format is JSON over one reliable WebSocket. Protocol types carry
  JSON-specific serde attributes, so a compact encoding needs a codec boundary.
- Each editor action waits for the server's acknowledgment before the next one
  is sent, so building feels sticky at internet latency.
- Accepted sockets do not set `TCP_NODELAY`.
- Browser players on another computer need HTTPS and secure WebSockets, which
  the native host does not provide.

## Stage 1: the simulation never waits

State: active on branch `sandbox-sim-ownership`.

Decisions:

- **One owner.** A dedicated simulation thread owns `Session` and runs the fixed
  60 Hz clock. Network tasks never lock it: they send typed commands over
  channels and receive outbound messages over per-peer channels. Admission and
  the health endpoint read published status instead of locking.
- **Cheap snapshots of the authored world.** Taking a consistent snapshot of
  regions, coarse cells, refined roots, override masks, volumes, and objects costs
  at most a few hundred microseconds on the largest library world, through
  structural sharing: immutable shared region data plus persistent or
  copy-on-write maps for edits. Derived data such as triangles, query indexes,
  and meshes is never part of a snapshot.
- **Saves and joins work from snapshots off the simulation thread.**
  Serialization and socket writes never run on it. A joiner never gets dropped
  because edits arrived during their transfer: those edits reach them afterwards,
  in revision order, with the backlog bounded by bytes rather than message count.
  Replace the fixed 5 second limit on the world transfer with a stall timeout
  that fails only when no bytes drain for 30 seconds.
- **Heavy edit work happens off the simulation thread.** Validation, clipping,
  canonicalization, and compilation that depend only on the request run
  elsewhere. The simulation thread performs the conflict check against newer
  changes, budget and fighter-overlap checks, and the apply. Remove the repeated
  validation and canonicalization of one request (currently at
  `server/src/lib.rs` 562, 591, 700, 1031 and `core/src/fine.rs` 289, 310).
- **Failures are contained.** No `unwrap` or `expect` on client-controlled data in
  the simulation path. If the simulation thread panics anyway, connections close
  with a visible error, the host app reports it, and the last saved world stays
  intact. A room never hangs.
- **`TCP_NODELAY`** on server sockets, and on native client sockets if ewebsock
  allows it; record it if not.
- **Measure.** Track tick duration (p50, p99, max) and attribute slow ticks to a
  cause; expose this in `/health` and server logs. Add a headless bot harness of
  scripted WebSocket clients that join, move, paste blueprints, request saves,
  and rejoin, usable from tests and as a command-line stress tool.

Not in this stage: the wire format and chunked transfer (stage 2), instant local
edits (stage 3), and joining from other networks (stage 4). Keep the JSON
protocol and the save format unchanged. White-box tests that mutate `Session`
internals may be rewritten against the command interface.

Parallel work: renderer stage 1 runs at the same time on another branch. Do not
modify `crates/client/src/render*`, shaders, `materials.rs`, `props.rs`,
`lighting.rs`, or `scene.rs`. Limit client changes to hosting and connection code.

Acceptance:

- fmt, Clippy, all workspace tests, and native and browser builds pass.
- Tests show the tick keeps its rate while a large save serializes and while a
  large world goes to a slow joiner; a throttled joiner completes; an injected
  simulation panic closes connections cleanly with the save intact; and edits
  made during a join reach the joiner in order.
- Measured with the bot harness on the largest library world, with two moving
  bots, one bot pasting the largest allowed blueprint every two seconds, forced
  saves, and a joining bot: report tick p50, p99, and max before and after.
  Target: no tick over 8 ms caused by saves or joins and none over 16 ms caused
  by edits. If edits miss that, report the measured cost and its cause.
- Observed: with the native app hosting a large imported world, a browser client
  and a native client on this Mac stay smooth while the other pastes and saves.
  Join, save, and reopen still work.

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

None yet for this outcome.
