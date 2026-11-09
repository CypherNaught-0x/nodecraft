//! My library in the Assets tab (#96), and the row Import from URL is typed in.
//!
//! My library is the one every project of this person shares: "Save to My library" copies ticked
//! files into it (`library.save_assets`), its scope lists them (`library.saved`), and Place puts
//! them on the open canvas, copying their bytes into the project (`library.place_saved`), as one
//! undo step. Only a person saves or removes; nothing is added on its own.
use super::library_browsers::{Browse, Browsers, Pick, SavedPage, Tab, bounded, kind_name, tick};
use super::*;
use nodecraft_types::Blake3;

/// What the browser build says when Import from URL is asked for.
pub(super) const NO_URL_IMPORT: &str =
    "Importing from a web address needs the desktop app, which checks where the address leads. Download the file, then drop it on the canvas.";

/// `bytes` for people: "512 B", "1.4 MB", "1.0 GB".
pub(super) fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS.get(unit).copied().unwrap_or("TB"))
}

/// The Import from URL… button; it opens the row the address is typed in.
pub(super) fn url_button(ui: &mut egui::Ui, browsers: &mut Browsers, busy: bool) {
    let available = cfg!(not(target_arch = "wasm32"));
    let why = if available { "A file is being imported" } else { NO_URL_IMPORT };
    let button = ui.add_enabled(available && !busy, egui::Button::new("Import from URL…"));
    if button
        .on_hover_text("Fetch one image, video, sound, 3D model or PDF from a public https address; only its file name is kept")
        .on_disabled_hover_text(why)
        .clicked()
    {
        browsers.url = if browsers.url.is_some() { None } else { Some(String::new()) };
    }
}

/// The address row, while it is open: the field, Import and Close, and why an address won't do.
pub(super) fn url_row(ui: &mut egui::Ui, t: &Tokens, browsers: &mut Browsers, busy: bool, action: &mut Option<Browse>) {
    let Some(address) = browsers.url.as_mut() else { return };
    let mut close = false;
    let mut submit = false;
    let mut refusal: Option<String> = None;
    ui.horizontal(|ui| {
        let field = ui.add(egui::TextEdit::singleline(address).hint_text("https://example.com/photo.png").desired_width(t.dialog_width * 0.6));
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        // Checked once a frame; the row says why when it won't do.
        refusal = nodecraft_engine::url_import::check(address).err().map(|error| error.message);
        let import = ui.add_enabled(!busy && refusal.is_none(), egui::Button::new("Import"));
        submit = (import.on_hover_text("Fetch it now; you can cancel while it downloads").clicked() || entered) && refusal.is_none() && !busy;
        close = ui.button("Close").clicked();
    });
    let trimmed = address.trim();
    if !trimmed.is_empty()
        && let Some(why) = refusal
    {
        ui.label(egui::RichText::new(why).small().color(t.text_muted));
    }
    if submit {
        *action = Some(Browse::FetchUrl(bounded(trimmed, nodecraft_engine::url_import::MAX_URL_CHARS * 4).to_owned()));
        browsers.url = None;
    } else if close {
        browsers.url = None;
    }
}

/// Over My library's list: how full it is, Refresh, and Repair when its index is damaged.
pub(super) fn header(ui: &mut egui::Ui, t: &Tokens, browsers: &mut Browsers, action: &mut Option<Browse>) {
    let mut refresh = false;
    ui.horizontal_wrapped(|ui| {
        if let Some(page) = browsers.saved_page() {
            let files = if page.entries == 1 { "1 file".to_owned() } else { format!("{} files", page.entries) };
            ui.label(egui::RichText::new(format!("{files} · {} of {}", size(page.used_bytes), size(page.max_bytes))).color(t.text_muted));
        }
        refresh = ui.button("Refresh").on_hover_text("Read My library again, with what other windows saved").clicked();
    });
    if cfg!(target_arch = "wasm32") {
        ui.label(egui::RichText::new("In the browser, My library lasts until the page is closed.").small().color(t.text_muted));
    }
    if let Some(page) = browsers.saved_page() {
        if let Some(why) = &page.index_damaged {
            ui.label(
                egui::RichText::new(format!(
                    "My library's index can't be read ({}). Repair keeps it aside and rebuilds the list from the saved files; their names are lost.",
                    sheet_view::line(why)
                ))
                .small()
                .color(t.danger),
            );
            if ui.button("Repair").on_hover_text("Rebuild My library's list from the files it kept; nothing is deleted").clicked() {
                *action = Some(Browse::RepairSaved);
            }
        }
        if !page.damaged.is_empty() {
            ui.label(
                egui::RichText::new(format!("{} entries can't be used by this version of NodeCraft; they are kept as they are.", page.damaged.len()))
                    .small()
                    .color(t.text_muted),
            );
        }
    }
    if refresh {
        browsers.saved_stamp = browsers.saved_stamp.wrapping_add(1);
    }
}

/// A square naming the entry's kind, where a project asset shows its thumbnail: the file isn't in
/// the project until it is placed.
fn tile(ui: &mut egui::Ui, t: &Tokens, kind: Option<Modality>, name: &str) {
    let side = t.library_thumbnail;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    ui.painter().rect_filled(rect, t.panel_radius, t.card_fill);
    let label = kind.map_or("File", kind_name);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(t.label_size), t.text_muted);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, format!("{label} {name}")));
}

/// My library's page: each entry with its tick, kind, origin and size, Place and Remove.
pub(super) fn list(
    ui: &mut egui::Ui,
    t: &Tokens,
    page: &SavedPage,
    picked: &std::collections::BTreeMap<String, Pick>,
    ticks: &mut Vec<(String, Option<Pick>)>,
    action: &mut Option<Browse>,
) {
    if page.total == 0 && page.index_damaged.is_none() {
        let empty = if page.entries > 0 {
            "Nothing in My library matches."
        } else {
            "Nothing saved yet. Tick assets or results in this project and press Save to My library."
        };
        ui.label(egui::RichText::new(empty).color(t.text_muted));
    }
    for entry in &page.assets {
        ui.horizontal(|ui| {
            tile(ui, t, entry.kind, &entry.name);
            ui.vertical(|ui| {
                let pick = Pick { tab: Tab::Assets, mine: true, save: None, place: None, export: None, asset: Some(entry.hash), result: None };
                tick(ui, picked, format!("saved:{}", entry.hash), &sheet_view::line(&entry.name), pick, ticks);
                let mut facts = vec![entry.kind.map_or_else(|| entry.mime.essence().to_owned(), |kind| kind_name(kind).to_owned())];
                facts.extend(entry.origin.iter().map(|origin| format!("from {}", sheet_view::line(origin))));
                facts.push(size(entry.bytes));
                ui.label(egui::RichText::new(facts.join(" · ")).small().color(t.text_muted));
                ui.horizontal(|ui| {
                    if ui.button("Place").on_hover_text("A card holding it, in the middle of the view, copied into this project; one undo step").clicked() {
                        *action = Some(Browse::PlaceSaved(vec![entry.hash]));
                    }
                    if ui.button("Remove from My library").on_hover_text("Take it out of My library; cards placed from it keep their own copy").clicked() {
                        *action = Some(Browse::Unsave(vec![entry.hash]));
                    }
                });
            });
        });
    }
}

/// "2 saved · 1 already there · 1 failed: the file is missing from this project".
fn saved_summary(result: &serde_json::Value) -> String {
    let items = result["saved"].as_array().cloned().unwrap_or_default();
    let count = |state: &str| items.iter().filter(|item| item["state"] == state).count();
    let mut parts = Vec::new();
    for (state, said) in [("saved", "saved to My library"), ("already", "already in My library")] {
        let n = count(state);
        if n > 0 {
            parts.push(format!("{n} {said}"));
        }
    }
    let failed: Vec<String> = items
        .iter()
        .filter(|item| item["state"] == "failed")
        .map(|item| format!("{}: {}", sheet_view::line(item["name"].as_str().unwrap_or("a file")), item["reason"].as_str().unwrap_or("it wasn't saved")))
        .collect();
    if !failed.is_empty() {
        parts.push(format!("{} failed ({})", failed.len(), failed.join("; ")));
    }
    if parts.is_empty() { "Nothing was saved.".into() } else { parts.join(" · ") }
}

impl NodeCraftApp {
    /// Copies the ticked project files into My library, and says what became of each.
    pub(super) fn save_to_library(&mut self, hashes: Vec<Blake3>) -> Result<(), Error> {
        let items: Vec<serde_json::Value> = hashes.iter().map(|hash| json!({"hash": hash})).collect();
        let result = self.session.dispatch("library.save_assets", &json!({"items": items}))?;
        self.library.browsers.clear_picks();
        self.library.browsers.saved_stamp = self.library.browsers.saved_stamp.wrapping_add(1);
        self.notices.success(saved_summary(&result));
        Ok(())
    }

    /// My library's entries as source cards in a grid centred in the view, selected.
    pub(super) fn place_saved(&mut self, t: &Tokens, hashes: Vec<Blake3>) -> Result<(), Error> {
        let centre = self.browse_centre(t)?;
        let mut fresh: Vec<NodeId> = Vec::with_capacity(hashes.len());
        for index in 0..hashes.len() {
            let id = ids::node_id(self.session.document(), ("library-saved", self.session.revision(), index))
                .filter(|id| !fresh.contains(id))
                .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate node ids; try again."))?;
            fresh.push(id);
        }
        let step = t.card_width + t.card_padding * 4.0;
        let columns = nodecraft_engine::library_attach_cmds::columns(hashes.len());
        let rows = hashes.len().div_ceil(columns);
        let at = Pos2::new(centre.x - step * columns as f32 * 0.5, centre.y - step * rows as f32 * 0.5);
        let items: Vec<serde_json::Value> = hashes.iter().map(|hash| json!({"hash": hash})).collect();
        let params = json!({"canvas": self.canvas, "items": items, "ids": fresh, "at": {"x": at.x, "y": at.y}, "step": step});
        self.session.dispatch("library.place_saved", &params)?;
        self.selection.replace(fresh.into_iter().collect());
        self.library.browsers.clear_picks();
        Ok(())
    }

    /// Rebuilds My library's damaged index, and says what it recovered.
    pub(super) fn repair_saved(&mut self) -> Result<(), Error> {
        let result = self.session.dispatch("library.repair_saved", &json!({}))?;
        self.library.browsers.saved_stamp = self.library.browsers.saved_stamp.wrapping_add(1);
        self.notices.success(if result["repaired"] == true {
            let (entries, kept) = (result["entries"].as_u64().unwrap_or_default(), result["kept"].as_u64().unwrap_or_default());
            let kept = if kept > 0 { format!("; {kept} it can't use were kept") } else { String::new() };
            format!(
                "Rebuilt My library from {entries} intact files{kept}. The damaged index was kept as {}.",
                sheet_view::line(result["kept_as"].as_str().unwrap_or("library.damaged.json"))
            )
        } else {
            "My library's index is fine; nothing was changed.".into()
        });
        Ok(())
    }

    /// Takes entries out of My library.
    pub(super) fn unsave(&mut self, hashes: Vec<Blake3>) -> Result<(), Error> {
        let result = self.session.dispatch("library.unsave_assets", &json!({"hashes": hashes}))?;
        self.library.browsers.clear_picks();
        self.library.browsers.saved_stamp = self.library.browsers.saved_stamp.wrapping_add(1);
        let removed = result["removed"].as_u64().unwrap_or_default();
        self.notices.success(if removed == 1 { "Removed 1 file from My library.".into() } else { format!("Removed {removed} files from My library.") });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::size;

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_400_000), "1.4 MB");
        assert_eq!(size(1024 * 1024 * 1024), "1.1 GB");
        assert_eq!(size(u64::MAX), "18446744.1 TB");
    }
}
