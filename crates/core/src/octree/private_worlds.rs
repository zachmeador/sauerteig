//! Checks on copies of private worlds in the repository's ignored `.local/`:
//! `the_cube1m.json` and `library/*.json`. They are ignored by default, since
//! CI has no such files; run them explicitly in a release build:
//! `cargo test --release -p sauerteig-core --lib private_worlds -- --ignored --nocapture`
//!
//! Session 1 of octree O1a recorded the answers of the code from before the
//! tree in `.local/query-recordings/`; `private_query_recordings_replay`
//! replays them against `World`, which now runs on the tree.

use super::tests::{old_loader_accepts, world_of};
use super::*;
use crate::physics::{Body, Space};
use crate::{CellEdit, CellFragment, MaterialVolume, RootEdit, Vec3};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn local() -> PathBuf {
    std::env::var_os("SAUERTEIG_PRIVATE_WORLDS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local"))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Saved {
    Legacy(Vec<CellEdit>),
    Parts {
        #[serde(default)]
        bounds: WorldBounds,
        #[serde(default)]
        regions: Vec<CellFragment>,
        #[serde(default)]
        cells: Vec<CellEdit>,
        #[serde(default)]
        roots: Vec<RootEdit>,
        #[serde(default)]
        volumes: Vec<MaterialVolume>,
    },
}

fn load(path: &Path) -> Format3 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    match serde_json::from_slice(&bytes).unwrap() {
        Saved::Legacy(cells) => Format3 {
            cells,
            ..Default::default()
        },
        Saved::Parts {
            bounds,
            regions,
            cells,
            roots,
            volumes,
        } => Format3 {
            bounds,
            regions,
            cells,
            roots,
            volumes,
        },
    }
}

fn worlds() -> Vec<(String, PathBuf)> {
    let mut out = vec![("the_cube1m".to_string(), local().join("the_cube1m.json"))];
    if let Ok(entries) = std::fs::read_dir(local().join("library")) {
        let mut library: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        library.sort();
        for path in library {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            out.push((name, path));
        }
    }
    out.retain(|(_, p)| p.exists());
    out
}

fn json_bytes(value: &impl serde::Serialize) -> usize {
    serde_json::to_vec(value).unwrap().len()
}

/// The server's counted size of a world: pieces against the 786,432-piece
/// guard and bytes against the 63 MiB geometry limit, each cell counted by
/// its own JSON.
fn counted(f: &Format3) -> (usize, usize) {
    let pieces = f.regions.len()
        + f.cells.len()
        + f.roots
            .iter()
            .map(|r| r.fragments.len().max(1))
            .sum::<usize>();
    let bytes = f.regions.iter().map(|r| json_bytes(r) + 1).sum::<usize>()
        + f.cells.iter().map(|c| json_bytes(c) + 1).sum::<usize>()
        + f.roots.iter().map(|r| json_bytes(r) + 1).sum::<usize>()
        + json_bytes(&f.volumes);
    (pieces, bytes)
}

const MAX_WORLD_FRAGMENTS: usize = 786_432;
const MAX_GEOMETRY_BYTES: usize = 63 * 1024 * 1024;

#[test]
#[ignore = "needs copies of private worlds in .local/"]
fn private_worlds_convert_to_format3_and_back() {
    let worlds = worlds();
    assert!(!worlds.is_empty(), "no worlds in {}", local().display());
    println!(
        "{:<26} {:>9} {:>8} {:>8} {:>7} {:>8} {:>9} {:>9} {:>7} {:>7} {:>8} {:>9} {:>7}",
        "world",
        "convert",
        "full",
        "shaped",
        "pieces",
        "water",
        "internal",
        "bytes",
        "write",
        "boxes",
        "load",
        "pieces",
        "MiB"
    );
    for (name, path) in worlds {
        let start = Instant::now();
        let f3 = load(&path);
        let parse = start.elapsed();
        let start = Instant::now();
        let tree = Octree::from_format3(&f3).unwrap_or_else(|e| panic!("{name}: {e}"));
        let convert = start.elapsed();
        let stats = tree.stats();
        let start = Instant::now();
        let written = tree.to_format3();
        let write = start.elapsed();
        assert!(written.roots.is_empty());
        let again = Octree::from_format3(&written).unwrap();
        assert_eq!(again.digest(), tree.digest(), "{name}");
        // The loader from before the octree would open the output within its
        // limits, and a world loaded as the server loads one holds the same
        // tree.
        old_loader_accepts(&written).unwrap_or_else(|e| panic!("{name}: {e}"));
        let start = Instant::now();
        let reopened = world_of(&written);
        let old_load = start.elapsed();
        let (pieces, bytes) = counted(&written);
        assert!(pieces <= MAX_WORLD_FRAGMENTS, "{name}: {pieces} pieces");
        assert!(bytes <= MAX_GEOMETRY_BYTES, "{name}: {bytes} bytes");
        assert_eq!(reopened.digest(), tree.digest(), "{name}");
        assert_eq!(world_of(&f3).digest(), tree.digest(), "{name}");
        let (old_pieces, old_bytes) = counted(&f3);
        println!(
            "{:<26} {:>7.0}ms {:>8} {:>8} {:>7} {:>8} {:>9} {:>9} {:>5.0}ms {:>7} {:>6.0}ms {:>9} {:>7.2}",
            name,
            convert.as_secs_f64() * 1000.0,
            stats.full_leaves,
            stats.shaped_leaves,
            stats.piece_leaves,
            stats.material_leaves,
            stats.internal_nodes,
            stats.bytes(),
            write.as_secs_f64() * 1000.0,
            written.volumes.len(),
            old_load.as_secs_f64() * 1000.0,
            pieces,
            bytes as f64 / 1048576.0,
        );
        println!(
            "{:<26} JSON parse {:.0}ms; before: {} regions, {} cells, {} roots, {} boxes, {} pieces, {:.2} MiB",
            "",
            parse.as_secs_f64() * 1000.0,
            f3.regions.len(),
            f3.cells.len(),
            f3.roots.len(),
            f3.volumes.len(),
            old_pieces,
            old_bytes as f64 / 1048576.0
        );
    }
}

// ---------------------------------------------------------------------------
// Queries: the recorded answers of the code from before the tree.

fn bits(v: f32) -> String {
    format!("{:08x}", v.to_bits())
}

fn option_bits(v: Option<f32>) -> String {
    v.map_or("none".into(), bits)
}

fn ulps(a: f32, b: f32) -> u32 {
    if a == b {
        return 0;
    }
    if a.is_sign_negative() != b.is_sign_negative() {
        return (a.to_bits() & 0x7fff_ffff) + (b.to_bits() & 0x7fff_ffff);
    }
    a.to_bits().abs_diff(b.to_bits())
}

/// Replays the answers recorded from the code before the tree against
/// `World`, loaded as the server loads it. Differences are counted by class;
/// unexplained ones are printed.
#[test]
#[ignore = "needs copies of private worlds and recordings in .local/"]
fn private_query_recordings_replay() {
    let directory = local().join("query-recordings");
    let float = |t: &str| f32::from_bits(u32::from_str_radix(t, 16).unwrap());
    let vec = |t: &[&str]| Vec3::new(float(t[0]), float(t[1]), float(t[2]));
    let body = Body::new(crate::PLAYER_RADIUS, crate::PLAYER_HEIGHT);
    let wider = Body::new(crate::PLAYER_RADIUS + 4.0e-5, crate::PLAYER_HEIGHT);
    for name in ["the_cube1m", "library/complex", "library/ralith1"] {
        let Ok(text) =
            std::fs::read_to_string(directory.join(format!("{}.txt", name.replace('/', "-"))))
        else {
            println!("{name}: no recording");
            continue;
        };
        let world = world_of(&load(&local().join(format!("{name}.json"))));
        let tree = &world.tree;
        // [queries, same, explained]
        let mut counts = std::collections::BTreeMap::<String, [usize; 3]>::new();
        let mut examples = Vec::new();
        for line in text.lines().skip(1) {
            let t: Vec<&str> = line.split(' ').collect();
            let (kind, same, explained): (String, bool, Option<&str>) = match t[0] {
                "box" => {
                    let (min, max) = (vec(&t[1..4]), vec(&t[4..7]));
                    let new =
                        world.intersects_box(min, max) || world.intersects_solid_volume(min, max);
                    ("box".into(), (t[7] == "1") == new, None)
                }
                "material" => {
                    let p = vec(&t[1..4]);
                    let new = format!("{:?}", world.volume_material_at(p));
                    let same = new == t[4..].join(" ");
                    let inside = new == "None" && tree.contains_point(p);
                    (
                        "material".into(),
                        same,
                        inside.then_some("water or glass inside solid"),
                    )
                }
                "support" | "ceiling" => {
                    let feet = vec(&t[1..4]);
                    let (low, high) = (float(t[4]), float(t[5]));
                    let ceiling = t[0] == "ceiling";
                    let ask = |body| {
                        if ceiling {
                            tree.first_ceiling(feet, low, high, body)
                        } else {
                            tree.support_between(feet, low, high, body)
                        }
                    };
                    let new = ask(body);
                    let same = option_bits(new) == t[6];
                    let old = (t[6] != "none").then(|| float(t[6]));
                    // The recorded face belongs to a leaf the body overlaps by
                    // no more than the contact tolerance.
                    let touch = !same && option_bits(ask(wider)) == t[6];
                    // The recorded face has solid on both sides of it at the
                    // footprint, grown by a millimetre to reach a leaf the body
                    // only touches: a face between two of the old regions,
                    // inside a merged leaf.
                    let internal = !same
                        && old.is_some_and(|h| {
                            let r = crate::PLAYER_RADIUS + 1.0e-3;
                            let solid = |lo: f32, hi: f32| {
                                world.intersects_box(
                                    Vec3::new(feet.x - r, lo, feet.z - r),
                                    Vec3::new(feet.x + r, hi, feet.z + r),
                                )
                            };
                            solid(h, h + 0.01) && solid(h - 0.01, h)
                        });
                    let class = if touch {
                        Some("a face the body only touches")
                    } else if internal {
                        Some("a face inside a merged leaf")
                    } else {
                        None
                    };
                    if !same && class.is_none() && examples.len() < 20 {
                        examples.push(format!("{line} -> new {new:?}, wider {:?}", ask(wider)));
                    }
                    (t[0].into(), same, class)
                }
                _ => {
                    let (origin, direction) = (vec(&t[1..4]), vec(&t[4..7]));
                    let new = match t[0] {
                        "ray" => world.raycast(origin, direction, 60.0),
                        "volumeray" => world.raycast_volumes(origin, direction, 60.0),
                        _ => world.raycast_solid_volumes(origin, direction, 60.0),
                    };
                    let old_point = (t[7] != "none").then(|| (vec(&t[7..10]), float(t[10])));
                    let same = match (old_point, &new) {
                        (None, None) => true,
                        (Some((p, d)), Some(h)) => {
                            (0..3).all(|i| ulps(p[i], h.point[i]) <= 4) && ulps(d, h.distance) <= 4
                        }
                        _ => false,
                    };
                    // Session 1's classes: the old code's `f32` samples a step
                    // either side of a hit round back onto the face, so it took
                    // an internal join for a face or lost a real one; or the
                    // hit lies on a cube boundary, where the old regions met.
                    let class = (!same).then(|| {
                        let d = direction.normalize();
                        let step =
                            |p: Vec3| (p.abs().max_element() * f32::EPSILON * 2.0).max(8.0e-5);
                        let joined = |p: Vec3| {
                            let e = step(p);
                            tree.contains_point(p - d * e) && tree.contains_point(p + d * e)
                        };
                        let collapsed = |p: Vec3, normal: Vec3| {
                            let ulp = p.abs().max_element() * f32::EPSILON;
                            [p - d * step(p), p + d * step(p)]
                                .iter()
                                .any(|q| (*q - p).dot(normal).abs() <= ulp)
                        };
                        let old_joined = old_point.is_some_and(|(p, _)| joined(p));
                        let lost = new.as_ref().is_some_and(|h| {
                            collapsed(h.point, h.normal)
                                && old_point.is_none_or(|(_, distance)| h.distance < distance)
                        });
                        let near_edge = old_point.is_some_and(|(p, _)| {
                            (0..3).filter(|&i| (p[i] * 8.0).fract() == 0.0).count() >= 1
                        });
                        if old_joined {
                            Some("the old hit lies between solids")
                        } else if lost {
                            Some("the old samples round onto the face")
                        } else if near_edge {
                            Some("the old hit lies on a cube boundary")
                        } else {
                            None
                        }
                    });
                    if !same && class.flatten().is_none() && examples.len() < 20 {
                        examples.push(format!("{line} -> new {new:?}"));
                    }
                    (t[0].into(), same, class.flatten())
                }
            };
            let entry = counts.entry(kind.clone()).or_default();
            entry[0] += 1;
            entry[1] += usize::from(same);
            if let Some(class) = explained.filter(|_| !same) {
                entry[2] += 1;
                counts.entry(format!("{kind}: {class}")).or_default()[0] += 1;
            }
        }
        println!("{name}: [queries, same, explained]");
        for (kind, c) in &counts {
            println!("  {kind}: {c:?}");
        }
        for example in &examples {
            println!("  unexplained: {example}");
        }
        assert!(
            examples.is_empty(),
            "{name}: differences outside the known classes"
        );
    }
}

/// World meshes on copies of private worlds: mesh units, triangles and the
/// time to build them all, and how many units 16, 32 and 64 m units would
/// give. Faces are never split at units, so triangles do not depend on the
/// unit size.
#[test]
#[ignore = "needs copies of private worlds in .local/"]
fn private_world_meshes() {
    println!(
        "{:<12} {:>8} {:>7} {:>9} {:>9} {:>6} {:>6} {:>6} {:>9} {:>14}",
        "world",
        "load",
        "mesh",
        "units",
        "triangles",
        "16 m",
        "32 m",
        "64 m",
        "no caps",
        "children"
    );
    for (name, path) in worlds() {
        let f3 = load(&path);
        let start = Instant::now();
        let world = world_of(&f3);
        let loaded = start.elapsed();
        let fallbacks = super::mesh::cap_fallbacks();
        let start = Instant::now();
        let keys = world.chunk_keys();
        let mut triangles = 0;
        let mut drawn = 0;
        for &key in &keys {
            let mesh = world.mesh_chunk(key);
            triangles += mesh.indices.len() / 3;
            drawn += usize::from(!mesh.indices.is_empty());
        }
        let meshed = start.elapsed();
        let units = |metres: i32| {
            let size = metres * 8;
            let root = world.tree.root_box().min;
            let mut keys = std::collections::BTreeSet::new();
            world.tree.for_each_leaf(|at, leaf| {
                if leaf.geometry != LeafGeometry::Empty {
                    keys.insert(std::array::from_fn::<i32, 3, _>(|a| {
                        (at.min[a] - root[a]).div_euclid(size)
                    }));
                }
            });
            keys.len()
        };
        let pieces = world.stats().piece_leaves;
        let loaded_fallbacks = super::mesh::cap_fallbacks() - fallbacks;
        // Every child of every shaped leaf, as the pieces an edit beside or
        // through it would leave.
        let fallbacks = super::mesh::cap_fallbacks();
        let mut children = 0;
        world.tree.for_each_leaf(|at, leaf| {
            if let LeafGeometry::Shaped(shape) = leaf.geometry {
                if at.size < 2 {
                    return;
                }
                let source = Source {
                    origin: at.min,
                    size: at.size,
                    shape,
                };
                for i in 0..8 {
                    super::mesh::piece_triangles(&source, at.child(i).fine_box());
                    children += 1;
                }
            }
        });
        println!(
            "{:<12} {:>6.0}ms {:>5.0}ms {:>9} {:>9} {:>6} {:>6} {:>6} {:>9} {:>14}",
            name,
            loaded.as_secs_f64() * 1000.0,
            meshed.as_secs_f64() * 1000.0,
            format!("{drawn}/{}", keys.len()),
            triangles,
            units(16),
            units(32),
            units(64),
            format!("{loaded_fallbacks}/{pieces}"),
            format!("{}/{children}", super::mesh::cap_fallbacks() - fallbacks),
        );
    }
}
