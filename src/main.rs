use nice_plug::prelude::*;

fn main() {
    if !nice_export_standalone::<sonomori::Sonomori>() {
        std::process::exit(1);
    }
}
