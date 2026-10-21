//! The window: the egui app, with the agent sidebar beside it and, with `--control`, the control
//! channel answering against the same session.
use crate::agent_panel::AgentPanel;
use crate::control::Channel;
use nodecraft_ui_egui::NodeCraftApp;

pub struct Shell {
    pub app: NodeCraftApp,
    pub agent: AgentPanel,
    pub channel: Option<Channel>,
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(channel) = &mut self.channel {
            channel.pump(&mut self.app, ctx);
        }
        // Before the app, so the sidebar's shortcuts aren't taken as the canvas's.
        self.agent.logic(&mut self.app, ctx);
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let intents = self.agent.show(ui, &self.app);
        // The canvas takes what the sidebar leaves.
        let rest = ui.available_rect_before_wrap();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| self.app.ui(ui, frame));
        self.agent.apply(intents, &mut self.app, ui.ctx());
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
