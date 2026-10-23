use super::*;
use crate::audio::{AudioIntent, AudioView};
use egui_kittest::{Harness, kittest::Queryable};

#[test]
fn a_waveform_drag_previews_and_commits_only_on_release_in_both_themes() {
    for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = Harness::builder().with_size(Vec2::new(500.0, 260.0)).build_ui_state(
            move |ui, intents: &mut Vec<Intent>| {
                let view =
                    AudioView::Sound { peaks: Some(std::sync::Arc::from([80u8; 64])), position_us: 0, duration_us: 10_000_000, playing: true, notice: None };
                show_audio(ui, Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(400.0, 200.0)), &view, intents, kind.tokens());
            },
            Vec::<Intent>::new(),
        );
        crate::theme::apply(&h.ctx, kind);
        h.run();
        let wave = h.get_by_label("Sound, 0:10").rect();
        let start = Pos2::new(wave.left() + 20.0, wave.center().y);
        let end = Pos2::new(wave.right() - 20.0, wave.center().y);
        h.hover_at(start);
        h.step();
        h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.state_mut().clear();
        for pos in [wave.center(), end] {
            h.hover_at(pos);
            h.step();
        }
        assert!(h.state().iter().any(|intent| matches!(intent, Intent::Audio(AudioIntent::PreviewSeek(_)))));
        assert!(!h.state().iter().any(|intent| matches!(intent, Intent::Audio(AudioIntent::Seek(_) | AudioIntent::FinishSeek))));
        h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.step();
        assert!(h.state().iter().any(|intent| matches!(intent, Intent::Audio(AudioIntent::FinishSeek))));
    }
}
