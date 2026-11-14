# Platforms

State: planned.

## Outcome

Sauerteig builds, passes its tests and plays the same on Windows, macOS and
Linux, on computers down to the minimum specification, and players can install
it without paid signing.

## Minimum specification

Approved by the maintainer on 2026-11-13:

- **8 GB of memory and graphics about as fast as a Steam Deck's,** with Vulkan,
  DirectX 12 or Metal.
- **Reference machines:** a Steam Deck, an 8 GB M1 MacBook Air and an Intel Iris
  Xe laptop. All three share system memory with their graphics. The Steam Deck
  has 16 GB, so it stands for the graphics speed and the MacBook Air for the
  memory.
- **Not yet checked.** Nobody has run Sauerteig on any of them. Development and
  every measurement so far use the maintainer's M5 Pro with 64 GB; older numbers
  come from an M3 Pro. [Stage 3](#stage-3-check-the-minimum-specification) needs
  a tester.
- **For scale,** Steam's October 2026 hardware survey counted 16 GB in 41.42% of
  systems and 32 GB in 37.06%.

What it decides:

- **World size.** A server's default world budget is what a computer at this
  specification can join using half its memory: 4 GiB of estimated memory.
  How many cubes that holds is measured after the [octree](octree.md);
  [shared-sandbox](shared-sandbox.md#world-size) explains the estimate, and its
  work order 2d builds the budget.
- **Order of work.** It made renderer N3 (lighter meshes), shared-sandbox 2h
  (lean regions) and 2g (the host shares its world) required. The octree
  replaces 2h, and whether N3 and 2g are still needed is checked against the
  octree's measurements.
- **Catching up.** A player who falls behind sees a progress line over a blank
  world (shared-sandbox 2f), because the client releases the old world before
  it loads the new one (shared-sandbox 2b). Keeping the last picture on screen
  would cost about 1.4 GiB more on nub1 (estimate).
- **Quality tiers.** The [renderer](renderer.md)'s lowest tier must run smoothly
  on this specification. Tiers vary detail only (render scale, shadow
  resolution, anti-aliasing), never which lights shine or cast shadows
  (maintainer, 2026-11-11).
- **Memory estimates** read each system's memory and count a discrete GPU's
  video memory separately. World meshes take about 2 KB per piece today, 0.9 GiB
  for nub1, as Metal allocates them on the M5 Pro (measured) before the octree
  leaves out hidden faces, and on a discrete GPU they live in video memory.
  wgpu 24 reports whether an adapter is discrete
  (`AdapterInfo::device_type`) but not how much memory it has, so that needs a
  query for each backend.

## Current behavior

- **Builds.** Players build from source ([README](../../README.md#try-it)), and
  Linux needs a few [system libraries](../development.md#linux-dependencies).
  `scripts/build-macos-app.sh` packages an unsigned Mac app. There are no
  downloads and no Windows or Linux packages.
- **CI** runs one job on GitHub's `ubuntu-latest` runner (x86-64): formatting,
  Clippy, the workspace tests and a build of the server and client, all in the
  dev profile. Nothing checks Windows, macOS, ARM64 or release builds. The
  repository is private.
- **The running app** has run only on Apple Silicon Macs with Metal: an M3 Pro,
  then the M5 Pro since 2026-11-11. Windows, Linux, Vulkan and DirectX 12 are
  untried.
- **Movement differs between builds** in the last bit
  ([stage 2](#stage-2-identical-movement-in-every-build)).
- **Platform-specific code** is small so far: the data folder for each system
  (`data_directory` in `crates/client/src/user_data.rs`) and one Unix-only test
  (`hosting_paths_cannot_follow_links_out_of_managed_saves` in
  `crates/client/src/world_saves.rs`).

## Stage 1: CI on Windows, macOS and Linux

State: parked until releases are near (maintainer, 2026-11-14). Meanwhile the
maintainer playtests on their own Windows computer, and the Linux job that
already runs on every push stays as it is. A first draft of the workflow below
is on the local branch `platforms-ci` (commit df4b48d), never pushed.

- **Decided:**
  - Windows, macOS and Linux are equal targets (maintainer, 2026-11-13).
  - CI stays within GitHub's free allowance. Paid services are ruled out, so an
    overrun must stop CI rather than bill anyone.
  - While the repository is private, Windows and macOS run on a schedule that
    fits the allowance. Once it is public, every job runs on every push and pull
    request, Clippy runs on every system, and Windows ARM64 (`windows-11-arm`)
    joins.
- **GitHub's rules,** checked 2026-11-13 in GitHub's billing documentation:
  - Standard GitHub-hosted runners are free in public repositories.
  - For private repositories, GitHub Free includes $12 of Actions compute a
    month (GitHub Pro, $18). A minute costs $0.006 on Linux x86-64, $0.005 on
    Linux ARM64, $0.010 on Windows and $0.062 on macOS, so $12 buys 2,000 Linux
    minutes, 1,200 Windows minutes or about 190 macOS minutes.
  - Without a payment method on file, jobs are blocked once the $12 is used.
    With one, budgets limit spending, and a personal account's budget can stop
    usage at its limit. GitHub emails at 90% and 100% of the allowance.
  - ARM64 runners (`ubuntu-24.04-arm`, `windows-11-arm`) have worked in private
    repositories since 2026-01-29, with 2 processors instead of the 4 they get
    in public ones.
- **Schedule while private.** The design lead checks every merge on a Mac, so
  macOS can run weekly; nothing else checks Windows or Linux ARM64. Today's
  Linux job takes about 5.5 minutes ([measured](#evidence)). The other durations
  are estimates for the implementer to replace with measurements:

  | Job | Runner | When | Minutes | Cost per run |
  | --- | --- | --- | ---: | ---: |
  | Linux x86-64: formatting, Clippy, tests, build, core tests in release | `ubuntu-latest` | every push and pull request | about 7 | $0.04 |
  | Windows x86-64: tests, build, core tests in release | `windows-latest` | pushes to `main` that change more than documentation; manual | 10–15 | $0.10–0.15 |
  | Linux ARM64: core tests in dev and release | `ubuntu-24.04-arm` | as Windows | about 4 | $0.02 |
  | macOS ARM64: tests, build, core tests in release | `macos-latest` | weekly; manual | about 10 | $0.62 |
  | GPU tests on lavapipe and WARP, added by renderer N1b | `ubuntu-latest`, `windows-latest` | weekly; manual | about 5 and 10–14 | $0.03 and $0.10–0.14 |

  At 30 code pushes to `main` a month, that comes to about $1.30 for Linux,
  $3.00–4.50 for Windows, $0.60 for Linux ARM64, $2.70 for weekly macOS runs
  (about 4.3 a month) and $0.55–0.75 for the weekly GPU tests: $8.15–9.85 of
  the $12 (estimate). Pushes to `ci/` branches (below), about $0.80–1.00 each,
  and manual runs that also build release binaries, about $2 each, come on top
  (estimates). The upper end passes three quarters of the allowance, so the
  acceptance checks the measured cost. CI ran 9 times in October 2026.
- **How it runs.** A session can't run the jobs it writes: sessions don't push,
  and GitHub offers a manual run only for a workflow that is already on the
  default branch. So the stage takes rounds:
  1. The session writes the jobs and checks what it can on the Mac: the
     workflow passes a linter such as `actionlint`, if one is available, and
     the macOS job's commands pass, apart from the release golden step.
  2. The lead commits the result to a `ci/` branch and pushes it, which runs
     every job.
  3. A follow-up session gets the run logs, fixes what failed, tries the cache
     and records each job's minutes. Steps 2 and 3 repeat until every job
     passes.
  4. After merging, the lead ticks the boxes that need runs on `main`.
- **Scope:**
  - In `.github/workflows/`, add the three jobs. Cancel a branch's older runs
    when a newer push arrives (`concurrency`), skip the Windows, ARM64 and macOS
    jobs when a push changes only documentation, and allow manual runs
    (`workflow_dispatch`). Pushes to branches named `ci/*` run every job, so the
    lead can try a session's work in CI before merging it.
  - Each of the four jobs runs core's tests in the release profile, which runs
    the movement goldens there. Until stage 2 lands, that step fails on macOS at
    least, so let it fail where it does; the design lead removes the allowance
    when merging stage 2.
  - Add a dependency cache if it saves more minutes than it costs. Cache storage
    (10 GB per repository) is separate from compute.
  - Fix what fails on Windows or macOS when the fix is small. Otherwise skip the
    test on that system with a comment naming the gap, and record it here.
  - A manual run builds release binaries for each system and keeps them as
    artifacts for a few days, so testers need no Rust toolchain. GitHub Free
    includes 500 MB of artifact storage.
  - Update the CI paragraph of the
    [development guide](../development.md#build-and-check), and the README's
    Contributing paragraph and its known limit about testing.
- **Out of scope:** packaging, signing, and the GPU tests on software drivers,
  which renderer N1b adds on the schedule above once this stage has merged.
- **Parallel work:** owns `.github/workflows/`. Also edits the CI paragraph in
  the development guide's Build and check section, and the README's
  Contributing paragraph and testing limit; legacy-import I1 edits only the
  README's import line. Runs beside every other wave 1 order. List every fix
  made in shared code, so the lead can check it against the other orders.
- **Acceptance:**
  - [ ] On a `ci/` branch, every job passes, apart from the release golden step
    on macOS before stage 2 lands.
  - [ ] Each job's measured minutes and the projected cost per month, at the
    rate of pushes and manual runs so far, are recorded. If the projection
    passes three quarters of the allowance, Windows runs nightly instead, and
    only after a day with code changes.
  - [ ] The lead, after merging: a push to `main` that changes code runs the
    Linux, Windows and Linux ARM64 jobs, and they pass; a push that changes only
    documentation starts only the Linux job; a manual run starts every job and
    keeps the release binaries.
  - [ ] Not checkable in CI: the client opening a window and drawing on Windows
    and Linux. That needs a tester
    ([stage 3](#stage-3-check-the-minimum-specification)).
- **Record:** each job's minutes, the projected monthly cost, and what failed on
  each system and how it was fixed or skipped.

## Stage 2: identical movement in every build

State: merged on 2026-11-14. Exact in dev and release builds on macOS ARM64 and,
under Rosetta, x86-64. The CI runs on Linux and Windows wait for stage 1, which
is parked; until then the maintainer's Windows playtest below stands in, and
shared-sandbox 2d ships only once it has passed.

- **Why.** The server simulates every player, and each client predicts its own
  movement with the same code. On each snapshot the client takes the server's
  state for its player and replays its unacknowledged inputs (the
  `ServerMessage::Snapshot` branch in `crates/client/src/game.rs`), moving any
  difference into the camera offset. If client and server compute different
  bits, collision can grow a last-bit difference into a visible correction.
- **Today:**
  - `arena_golden` (`crates/core/tests/arena_golden.rs`) runs 19 scripted
    scenarios and compares every tick bit for bit with
    `golden/arena_trajectories.txt`, recorded on aarch64-macos in a dev build.
  - In a release build (the workspace's `[profile.release]`: opt-level 2, thin
    LTO), the `turning` scenario differs by one unit in the last place at tick
    9 ([evidence](#evidence)). The dev profile passes, and it is the only one CI
    runs.
  - The two scenarios marked `trig`, `turning` and `pitched_flight`, are exact
    only on the recording platform and are compared within `TRIG_TOLERANCE`
    (2e-3) everywhere else, so a release run on Linux CI would hide a difference
    like this one. The other 17 scenarios never turn the view, so their sines
    and cosines are exact, and they match bit for bit everywhere.
  - Nobody has looked for the cause. Likely candidates, not yet confirmed:
    - `forward` (`crates/core/src/lib.rs`) and the steering in
      `simulate_player_with` (`crates/core/src/physics.rs`) call `f32::sin` and
      `cos`, which come from each system's math library (Apple's, glibc's or
      the Windows C runtime's), whose last bits can differ.
    - An optimized build can merge a `sin` and a `cos` of the same angle into
      one combined call that rounds differently.
    - `EaseTable::new` (`crates/core/src/movement.rs`) derives the Sauerbraten
      profile's factors with `powf`. Collision reads triangles that
      `shape_triangles` (`crates/core/src/geometry.rs`) and `clip_plane`
      (`crates/core/src/fine.rs`) filter with `powi` thresholds and choose with
      `atan2`.
- **Decided:** movement is bit-identical in dev and release builds, on x86-64
  and ARM64, and on Windows, macOS and Linux, with no tolerance (maintainer,
  2026-11-13).
- **Scope:**
  - Find the cause of the release mismatch and record it.
  - Make everything the movement step depends on give the same bits in every
    build and on every system. Use one deterministic implementation of sine,
    cosine and any other math library function the simulation calls, for
    example the `libm` crate (a Rust port of musl's math library), instead of
    each system's. Check whether `powi` and `atan2` in the geometry code can
    differ between builds or systems, and if they can, make them deterministic
    too.
  - Guard against regressions, for example with a Clippy `disallowed-methods`
    list for `crates/core` that names the system math functions.
  - Delete `TRIG_TOLERANCE` and the recording-platform exception, so every
    scenario is exact on every system. If the new functions change Arena's
    recorded bits, record once more with `SAUERTEIG_RECORD_GOLDEN=1`, show that
    every tick stays within the old tolerance of the old recording, and say why
    in the stage notes.
  - Add a golden of the same form for the Sauerbraten profile, which will reach
    players through world settings.
  - [Octree](octree.md) O1a replaces collision while keeping Arena's goldens
    bit for bit. Whichever of the two merges second runs the goldens on top of
    the other's changes, under this stage's rule.
  - The rule binds every later movement golden as well, such as one over the
    [benchmark world](large-worlds.md#stage-1-a-generated-benchmark-world)
    decoded through shared-sandbox 2d's codec. The `compact_world` scenario in
    `arena_golden` covers two hand-made regions only.
  - State the rule in the
    [core contract](../../crates/core/CONTRACT.md#movement-profiles).
- **Out of scope:** the CI workflow files, which stage 1 owns.
- **Parallel work:** edits `crates/core` (`forward` in `lib.rs`, `physics.rs`,
  `movement.rs`, perhaps `geometry.rs`, `fine.rs` and `Cargo.toml`), its tests
  and Clippy settings, and the core contract. Runs beside stage 1, octree O1a,
  shared-sandbox 2b, renderer N1 and legacy-import I1. Octree O1a rewrites the
  collision half of `physics.rs` and most of `geometry.rs` and `fine.rs`; the
  second to merge rebases onto the first. Don't run it beside a world-format
  stage that edits movement.
- **Acceptance:**
  - [x] The cause is recorded (see the Record below).
  - [x] On the Mac, with every scenario exact, core's tests pass in dev and
    release builds, and for `x86_64-apple-darwin` under Rosetta. Linux
    containers were not run: Docker is installed but its daemon wasn't
    running, and the session didn't start it.
  - [x] The Sauerbraten golden passes the same way (same limits).
  - [ ] In CI, core's tests pass in dev and release builds on Linux x86-64,
    Linux ARM64, Windows x86-64 and macOS ARM64. This waits for stage 1, parked
    until releases are near.
  - [ ] **Maintainer:** a player on the maintainer's Windows computer joining
    a Mac host sees no snapping while strafing and turning, before
    shared-sandbox 2d ships.
- **Record:**
  - **Cause.** In an optimized build, LLVM merges the `sin` and `cos` of the
    same angle in `forward` and the steering of `simulate_player_with` into one
    call of Apple's combined `__sincosf_stret`, whose result can differ in the
    last bit from separate `sinf` and `cosf` calls, which the dev build makes.
    `nm` on the release test binary lists `___sincosf_stret`. That is why
    tick 9 of `turning` (the first tick with a non-trivial yaw) differs by one
    unit in the last place in release only. On other systems the same call can
    differ more, since each C library rounds its own way.
  - **Replaced.** `f32::sin` and `cos` in `forward` (`lib.rs`) and the strafe
    vector in `simulate_player_with` (`physics.rs`) became `libm::sinf` and
    `cosf`. `powf` in `EaseTable::new` (`movement.rs`) became `libm::pow`. The
    two `f64::atan2` calls (`shape_contains` in `geometry.rs`, the cap boundary
    turn in `fine.rs`) became `libm::atan2`. The `powi(4)` thresholds in
    `shape_triangles` and `clip_plane` became explicit multiplications, in
    the same order `powi` uses, so their value is unchanged; `powi` is
    otherwise a compiler routine that could differ by build. `sqrt`,
    `normalize`, `length`, `rem_euclid` and `clamp` are exact IEEE operations
    and stay. The client's own uses of the system functions (animation, audio,
    lighting) don't feed the simulation and stay.
  - **Guard.** `crates/core/clippy.toml` lists `disallowed-methods` for the
    transcendental, `powf`, `powi`, `hypot` and `cbrt` methods of `f32` and
    `f64`. Clippy applies it to the library, tests and examples, so the
    tests' own inputs and the movement oracle use `libm` as well. I checked
    that a violation is reported.
  - **Goldens.** `TRIG_TOLERANCE`, the `trig` markers and the recording
    platform line are gone; all 19 scenarios compare bit for bit everywhere.
    The same 19 scenarios now also run under `MovementProfile::sauerbraten()`
    against `golden/sauerbraten_trajectories.txt`, with the same per-tick
    form. Both files are rewritten by `SAUERTEIG_RECORD_GOLDEN=1`.
  - **Arena's bits changed,** in the two scenarios that turn the view
    (`turning`: 242 of its 1,200 values, `pitched_flight`: 8 of its 720). Everything else
    is unchanged, and every grounded flag is identical. Checked by comparing
    the new recording with the old one tick by tick: the largest difference in
    any value is 9.5e-7, against the old tolerance of 2e-3. The reason is that
    `libm` rounds sines and cosines differently from Apple's library in the
    last bit, and the difference then travels through the step.
  - **Results on the Mac (aarch64-apple-darwin, M5 Pro).** `cargo test
    --workspace --locked` and `cargo test -p sauerteig-core --release --locked`
    pass, as does `cargo test -p sauerteig-core --locked --target
    x86_64-apple-darwin` and the same with `--release` under Rosetta (both
    goldens exact). Fmt and `cargo clippy --workspace --all-targets --locked
    -- -D warnings` are clean.
  - **Waiting for CI:** Linux x86-64, Linux ARM64, Windows x86-64 and macOS
    ARM64 in both profiles, and the second-machine check. No Linux container
    ran. Under Rosetta only x86-64 macOS was reached.
  - **Choices.** The disallow list leaves `mul_add` out, because a fused
    multiply-add is exact. The Sauerbraten golden reuses the Arena scenarios
    unchanged, so its worlds are the same floors and ramps, and it doesn't
    record `Player::movement` (falling velocity shows in the position).

## GPU backends

The renderer's work must run on Vulkan and DirectX 12 as well as Metal
(maintainer, 2026-11-13). The [renderer](renderer.md) stages own the specific
checks:

- **N1** requests features and limits per adapter, offers 2× and 8×
  anti-aliasing only where the adapter supports them, and asks for Vulkan,
  DirectX 12 and Metal only. Today `Renderer::create` asks wgpu for every
  backend (`Backends::all()`), which includes OpenGL. F3 and the capture report
  then name the backend and driver, so each tester's report says which path
  ran.
- **GPU tests without the hardware.** N1b adds CI jobs that run the GPU
  tests on software drivers, Mesa's lavapipe for Vulkan on Linux and WARP for
  DirectX 12 on Windows, once stage 1 runs; both are parked until releases are
  near. They check correctness and API rules, not speed. Meanwhile the
  maintainer's Windows computer covers DirectX 12 in playtests.
- **N2** gives every light actor shadows without copying part of a depth
  texture. Direct3D 12 copies textures that can be depth targets only as whole
  subresources, except on GPUs with programmable sample positions, while wgpu 24
  checks only bounds and alignment and its DirectX 12 backend passes the region
  through (`copy_texture_to_texture` in wgpu-hal 24.0.4). So a partial copy that
  works on Metal and passes wgpu's checks says nothing about DirectX 12
  (documentation checked 2026-11-13).
- **Real GPUs.** Software drivers can't stand in for real Vulkan and DirectX 12
  GPUs or for the minimum specification's speed; those checks need testers
  ([stage 3](#stage-3-check-the-minimum-specification)).

## Stage 3: check the minimum specification

State: needs a tester with a reference machine. The project has none.

Other work orders mark the checks that need such hardware: renderer 2b's lowest
quality level, N1 and N2 on real Vulkan and DirectX 12 GPUs, shared-sandbox
2d's join near the default budget and its reading of a discrete GPU's video
memory, and the save and quit fixes on Windows and Linux. This stage gathers
them into one session per reference machine. A first pass can measure today's
build as a baseline. The main pass comes after the octree and shared-sandbox
2d, when big worlds join quickly, and renderer 2b, when quality levels exist.
Testers use the generated benchmark world
([large-worlds](large-worlds.md#stage-1-a-generated-benchmark-world) stage 1),
because the maintainer's library of imported maps can't be shared, and release
builds the maintainer makes, or stage 1's once it runs, so they need no Rust
toolchain. While the repository is private, a tester also needs access to it.

- **With every number,** record the machine, system, graphics driver, backend,
  resolution and build.
- **Joining:** time from Join to playing, peak memory of the client and of the
  whole system, whether the system swaps, and the client's own memory estimate,
  for a world at the default budget.
- **Hosting:** the same while hosting. Until shared-sandbox 2g, the host holds
  two copies of the world.
- **Frame time** at the lowest quality level and native resolution (1280×800
  on a Steam Deck): median and 95th percentile on the benchmark world's capture
  poses, and in play with two fighters and a builder editing.
- **Acceptance:**
  - [ ] On each reference machine, joining a world at the default budget
    succeeds without swapping, and the lowest quality level holds 60 frames a
    second (16.7 ms at the 95th percentile) with two fighters and a builder.
  - [ ] The results are recorded here, and each failure becomes work in the
    track that owns it.

## Later stages

- **Packaging without paid signing.** Not current work. SignPath Foundation
  needs the repository to be public and release builds from CI. Flathub needs
  public source, which it builds itself. The Microsoft Store needs neither.
  Checked 2026-11-13:
  - **Windows.** SignPath Foundation signs open-source projects free of charge.
    It requires an OSI-approved license without commercial dual-licensing for
    all components, no proprietary components, an existing release, binaries
    built automatically from the source repository, manual approval of every
    release, and multi-factor authentication for every team member. The
    certificate names SignPath Foundation as the publisher. Sauerteig's code is
    MIT, but the client embeds the CC0 materials and props, and CC0 isn't
    OSI-approved, so ask SignPath whether embedded art counts as a component
    before applying.
  - **Windows, through the Microsoft Store.** Registration has been free for
    individual developers since September 2025, after an identity check with a
    government ID, and Microsoft signs the MSIX packages it publishes.
  - **Linux, uncertain.** Flathub takes apps through a reviewed pull request,
    builds them from source without network access, and needs an `io.github.`
    app ID for a project hosted on GitHub. Its generative-AI policy requires
    disclosing AI-generated code, documentation and packaging with their
    approximate extent, and lets reviewers reject a submission for the extent
    or role of generated material. The manifest must contain nothing
    AI-generated or AI-assisted, and AI tools and agents must not open or
    automate the submission. Nearly all of Sauerteig is written by AI agents,
    so Flathub may refuse it.
  - **macOS.** No free route to notarization exists. A player opens each
    downloaded build once through System Settings, Privacy & Security, Open
    Anyway, which appears for about an hour after the blocked attempt. Homebrew
    5.0 (November 2025) deprecated `--no-quarantine` and set September 2026 for
    disabling casks that fail Gatekeeper checks, so Homebrew's own cask
    collection is no route.
    Whether macOS asks for Local Network access again after each update of an
    unsigned build is untested.
  - Without automatic updates, players' versions drift. Shared-sandbox 2d's
    version message tells them which release they need.

## Evidence

- **CI duration,** measured on GitHub's `ubuntu-latest` in the run of
  2026-10-29, without a cache: setup and Linux packages 20 s, formatting 2 s,
  Clippy 1 min 52 s, tests 3 min 12 s, build 1 s. The browser build, removed
  since, took another 5 min 14 s.
- **Release mismatch,** reproduced in two separate sessions on the M5 Pro on
  2026-11-12, with core unchanged from `main`: `arena_golden` fails in the
  release profile with "turning: tick 9 differs from recording" and passes in
  the dev profile.
