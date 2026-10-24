//! CPU-only sparse subdivision/paste sanity check; not a graphics benchmark.
use sauerteig_core::*;
use std::{hint::black_box, time::Instant};
fn measure(label: &str, mut run: impl FnMut()) {
    for _ in 0..4 {
        run();
    }
    let mut values = Vec::new();
    for _ in 0..40 {
        let start = Instant::now();
        run();
        values.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    values.sort_by(f64::total_cmp);
    println!("{label}: p50 {:.3}ms / p95 {:.3}ms", values[19], values[37]);
}
fn main() {
    let original = World::arena();
    let pos = [0, 0, 8];
    let source = CellFragment::from_cell(pos, original.get(pos).unwrap());
    let mut cut = source.bounds;
    cut.min[0] += 1;
    cut.max[1] -= 1;
    cut.min[2] += 1;
    let carved = RootEdit {
        pos,
        fragments: source
            .bounds
            .subtract(cut)
            .into_iter()
            .map(|bounds| CellFragment {
                bounds,
                ..source.clone()
            })
            .collect(),
    };
    let restore = RootEdit {
        pos,
        fragments: vec![source],
    };
    let mut world = original.clone();
    let mut add = true;
    measure("Fine carve/restore + chunk meshing", || {
        let dirty = world
            .apply_root_edits(std::slice::from_ref(if add { &carved } else { &restore }))
            .unwrap();
        for key in dirty {
            black_box(world.mesh_chunk(key));
        }
        add = !add;
    });
    let roots: Vec<_> = (-32..32)
        .flat_map(|x| {
            (-32..32).map(move |z| {
                let pos = [x, 20, z];
                let mut fragment = CellFragment::from_cell(pos, Cell::solid(1));
                fragment.bounds.max[1] = fragment.bounds.min[1] + 1;
                RootEdit {
                    pos,
                    fragments: vec![fragment],
                }
            })
        })
        .collect();
    let clear: Vec<_> = roots
        .iter()
        .map(|r| RootEdit {
            pos: r.pos,
            fragments: vec![],
        })
        .collect();
    let mut add = true;
    measure("4096-root thin floor paste + meshes", || {
        let dirty = world
            .apply_root_edits(if add { &roots } else { &clear })
            .unwrap();
        for key in dirty {
            black_box(world.mesh_chunk(key));
        }
        add = !add;
    });
    world.apply_root_edits(&roots).unwrap();
    measure("1000 rays through detailed floor", || {
        for i in 0..1000 {
            black_box(world.raycast(Vec3::new((i % 60) as f32 - 30.5, 25.0, 0.5), -Vec3::Y, 30.0));
        }
    });
}
