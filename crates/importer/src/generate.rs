//! `sauerteig-import generate`: writes a generated benchmark world and a
//! capture pose file for it.

use sauerteig_core::{
    generate_benchmark_world, BenchmarkOptions, BenchmarkPose, BenchmarkPreset, BenchmarkWorld,
    CellEdit, CellFragment, MaterialVolume, WorldBounds, WorldObject, BENCHMARK_SEED,
    MAX_WORLD_PIECES, WORLD_FORMAT_VERSION,
};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// What the server and the importer accept of one world file: a little under
/// the 64 MiB limit of a save file and a join snapshot.
const MAX_FILE_BYTES: usize = 63 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub(crate) struct Arguments {
    pub preset: BenchmarkPreset,
    pub output: PathBuf,
    pub poses: PathBuf,
    pub seed: u64,
    pub edited: bool,
}

/// Parses what follows `generate`. None requests help.
pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Arguments>, String> {
    let Some(name) = args.next() else {
        return Ok(None);
    };
    if name == "--help" || name == "-h" {
        return Ok(None);
    }
    let preset = BenchmarkPreset::from_name(&name)
        .ok_or_else(|| format!("unknown preset {name}; use test, small or nub1"))?;
    let mut options = BenchmarkOptions::new(preset);
    let mut output = None;
    let mut poses = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("missing {arg} value"));
        match arg.as_str() {
            "--output" => output = Some(PathBuf::from(value()?)),
            "--poses" => poses = Some(PathBuf::from(value()?)),
            "--seed" => {
                options.seed = value()?.parse().map_err(|_| "invalid seed")?;
            }
            "--edits" => {
                options.edited = match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err("--edits takes on or off".into()),
                };
            }
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    let output =
        output.ok_or("generate requires --output PATH; existing paths are never overwritten")?;
    let poses = poses.unwrap_or_else(|| output.with_extension("poses.json"));
    if output == poses {
        return Err("world and pose paths must differ".into());
    }
    Ok(Some(Arguments {
        preset,
        output,
        poses,
        seed: options.seed,
        edited: options.edited,
    }))
}

/// A world file in format 3, as the server loads it.
#[derive(Serialize)]
struct WorldFile<'a> {
    format_version: u32,
    bounds: WorldBounds,
    regions: &'a [CellFragment],
    volumes: &'a [MaterialVolume],
    revision: u64,
    cells: &'a [CellEdit],
    roots: [(); 0],
    objects: &'a [WorldObject],
}

#[derive(Serialize)]
struct PoseEntry<'a> {
    world: &'a str,
    #[serde(flatten)]
    pose: &'a BenchmarkPose,
}

/// The bytes of the world file and of the pose file.
pub(crate) fn render(
    world: &BenchmarkWorld,
    world_path: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let format3 = world.format3();
    let file = WorldFile {
        format_version: WORLD_FORMAT_VERSION,
        bounds: world.bounds,
        regions: &format3.regions,
        volumes: &format3.volumes,
        revision: 0,
        cells: &format3.cells,
        roots: [],
        objects: &world.objects,
    };
    let world_bytes = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    let entries: Vec<PoseEntry> = world
        .poses
        .iter()
        .map(|pose| PoseEntry {
            world: world_path,
            pose,
        })
        .collect();
    let mut pose_bytes = serde_json::to_vec_pretty(&entries).map_err(|e| e.to_string())?;
    pose_bytes.push(b'\n');
    Ok((world_bytes, pose_bytes))
}

pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let Some(args) = parse(args)? else {
        help();
        return Ok(());
    };
    for (path, label) in [(&args.output, "output"), (&args.poses, "poses")] {
        if path.exists() {
            return Err(format!("{label} already exists: {}", path.display()));
        }
    }
    let started = std::time::Instant::now();
    let world = generate_benchmark_world(BenchmarkOptions {
        preset: args.preset,
        seed: args.seed,
        edited: args.edited,
    })?;
    let stats = world.tree.stats();
    if stats.leaves() > MAX_WORLD_PIECES {
        return Err(format!(
            "the world has {} leaves; the server's guard is {MAX_WORLD_PIECES}",
            stats.leaves()
        ));
    }
    let (world_bytes, pose_bytes) = render(&world, &args.output.display().to_string())?;
    if world_bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "world JSON is {} bytes, over the {MAX_FILE_BYTES}-byte limit of saves and joins",
            world_bytes.len()
        ));
    }
    write_new(&args.output, &world_bytes)?;
    write_new(&args.poses, &pose_bytes)?;
    let digest: String = world
        .tree
        .digest()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    eprintln!(
        "Generated {} (seed {}, edited layer {}) in {:.2} s: {} leaves ({} full, {} shaped, {} \
         water or glass), {} lights, {} spawns, {} poses.\nTree digest {digest}, objects and \
         poses {:016x}.\nWorld: {} ({} bytes)\nPoses: {}",
        args.preset.name(),
        args.seed,
        if args.edited { "on" } else { "off" },
        started.elapsed().as_secs_f64(),
        stats.leaves(),
        stats.full_leaves,
        stats.shaped_leaves,
        stats.material_leaves,
        world.features.lights,
        world.features.spawns,
        world.poses.len(),
        world.extras_digest(),
        args.output.display(),
        world_bytes.len(),
        args.poses.display(),
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn help() {
    println!(
        "Generated benchmark worlds

  sauerteig-import generate PRESET --output WORLD.json [options]

Presets: test (a few thousand leaves), small (the size of an imported map of
about 340,000 leaves) and nub1 (about 510,000 leaves).

Options:
  --poses FILE     Capture poses for the world; default WORLD.poses.json
  --seed N         Default {BENCHMARK_SEED}; the recorded digests use the default
  --edits on|off   The edited layer: default on for test and small, off for nub1

The same preset, seed and edits give the same files on every system. Existing
files are never overwritten."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauerteig_core::{Octree, World};

    fn arguments(args: &[&str]) -> Result<Arguments, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))?.ok_or_else(|| "unexpected help".into())
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    fn test_files() -> (BenchmarkWorld, Vec<u8>, Vec<u8>) {
        let world = generate_benchmark_world(BenchmarkOptions::new(BenchmarkPreset::Test)).unwrap();
        let (world_bytes, pose_bytes) = render(&world, "test.json").unwrap();
        (world, world_bytes, pose_bytes)
    }

    /// The test preset's files, byte for byte. They must match on every
    /// system, since the generator and the JSON writer use no floating-point
    /// math beyond exact eighths of a metre.
    #[test]
    fn the_test_preset_writes_the_recorded_files() {
        let (_, world, poses) = test_files();
        assert_eq!(
            (world.len(), fnv(&world), poses.len(), fnv(&poses)),
            (
                WORLD_FILE_BYTES,
                WORLD_FILE_DIGEST,
                POSE_FILE_BYTES,
                POSE_FILE_DIGEST
            )
        );
    }

    const WORLD_FILE_BYTES: usize = 1_983_256;
    const WORLD_FILE_DIGEST: u64 = 12_182_345_955_731_919_108;
    const POSE_FILE_BYTES: usize = 3_281;
    const POSE_FILE_DIGEST: u64 = 16_324_509_455_315_688_999;

    #[test]
    fn the_world_file_loads_like_a_converted_map() {
        let (world, bytes, _) = test_files();
        #[derive(serde::Deserialize)]
        struct File {
            format_version: u32,
            bounds: WorldBounds,
            regions: Vec<CellFragment>,
            volumes: Vec<MaterialVolume>,
            cells: Vec<CellEdit>,
            roots: Vec<serde_json::Value>,
            objects: Vec<WorldObject>,
        }
        let file: File = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(file.format_version, WORLD_FORMAT_VERSION);
        assert!(file.cells.is_empty() && file.roots.is_empty());
        assert_eq!(file.objects, world.objects);
        let tree = Octree::from_parts(file.bounds, &file.regions, &file.cells, &[], &file.volumes)
            .unwrap();
        assert_eq!(tree.digest(), world.tree.digest());
        let mut play =
            World::from_compact_parts(file.bounds, &file.regions, &file.cells, &[]).unwrap();
        play.replace_volumes(&file.volumes).unwrap();
        assert_eq!(play.digest(), world.tree.digest());
    }

    #[test]
    fn the_pose_file_names_the_world_and_every_pose() {
        let (world, _, bytes) = test_files();
        let poses: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(poses.len(), world.poses.len());
        for pose in &poses {
            assert_eq!(pose["world"], "test.json");
            assert!(pose["name"].is_string());
            assert_eq!(pose["eye"].as_array().unwrap().len(), 3);
            assert_eq!(pose["look_at"].as_array().unwrap().len(), 3);
        }
        let names: Vec<_> = poses.iter().map(|p| p["name"].as_str().unwrap()).collect();
        for expected in [
            "spawn",
            "overlook",
            "room-1",
            "water",
            "flight",
            "teleports",
        ] {
            assert!(names.contains(&expected), "{names:?}");
        }
    }

    #[test]
    fn arguments_choose_the_preset_and_default_the_edited_layer() {
        let small = arguments(&["small", "--output", "w.json"]).unwrap();
        assert_eq!(small.preset, BenchmarkPreset::Small);
        assert!(small.edited);
        assert_eq!(small.seed, BENCHMARK_SEED);
        assert_eq!(small.poses, PathBuf::from("w.poses.json"));
        assert!(!arguments(&["nub1", "--output", "w.json"]).unwrap().edited);
        let custom = arguments(&[
            "test", "--output", "w.json", "--edits", "off", "--seed", "7", "--poses", "p.json",
        ])
        .unwrap();
        assert_eq!((custom.edited, custom.seed), (false, 7));
        assert_eq!(custom.poses, PathBuf::from("p.json"));
    }

    #[test]
    fn bad_arguments_are_refused_before_anything_is_written() {
        assert!(arguments(&["huge", "--output", "w.json"])
            .unwrap_err()
            .contains("unknown preset"));
        assert!(arguments(&["small"])
            .unwrap_err()
            .contains("requires --output"));
        assert!(arguments(&["small", "--output", "w.json", "--edits", "maybe"]).is_err());
        assert!(arguments(&["small", "--output", "w.json", "--seed", "x"]).is_err());
        assert!(
            arguments(&["small", "--output", "w.json", "--poses", "w.json"])
                .unwrap_err()
                .contains("must differ")
        );
        assert!(arguments(&["small", "--output", "w.json", "--crop"]).is_err());
        assert!(parse(std::iter::empty()).unwrap().is_none());
    }
}
