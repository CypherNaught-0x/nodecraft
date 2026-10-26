//! The window: the egui app, with the agent sidebar beside it and, with `--control`, the control
//! channel answering against the same session.
use crate::agent_panel::AgentPanel;
use crate::control::Channel;
use nodecraft_ui_egui::NodeCraftApp;

pub struct Shell {
    pub app: NodeCraftApp,
    pub agent: AgentPanel,
    pub channel: Option<Channel>,
    pub storage_window: Option<crate::storage_window::StorageWindow>,
}

impl Shell {
    /// One frame of the window: the sidebar, then the canvas in what it leaves.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let intents = self.agent.show(ui, &self.app);
        let rest = ui.available_rect_before_wrap();
        // The canvas's cards draw on a layer above the sidebar's and clip only to their parent, so
        // the canvas is clipped to its own rect, not just laid out in it.
        ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
            ui.shrink_clip_rect(rest);
            self.app.show(ui);
        });
        self.agent.apply(intents, &mut self.app, ui.ctx());
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(window) = &mut self.storage_window {
            window.pump(&mut self.app, ctx);
        }
        if let Some(channel) = &mut self.channel {
            channel.pump(&mut self.app, ctx);
        }
        // Before the app, so the sidebar's shortcuts aren't taken as the canvas's.
        self.agent.logic(&mut self.app, ctx);
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self) {
        self.app.on_exit();
        if let Some(channel) = &self.channel {
            channel.close();
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}
