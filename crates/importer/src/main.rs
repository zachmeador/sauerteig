mod cli;
mod generate;

use cli::{Center, Command};
use sauerteig_core::FINE_UNITS;
use sauerteig_import::{convert, inspect_cfg, read_ogz, MAX_COMPRESSED_BYTES};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

const MAX_AUXILIARY_BYTES: usize = 1024 * 1024;
const MAX_WORLD_BYTES: usize = 64 * 1024 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("Import error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|first| first == "generate") {
        args.next();
        return generate::run(args);
    }
    let Some(args) = cli::parse(args)? else {
        help();
        return Ok(());
    };
    if let Command::Convert { output, .. } = &args.command {
        require_new_path(output, "output")?;
    }
    if let Some(report) = &args.report {
        require_new_path(report, "report")?;
    }

    let map = read_ogz(&read_bounded(&args.input, MAX_COMPRESSED_BYTES)?)?;
    let cfg_path = args.cfg.unwrap_or_else(|| args.input.with_extension("cfg"));
    let cfg_report = if cfg_path.exists() {
        let bytes = read_bounded(&cfg_path, MAX_AUXILIARY_BYTES)?;
        Some(inspect_cfg(&String::from_utf8_lossy(&bytes)))
    } else {
        None
    };
    let (report, world) = match args.command {
        Command::Inspect => (
            serde_json::json!({"input":args.input,"inventory":map.inventory(),"cfg":cfg_report}),
            None,
        ),
        Command::Convert {
            output,
            mut options,
            center,
            material_map,
        } => {
            if let Some(path) = material_map {
                options.materials =
                    serde_json::from_slice(&read_bounded(&path, MAX_AUXILIARY_BYTES)?)
                        .map_err(|error| format!("invalid material mapping: {error}"))?;
            }
            options.center = match center {
                None => None,
                Some(Center::Coordinates(coordinates)) => Some(coordinates),
                Some(Center::Spawn) => {
                    let spawn = map
                        .entities
                        .iter()
                        .find(|entity| entity.kind == 3)
                        .ok_or("map has no playerstart; provide --center X,Y,Z")?;
                    // Align the region center only; geometry is never rounded.
                    let step = f64::from(options.units_per_metre) / f64::from(FINE_UNITS);
                    Some(
                        spawn
                            .position
                            .map(|value| (f64::from(value) / step).round() * step),
                    )
                }
            };
            let (world, report) = convert(&map, &options)?;
            eprintln!(
                "Imported an octree of {} leaves ({} full, {} shaped, {} pieces, {} holding only a \
                 material; root {} m), written as {} regions, {} cells and {} material volumes \
                 ({} water / {} glass source leaves), {} lights, {} spawns. \
                 Cropped {} geometry / {} volume leaves; clipped {} geometry / {} volume leaves. \
                 Entities left out: {} without a position, {} beyond the world. \
                 Unsupported geometry omissions: {}.",
                report.tree.leaves,
                report.tree.full_leaves,
                report.tree.shaped_leaves,
                report.tree.piece_leaves,
                report.tree.material_leaves,
                f64::from(report.tree.root_size_eighths) / f64::from(FINE_UNITS),
                report.imported_regions,
                report.imported_coarse_cells,
                report.imported_volume_boxes,
                report.imported_water_leaves,
                report.imported_glass_leaves,
                report.imported_lights,
                report.imported_spawns,
                report.leaves_outside_region,
                report.volume_leaves_outside_region,
                report.leaves_clipped_at_region,
                report.volume_leaves_clipped_at_region,
                report.entities_omitted_out_of_range.len(),
                report.entities_omitted_beyond_world.len(),
                report.unsupported_leaves_omitted.len(),
            );
            (
                serde_json::json!({"input":args.input,"geometry":report,"cfg":cfg_report}),
                Some((output, world)),
            )
        }
    };
    let world_bytes = world
        .map(|(path, world)| serde_json::to_vec(&world).map(|bytes| (path, bytes)))
        .transpose()
        .map_err(|error| error.to_string())?;
    if world_bytes
        .as_ref()
        .is_some_and(|(_, bytes)| bytes.len() > MAX_WORLD_BYTES)
    {
        return Err("world JSON exceeds server 64 MiB save-file limit".into());
    }
    let report_bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    if let Some(path) = args.report {
        write_new(&path, &report_bytes)?;
        eprintln!("Report: {}", path.display());
    } else {
        println!("{}", String::from_utf8_lossy(&report_bytes));
    }
    if let Some((path, bytes)) = world_bytes {
        write_new(&path, &bytes)?;
        eprintln!("World: {}", path.display());
    }
    Ok(())
}

fn require_new_path(path: &Path, label: &str) -> Result<(), String> {
    if path.exists() {
        return Err(format!("{label} already exists: {}", path.display()));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds {limit}-byte limit", path.display()));
    }
    Ok(bytes)
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

fn help() {
    println!(
        "Sauerteig legacy map importer

  sauerteig-import inspect MAP.ogz [--report REPORT.json] [--cfg MAP.cfg]
  sauerteig-import convert MAP.ogz --output WORLD.json [options]
  sauerteig-import generate PRESET --output WORLD.json   (see generate --help)

Conversion options:
  --units-per-metre N     Power of two, default 8 (never automatic shrinking)
  --center X,Y,Z|spawn    Legacy Z-up point placed at [0,24,0] in Sauerteig; the map is
                          mirrored into Sauerteig's right-handed axes
  --crop                 Explicitly retain only the 128×48×128m region
  --skip-unsupported     Explicitly omit unsupported degenerate cube shapes
  --material-map FILE    JSON object mapping legacy slot IDs to material IDs

Shared options:
  --report FILE          Detailed reference/loss report; default WORLD.import.json
  --cfg FILE             Inspect literal references only; never execute scripts

Supported: OCTA versions 27 and 29–33 (28 is refused). Entities with no usable
position, and entities beyond what the world can hold, are left out and listed in
the report. Originals and existing outputs are never overwritten.
Without --crop, the complete original-scale world is retained as one octree.
World bounds and authored-size limits are validated. Missing textures use neutral material 0.
Models, gameplay entities, volumes and baked rendering need separate adapters.
After import, start a separate server with --world WORLD.json to view the result."
    );
}
