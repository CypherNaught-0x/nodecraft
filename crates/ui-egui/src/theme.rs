//! Theme tokens: every colour, radius and spacing the shell paints with, in one place.
//!
//! The values and their rationale are documented in `docs/ui-design.md`; change both together.

use egui::{Color32, CornerRadius, Stroke};

/// Which palette the shell paints with. Dark is the default (the reference look).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

/// Design tokens. Sizes are in points at 100% canvas zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    /// Infinite canvas background.
    pub canvas_bg: Color32,
    /// Grid dots on the canvas.
    pub grid_dot: Color32,
    /// Floating panels and menus over the canvas (rail, toolbars, zoom badge).
    pub panel_fill: Color32,
    pub panel_stroke: Color32,
    /// Node card body.
    pub card_fill: Color32,
    pub card_stroke: Color32,
    pub card_stroke_hover: Color32,
    pub card_stroke_selected: Color32,
    /// Text inside cards and panels.
    pub text: Color32,
    /// Secondary text: the uppercase label above a card, hints.
    pub text_muted: Color32,
    /// The `+` output handle on a card's edge.
    pub handle_fill: Color32,
    pub handle_glyph: Color32,
    /// Accent for focus and selection outside cards.
    pub accent: Color32,
    /// Translucent fill of the box-select rectangle (outlined with `accent`).
    pub marquee_fill: Color32,

    pub navigation_width: f32,
    pub minimap_height: f32,
    pub navigation_toolbar_height: f32,
    pub navigation_margin: f32,
    pub navigation_gap: f32,
    pub fit_padding: f32,
    pub minimap_node_min: f32,

    /// Distance between grid dots, at 100% zoom.
    pub grid_spacing: f32,
    /// Grid dot radius, in screen points (dots do not scale with zoom).
    pub grid_dot_radius: f32,
    pub card_radius: u8,
    pub panel_radius: u8,
    /// Inner padding of a card.
    pub card_padding: f32,
    /// Gap between the label and the top of its card.
    pub label_gap: f32,
    pub label_size: f32,
    pub body_size: f32,
    pub handle_radius: f32,
    pub card_width: f32,
    pub card_min_height: f32,
    pub card_max_height: f32,
    pub control_row_height: f32,
    pub control_gap: f32,
    pub title_min_screen_px: f32,
    pub handle_min_screen_radius: f32,
    pub handle_stroke: f32,
    pub preview_chars: usize,
    pub text_rows: usize,
}

pub const DARK: Tokens = Tokens {
    canvas_bg: Color32::from_rgb(0x08, 0x08, 0x09),
    grid_dot: Color32::from_rgb(0x2a, 0x2a, 0x2e),
    panel_fill: Color32::from_rgb(0x17, 0x17, 0x19),
    panel_stroke: Color32::from_rgb(0x2c, 0x2c, 0x30),
    card_fill: Color32::from_rgb(0x12, 0x12, 0x14),
    card_stroke: Color32::from_rgb(0x34, 0x34, 0x39),
    card_stroke_hover: Color32::from_rgb(0x5a, 0x5a, 0x61),
    card_stroke_selected: Color32::from_rgb(0xe6, 0xe6, 0xea),
    text: Color32::from_rgb(0xe8, 0xe8, 0xec),
    text_muted: Color32::from_rgb(0x8a, 0x8a, 0x93),
    handle_fill: Color32::from_rgb(0x08, 0x08, 0x09),
    handle_glyph: Color32::from_rgb(0xe8, 0xe8, 0xec),
    accent: Color32::from_rgb(0x4c, 0xc3, 0x8a),
    marquee_fill: Color32::from_rgba_unmultiplied_const(0x4c, 0xc3, 0x8a, 0x24),
    navigation_width: 320.0,
    minimap_height: 160.0,
    navigation_toolbar_height: 30.0,
    navigation_margin: 16.0,
    navigation_gap: 6.0,
    fit_padding: 24.0,
    minimap_node_min: 2.0,
    grid_spacing: 20.0,
    grid_dot_radius: 1.0,
    card_radius: 14,
    panel_radius: 12,
    card_padding: 16.0,
    label_gap: 6.0,
    label_size: 10.5,
    body_size: 13.0,
    handle_radius: 9.0,
    card_width: 320.0,
    card_min_height: 220.0,
    card_max_height: 720.0,
    control_row_height: 64.0,
    control_gap: 6.0,
    title_min_screen_px: 9.0,
    handle_min_screen_radius: 5.0,
    handle_stroke: 1.5,
    preview_chars: 256,
    text_rows: 6,
};

pub const LIGHT: Tokens = Tokens {
    canvas_bg: Color32::from_rgb(0xf4, 0xf4, 0xf5),
    grid_dot: Color32::from_rgb(0xcf, 0xcf, 0xd4),
    panel_fill: Color32::from_rgb(0xff, 0xff, 0xff),
    panel_stroke: Color32::from_rgb(0xdd, 0xdd, 0xe1),
    card_fill: Color32::from_rgb(0xff, 0xff, 0xff),
    card_stroke: Color32::from_rgb(0xd4, 0xd4, 0xd9),
    card_stroke_hover: Color32::from_rgb(0xa0, 0xa0, 0xa8),
    card_stroke_selected: Color32::from_rgb(0x18, 0x18, 0x1b),
    text: Color32::from_rgb(0x18, 0x18, 0x1b),
    text_muted: Color32::from_rgb(0x6b, 0x6b, 0x74),
    handle_fill: Color32::from_rgb(0xff, 0xff, 0xff),
    handle_glyph: Color32::from_rgb(0x18, 0x18, 0x1b),
    accent: Color32::from_rgb(0x1f, 0x9d, 0x63),
    marquee_fill: Color32::from_rgba_unmultiplied_const(0x1f, 0x9d, 0x63, 0x1c),
    ..DARK
};

impl ThemeKind {
    pub fn tokens(self) -> &'static Tokens {
        match self {
            ThemeKind::Dark => &DARK,
            ThemeKind::Light => &LIGHT,
        }
    }
}

/// egui visuals for the shell's own widgets (menus, buttons, tooltips), built from the tokens.
pub fn visuals(kind: ThemeKind) -> egui::Visuals {
    let t = kind.tokens();
    let mut v = match kind {
        ThemeKind::Dark => egui::Visuals::dark(),
        ThemeKind::Light => egui::Visuals::light(),
    };
    v.panel_fill = t.canvas_bg;
    v.window_fill = t.panel_fill;
    v.window_stroke = Stroke::new(1.0, t.panel_stroke);
    v.window_corner_radius = CornerRadius::same(t.panel_radius);
    v.menu_corner_radius = CornerRadius::same(t.panel_radius);
    v.extreme_bg_color = t.canvas_bg;
    v.selection.stroke = Stroke::new(1.0, t.accent);
    let hover = t.panel_stroke;
    craft_ui::theme::WidgetPalette { noninteractive: t.panel_fill, inactive: t.panel_fill, hovered: hover, active: hover, open: hover }.apply_to(
        &mut v.widgets,
        CornerRadius::same(8),
        Stroke::new(1.0, t.text),
    );
    v
}

/// Install the theme on a context. Call once at startup and on theme change.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    ctx.set_visuals(visuals(kind));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance contrast ratio.
    fn contrast(a: Color32, b: Color32) -> f32 {
        fn lum(c: Color32) -> f32 {
            let ch = |v: u8| {
                let s = f32::from(v) / 255.0;
                if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        }
        let (hi, lo) = {
            let (x, y) = (lum(a), lum(b));
            if x > y { (x, y) } else { (y, x) }
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn text_is_readable_on_cards_and_canvas() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let t = kind.tokens();
            assert!(contrast(t.text, t.card_fill) >= 7.0, "{kind:?} body text");
            assert!(contrast(t.text_muted, t.canvas_bg) >= 4.5, "{kind:?} card label");
            assert!(contrast(t.handle_glyph, t.handle_fill) >= 7.0, "{kind:?} handle");
        }
    }

    #[test]
    fn grid_and_card_edges_are_visible_but_quiet() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let t = kind.tokens();
            let dots = contrast(t.grid_dot, t.canvas_bg);
            assert!((1.2..3.0).contains(&dots), "{kind:?} grid contrast {dots}");
            assert!(contrast(t.card_stroke_selected, t.canvas_bg) > contrast(t.card_stroke, t.canvas_bg), "{kind:?}");
        }
    }
}
