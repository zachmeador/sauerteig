//! Checks on copies of private worlds in the repository's ignored `.local/`:
//! `the_cube1m.json` and `library/*.json`. They are ignored by default, since
//! CI has no such files; run them explicitly in a release build:
//! `cargo test --release -p sauerteig-core --lib private_worlds -- --ignored --nocapture`
//!
//! The query comparison records today's answers in
//! `.local/query-recordings/`, so they can be replayed against the tree once
//! today's code is gone.

use super::tests::Rng;
use super::*;
use crate::physics::{Body, Space};
use crate::{CellEdit, CellFragment, MaterialVolume, RayHit, RootEdit, Vec3, World};
use serde::Deserialize;
use std::fmt::Write as _;
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

fn old_world(f: &Format3) -> World {
    let mut world = World::from_compact_parts(f.bounds, &f.regions, &f.cells, &f.roots).unwrap();
    world.replace_volumes(&f.volumes).unwrap();
    world
}

fn json_bytes(value: &impl serde::Serialize) -> usize {
    serde_json::to_vec(value).unwrap().len()
}

/// The server's counted size of a world: pieces against the 786,432-piece
/// guard and bytes against the 63 MiB geometry limit.
fn counted(f: &Format3) -> (usize, usize) {
    let pieces = f.regions.len()
        + f.cells.len()
        + f.roots
            .iter()
            .map(|r| r.fragments.len().max(1))
            .sum::<usize>();
    let cell_root = |cell: &CellEdit| RootEdit {
        pos: cell.pos,
        fragments: cell
            .cell
            .map(|c| vec![CellFragment::from_cell(cell.pos, c)])
            .unwrap_or_default(),
    };
    let bytes = f.regions.iter().map(|r| json_bytes(r) + 1).sum::<usize>()
        + f.cells
            .iter()
            .map(|c| json_bytes(&cell_root(c)) + 1)
            .sum::<usize>()
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
        "old load",
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
        // The loader from before the octree opens the output within its
        // limits, and holds the same content.
        let start = Instant::now();
        let reopened = old_world(&written);
        let old_load = start.elapsed();
        let (pieces, bytes) = counted(&written);
        assert!(
            written.regions.len() <= crate::MAX_COMPACT_REGIONS,
            "{name}"
        );
        assert!(pieces <= MAX_WORLD_FRAGMENTS, "{name}: {pieces} pieces");
        assert!(bytes <= MAX_GEOMETRY_BYTES, "{name}: {bytes} bytes");
        assert_eq!(
            Octree::from_world(&reopened).unwrap().digest(),
            tree.digest(),
            "{name}"
        );
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
// Queries: today's code against the tree.

fn bits(v: f32) -> String {
    format!("{:08x}", v.to_bits())
}

fn vec_bits(v: Vec3) -> String {
    format!("{} {} {}", bits(v.x), bits(v.y), bits(v.z))
}

fn option_bits(v: Option<f32>) -> String {
    v.map_or("none".into(), bits)
}

fn ray_text(hit: &Option<RayHit>) -> String {
    match hit {
        None => "none".into(),
        Some(h) => format!(
            "{} {} {} {:?} {:?} {:?}",
            vec_bits(h.point),
            bits(h.distance),
            h.face,
            h.pos,
            h.bounds.min,
            h.bounds.max
        ),
    }
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

#[derive(Default, Debug)]
struct Timing {
    old: std::time::Duration,
    new: std::time::Duration,
}

impl Timing {
    fn both<T>(&mut self, old: impl FnOnce() -> T, new: impl FnOnce() -> T) -> (T, T) {
        let start = Instant::now();
        let a = old();
        self.old += start.elapsed();
        let start = Instant::now();
        let b = new();
        self.new += start.elapsed();
        (a, b)
    }
}

#[derive(Default, Debug)]
struct Counts {
    boxes: usize,
    box_differ: usize,
    materials: usize,
    material_differ: usize,
    material_in_solid: usize,
    material_in_solid_farthest: f32,
    supports: usize,
    support_differ: usize,
    /// Today's answer changes when the footprint shrinks by 0.02 mm: it came
    /// from a face the body only touches, such as an internal face between
    /// two of today's regions that the tree merged.
    support_grazing: usize,
    ceilings: usize,
    ceiling_differ: usize,
    rays: usize,
    ray_hits: usize,
    rays_inside: usize,
    ray_differ: usize,
    /// Different first hits where today's `f32` samples beside one of the
    /// hits round back onto its face, at a grazing angle or far from the
    /// origin, so today's internal-join test cannot tell the two sides apart.
    ray_collapsed: usize,
    /// Same hit point, different face: the hit lies on an edge.
    ray_face_differ: usize,
    /// Same hit and face, different cube: the hit lies in a merged leaf
    /// larger than today's region (its box holds today's box).
    ray_cube_merged: usize,
    /// Same hit and face, different metre: the hit lies on a cube boundary.
    ray_cube_boundary: usize,
    volume_rays: usize,
    volume_ray_differ: usize,
    step_ups: usize,
    /// Step-up probes inside solid where today's answer is a face with
    /// solid or glass right above it: an internal face between two of
    /// today's regions or glass boxes, which merged leaves no longer have.
    step_up_internal: usize,
    step_up_differ: usize,
}

fn compare(
    name: &str,
    world: &World,
    tree: &Octree,
    queries: usize,
    record: &mut String,
) -> Counts {
    let mut rng = Rng::new(0x5eed);
    let leaves = tree.leaves();
    let solid: Vec<_> = leaves
        .iter()
        .filter(|(_, l)| l.geometry != LeafGeometry::Empty)
        .copied()
        .collect();
    let liquid: Vec<_> = leaves
        .iter()
        .filter(|(_, l)| volume_material_of(l.cube_material).is_some())
        .copied()
        .collect();
    let body = Body::new(crate::PLAYER_RADIUS, crate::PLAYER_HEIGHT);
    let near = |rng: &mut Rng, at: NodeBox, margin: f32| {
        let b = at.fine_box();
        let lo = b.world_min() - Vec3::splat(margin);
        let span = b.world_max() - b.world_min() + Vec3::splat(2.0 * margin);
        lo + Vec3::new(rng.unit(), rng.unit(), rng.unit()) * span
    };
    let pick =
        |rng: &mut Rng, from: &[(NodeBox, Leaf)]| from[rng.below(from.len() as u64) as usize].0;
    let mut c = Counts::default();
    let mut boxes = Timing::default();
    let mut supports = Timing::default();
    let mut rays = Timing::default();
    let mut examples = Vec::new();
    let note = |examples: &mut Vec<String>, text: String| {
        if examples.len() < 12 {
            examples.push(text);
        }
    };
    writeln!(record, "world {name}").unwrap();
    for _ in 0..queries {
        // Box overlap near geometry.
        let at = pick(&mut rng, &solid);
        let centre = near(&mut rng, at, 1.0);
        let half = Vec3::new(rng.unit(), rng.unit(), rng.unit()) * 0.8 + Vec3::splat(0.005);
        let (min, max) = (centre - half, centre + half);
        let (old, new) = boxes.both(
            || world.intersects_box(min, max) || world.intersects_solid_volume(min, max),
            || tree.intersects_box(min, max) || tree.intersects_solid_volume(min, max),
        );
        c.boxes += 1;
        writeln!(
            record,
            "box {} {} {}",
            vec_bits(min),
            vec_bits(max),
            u8::from(old)
        )
        .unwrap();
        if old != new {
            c.box_differ += 1;
            note(
                &mut examples,
                format!("box {min:?} {max:?}: {old} vs {new}"),
            );
        }
        // Point material, near water and glass when there is any.
        let at = if !liquid.is_empty() && rng.chance(70) {
            pick(&mut rng, &liquid)
        } else {
            pick(&mut rng, &solid)
        };
        let p = near(&mut rng, at, 0.5);
        let old = world.volume_material_at(p);
        let new = tree.volume_material_at(p);
        c.materials += 1;
        writeln!(record, "material {} {:?}", vec_bits(p), old).unwrap();
        if old != new {
            // Today's terrain test is a box shrunk by its contact tolerance,
            // which misses solids near the origin; the tree tests the point.
            if new.is_none() && tree.contains_point(p) {
                c.material_in_solid += 1;
                c.material_in_solid_farthest =
                    c.material_in_solid_farthest.max(p.abs().max_element());
            } else {
                c.material_differ += 1;
                note(&mut examples, format!("material {p:?}: {old:?} vs {new:?}"));
            }
        }
        // Support and ceiling where a body can stand, as movement asks.
        let at = pick(&mut rng, &solid);
        let b = at.fine_box();
        let mut feet = near(&mut rng, at, 0.0);
        feet.y = b.world_max().y + rng.unit() * 0.6 - 0.05;
        if world.free_at(feet, body) {
            for (low, high) in [(feet.y - 0.075, feet.y), (feet.y - 0.6, feet.y)] {
                let (old, new) = supports.both(
                    || world.support_between(feet, low, high, body),
                    || tree.support_between(feet, low, high, body),
                );
                c.supports += 1;
                writeln!(
                    record,
                    "support {} {} {} {}",
                    vec_bits(feet),
                    bits(low),
                    bits(high),
                    option_bits(old)
                )
                .unwrap();
                if old.map(f32::to_bits) != new.map(f32::to_bits) {
                    let narrow = Body::new(crate::PLAYER_RADIUS - 2.0e-5, crate::PLAYER_HEIGHT);
                    if world.support_between(feet, low, high, narrow) != old {
                        c.support_grazing += 1;
                    } else {
                        c.support_differ += 1;
                    }
                    note(
                        &mut examples,
                        format!("support {feet:?} {low} {high}: {old:?} vs {new:?}"),
                    );
                }
            }
        }
        let mut feet = near(&mut rng, at, 0.0);
        feet.y = b.world_min().y - crate::PLAYER_HEIGHT - rng.unit() * 0.5;
        if world.free_at(feet, body) {
            let low = feet.y + crate::PLAYER_HEIGHT;
            let high = low + 0.5;
            let old = world.first_ceiling(feet, low, high, body);
            let new = tree.first_ceiling(feet, low, high, body);
            c.ceilings += 1;
            writeln!(
                record,
                "ceiling {} {} {} {}",
                vec_bits(feet),
                bits(low),
                bits(high),
                option_bits(old)
            )
            .unwrap();
            if old.map(f32::to_bits) != new.map(f32::to_bits) {
                c.ceiling_differ += 1;
                note(
                    &mut examples,
                    format!("ceiling {feet:?}: {old:?} vs {new:?}"),
                );
            }
        }
        // A step-up probe where the target overlaps a ledge.
        let mut feet = near(&mut rng, at, 0.3);
        feet.y = b.world_max().y - rng.unit() * 0.5;
        if !world.free_at(feet, body) {
            let (low, high) = (feet.y - 0.3, feet.y + 0.2);
            let old = world.support_between(feet, low, high, body);
            let new = tree.support_between(feet, low, high, body);
            c.step_ups += 1;
            if old.map(f32::to_bits) != new.map(f32::to_bits) {
                let (min, max) = (
                    feet - Vec3::new(0.3, 0.0, 0.3),
                    feet + Vec3::new(0.3, 0.0, 0.3),
                );
                let covered = old.is_some_and(|h| {
                    let (lo, hi) = (
                        Vec3::new(min.x, h, min.z),
                        Vec3::new(max.x, h + 0.01, max.z),
                    );
                    new.is_none_or(|n| n < h)
                        && (tree.intersects_box(lo, hi) || tree.intersects_solid_volume(lo, hi))
                });
                let narrow = Body::new(crate::PLAYER_RADIUS - 2.0e-5, crate::PLAYER_HEIGHT);
                if covered {
                    c.step_up_internal += 1;
                } else if world.support_between(feet, low, high, narrow) != old {
                    c.support_grazing += 1;
                } else {
                    c.step_up_differ += 1;
                    note(
                        &mut examples,
                        format!("step-up {feet:?} {low} {high}: {old:?} vs {new:?}"),
                    );
                }
            }
        }
        // Rays near geometry, some from inside solid.
        let at = pick(&mut rng, &solid);
        let inside = rng.chance(20);
        let origin = near(&mut rng, at, if inside { 0.0 } else { 2.0 });
        let direction = loop {
            let d = Vec3::new(rng.unit(), rng.unit(), rng.unit()) * 2.0 - Vec3::ONE;
            if d.length_squared() > 0.01 && d.length_squared() <= 1.0 {
                break d;
            }
        };
        let (old, new) = rays.both(
            || world.raycast(origin, direction, 60.0),
            || tree.raycast(origin, direction, 60.0),
        );
        c.rays += 1;
        c.ray_hits += usize::from(old.is_some());
        c.rays_inside += usize::from(tree.contains_point(origin));
        writeln!(
            record,
            "ray {} {} {}",
            vec_bits(origin),
            vec_bits(direction),
            ray_text(&old)
        )
        .unwrap();
        let collapsed = |h: &Option<RayHit>| {
            h.is_some_and(|h| {
                let dn = direction.normalize();
                let e = (h.point.abs().max_element() * f32::EPSILON * 2.0).max(8.0e-5);
                let ulp = h.point.abs().max_element() * f32::EPSILON;
                [h.point - dn * e, h.point + dn * e]
                    .iter()
                    .any(|q| (*q - h.point).dot(h.normal).abs() <= ulp)
            })
        };
        match (&old, &new) {
            (None, None) => {}
            (Some(a), Some(b))
                if (0..3).all(|i| ulps(a.point[i], b.point[i]) <= 4)
                    && ulps(a.distance, b.distance) <= 4 =>
            {
                if a.face != b.face {
                    c.ray_face_differ += 1;
                    let edge = (0..3)
                        .filter(|&i| (a.point[i] * 8.0).fract() == 0.0)
                        .count();
                    if edge < 2 {
                        note(
                            &mut examples,
                            format!("ray face {origin:?} {direction:?}: {a:?} vs {b:?}"),
                        );
                    }
                } else if a.pos == b.pos && b.bounds.contains(a.bounds) {
                    c.ray_cube_merged += usize::from(a.bounds != b.bounds);
                } else {
                    c.ray_cube_boundary += 1;
                    let on_boundary = (0..3).any(|i| (a.point[i] * 8.0).fract() == 0.0);
                    if !on_boundary {
                        note(
                            &mut examples,
                            format!("ray cube {origin:?} {direction:?}: {a:?} vs {b:?}"),
                        );
                    }
                }
            }
            _ if collapsed(&old) || collapsed(&new) => c.ray_collapsed += 1,
            _ => {
                c.ray_differ += 1;
                note(
                    &mut examples,
                    format!("ray {origin:?} {direction:?}: {old:?} vs {new:?}"),
                );
            }
        }
        if !liquid.is_empty() {
            let at = pick(&mut rng, &liquid);
            let origin = near(&mut rng, at, 2.0);
            for solid_only in [false, true] {
                let (old, new) = if solid_only {
                    (
                        world.raycast_solid_volumes(origin, direction, 60.0),
                        tree.raycast_solid_volumes(origin, direction, 60.0),
                    )
                } else {
                    (
                        world.raycast_volumes(origin, direction, 60.0),
                        tree.raycast_volumes(origin, direction, 60.0),
                    )
                };
                c.volume_rays += 1;
                writeln!(
                    record,
                    "{} {} {} {}",
                    if solid_only { "glassray" } else { "volumeray" },
                    vec_bits(origin),
                    vec_bits(direction),
                    ray_text(&old)
                )
                .unwrap();
                let same = match (&old, &new) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.distance == b.distance && a.point == b.point,
                    _ => false,
                };
                if !same {
                    c.volume_ray_differ += 1;
                    note(
                        &mut examples,
                        format!("volume ray {origin:?} {direction:?}: {old:?} vs {new:?}"),
                    );
                }
            }
        }
    }
    for example in &examples {
        println!("  {example}");
    }
    for (kind, timing) in [("box", boxes), ("support", supports), ("ray", rays)] {
        println!(
            "  {kind} queries: today {:.1} ms, tree {:.1} ms",
            timing.old.as_secs_f64() * 1000.0,
            timing.new.as_secs_f64() * 1000.0
        );
    }
    c
}

#[test]
#[ignore = "needs copies of private worlds in .local/"]
fn private_queries_agree_with_todays_world() {
    let queries: usize = std::env::var("SAUERTEIG_QUERIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    let directory = local().join("query-recordings");
    std::fs::create_dir_all(&directory).unwrap();
    for name in ["the_cube1m", "library/complex", "library/ralith1"] {
        let path = local().join(format!("{name}.json"));
        if !path.exists() {
            println!("{name}: missing");
            continue;
        }
        let f3 = load(&path);
        let world = old_world(&f3);
        let tree = Octree::from_format3(&f3).unwrap();
        let mut record = String::new();
        println!("{name}:");
        let c = compare(name, &world, &tree, queries, &mut record);
        println!("  {c:?}");
        let file = directory.join(format!("{}.txt", name.replace('/', "-")));
        std::fs::write(file, record).unwrap();
        assert_eq!(c.box_differ, 0, "{name}");
        assert_eq!(c.material_differ, 0, "{name}");
        assert_eq!(c.support_differ, 0, "{name}");
        assert_eq!(c.ceiling_differ, 0, "{name}");
        assert_eq!(c.ray_differ, 0, "{name}");
        assert_eq!(c.volume_ray_differ, 0, "{name}");
        assert_eq!(c.step_up_differ, 0, "{name}");
    }
}

/// Replays the recorded answers of today's code against the tree, for use
/// once today's code is gone.
#[test]
#[ignore = "needs copies of private worlds and recordings in .local/"]
fn private_query_recordings_replay() {
    let directory = local().join("query-recordings");
    let float = |t: &str| f32::from_bits(u32::from_str_radix(t, 16).unwrap());
    let vec = |t: &[&str]| Vec3::new(float(t[0]), float(t[1]), float(t[2]));
    let body = Body::new(crate::PLAYER_RADIUS, crate::PLAYER_HEIGHT);
    for name in ["the_cube1m", "library/complex", "library/ralith1"] {
        let Ok(text) =
            std::fs::read_to_string(directory.join(format!("{}.txt", name.replace('/', "-"))))
        else {
            println!("{name}: no recording");
            continue;
        };
        let tree = Octree::from_format3(&load(&local().join(format!("{name}.json")))).unwrap();
        let mut counts = std::collections::BTreeMap::<&str, [usize; 3]>::new();
        for line in text.lines().skip(1) {
            let t: Vec<&str> = line.split(' ').collect();
            let (kind, same, explained) = match t[0] {
                "box" => {
                    let (min, max) = (vec(&t[1..4]), vec(&t[4..7]));
                    let new =
                        tree.intersects_box(min, max) || tree.intersects_solid_volume(min, max);
                    ("box", (t[7] == "1") == new, false)
                }
                "material" => {
                    let p = vec(&t[1..4]);
                    let new = format!("{:?}", tree.volume_material_at(p));
                    let same = new == t[4..].join(" ");
                    (
                        "material",
                        same,
                        !same && new == "None" && tree.contains_point(p),
                    )
                }
                "support" | "ceiling" => {
                    let feet = vec(&t[1..4]);
                    let (low, high) = (float(t[4]), float(t[5]));
                    let new = if t[0] == "support" {
                        tree.support_between(feet, low, high, body)
                    } else {
                        tree.first_ceiling(feet, low, high, body)
                    };
                    (t[0], option_bits(new) == t[6], false)
                }
                _ => {
                    let (origin, direction) = (vec(&t[1..4]), vec(&t[4..7]));
                    let new = match t[0] {
                        "ray" => tree.raycast(origin, direction, 60.0),
                        "volumeray" => tree.raycast_volumes(origin, direction, 60.0),
                        _ => tree.raycast_solid_volumes(origin, direction, 60.0),
                    };
                    let old_point = (t[7] != "none").then(|| (vec(&t[7..10]), float(t[10])));
                    let same = match (old_point, &new) {
                        (None, None) => true,
                        (Some((p, d)), Some(h)) => {
                            (0..3).all(|i| ulps(p[i], h.point[i]) <= 4) && ulps(d, h.distance) <= 4
                        }
                        _ => false,
                    };
                    (t[0], same, false)
                }
            };
            let entry = counts.entry(kind).or_default();
            entry[0] += 1;
            entry[1] += usize::from(same);
            entry[2] += usize::from(explained);
        }
        println!("{name}: [queries, same, explained] {counts:?}");
    }
}
