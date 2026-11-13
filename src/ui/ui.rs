//! Which editor implementation this build links.
//!
//! Change `BACKEND` and rebuild. The unselected implementations stay in the
//! tree and are left out of the plugin, so the running editor has one paint
//! path and one GPU stack.
//!
//! A new backend needs a variant here, a line in the table in `build.rs`, and
//! a `#[cfg]` module in `src/ui/mod.rs`.

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ui {
    Iced,
}

#[allow(dead_code)]
pub const BACKEND: Ui = Ui::Iced;
