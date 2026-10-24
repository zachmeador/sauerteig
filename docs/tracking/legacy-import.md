# Legacy map restoration

State: partial.

## Outcome

Open an old map with its geometry, appearance, lighting, and supported gameplay
behavior intact, with a clear report for anything missing. A successful parse or
server load alone does not satisfy this outcome.

## Working now

The converter reads supported OCTA files, retains solid geometry at original scale,
and imports light/spawn positions. It produces editable Sauerteig JSON and an
import report, never overwriting the source. The local private archive has 19
loadable copies: 15 without solid-geometry omissions and four recovery copies that
each omit 23 zero-thickness sheets. Four other sources fail gzip checksum checks;
they need source recovery or another original, not a relaxed integrity check.

The archive copies use fallback material. Source texture/model references are not
loaded assets. Original light properties and spawn facing are not applied. These
copies are geometry recoveries, not restored maps.

## Remaining work

- [ ] Resolve texture slots and mapmodel indices from map and installation configs
  without executing arbitrary source scripts. Explain unresolved references.
- [ ] Load images and models, preserve UV settings and entity transforms, and
  retain per-asset license/attribution information for anything redistributed.
- [ ] Import original light properties once [lighting](lighting.md) supports them.
- [ ] Preserve spawn heading and use it on join/respawn; storing rotation alone
  is insufficient while the server reads only spawn positions.
- [ ] Represent legacy zero-thickness surfaces without losing visible faces.
- [ ] Adapt pickups, triggers, clip/liquid volumes, sounds, and sky/environment
  settings, explicitly reporting unsupported behavior.
- [ ] Offer an OGZ import UI with a preview of omissions and unresolved media
  before creating a saved copy.

## Acceptance and evidence

Compare representative imports with the original game for surfaces, model
placement, lighting, collision, spawn facing, and gameplay behavior. Check native
and browser, editing, save/reopen, and multiplayer. There is currently no exhaustive
comparison of this kind.

The 19 local copies passed isolated server startup/health and graceful save checks;
those checks had no players and establish loadability only. Existing tests cover
parser limits, transforms, shape conversion, and omission reporting. The
[importer docs](../../crates/importer/README.md) own format support and technical
limits; do not duplicate that specification here.
