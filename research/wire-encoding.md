# Wire encoding and streamed world transfer

Design reference for [shared-sandbox](../docs/tracking/shared-sandbox.md) stage 2:
a compact encoding, compression, and a world transfer in chunks. A research agent
compiled this on 2026-10-25 with measurements on the library's real worlds. The
prototype codecs it verified are in [`wire-encoding/`](wire-encoding/): `wire.rs`
holds mirror structs for the world types, and `packed.rs` holds the bulk-table
format. They are prototypes: their decoders index freely and must be rewritten to
return errors before use.

## Bottom line

Stream the world as about 150 independently decodable zstd chunks in WebSocket
binary frames.
- **Bulk tables** (regions, cells, roots, volumes) use a small hand-packed delta
  format.
- **Every other message** uses postcard.
- **JSON stays** for save import and export, tests, and as a debugging transport.

**Results:**
- The largest library world drops from 44.87 MB of JSON (3.18 MB gzipped) to
  165 KB.
- All 20 library worlds shrink from 524 MB of JSON to 1.6 MB chunked.
- At 20 Mbps the transfer stops mattering.

**Client-side cost is what remains:** building the world takes about 1 s of CPU in
wasm. So incremental construction is the real stage-2 work.

## Recommendation

1. **Codec boundary.**
   - A `wire` module of mirror structs, without `skip_serializing_if`, defaults,
     or internal tags. It has `From` conversions both ways and round-trip tests.
   - Game types keep their JSON serde.
   - `Hello` stays JSON and gains a protocol number, so every client generation
     can read a version-mismatch error.
2. **Encoding.**
   - **Postcard for messages:** input, snapshots, patch envelopes, the manifest,
     objects, and players. It is a stable format since 1.0 and adds only 21 KB of
     wasm.
     - A 16-player snapshot is 0.83 KB instead of 3.7 KB of JSON. That cuts a
       host's upload to 16 clients from 9.5 to 2.1 Mbit/s.
     - An input message is 27 B instead of 153 B.
   - **Hand-packed deltas for bulk tables:**
     - Regions are sorted along a Morton curve.
     - Positions are stored as gaps in curve space (0 for face-to-face octree
       leaves), sizes as log2, and cells as palette indices, all as varints.
     - Compressed, this costs 0.06–0.31 bytes per region across the library.
       On the largest world, bitcode needs 2.9 and postcard 3.9.
3. **Compression.** zstd per chunk.
   - **Server:** C `zstd`.
     - Level 9 for the immutable base layer, built once per world (78 ms for the
       largest) and cached.
     - Level 3 for patches.
     - Nothing under about 512 B.
   - **Native client:** C zstd.
   - **Wasm client:** the pure-Rust `ruzstd` decoder, which adds 137 KB. Never
     encode with ruzstd: only level 1 works in 0.9.1.
   - **Fallback:** raw deflate through `miniz_oxide` (+27 KB), equal to zstd-9 on
     these chunks.
4. **Versioning and safety.**
   - **Frames** are `[kind u8][payload]`, and each chunk header carries its class
     and version.
   - **Old decoders stay**, with golden fixtures.
   - **Saves** become the same chunks behind a magic number, and JSON stays
     importable.
   - **Metadata that evolves**, such as world settings, goes in small JSON
     sections. Bulk tables stay positional.
   - **Decoders return errors and never panic.** They cap counts, palette
     indices, window size, and declared raw length before decompressing.

## Measurements

**Encodings on the largest world** (306,094 regions). Medians on an Apple M3 Pro;
wasm timings under Node's V8.

| Encoding | Raw MB | + zstd-3 MB | Native encode / decode ms | Wasm decode ms |
| --- | ---: | ---: | --- | --- |
| JSON welcome (today) | 44.87 | 2.86 | 47 / 247 | 332 (cold 694) |
| postcard | 9.24 | 1.91 | 10.4 / 9.3 | 11.3 |
| bitcode | 7.57 | 1.69 | 4.0 / 2.4 | 3.5 |
| rkyv | 24.61 | 1.87 | 6.6 / 2.8 | 3.6 |
| **Packed tables** | 1.03 | 0.144 | 34 / 2.7 | 2.8 (5.8 with ruzstd) |

**Join of the largest world:**

| Approach | Bytes | Client work |
| --- | ---: | --- |
| JSON as today | 44.87 MB, an estimated 17.9 s at 20 Mbps | parse 0.33–0.69 s, build 0.97–1.68 s, mesh 0.1 s, all in one task |
| Packed chunks + zstd-9 | 165 KB | decode 5–8 ms; a time-sliced build is playable in 63–191 ms and complete in 1.25–3.7 s |

**Notes:**
- **Native build** of the largest world takes 1.0–1.2 s on one thread and an
  estimated 0.18 s across 12 threads.
- **Memory** after building in wasm is 455 MB.
- **Per-region build cost** has a mean of 2.4 µs and a maximum of 138 µs. So slice
  the build by clock time, not by region count: fixed 512-region slices overran a
  4 ms budget by up to 28 ms.

## Chunked transfer design

- **Split.**
  - **The manifest comes first:** world identity, revision R, bounds, grid origin,
    spawn, totals, and players.
  - **Then self-contained chunks:** each has its own palette and absolute Morton
    start, holds at most 2,048 entries and 128 KiB raw, and compresses to about
    1 KB typically.
  - **Large regions** (over 64 m) go first.
  - **Base regions** follow in Morton runs. They are immutable, so they are
    encoded once and cached.
  - **Overrides** go in 64 m tiles, cached per tile and revision. These are coarse
    cells, empty masks, and refined roots.
  - **Volumes and objects** get one chunk each.
- **Order.**
  - Large regions, then volumes and objects.
  - Then base chunks by distance from the joiner's spawn.
  - Override tiles go after the base chunks they intersect.
  - Then `SnapshotEnd { R }`.
- **Progress.** Show built regions against the total, since building dominates.
  Keep chunks under 32 KiB so snapshot frames can interleave.
- **Edits during a transfer.**
  - The server streams from a stage-1 snapshot at R and queues later patches for
    the joiner, bounded by bytes. On overflow, it resends dirty tiles after
    `SnapshotEnd`.
  - The client holds arriving patches until its build completes, applies them in
    order, and rejoins if there is a gap.
  - Binary patches also stop a maximum paste from costing about 8.3 MB of JSON.
- **Incremental build.**
  - **What core needs:** a public, `Send` region-compile step; insertion without
    rebuilding one global query index (an index and batch key per chunk); and a
    finish step.
  - **In wasm:** a per-frame time budget, building the nearest regions first,
    then meshing, then uploading. Wasm threads need atomics and cross-origin
    isolation, so time-slice instead.
  - **On native:** add worker threads for decoding and compiling.

## Survey verdicts

- **bincode 2** is unmaintained (RUSTSEC-2025-0141).
- **bitcode** is the best generic option and the fallback for bulk tables, but
  format stability is not one of its goals.
- **rkyv's** layout follows Rust types and semver, and its zero-copy reads buy
  nothing when the data is decompressed and rebuilt anyway.
- **Self-describing formats** (MessagePack with field names, CBOR) are about 4×
  larger than postcard and slower to decode.
- **Browsers decode raw deflate natively**, but not brotli in Chrome, and zstd is
  unshipped.
- **tungstenite has no per-message deflate**, so compression must happen in the
  application.
- **C zstd can't build for wasm** with Apple's clang, hence ruzstd.

## Measured versus estimated

**Measured:** every size, the native and V8 timings, the frame-loop simulation,
and wasm module sizes.

**Estimated:**
- transfer times;
- GPU buffer creation;
- slower laptops (expect 2–4×);
- the incremental core API.

**Caveats:**
- Every library world is an import (revision 0), so edit-layer figures use
  synthetic edits.
- Real browser frames in Chrome, Safari, and Firefox were not measured.
