//! The same exact-result save actions on previews, cards and toolbars (#967).
//! This module only reports history intents; the app owns dialogs and background writers.
use crate::{history::HistoryIntent, icons::Icon, theme::Tokens};
use nodecraft_graph::Node;
use nodecraft_types::{AssetRef, Error, GenerationId, Value};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub generation: GenerationId,
    pub port: Option<String>,
    pub item: Option<usize>,
    pub label: String,
    pub name: String,
}

pub(crate) type Targets = Result<Vec<Target>, Error>;

impl Target {
    pub fn file(&self) -> HistoryIntent {
        HistoryIntent::Export { generation: self.generation, port: self.port.clone(), item: self.item, name: self.name.clone() }
    }

    fn folder(&self) -> HistoryIntent {
        HistoryIntent::SaveToFolder { generation: self.generation, port: self.port.clone(), item: self.item }
    }
}

/// A preview's position, without copying or comparing content hashes (two results can match).
#[derive(Clone, Copy)]
pub(crate) enum Hit {
    Image(usize, usize),
    Video,
    Audio,
    Mesh,
}

#[derive(Clone)]
struct PinnedTargets {
    owner: egui::Id,
    session: Arc<()>,
    targets: Targets,
}

#[cfg(test)]
pub(crate) fn pinned_snapshot_count(ctx: &egui::Context) -> usize {
    ctx.data_mut(|data| data.count::<PinnedTargets>())
}

pub(crate) fn active(node: &Node) -> Targets {
    let Some(generation) = node.history.iter().find(|generation| Some(generation.id) == node.active) else { return Ok(Vec::new()) };
    crate::history::export_targets(node, generation)
        .map(|targets| targets.into_iter().map(|(label, port, item, name)| Target { generation: generation.id, label, port, item, name }).collect())
}

/// Pin the menu to the result that was right-clicked, even if a running job finishes while
/// the menu is open. A transient live-action preview has no saved result to export yet.
pub(crate) fn pinned(ctx: &egui::Context, response: &egui::Response, node: &Node, hit: Option<Hit>, transient: bool, session: &Arc<()>) -> Targets {
    // One aggregate slot for the active context menu, regardless of cards/projects visited.
    let key = egui::Id::new("result_save_context_targets");
    if !response.secondary_clicked()
        && !response.long_touched()
        && let Some(pinned) = ctx.data_mut(|data| data.get_temp::<PinnedTargets>(key))
        && pinned.owner == response.id
    {
        return if Arc::ptr_eq(session, &pinned.session) { pinned.targets } else { Ok(Vec::new()) };
    }
    let targets = match hit {
        Some(_) if transient => Ok(Vec::new()),
        Some(hit) => preview(node, hit),
        None => active(node),
    };
    ctx.data_mut(|data| data.insert_temp(key, PinnedTargets { owner: response.id, session: session.clone(), targets: targets.clone() }));
    targets
}

/// Resolve the actual borrowed asset the preview paints back to its output/list position.
/// Flattening matches `history.export`, including a batch's per-prompt nested lists.
pub(crate) fn preview(node: &Node, hit: Hit) -> Targets {
    if node.kind.starts_with("source.") || node.kind == nodecraft_nodes::folder::ID {
        return Ok(Vec::new());
    }
    let Some(generation) = node.history.iter().find(|generation| Some(generation.id) == node.active) else { return Ok(Vec::new()) };
    let rows;
    let asset = match hit {
        Hit::Image(row, column) => {
            rows = nodecraft_engine::split::result_rows(generation).0;
            rows.get(row).and_then(|row| row.get(column)).copied()
        }
        Hit::Video => crate::video::video_of(node),
        Hit::Audio => crate::audio::audio_of(node),
        Hit::Mesh => crate::mesh_view::mesh_of(node),
    };
    let Some(asset) = asset else { return Ok(Vec::new()) };
    let matches = |value: &Value| matches!(value, Value::Media(candidate) if std::ptr::eq::<AssetRef>(candidate, asset));
    let mut budget = nodecraft_engine::export_leaves::Budget::default();
    for (port, value) in node.outputs.iter().zip(&generation.outputs) {
        for leaf in nodecraft_engine::export_leaves::iter(value, !generation.items.is_empty(), &mut budget) {
            let leaf = leaf?;
            if matches(leaf.value) {
                return Ok(vec![Target {
                    generation: generation.id,
                    port: Some(port.name.clone()),
                    item: leaf.item,
                    label: port.name.clone(),
                    name: crate::history::suggested_name(asset.mime.essence()),
                }]);
            }
        }
    }
    Ok(Vec::new())
}

/// Draw on demand, never scan large histories/lists in every card frame.
pub(crate) fn menu(ui: &mut egui::Ui, checked: &Targets, t: &Tokens) -> Option<HistoryIntent> {
    let targets = checked.as_ref().map(Vec::as_slice).unwrap_or_default();
    if targets.is_empty() {
        ui.add_enabled(false, Icon::Save.button(ui, "Save as…", t));
        ui.add_enabled(false, Icon::Open.button(ui, "Save to folder…", t));
        ui.label(checked.as_ref().err().map_or("No saved media result. Generate or select a media result first.", |error| error.message.as_str()));
        return None;
    }
    let mut intent = None;
    if let [target] = targets {
        if ui.add(Icon::Save.button(ui, "Save as…", t)).clicked() {
            intent = Some(target.file());
        }
        let response = ui.add_enabled(cfg!(all(unix, not(target_arch = "wasm32"))), Icon::Open.button(ui, "Save to folder…", t));
        if response.clicked() {
            intent = Some(target.folder());
        }
        response.on_disabled_hover_text("Saving to a folder needs the desktop app on macOS or Linux.");
    } else {
        ui.menu_button("Save as…", |ui| {
            for target in targets {
                if ui.add(Icon::Save.button(ui, &target.label, t)).clicked() {
                    intent = Some(target.file());
                }
            }
            if intent.is_some() {
                ui.close();
            }
        });
        ui.add_enabled_ui(cfg!(all(unix, not(target_arch = "wasm32"))), |ui| {
            ui.menu_button("Save to folder…", |ui| {
                for target in targets {
                    if ui.add(Icon::Open.button(ui, &target.label, t)).clicked() {
                        intent = Some(target.folder());
                    }
                }
                if intent.is_some() {
                    ui.close();
                }
            });
        })
        .response
        .on_disabled_hover_text("Saving to a folder needs the desktop app on macOS or Linux.");
    }
    if intent.is_some() {
        ui.close();
    }
    intent
}
