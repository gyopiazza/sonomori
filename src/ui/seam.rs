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
        // A backend under src/ui may name its library. `face.rs` is the
        // vocabulary, so it is scanned with the rest of the crate.
        if path.starts_with(ui) && path != ui.join("face.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if names_a_library(&text) {
            hits.push(path.display().to_string());
        }
    }
}

fn names_a_library(text: &str) -> bool {
    has_word(text, "iced")
}

/// `sliced` contains the letters of the iced crate name. Match the word.
fn has_word(text: &str, word: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let word = word.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let needle = word.as_bytes();
    let mut start = 0;
    while let Some(found) = lower[start..].find(&word) {
        let at = start + found;
        let before = at == 0 || !is_ident(bytes[at - 1]);
        let after_at = at + needle.len();
        let after = after_at >= bytes.len() || !is_ident(bytes[after_at]);
        if before && after {
            return true;
        }
        start = at + needle.len();
    }
    false
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[test]
fn iced_is_a_word_and_sliced_is_not() {
    assert!(names_a_library("calls iced here"));
    assert!(names_a_library("Iced::widget"));
    assert!(!names_a_library("sliced transient"));
    assert!(!names_a_library("the slices"));
}
