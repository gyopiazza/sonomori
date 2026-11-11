//! Which editor implementation this build links.
//!
//! Change `BACKEND` and rebuild to link another implementation. The
//! unselected implementations stay in the tree and are left out of the plugin,
//! so the running editor has one paint path and one GPU stack.

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ui {
    Rune,
    Egui,
    Iced,
}

#[allow(dead_code)]
pub const BACKEND: Ui = Ui::Iced;
