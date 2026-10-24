use sauerteig_import::ImportOptions;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct Arguments {
    pub input: PathBuf,
    pub report: Option<PathBuf>,
    pub cfg: Option<PathBuf>,
    pub command: Command,
}

#[derive(Debug)]
pub(crate) enum Command {
    Inspect,
    Convert {
        output: PathBuf,
        options: ImportOptions,
        center: Option<Center>,
        material_map: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Center {
    Coordinates([f64; 3]),
    Spawn,
}

/// Parse arguments without reading maps or mapping files. None requests help.
pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Arguments>, String> {
    let Some(command) = args.next() else {
        return Ok(None);
    };
    if command == "--help" || command == "-h" {
        return Ok(None);
    }
    if command != "inspect" && command != "convert" {
        return Err("first argument must be inspect or convert (see --help)".into());
    }
    let input = PathBuf::from(args.next().ok_or("missing input .ogz path")?);
    let mut options = ImportOptions::default();
    let mut output = None;
    let mut report = None;
    let mut cfg = None;
    let mut center = None;
    let mut material_map = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--report" => report = Some(PathBuf::from(value(&mut args, &arg)?)),
            "--cfg" => cfg = Some(PathBuf::from(value(&mut args, &arg)?)),
            "--output" | "--units-per-metre" | "--units-per-meter" | "--crop"
            | "--skip-unsupported" | "--center" | "--material-map" => {
                if command != "convert" {
                    return Err(format!("{arg} is only valid with convert"));
                }
                match arg.as_str() {
                    "--output" => output = Some(PathBuf::from(value(&mut args, &arg)?)),
                    "--units-per-metre" | "--units-per-meter" => {
                        options.units_per_metre = value(&mut args, &arg)?
                            .parse()
                            .map_err(|_| "invalid scale")?;
                    }
                    "--crop" => options.crop = true,
                    "--skip-unsupported" => options.skip_unsupported = true,
                    "--center" => center = Some(parse_center(&value(&mut args, &arg)?)?),
                    "--material-map" => {
                        material_map = Some(PathBuf::from(value(&mut args, &arg)?));
                    }
                    _ => unreachable!(),
                }
            }
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    let command = if command == "convert" {
        let output =
            output.ok_or("convert requires --output PATH; existing paths are never overwritten")?;
        let report_path = report.get_or_insert_with(|| output.with_extension("import.json"));
        if output == *report_path {
            return Err("world and report paths must differ".into());
        }
        Command::Convert {
            output,
            options,
            center,
            material_map,
        }
    } else {
        Command::Inspect
    };
    Ok(Some(Arguments {
        input,
        report,
        cfg,
        command,
    }))
}

fn value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("missing {option} value"))
}

fn parse_center(value: &str) -> Result<Center, String> {
    if value == "spawn" {
        return Ok(Center::Spawn);
    }
    let values = value
        .split(',')
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "center must be X,Y,Z in original Cube coordinates")?;
    let coordinates: [f64; 3] = values
        .try_into()
        .map_err(|_| "center needs three coordinates")?;
    if !coordinates.iter().all(|value| value.is_finite()) {
        return Err("center coordinates must be finite".into());
    }
    Ok(Center::Coordinates(coordinates))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(args: &[&str]) -> Result<Arguments, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))?.ok_or_else(|| "unexpected help".into())
    }

    #[test]
    fn repeated_center_uses_the_last_selection_in_either_order() {
        for (first, last, expected) in [
            ("spawn", "1,2,3", Center::Coordinates([1.0, 2.0, 3.0])),
            ("1,2,3", "spawn", Center::Spawn),
        ] {
            let args = arguments(&[
                "convert",
                "map.ogz",
                "--output",
                "world.json",
                "--center",
                first,
                "--center",
                last,
            ])
            .unwrap();
            let Command::Convert { center, .. } = args.command else {
                panic!("expected conversion");
            };
            assert_eq!(center, Some(expected));
        }
    }

    #[test]
    fn rejects_ignored_conversion_options_during_inspection() {
        for option in ["--output", "--crop", "--center", "--material-map"] {
            assert!(arguments(&["inspect", "map.ogz", option, "value"])
                .unwrap_err()
                .contains("only valid with convert"));
        }
    }

    #[test]
    fn validates_output_paths_before_any_file_is_read() {
        assert!(arguments(&["convert", "map.ogz"])
            .unwrap_err()
            .contains("requires --output"));
        assert!(arguments(&[
            "convert",
            "map.ogz",
            "--output",
            "world.json",
            "--report",
            "world.json",
        ])
        .unwrap_err()
        .contains("must differ"));
        let args = arguments(&["convert", "map.ogz", "--output", "world.json"]).unwrap();
        assert_eq!(args.report, Some(PathBuf::from("world.import.json")));
    }

    #[test]
    fn malformed_centers_are_rejected() {
        for center in ["1,2", "1,2,3,4", "1,2,no", "NaN,0,0", "inf,0,0"] {
            assert!(arguments(&[
                "convert",
                "map.ogz",
                "--output",
                "world.json",
                "--center",
                center,
            ])
            .is_err());
        }
    }
}
