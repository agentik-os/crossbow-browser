//! Settings → General → Selected Text: whether the page menu offers Copy as
//! Quote. Kept on this device only.

use super::section;
use crate::quote::QuoteHost;
use crate::ui::style::Ink;
use crate::ui::theme::Palette;
use crossbow_core::store::Store;
use egui::Ui;

pub(crate) const SECTION: &str = "Selected Text";

pub(super) fn show(quote: &mut QuoteHost, store: &Store, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, SECTION);
    super::link_page::switch(quote.switch(store), store, ui, &ink, pal);
}

#[cfg(test)]
#[path = "quote_settings_tests.rs"]
mod tests;
