//! Measures what distinguishes the built-in movement profiles at the fixed
//! 60 Hz tick, next to a literal port of Sauerbraten's velocity math run at the
//! engine's own 5 ms frames. Run with:
//! `cargo run -p sauerteig-core --example movement_profiles`

#[path = "../tests/oracle/mod.rs"]
mod oracle;

use oracle::{measure_oracle, measure_profile, Metrics};
use sauerteig_core::MovementProfile;

fn main() {
    let arena = measure_profile(&MovementProfile::arena()).values();
    let sauerbraten = measure_profile(&MovementProfile::sauerbraten()).values();
    let reference = measure_oracle().values();
    println!(
        "{:<30}{:>10}{:>14}{:>20}{:>11}",
        "", "Arena", "Sauerbraten", "Sauerbraten oracle", "vs oracle"
    );
    for (index, name) in Metrics::NAMES.iter().enumerate() {
        let difference = (10_000.0 * (sauerbraten[index] / reference[index] - 1.0)).round() / 100.0;
        println!(
            "{name:<30}{:>10.3}{:>14.3}{:>20.3}{:>+10.2}%",
            arena[index],
            sauerbraten[index],
            reference[index],
            difference + 0.0
        );
    }
}
