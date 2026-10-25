# Large worlds and performance

State: partial.

## Outcome

Explore and edit large original-scale maps comfortably in native and browser
clients on recent ordinary laptops, including integrated graphics, with multiple
players building and fighting together.

## Working now

Compact authored leaves, a spatial query index, local edit overrides, dynamic bounds,
and camera/shadow culling avoid expanding large cubes into metre cells. Storage
accepts worlds up to 8,192 metres per axis within its coordinate envelope.
Original-scale archive conversions and server loads have been exercised.

That storage support is not complete large-map usability. The camera far plane
is currently 180 metres and editor flight is fixed at 16 metres/second. Large maps
also lack a convenient way to locate interesting geometry or navigate between
distant areas. Existing profiles measure CPU work, not completed GPU frames.

## Remaining work and acceptance

This work builds on [shared-sandbox](shared-sandbox.md) stage 2, which streams
world transfer and builds the client world incrementally, and on the
[renderer](renderer.md) stages for view distance and detail levels.

- [ ] Make viewing distance and editor traversal suitable for large maps, with
  deliberate performance limits rather than simply increasing every bound.
- [ ] Exercise rendering, collision, local edits, blueprints, and save/rejoin far
  from the origin on actual large maps in both clients.
- [ ] Measure frame times, memory, startup, and edit/remesh spikes on a named
  integrated-graphics laptop, recording map, resolution, client, and player count.
- [ ] Run two fighters plus a builder editing geometry during combat, and use
  the measurements to set a practical performance budget.
- [ ] Address measured bottlenecks and rerun the same scenarios before claiming
  the ordinary-laptop target is met.

## Evidence

Geometry regression tests and CPU profiling tools exist; previous manual checks
covered a large imported map and native/browser shared sessions. They do not prove
sustained performance across this archive or the target hardware range.
Use the [performance checks](../development.md#performance-checks) for commands;
keep only the latest useful benchmark findings here when measurements are available.
