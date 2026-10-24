use sauerteig_import::{convert, inspect_cfg, read_ogz, ImportOptions, MAX_COMPRESSED_BYTES};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("Import error: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        help();
        return Ok(());
    };
    if command == "--help" || command == "-h" {
        help();
        return Ok(());
    }
    if command != "inspect" && command != "convert" {
        return Err("first argument must be inspect or convert (see --help)".into());
    }
    let input = PathBuf::from(args.next().ok_or("missing input .ogz path")?);
    let mut options = ImportOptions::default();
    let mut output = None;
    let mut report_path = None;
    let mut cfg = None;
    let mut spawn_center = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => {
                output = Some(PathBuf::from(args.next().ok_or("missing --output value")?))
            }
            "--report" => {
                report_path = Some(PathBuf::from(args.next().ok_or("missing --report value")?))
            }
            "--cfg" => cfg = Some(PathBuf::from(args.next().ok_or("missing --cfg value")?)),
            "--units-per-metre" | "--units-per-meter" => {
                options.units_per_metre = args
                    .next()
                    .ok_or("missing scale")?
                    .parse()
                    .map_err(|_| "invalid scale")?
            }
            "--crop" => options.crop = true,
            "--skip-unsupported" => options.skip_unsupported = true,
            "--center" => {
                let value = args.next().ok_or("missing center")?;
                if value == "spawn" {
                    spawn_center = true;
                } else {
                    let values = value
                        .split(',')
                        .map(str::parse::<f64>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| "center must be X,Y,Z in original Cube coordinates")?;
                    options.center = Some(
                        values
                            .try_into()
                            .map_err(|_| "center needs three coordinates")?,
                    );
                }
            }
            "--material-map" => {
                let path = args.next().ok_or("missing material mapping JSON file")?;
                let bytes = read_bounded(Path::new(&path), 1024 * 1024)?;
                options.materials = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid material mapping: {e}"))?;
            }
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    if command == "convert" && output.is_none() {
        return Err("convert requires --output PATH; existing paths are never overwritten".into());
    }
    if let Some(out) = &output {
        if out.exists() {
            return Err(format!("output already exists: {}", out.display()));
        }
        if report_path.is_none() {
            report_path = Some(out.with_extension("import.json"));
        }
    }
    if let Some(report) = &report_path {
        if report.exists() {
            return Err(format!("report already exists: {}", report.display()));
        }
    }
    if output.is_some() && output == report_path {
        return Err("world and report paths must differ".into());
    }
    let map = read_ogz(&read_bounded(&input, MAX_COMPRESSED_BYTES)?)?;
    let cfg_path = cfg.unwrap_or_else(|| input.with_extension("cfg"));
    let cfg_report = if cfg_path.exists() {
        let bytes = read_bounded(&cfg_path, 1024 * 1024)?;
        Some(inspect_cfg(&String::from_utf8_lossy(&bytes)))
    } else {
        None
    };
    if spawn_center {
        let spawn = map
            .entities
            .iter()
            .find(|e| e.kind == 3)
            .ok_or("map has no playerstart; provide --center X,Y,Z")?;
        // Quantize only the camera/region center, never geometry. The source
        // resolution at the default scale is one Cube unit = one fine unit.
        let step = f64::from(options.units_per_metre) / 8.0;
        options.center = Some(spawn.position.map(|v| (f64::from(v) / step).round() * step));
    }
    let (report, world) = if command == "convert" {
        let (world, report) = convert(&map, &options)?;
        eprintln!("Imported {} coarse cells, {} detailed roots, {} lights, {} spawns. Cropped {} leaves; clipped {}. Unsupported omissions: {}.",report.imported_coarse_cells,report.imported_detailed_roots,report.imported_lights,report.imported_spawns,report.leaves_outside_region,report.leaves_clipped_at_region,report.unsupported_leaves_omitted.len());
        (
            serde_json::json!({"input":input,"geometry":report,"cfg":cfg_report}),
            Some(world),
        )
    } else {
        (
            serde_json::json!({"input":input,"inventory":map.inventory(),"cfg":cfg_report}),
            None,
        )
    };
    let world_bytes = world
        .map(|world| serde_json::to_vec(&world))
        .transpose()
        .map_err(|e| e.to_string())?;
    if world_bytes
        .as_ref()
        .is_some_and(|bytes| bytes.len() > 256 * 1024 * 1024)
    {
        return Err("world JSON exceeds server 256 MiB save-file limit".into());
    }
    let report_bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    if let Some(path) = report_path {
        write_new(&path, &report_bytes)?;
        eprintln!("Report: {}", path.display());
    } else {
        println!("{}", String::from_utf8_lossy(&report_bytes));
    }
    if let Some(bytes) = world_bytes {
        let path = output.unwrap();
        write_new(&path, &bytes)?;
        eprintln!("World: {}", path.display());
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
    println!("Sauerteig legacy map importer\n\n  sauerteig-import inspect MAP.ogz [--report REPORT.json] [--cfg MAP.cfg]\n  sauerteig-import convert MAP.ogz --output WORLD.json [options]\n\nOptions:\n  --units-per-metre N     Power of two, default 8 (never automatic shrinking)\n  --center X,Y,Z|spawn    Legacy Z-up point placed at [0,24,0] in Sauerteig\n  --crop                 Explicitly retain only the 128×48×128m region\n  --skip-unsupported     Explicitly omit unsupported degenerate cube shapes\n  --material-map FILE    JSON object mapping legacy slot IDs to material IDs\n  --report FILE          Detailed reference/loss report; default WORLD.import.json\n  --cfg FILE             Inspect literal references only; never execute scripts\n\nOnly OCTA versions 29–33 are supported. Originals and existing outputs are never overwritten.\nWithout --crop, maps that do not fit are rejected. Missing textures use neutral material 0.\nModels, gameplay entities, volumes and baked rendering need separate adapters.\nAfter import, start a separate server with --world WORLD.json to view the result.");
}
