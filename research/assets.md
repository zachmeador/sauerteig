# Art direction and asset candidates

Concrete, worn steel, rust, industrial lighting, and restrained outdoor
vegetation. Build architecture and large landforms with cubes; use models for
fixtures, equipment, rocks, and foliage. Keep photographic surface detail and
recognizable silhouettes without bright toy-like colors or expensive scan meshes.

The initial collection should stay below 50 editor entries. This shortlist has
38: 16 materials, 16 industrial models, and six nature models. The included
subset and its provenance are defined by the [material manifest](../assets/materials/manifest.json)
and [prop manifest](../assets/props/manifest.json).

## Materials

Default to 512 px textures, with selective 1K upgrades where visible detail
needs them. Keep concrete and metal dominant; use painted colors as accents.
Align world-space texture scales across adjacent surfaces.

| Candidate | Use |
| --- | --- |
| [Concrete Floor](https://polyhaven.com/a/concrete_floor) | Main worn interior floor |
| [Rough Concrete](https://polyhaven.com/a/rough_concrete) | Plain walls and structural masses |
| [Concrete](https://polyhaven.com/a/concrete) | Panelled bunker walls |
| [Factory Wall](https://polyhaven.com/a/factory_wall) | Painted industrial wall panels |
| [Metal Plate](https://polyhaven.com/a/metal_plate) | Diamond-plate walkways |
| [Rusty Painted Metal](https://polyhaven.com/a/rusty_painted_metal) | Rusted red sheet-metal accent |
| [Corrugated Iron](https://polyhaven.com/a/corrugated_iron) | Roofing and utility sheds |
| [Rust Coarse 01](https://polyhaven.com/a/rust_coarse_01) | Corroded machinery and trim |
| [Asphalt 03](https://polyhaven.com/a/asphalt_03) | Outdoor service yards |
| [Gravel Embedded Concrete](https://polyhaven.com/a/gravel_embedded_concrete) | Yard edges and rough paving |
| [Red Brick 03](https://polyhaven.com/a/red_brick_03) | Older industrial masonry |
| [Rough Wood](https://polyhaven.com/a/rough_wood) | Boards and improvised barriers |
| [Rock Face](https://polyhaven.com/a/rock_face) | Editor-built cliffs and cuttings |
| [Forest Ground 01](https://polyhaven.com/a/forrest_ground_01) | Dry outdoor ground |
| [Forest Leaves 02](https://polyhaven.com/a/forest_leaves_02) | Mossy, sheltered ground |
| [Brown Mud Leaves 01](https://polyhaven.com/a/brown_mud_leaves_01) | Damp soil and drainage edges |

## Industrial models

Extract useful pieces from source kits and share their materials. Preserve the
silhouette; bake small bolts, grille detail, and dents into textures. Lamps use
emissive surfaces alongside the game's dynamic lights.

| Candidate | Preparation |
| --- | --- |
| [Industrial Wall Lamp](https://polyhaven.com/a/industrial_wall_lamp) | Keep cage silhouette; bake tiny hardware; use emissive glass |
| [Hanging Industrial Lamp](https://polyhaven.com/a/hanging_industrial_lamp) | Simplify chain and cage; fixed hanging version |
| [Security Light](https://polyhaven.com/a/security_light) | Outdoor fixture; simplify bracket |
| [Utility Box 01](https://polyhaven.com/a/utility_box_01) | Static cabinet; simple box collision |
| [Barrel_01](https://polyhaven.com/a/Barrel_01) | Select one finish; cylinder collision |
| [Wooden Crate 01](https://polyhaven.com/a/wooden_crate_01) | Simplify panels and small fasteners |
| [Metal Jerrycan Green](https://polyhaven.com/a/metal_jerrycan_green) | Bake small dents and cap detail |
| [Exterior Aircon Unit](https://polyhaven.com/a/exterior_aircon_unit) | Bake grille/fan detail; select one finish |
| [Security Camera 01](https://polyhaven.com/a/security_camera_01) | Export static pose; remove unused rig |
| [Water Manhole Cover](https://polyhaven.com/a/water_manhole_cover) | Bake face relief onto a shallow mesh |
| [Propane Tank](https://polyhaven.com/a/propane_tank) | Simplify valve and base |
| [Modular Industrial Pipes 01](https://polyhaven.com/a/modular_industrial_pipes_01): straight, elbow, tee | Extract only three shapes; share material and grid-aligned pivots |
| [Modular Airduct Rectangular 01](https://polyhaven.com/a/modular_airduct_rectangular_01): straight, elbow | Extract two shapes; bake seams and bolts |

## Nature

Choose one variation from each source. Trees need textured bark, irregular
branching, and convincing foliage silhouettes. Inspect source LODs, then bake or
rebuild foliage into branch cards if necessary. Keep expensive source geometry
offline. Tree conversion is still needed before these candidates can be included.

| Candidate | Preparation |
| --- | --- |
| [Fern 02](https://polyhaven.com/a/fern_02) | One clump, alpha-masked leaves, reduced distant mesh |
| [Shrub Sorrel 01](https://polyhaven.com/a/shrub_sorrel_01) | One low groundcover clump, muted foliage, sparse placement |
| [Rock 07](https://polyhaven.com/a/rock_07) | Use a reduced LOD; source page is tagged LODs |
| [Boulder 01](https://polyhaven.com/a/boulder_01) | Use a reduced LOD; bake detail; source page is tagged LODs |
| [Pine Tree 01](https://polyhaven.com/a/pine_tree_01) | **Conditional:** one low-detail tree with branch/needle cards |
| [Island Tree 01](https://polyhaven.com/a/island_tree_01) | **Conditional:** one broadleaf tree with baked foliage clusters |

## Working budgets

These are asset preparation targets to test on an integrated-GPU laptop:

- Common props: roughly 300–2,000 nearby triangles, up to 5,000 where silhouette
  needs it. Pipe and duct pieces should usually stay below 1,000.
- Rocks: 300–2,000 triangles; groundcover: 100–800. Trees: 5,000–12,000 nearby,
  1,000–3,000 at middle distance, and a baked distant representation.
- Use instancing, distance culling, and simpler distant meshes. Foliage should
  use alpha masking with coverage-preserving mipmaps; watch overdraw.
- Aim for 20–35 MiB of compressed assets for a first map and under 70 MiB for
  the initial collection. Keep resident textures below 128 MiB in that map.
- Prepare simple collision proxies for props that obstruct players.

Use static meshes, material textures, LODs, and a source manifest. Shared atlases
can help related small props. Preserve visible shape and detail before reducing
triangle counts further. Check frame times in a mixed indoor/outdoor map before
expanding the collection.

## Sources and licenses

[Poly Haven assets are CC0](https://polyhaven.com/license). Preserve asset URLs,
authors, source checksums, and conversion changes in the manifests. Website logos
and preview renders are separate content. The [model standards](https://docs.polyhaven.com/en/technical-standards/models)
explain the source library's focus on detail and optional LODs.

[a52's PBR Industrial Asset Pack](https://opengameart.org/content/pbr-industrial-asset-pack)
is a CC0 reserve source for tanks, stairs, and control panels. Its baked models
need metallic/roughness preparation.

[EZ-Tree](https://github.com/dgreenheck/ez-tree) is an optional offline trunk
generator with MIT code. Its bundled foliage needs a separate provenance check;
see [the upstream texture-license discussion](https://github.com/dgreenheck/ez-tree/issues/30).
Use explicitly sourced CC0 foliage if experimenting with it.

Keep code under MIT and assets under their own suitable licenses. Favor assets
that can be redistributed and edited with maps and blueprints. Do not import
entire packs merely because they are available.
