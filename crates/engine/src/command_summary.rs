//! One-line summaries of the commands a person runs without parameters (#1121): the quick-add
//! palette shows one under each command's label. `params_doc` is written for agents (shapes and
//! edge cases); a summary says in a few words what the command does for a person. Plain sentence
//! case, no full stop. A command that takes parameters needs none.
use crate::commands::CommandSpec;

impl CommandSpec {
    /// The command's one-line summary for people, if it has one.
    pub fn summary(&self) -> Option<&'static str> {
        summary(self.id)
    }
}

/// The one-line summary of the command `id`.
pub fn summary(id: &str) -> Option<&'static str> {
    Some(match id {
        "edit.undo" => "Takes back the last change",
        "edit.redo" => "Puts back the change you undid",
        "file.open" => "Opens a project file",
        "file.save" => "Saves the project where it was last saved",
        "file.save_as" => "Saves the project to a new file",
        "file.cancel" => "Stops the save or open in progress",
        "file.focus_owner" => "Brings the window that keeps your recovery forward",
        "file.status" => "Whether the project has unsaved changes",
        "run.changed" => "Prices and runs every card that changed",
        "run.workflow" => "Prices and runs every card on this canvas",
        "run.reuse" => "Uses cached results where the inputs match",
        "run.held" => "Paid results waiting for a place to go",
        "run.jobs" => "Runs queued, running and finished",
        "canvas.stale" => "Finds results that are out of date",
        "canvas.delete" => "Deletes the selected cards",
        "canvas.fit_all" => "Zooms to show every card",
        "canvas.fit_selection" => "Zooms to the selected cards",
        "canvas.minimap_toggle" => "Shows or hides the minimap",
        "canvas.prompt_overlays_toggle" => "Shows or hides prompts over media cards",
        "canvas.comments_toggle" => "Shows or hides the discussions panel",
        "canvas.search_open" => "Finds cards by title, prompt or text",
        "canvas.search_close" => "Closes search and keeps the selection",
        "canvas.search_next" => "Goes to the next search result",
        "canvas.search_previous" => "Goes to the previous search result",
        "canvas.tag_filter_open" => "Highlights the cards with a colour tag",
        "canvas.tag_filter_close" => "Closes the tag filter and keeps the selection",
        "canvas.tag_filter_next" => "Goes to the next tagged card",
        "canvas.tag_filter_previous" => "Goes to the previous tagged card",
        "app.check_updates" => "Looks for a newer NodeCraft release",
        "app.install_update" => "Downloads and checks the newer release",
        "app.restart_to_update" => "Restarts into the downloaded release",
        "app.update_status" => "This version and the last update check",
        "automation.mcp_server_status" => "Whether the built-in MCP server is running",
        "comfy.models.list" => "Models the ComfyUI server can load",
        "comfy.nodes.refresh" => "Reads the nodes the ComfyUI server offers",
        "comfy.nodes.status" => "What the last ComfyUI node read found",
        "export.status" => "How the latest background export is going",
        "library.add" => "Adds this Technique to the library",
        "library.assets" => "Media in this project",
        "library.elements" => "Elements saved in the library",
        "library.history" => "Every generation in this project",
        "library.list" => "Techniques saved in the library",
        "library.repair_saved" => "Rebuilds My library's index if it can't be read",
        "library.saved" => "Media saved to My library",
        "library.shaders" => "Shaders saved in the library",
        "library.styles" => "Styles trained in this project",
        "element.list" => "Elements on this canvas",
        "mask.status" => "Masks whose image has changed",
        "node.describe" => "What a node kind takes and gives",
        "options.refresh" => "Fetches checkpoints, LoRAs and other option lists",
        "options.status" => "What the last option list fetch installed",
        "perf.start" => "Records frame and command timings",
        "perf.stop" => "Stops recording timings and keeps them",
        "perf.clear" => "Drops the recorded timings",
        "perf.report" => "Copies the timing report to the clipboard",
        "plugins.list" => "Installed plugin packages",
        "plugins.prune" => "Removes plugin versions nothing uses",
        "providers.settings_get" => "This site's provider server addresses",
        "providers.settings_set" => "Saves this site's provider server addresses",
        "share.start" => "Shares this project live with others",
        "share.stop" => "Leaves the live session; your copy stays",
        "share.status" => "How live sharing is going",
        "share.links" => "Links others join this project with",
        "sheet.list" => "This project's Batch Generate sheets",
        "sheet.rows" => "The rows of this Batch Generate sheet",
        "skill.list" => "Skills the agent can use",
        "style.list" => "Styles trained on this canvas",
        "technique.list" => "Techniques published in this project",
        "technique.remove" => "Removes this Technique version",
        "technique.show" => "This Technique's inputs, outputs and version",
        "template.list" => "Starter templates you can open",
        "template.refresh" => "Fetches the latest starter template list",
        "templates.clear_cache" => "Removes downloaded starter templates",
        "templates.status" => "What the last template update fetched",
        "templates.update" => "Downloads new starter templates",
        "usage.export_csv" => "The usage log as a spreadsheet",
        "usage.ownership" => "Charges set aside for review",
        "usage.unresolved" => "Charges that may have been billed",
        "usage.unsettled" => "Open charges reconciled with providers",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use crate::commands::Registry;

    /// Every command a person can run without parameters (none required, or only the canvas the
    /// palette supplies) has a short summary.
    #[test]
    fn every_parameter_free_command_has_a_short_summary() {
        let registry = Registry::builtins().unwrap();
        let mut missing = Vec::new();
        for spec in registry.specs() {
            let schema = (spec.input_schema)();
            let required: Vec<&str> =
                schema.get("required").and_then(|r| r.as_array()).map(|r| r.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
            if !required.iter().all(|key| *key == "canvas") {
                continue;
            }
            match spec.summary() {
                Some(summary) => {
                    assert!(!summary.is_empty() && summary.chars().count() <= 60, "{}: {summary:?}", spec.id);
                    assert!(!summary.ends_with('.'), "{}: one line, no full stop", spec.id);
                }
                None => missing.push(spec.id),
            }
        }
        assert!(missing.is_empty(), "commands without a summary: {missing:?}");
    }
}
