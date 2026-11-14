# Legacy map restoration

State: partial.

## Outcome

Open an old map with its geometry, appearance, lighting, and supported gameplay
behavior intact, with a clear report for anything missing. A successful parse or
server load alone does not satisfy this outcome.

## Working now

The converter reads OCTA versions 29–33, retains solid geometry and water/glass
assignments at original scale, and imports light/spawn positions. It produces
editable Sauerteig JSON and an import report, never overwriting the source. The
maintainer's private archive has 23 maps, and the library holds 22 of them: 18
without solid-geometry omissions and four recovery copies that each omit 23
zero-thickness sheets. Four maps, nub1, ralith, ralith1 and ralith2, are OCTA
version 27, which the importer doesn't read, and they fail its gzip checksum
check. Since 2026-11-11 the three ralith maps are in the library through a
hand-made shim ([I1](#i1-the-importer-reads-octa-version-27) below). nub1, the
one archive map not in the library, also exceeds the 400,000-region cap.

The archive copies use fallback material. Source texture/model references are not
loaded assets. Original light properties and spawn facing are not applied. These
copies remain partial recoveries, not restored maps. Water/glass now render and
have basic swimming/collision behavior. Original liquid styles and unsupported
material flags remain explicitly reported rather than applied.

## Remaining work

Original surfaces depend on the [world format](world-format.md)'s material library
and per-face texture alignment; original lights depend on [lighting](lighting.md).
For private restoration, textures and models may be read from the user's own
Sauerbraten installation without redistributing them.

- [ ] Resolve texture slots and mapmodel indices from map and installation configs
  without executing arbitrary source scripts. Explain unresolved references.
- [ ] Load images and models, preserve UV settings and entity transforms, and
  retain per-asset license/attribution information for anything redistributed.
- [ ] Import original light properties once [lighting](lighting.md) supports them.
- [ ] Preserve spawn heading and use it on join/respawn; storing rotation alone
  is insufficient while the server reads only spawn positions.
- [ ] Represent legacy zero-thickness surfaces without losing visible faces.
- [ ] Adapt pickups, triggers, other clip/liquid volumes, sounds, and sky/environment
  settings, explicitly reporting unsupported behavior.
- [ ] Offer an OGZ import UI with a preview of omissions and unresolved media
  before creating a saved copy.
- [ ] Fix mirrored imports: the 2026-11-11 bug audit found imported maps
  mirrored. [Octree](octree.md) O3 fixes it while it rewrites the importer, and
  the library is then re-imported from the originals.

## I1: the importer reads OCTA version 27

State: ready. Wave 1, small.

- **Why.** The importer reads versions 29–33, so the library's ralith copies
  came through a shim (`target/old-worlds/scripts/prepare.py`) that lives in the
  gitignored `target/` and goes with `cargo clean`. The shim:
  - rewrote the 196-byte version 27 header as the 36-byte version 29 header,
    dropping the light precision, ambient light, water and lava colours and the
    map title;
  - recompressed the data, which wrote a fresh gzip checksum;
  - removed one entity from each of ralith and ralith1: an orphan teleport
    destination at z = −134,217,728 source units, about 16,800 km below the map.
    With the header and checksum handled, the importer would still refuse it
    ("entity position exceeds supported import coordinates").
- **Checked on 2026-11-13:**
  - Sauerbraten's `worldio.cpp` reads versions up to 28 through a compatibility
    header. Its lighting fields and title become map variables, the file has no
    map variables or vslots, and a blend map loads only from version 28 on.
    What the importer reads after the header is the same as in version 29.
  - On copies of the four version 27 maps, the gzip check fails for the same
    reason: the stored CRC-32 covers only the inflated data after byte 205.
    Their stored lengths match and their deflate streams end cleanly. The
    design lead confirmed this on the originals, read only; a version 29 map
    from the same archive, cube4, has an ordinary checksum over all its data.
- **Decided:**
  - Read version 27 directly: parse the 196-byte header, keep its lighting
    fields and title in the import report, and read the rest as version 29 with
    no map variables and no vslots (maintainer, 2026-11-13).
  - Version 28 shares the header but can carry a blend map. It stays refused
    until a version 28 map is available to test.
- **Decided** (design lead, 2026-11-13):
  - Accept exactly this checksum defect: a stored CRC-32 that matches the data
    after byte 205, with the right length and a complete stream, is accepted and
    noted in the report. Any other mismatch still fails. This narrows the
    earlier rule that a failed checksum needs source recovery rather than a
    relaxed check: the data is still checked, against the checksum its writer
    produced.
  - An entity whose position is not finite or lies beyond the importer's
    coordinate limit (1,048,576 m, the check in `convert`) is left out and
    listed in the report with its index, kind and position, instead of failing
    the conversion. Other entities keep extending the world's bounds as today.
- **Scope:** `read_ogz`, `read_octa` and `Header` in
  `crates/importer/src/lib.rs`; the entity rule in `convert.rs`; `ImportReport`;
  and tests with synthetic version 27 data beside the existing fixtures,
  covering the header, both checksum cases and a far entity. Update the
  supported versions and the new rules in the
  [importer docs](../../crates/importer/README.md), the help text in the
  importer's `main.rs`, and the import line of the top-level
  [README](../../README.md#what-works-today).
- **Out of scope:** nub1's region count, which [octree](octree.md) O1a removes,
  and anything in `target/old-worlds/`.
- **Parallel work:** the importer, plus one line of the top-level README. Runs
  beside every other wave 1 order. [Large-worlds](large-worlds.md) stage 1 adds
  a command to the importer, editing `cli.rs` and `main.rs`; keep to separate
  parts of `main.rs` and separate sections of the importer's README, and the
  second to merge rebases onto the first. [Platforms](platforms.md) stage 1
  edits other parts of the top-level README. Octree O1a edits `convert.rs`
  only where the importer writes its region list, and octree O3 later rewrites
  the reader and conversion; if I1 hasn't merged by then, O3 absorbs it.
- **Acceptance,** on the maintainer's Mac, which holds the private archive:
  - [ ] Copy the three ralith originals (the files `prepare.py` reads) into the
    worktree and convert them with the stock importer. The world files are
    byte-identical to the shim's conversions in `target/old-worlds/fresh/`
    (SHA-256 in `target/old-worlds/published.json`). If the importer's output
    has changed since 2026-11-11, they are equal in bounds, regions, volumes and
    objects instead, and the stage notes say what changed.
  - [ ] They also match the library's copies in bounds, regions, volumes and
    objects. A server may have rewritten a library copy's layout and object
    counter, so compare content, not bytes.
  - [ ] The reports show the header's title and lighting fields, the checksum
    note and, for ralith and ralith1, the omitted entity.
  - [ ] A copy of nub1 gets past the header and checksum. Before octree O1a it
    stops only at the region cap ("Too many authored regions."); after O1a it
    converts.
  - [ ] A version 27 file with any other checksum mismatch is refused, as
    before.
  - [ ] In the app, import a converted ralith2 into a scratch library
    (`SAUERTEIG_DATA_DIR`), host it and fly around: it looks like the library's
    copy.
  - [ ] Nothing in `.local/`, the application-data folder, `target/old-worlds/`
    or the archive changes.
- **Record:** the digests, how the reports differ from the shim's, and nub1's
  result.

## Acceptance and evidence

Compare representative imports with the original game for surfaces, model
placement, lighting, collision, spawn facing, and gameplay behavior. Check
editing, save/reopen, and multiplayer. There is currently no exhaustive
comparison of this kind.

On 2026-10-24, the 19 copies then in the library passed two isolated server
startup/health and graceful save cycles each after adding volume materials;
terrain, objects, bounds, and material assignments stayed identical. Seventeen
maps contain water/glass, with at most 4,227 compact boxes. A regenerated cube4
copy also opened in the native renderer.
These are not visual comparisons against the source engine. The server checks
had no players and establish loadability only. Existing tests cover
parser limits, transforms, shape conversion, and omission reporting. The
[importer docs](../../crates/importer/README.md) own format support and technical
limits; do not duplicate that specification here.
