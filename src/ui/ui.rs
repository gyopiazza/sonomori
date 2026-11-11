//! Which editor implementation this build links.
//!
//! Change `BACKEND` and rebuild to link the other implementation. The
//! unselected implementation stays in the tree and is left out of the plugin,
//! so the running editor has one paint path and one GPU stack.

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ui {
    Rune,
    Egui,
}

#[allow(dead_code)]
pub const BACKEND: Ui = Ui::Egui;
