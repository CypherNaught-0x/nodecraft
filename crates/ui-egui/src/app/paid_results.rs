//! Keep the runtime alive while paid bytes are being sealed or saved. Never block a frame.
use super::NodeCraftApp;
use std::time::Duration;

fn close_request(ctx: &egui::Context, busy: bool) -> bool {
    if ctx.viewport_id() != egui::ViewportId::ROOT {
        return false;
    }
    let id = egui::Id::new("nodecraft.paid_result_close");
    let waiting = ctx.data(|data| data.get_temp::<bool>(id)).unwrap_or(false);
    let requested = ctx.input(|input| input.viewport().close_requested());
    if busy && (requested || waiting) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.data_mut(|data| data.insert_temp(id, true));
        ctx.request_repaint_after(Duration::from_millis(50));
        return true;
    }
    if waiting {
        ctx.data_mut(|data| data.remove::<bool>(id));
        // Ordinary project/export quit guards still inspect this new request.
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    false
}
impl NodeCraftApp {
    pub(super) fn paid_result_close_request(&mut self, ctx: &egui::Context) -> bool {
        let waiting = close_request(ctx, self.runs.as_ref().is_some_and(super::runs::Runs::is_preparing_results));
        if waiting {
            self.notice = Some("Saving paid results before closing…".into());
        }
        waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn topaz37_paid_result_quit_waits_without_blocking_and_reissues_normal_close() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        let mut output = ctx.run_ui(input, |ui| assert!(close_request(ui.ctx(), true)));
        assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter().any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        output.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| assert!(close_request(ui.ctx(), true)));
        assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter().any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        output.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| assert!(!close_request(ui.ctx(), false)));
        assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter().any(|command| matches!(command, egui::ViewportCommand::Close)));
        output.textures_delta.clear();
    }
}
