fn main() {
    println!("cargo:rerun-if-changed=src/ui/ui.rs");
    println!("cargo:rustc-check-cfg=cfg(ui_rune)");
    println!("cargo:rustc-check-cfg=cfg(ui_egui)");

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
        let rune = rest.contains("Ui::Rune");
        let egui = rest.contains("Ui::Egui");
        if rune == egui {
            panic!("src/ui/ui.rs must set BACKEND to exactly one of Ui::Rune or Ui::Egui");
        }
        if choice.is_some() {
            panic!("src/ui/ui.rs must set BACKEND on exactly one line");
        }
        choice = Some(if rune { "ui_rune" } else { "ui_egui" });
    }
    let Some(cfg) = choice else {
        panic!("src/ui/ui.rs must set BACKEND to Ui::Rune or Ui::Egui");
    };
    println!("cargo:rustc-cfg={cfg}");
}
