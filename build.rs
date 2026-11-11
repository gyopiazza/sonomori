fn main() {
    println!("cargo:rerun-if-changed=src/ui/ui.rs");
    println!("cargo:rustc-check-cfg=cfg(ui_rune)");
    println!("cargo:rustc-check-cfg=cfg(ui_egui)");
    println!("cargo:rustc-check-cfg=cfg(ui_iced)");

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
        let picked: Vec<&str> = [
            ("Ui::Rune", "ui_rune"),
            ("Ui::Egui", "ui_egui"),
            ("Ui::Iced", "ui_iced"),
        ]
        .into_iter()
        .filter(|(needle, _)| rest.contains(needle))
        .map(|(_, cfg)| cfg)
        .collect();
        if picked.len() != 1 {
            panic!(
                "src/ui/ui.rs must set BACKEND to exactly one of Ui::Rune, Ui::Egui, or Ui::Iced"
            );
        }
        if choice.is_some() {
            panic!("src/ui/ui.rs must set BACKEND on exactly one line");
        }
        choice = Some(picked[0]);
    }
    let Some(cfg) = choice else {
        panic!("src/ui/ui.rs must set BACKEND to Ui::Rune, Ui::Egui, or Ui::Iced");
    };
    println!("cargo:rustc-cfg={cfg}");
}
