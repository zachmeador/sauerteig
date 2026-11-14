//! Compares a re-imported world with an older copy of the same map, which was
//! imported before the importer fixed its handedness and so is the new
//! import's mirror image across X = 0. Both files load as the server loads
//! them; the old tree is reflected onto the new one's root and the two
//! digests, and the objects, are compared.
//!
//! usage: compare_reflected NEW.json OLD.json
use sauerteig_core::{ObjectKind, Octree};
use sauerteig_import::{reflect::reflect_x, ImportedWorld};
use std::process::ExitCode;

fn load(path: &str) -> Result<(ImportedWorld, Octree), Box<dyn std::error::Error>> {
    let world: ImportedWorld = serde_json::from_slice(&std::fs::read(path)?)?;
    let tree = Octree::from_parts(
        world.bounds,
        &world.regions,
        &world.cells,
        &world.roots,
        &world.volumes,
    )?;
    Ok((world, tree))
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(new_path), Some(old_path)) = (args.next(), args.next()) else {
        return Err("usage: compare_reflected NEW.json OLD.json".into());
    };
    let (new_world, new_tree) = load(&new_path)?;
    let (old_world, old_tree) = load(&old_path)?;
    let reflected = reflect_x(&old_tree, new_tree.root_box())?;
    let (a, b) = (new_tree.stats(), reflected.stats());
    let same_tree = new_tree.digest() == reflected.digest();
    if !same_tree {
        let (mut left, mut right) = (Vec::new(), Vec::new());
        new_tree.for_each_leaf(|at, leaf| left.push((at, leaf)));
        reflected.for_each_leaf(|at, leaf| right.push((at, leaf)));
        let mut differing = 0;
        for (new, old) in left.iter().zip(&right) {
            if new != old {
                differing += 1;
                if differing <= 5 {
                    println!("new {new:?}\nold {old:?}");
                }
            }
        }
        println!("{differing} of {} leaves differ", left.len());
    }
    let key = |kind: ObjectKind| format!("{kind:?}");
    let mut new_objects: Vec<_> = new_world
        .objects
        .iter()
        .map(|o| (o.id, key(o.kind), o.position.to_array().map(f32::to_bits)))
        .collect();
    let mut old_objects: Vec<_> = old_world
        .objects
        .iter()
        .map(|o| {
            let mut p = o.position.to_array();
            p[0] = -p[0];
            (o.id, key(o.kind), p.map(f32::to_bits))
        })
        .collect();
    new_objects.sort();
    old_objects.sort();
    let same_objects = new_objects == old_objects;
    println!(
        "{} digest {} (leaves {} vs {}), objects {} ({} vs {}), bounds {}",
        new_path,
        if same_tree { "equal" } else { "DIFFERENT" },
        a.leaves(),
        b.leaves(),
        if same_objects { "equal" } else { "DIFFERENT" },
        new_objects.len(),
        old_objects.len(),
        if new_world.bounds == new_tree.bounds() {
            "kept"
        } else {
            "changed"
        },
    );
    Ok(same_tree && same_objects)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
