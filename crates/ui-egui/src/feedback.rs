//! Inline feedback that belongs to one control (#1110), as opposed to the notice stack.
//! Colour is never the only cue: an error carries the error icon beside its danger-coloured text.
use crate::{icons::Icon, theme::Tokens};

/// An error that belongs to a control (a card's parameter, a field): the error icon and the
/// message in `danger`, wrapping within the space the control has. Returns the message's label.
pub fn inline_error(ui: &mut egui::Ui, t: &Tokens, text: &str) -> egui::Response {
    ui.horizontal_top(|ui| {
        ui.add(Icon::Error.image(ui.ctx(), t.icon_size, t).tint(t.danger));
        ui.add(egui::Label::new(egui::RichText::new(text).color(t.danger)).wrap())
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn an_inline_error_shows_the_error_icon_beside_its_text() {
        let mut h = Harness::builder().with_size(egui::Vec2::new(300.0, 120.0)).build_ui(|ui| {
            inline_error(ui, &theme::DARK, "Width must be at least 64.");
        });
        h.run();
        let text = h.get_by_label("Width must be at least 64.").rect();
        let icon = h.get_by_label("Error").rect();
        assert!(icon.right() <= text.left(), "the icon leads the message");
    }
}
