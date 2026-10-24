# Sauerteig starter assets

Research checked 2026-10-24. No assets downloaded or imported; published specifications below are not local measurements.

Use a small, curated **Poly Haven** set: concrete, worn steel, rust, industrial lamps and utility equipment, with restrained textured vegetation outdoors. It fits the requested harsher industrial look and keeps licensing simple. Build the rooms, stairs, buttresses and large outdoor forms with the editor; use models for fixtures and recognizable details.

The proposed set is **38 logical assets: 16 materials + 16 industrial models + 6 nature models**. A material's component maps and a model's LODs are parts of that asset, not additional editor entries. This is a selection from the library, not a recommendation to ship whole source packs. The two tree entries are conditional on producing acceptable lightweight versions.

## License and source

[Poly Haven's asset license](https://polyhaven.com/license) explicitly covers its textures, models and HDRIs under **CC0 1.0**, allowing modification, commercial use and redistribution without required attribution. Each selection below links to its original asset page, which also identifies its authors. Keep an asset manifest with those URLs, author names, CC0 designation and modifications when importing. The project's MIT code license does not replace the assets' CC0 provenance. Site logos and example renders are separate website content; use actual asset downloads.

Poly Haven provides material maps as JPG/PNG/EXR, with 1K options and larger source resolutions. Model pages offer Blender, glTF, FBX and USD choices. Use Blender sources where rebaking or extracting kit parts is needed, then export static glTF/GLB for the Rust runtime. Preserve the source offline; do not make the shipping client interpret Blender files or Geometry Nodes.

## Sixteen materials

Start at **512 px per map in the default build**, downsampled from 1K sources; make 1K an upgrade for surfaces that visibly need it. Use base color, OpenGL normal and a packed AO/roughness/metalness map. These source pages supply the component maps; inspect metallic values when adapting older specular materials. Do not ship displacement or EXR maps in the starter runtime.

| Asset | Purpose | Published source resolution |
| --- | --- | --- |
| [Concrete Floor](https://polyhaven.com/a/concrete_floor) | Main worn interior floor | Up to 8K |
| [Rough Concrete](https://polyhaven.com/a/rough_concrete) | Plain walls and structural masses | Up to 8K |
| [Concrete](https://polyhaven.com/a/concrete) | Panelled bunker walls | Up to 8K |
| [Factory Wall](https://polyhaven.com/a/factory_wall) | Painted industrial wall panels | Up to 8K |
| [Metal Plate](https://polyhaven.com/a/metal_plate) | Diamond-plate walkways | Up to 8K |
| [Rusty Painted Metal](https://polyhaven.com/a/rusty_painted_metal) | Rusted red sheet-metal accent | Up to 16K |
| [Corrugated Iron](https://polyhaven.com/a/corrugated_iron) | Roofing and utility sheds | Up to 16K |
| [Rust Coarse 01](https://polyhaven.com/a/rust_coarse_01) | Corroded machinery and trim | Up to 16K |
| [Asphalt 03](https://polyhaven.com/a/asphalt_03) | Outdoor service yards | Up to 8K |
| [Gravel Embedded Concrete](https://polyhaven.com/a/gravel_embedded_concrete) | Yard edges and rough paving | Up to 8K |
| [Red Brick 03](https://polyhaven.com/a/red_brick_03) | Older industrial masonry | Up to 8K |
| [Rough Wood](https://polyhaven.com/a/rough_wood) | Boards and improvised barriers | Up to 8K |
| [Rock Face](https://polyhaven.com/a/rock_face) | Editor-built cliffs and cuttings | Up to 16K |
| [Forest Ground 01](https://polyhaven.com/a/forrest_ground_01) | Dry outdoor ground | Up to 8K |
| [Forest Leaves 02](https://polyhaven.com/a/forest_leaves_02) | Mossy, sheltered ground | Up to 8K |
| [Brown Mud Leaves 01](https://polyhaven.com/a/brown_mud_leaves_01) | Damp soil and drainage edges | Up to 8K |

Keep concrete and metal dominant. Use the green and red painted surfaces as accents, and align texture scale across editor geometry. Downsampling the maps should retain their photographic surface detail without making geometry look like bright toy blocks.

## Sixteen industrial models

Counts in the third column are the site's rounded **source asset totals**, not verified triangles per runtime instance or per LOD. Kit totals can cover multiple pieces. Source texture sets can be 4K/8K; select 1K for conversion and reduce small props to 512 px or shared atlases.

| Source and selected entries | Editor entries | Published triangles | Work before shipping |
| --- | ---: | ---: | --- |
| [Industrial Wall Lamp](https://polyhaven.com/a/industrial_wall_lamp) | 1 | 4K | Keep cage silhouette; bake tiny hardware; use emissive glass |
| [Hanging Industrial Lamp](https://polyhaven.com/a/hanging_industrial_lamp) | 1 | 10K | Simplify chain and cage; fixed hanging version |
| [Security Light](https://polyhaven.com/a/security_light) | 1 | 5K | Outdoor fixture; simplify bracket |
| [Utility Box 01](https://polyhaven.com/a/utility_box_01) | 1 | 4K | Static cabinet; simple box collision |
| [Barrel_01](https://polyhaven.com/a/Barrel_01) | 1 | 3K | Select one finish; cylinder collision |
| [Wooden Crate 01](https://polyhaven.com/a/wooden_crate_01) | 1 | 7K | Simplify panels and small fasteners |
| [Metal Jerrycan Green](https://polyhaven.com/a/metal_jerrycan_green) | 1 | 10K | Bake small dents and cap detail |
| [Exterior Aircon Unit](https://polyhaven.com/a/exterior_aircon_unit) | 1 | 19K | Bake grille/fan detail; select one finish |
| [Security Camera 01](https://polyhaven.com/a/security_camera_01) | 1 | 14K | Export static pose; remove unused rig |
| [Water Manhole Cover](https://polyhaven.com/a/water_manhole_cover) | 1 | 6K | Bake face relief onto a shallow mesh |
| [Propane Tank](https://polyhaven.com/a/propane_tank) | 1 | 5K | Simplify valve and base |
| [Modular Industrial Pipes 01](https://polyhaven.com/a/modular_industrial_pipes_01): straight, elbow, tee | 3 | 12K for source asset | Extract only three shapes; share material and grid-aligned pivots |
| [Modular Airduct Rectangular 01](https://polyhaven.com/a/modular_airduct_rectangular_01): straight, elbow | 2 | 45K for source asset | Extract two shapes; bake seams and bolts |
| **Total** | **16** | | |

The three lamps are the strongest first model imports: they give indoor and outdoor spaces an identity. An emissive fixture is separate from its light entity; use the editor's light system to control actual illumination.

## Six nature models

Choose one mesh variation from each source, rather than exposing every variation in the editor. This keeps the six-entry count real.

| Source | Published triangles | Intended runtime treatment |
| --- | ---: | --- |
| [Fern 02](https://polyhaven.com/a/fern_02) | 6K for source asset | One clump, alpha-masked leaves, reduced distant mesh |
| [Shrub Sorrel 01](https://polyhaven.com/a/shrub_sorrel_01) | 3K for source asset | One low groundcover clump, muted foliage, sparse placement |
| [Rock 07](https://polyhaven.com/a/rock_07) | 28K | Use a reduced LOD; source page is tagged LODs |
| [Boulder 01](https://polyhaven.com/a/boulder_01) | 124K | Use a reduced LOD; bake detail; source page is tagged LODs |
| [Pine Tree 01](https://polyhaven.com/a/pine_tree_01) | 17M for source asset | **Conditional:** one low-detail tree with branch/needle cards |
| [Island Tree 01](https://polyhaven.com/a/island_tree_01) | 4M for source asset | **Conditional:** one broadleaf tree with baked foliage clusters |

**Trees are the unresolved preparation task.** Both pages are tagged LODs and Geometry Nodes, but the per-LOD counts and resulting appearance were not verified. The pages' selected download configurations displayed 1.31 GB for Pine Tree 01 and 338.5 MB for Island Tree 01; these are source download figures, not estimates of a cooked tree. Do not import their full geometry into the runtime. First inspect the provided LODs, choose one variation and bake/rebuild the canopy into textured branch cards. Simplifying a whole tree blindly can ruin the foliage.

The intended quality is recognizable textured bark, irregular branching and leafy silhouettes above the minimum PS2 reference. This is an art and performance target, not a verified result. If the included LODs cannot reach that quality within budget, retain these two CC0 sources as baking/reference material and author two simpler trunks and canopies. The starter stays at 38 entries either way; it is 36 ready candidates plus two tree candidates until this is proven.

## Runtime targets, not measurements

- Default materials: 512 px; selective 1K upgrades. Props: mostly 512 px, trees up to 1K for bark and foliage atlases. Generate mipmaps and preserve leaf coverage in distant mips.
- Common props: roughly 300–2,000 triangles in the nearest shipping mesh, with exceptions up to 5,000 where silhouette needs it. Pipe/duct pieces should usually be below 1,000 each. Add one simpler distant LOD where repeated use warrants it.
- Nature: rocks roughly 300–2,000 triangles; groundcover roughly 100–800; trees roughly 5,000–12,000 nearby, 1,000–3,000 at middle distance and a baked distant representation. Limit foliage overdraw and use alpha masking, not piles of blended transparent layers.
- Aim for a compressed first-map asset download of **20–35 MiB**, with the entire starter collection **under 70 MiB** and later assets fetched on demand. These numbers need an actual conversion pass to validate.
- Aim for **128 MiB or less of resident textures** in the first map. Three uncompressed RGBA8 512 px maps with mipmaps cost about 4 MiB per material, so the sixteen base materials alone cost about 64 MiB. At 1K they would cost about 256 MiB. Download compression does not establish GPU memory use; GPU block compression must have a supported fallback.
- Use instancing for repeated props/vegetation, distance culling and simple collision proxies. Verify a representative indoor/outdoor map on an integrated-GPU laptop in the browser before expanding the collection.

Keep conversion simple: fixed glTF/GLB meshes, material maps, LODs and a source manifest. Shared atlases are useful for the pipe/duct families and small fixtures; a universal atlas or new procedural asset framework is unnecessary for this starter. Preserve silhouettes and visible surface detail before chasing the smallest triangle count.

## Other sources checked

[a52's PBR Industrial Asset Pack](https://opengameart.org/content/pbr-industrial-asset-pack) is a useful **CC0 alternative** for tanks, stairs and control panels. Its author-posted page supplies Blender 4.0 archives (58 MB and 59.1 MB) and explicitly says the baked models lack metallic/roughness maps. It is a reserve source if one of those shapes is needed; do not add the whole pack. Per-model polygon counts and LODs were not published on that page or measured here.

[EZ-Tree](https://github.com/dgreenheck/ez-tree) can export GLB and its [code license is MIT](https://github.com/dgreenheck/ez-tree/blob/main/LICENSE). It could generate lightweight trunks offline, but its bundled foliage is not cleared by that code license alone: [upstream issue #30](https://github.com/dgreenheck/ez-tree/issues/30) remains open about leaf texture provenance. Use explicitly sourced CC0 replacement foliage if experimenting with the tool. It is not a dependency of this starter recommendation.

Kenney/KayKit-style gradient nature kits miss the requested visual detail. Full Poly Haven scans miss the runtime budget until converted. Marketplace packs that only permit redistribution inside a finished game are awkward for an open editor with reusable downloadable assets. No NC, ND or SA asset is part of the recommendation.

Poly Haven's [model standards](https://docs.polyhaven.com/en/technical-standards/models) explain why preparation is needed: assets favor detail, LODs are optional, and source quality targets serve more than real-time games. Library availability and page specifications establish candidates, not in-engine performance.
