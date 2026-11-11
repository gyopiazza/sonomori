//! The screen and the plugin must not name a UI library. Backend files live
//! under this folder and are skipped here, except `face`, which is the
//! vocabulary the screen is written against.

#[test]
fn product_code_does_not_name_a_ui_library() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let ui = src.join("ui");
    let mut hits = Vec::new();
    visit(&src, &ui, &mut hits);
    assert!(
        hits.is_empty(),
        "only the editor backends under src/ui may name a UI library, found {hits:?}"
    );
}

fn visit(dir: &std::path::Path, ui: &std::path::Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit(&path, ui, hits);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        // Only the vocabulary file is scanned. A backend may also be named
        // `face.rs`, and that file is allowed to name its library.
        if path.starts_with(ui) && path != ui.join("face.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if text.contains("egui") || text.contains("rune") {
            hits.push(path.display().to_string());
        }
    }
}
