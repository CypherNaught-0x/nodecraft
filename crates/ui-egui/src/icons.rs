//! NodeCraft's finite Tabler outline catalog (#955). Assets and license: `assets/icons/tabler`.
//! Craft prepares embedded SVGs once; egui's SVG loader rasterizes at the requested pixel size.
//! The white intermediate is an alpha mask. Visible colours always come from theme tokens.
use crate::theme::{self, Tokens};
use craft_ui::icons::{SvgIconSet, SvgStyle};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Search,
    Comments,
    Key,
    Wallet,
    Open,
    Save,
}

const ASSETS: &[(&str, &[u8])] = &[
    ("search", include_bytes!("../../../assets/icons/tabler/search.svg")),
    ("message-circle", include_bytes!("../../../assets/icons/tabler/message-circle.svg")),
    ("key", include_bytes!("../../../assets/icons/tabler/key.svg")),
    ("wallet", include_bytes!("../../../assets/icons/tabler/wallet.svg")),
    ("folder-open", include_bytes!("../../../assets/icons/tabler/folder-open.svg")),
    ("device-floppy", include_bytes!("../../../assets/icons/tabler/device-floppy.svg")),
];

fn catalog() -> &'static SvgIconSet {
    static SET: OnceLock<SvgIconSet> = OnceLock::new();
    SET.get_or_init(|| {
        SvgIconSet::new(
            ASSETS,
            // Both palettes share the stroke. Only the draw-time colour varies by theme/state.
            SvgStyle { current_color: "white", stroke_width: Some(theme::DARK.icon_stroke) },
            "bytes://nodecraft/control-icons/v1/",
            "search",
        )
    })
}

impl Icon {
    pub const ALL: [Self; 6] = [Self::Search, Self::Comments, Self::Key, Self::Wallet, Self::Open, Self::Save];

    pub fn name(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Comments => "message-circle",
            Self::Key => "key",
            Self::Wallet => "wallet",
            Self::Open => "folder-open",
            Self::Save => "device-floppy",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Search => "Search",
            Self::Comments => "Comments",
            Self::Key => "API keys",
            Self::Wallet => "Budgets",
            Self::Open => "Open",
            Self::Save => "Save",
        }
    }

    /// Install only the SVG loader (idempotently), then use this icon at a token-sized extent.
    /// `size` is in screen points: egui requests a new SVG raster when DPI or UI zoom changes.
    pub fn image(self, ctx: &egui::Context, size: f32, t: &Tokens) -> egui::Image<'static> {
        egui_extras::install_image_loaders(ctx);
        egui::Image::new(catalog().source(self.name())).fit_to_exact_size(egui::Vec2::splat(size)).tint(t.text).alt_text(self.label())
    }

    /// A labelled control keeps the same text/keyboard semantics as an ordinary egui button.
    pub fn button(self, ui: &egui::Ui, label: impl Into<egui::WidgetText>, t: &Tokens) -> egui::Button<'static> {
        egui::Button::image_and_text(self.image(ui.ctx(), t.icon_size, t), label)
    }

    /// Icon-only controls always expose an action label and tooltip, including when the caller
    /// supplies an empty label. Ordinary panels should prefer the labelled button above.
    pub fn icon_button(self, ui: &mut egui::Ui, label: &str, t: &Tokens) -> egui::Response {
        let label = if label.trim().is_empty() { self.label() } else { label };
        let response = ui.add(egui::Button::image(self.image(ui.ctx(), t.icon_size, t)));
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
        response.on_hover_text(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{
        Vec2,
        load::{ImagePoll, SizeHint},
    };
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn every_embedded_icon_renders_at_both_sizes_themes_and_pixel_densities() {
        for kind in [theme::ThemeKind::Dark, theme::ThemeKind::Light] {
            for density in [1.0, 2.0] {
                let t = kind.tokens();
                let mut h = Harness::builder().with_size(Vec2::new(500.0, 180.0)).with_pixels_per_point(density).build_ui(move |ui| {
                    theme::apply(ui.ctx(), kind);
                    for size in [t.icon_size, t.icon_large_size] {
                        ui.horizontal(|ui| {
                            for icon in Icon::ALL {
                                ui.vertical(|ui| {
                                    ui.add(icon.image(ui.ctx(), size, t));
                                    ui.label(icon.label());
                                });
                            }
                        });
                    }
                });
                h.run();
                let rendered = h.render().unwrap();
                assert_eq!(rendered.width(), (500.0 * density) as u32);
                for icon in Icon::ALL {
                    assert!(catalog().contains(icon.name()), "no fallback for {icon:?}");
                    for size in [t.icon_size, t.icon_large_size] {
                        let pixels = (size * density) as u32;
                        let source = catalog().source(icon.name());
                        let egui::ImageSource::Bytes { uri, bytes } = source else { panic!("embedded icon has no bytes") };
                        h.ctx.include_bytes(uri.clone(), bytes);
                        let image = h.ctx.try_load_image(&uri, SizeHint::Size { width: pixels, height: pixels, maintain_aspect_ratio: true }).unwrap();
                        let ImagePoll::Ready { image } = image else { panic!("embedded SVG was not ready") };
                        assert_eq!(image.size, [pixels as usize, pixels as usize]);
                        assert!(image.pixels.iter().any(|pixel| pixel.a() > 0), "{icon:?} is visible at {pixels}px");
                    }
                }
                if let Some(root) = std::env::var_os("NODECRAFT_ICONS955_RENDER_ROOT") {
                    rendered.save(std::path::Path::new(&root).join(format!("icons-{kind:?}-{density}.png"))).unwrap();
                }
            }
        }
    }

    #[test]
    fn icon_only_controls_keep_action_labels_and_clicks() {
        let mut h = Harness::builder().build_ui_state(
            |ui, clicked: &mut bool| {
                if Icon::Search.icon_button(ui, "Search canvas", &theme::DARK).clicked() {
                    *clicked = true;
                }
                Icon::Save.icon_button(ui, "", &theme::DARK);
            },
            false,
        );
        h.get_by_label("Search canvas").click();
        h.run();
        assert!(*h.state());
        h.get_by_label("Save");
    }

    #[test]
    #[ignore = "manual local screenshot capture; requires NODECRAFT_ICONS955_APP_RENDER_ROOT"]
    fn render_icon_controls_in_the_app() {
        let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_ICONS955_APP_RENDER_ROOT").unwrap());
        let mut h = Harness::builder()
            .with_size(Vec2::new(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut crate::NodeCraftApp| app.show(ui), crate::NodeCraftApp::headless().unwrap());
        h.run();
        h.render().unwrap().save(root.join("canvas.png")).unwrap();
        h.get_by_label("File").click();
        h.run();
        h.render().unwrap().save(root.join("files.png")).unwrap();
    }
}
