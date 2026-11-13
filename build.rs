fn main() {
    println!("cargo:rerun-if-changed=src/ui/ui.rs");

    // One line per backend: the `Ui` variant in src/ui/ui.rs, and the cfg
    // that src/ui/mod.rs uses to compile that module.
    const BACKENDS: &[(&str, &str)] = &[("Ui::Iced", "ui_iced")];

    for (_, cfg) in BACKENDS {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }

    let src = std::fs::read_to_string("src/ui/ui.rs").expect("read src/ui/ui.rs");
    let mut choice = None;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        let Some(rest) = line.strip_prefix("pub const BACKEND") else {
            continue;
        };
        let picked: Vec<&str> = BACKENDS
            .iter()
            .filter(|(needle, _)| rest.contains(needle))
            .map(|(_, cfg)| *cfg)
            .collect();
        if picked.len() != 1 {
            let names: Vec<&str> = BACKENDS.iter().map(|(needle, _)| *needle).collect();
            panic!(
                "src/ui/ui.rs must set BACKEND to exactly one of {}",
                names.join(", ")
            );
        }
        if choice.is_some() {
            panic!("src/ui/ui.rs must set BACKEND on exactly one line");
        }
        choice = Some(picked[0]);
    }
    let Some(cfg) = choice else {
        let names: Vec<&str> = BACKENDS.iter().map(|(needle, _)| *needle).collect();
        panic!(
            "src/ui/ui.rs must set BACKEND to {}",
            names.join(", ")
        );
    };
    println!("cargo:rustc-cfg={cfg}");
}
