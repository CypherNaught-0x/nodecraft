//! Offering unsaved work back after an unclean exit (#408). At startup a journal holding work
//! is kept aside (`storage`); here it is replayed and the person decides: **Recover** adopts the
//! replayed project at the file its session had open (checkpointed at once, unsaved), **Start
//! from last save** opens that file as last saved, then drops only the selected copy. Nothing is lost silently: the
//! kept copy stays on disk until the recovered work is saved (so quitting first, or crashing
//! again, offers it again next time), a damaged journal offers what could be replayed and says
//! so, and one that can't be read is left where it is, with its name.
use super::{NodeCraftApp, storage};
use crate::theme::Tokens;
use nodecraft_format::{Snapshot, StorageLocation};
use serde_json::json;
use std::path::{Path, PathBuf};

/// What a kept journal held.
pub struct Found {
    pub snapshot: Snapshot,
    pub title: String,
    /// Edits replayed after its checkpoint.
    pub changes: u64,
    /// Replay stopped before an incomplete or damaged record.
    pub damaged: bool,
}

/// The question put to the person at startup.
pub struct RecoveryOffer {
    /// The kept copy of the journal.
    pub kept: PathBuf,
    /// The project file its session had open, if it had one.
    pub location: Option<PathBuf>,
    /// What it held, or why it can't be replayed.
    pub found: Result<Found, String>,
    /// The live document over which this question was attached.
    offered_over: Option<nodecraft_engine::session::DocumentLineage>,
}

impl RecoveryOffer {
    /// Reads and replays the kept journal at `kept`.
    pub fn read(kept: PathBuf) -> Self {
        let found = storage::replay_kept(&kept).map_err(|error| error.message).map(|recovered| Found {
            title: recovered.snapshot.project.title().to_owned(),
            changes: recovered.applied,
            damaged: recovered.damage.is_some() || recovered.truncated_tail,
            snapshot: recovered.snapshot,
        });
        Self { location: storage::recorded_location(&kept), kept, found, offered_over: None }
    }

    /// The question to ask at startup, if any: about `kept` (the journal this launch kept
    /// aside), or else an earlier copy that still replays (work recovered and never
    /// saved). Copies that can't be replayed were shown once and are left for the person.
    pub fn at_startup(recovery: &Path, kept: Option<PathBuf>) -> Option<Self> {
        let mut copies = storage::kept_journals(recovery);
        let offer = match kept {
            Some(kept) => {
                copies.retain(|copy| *copy != kept);
                Self::read(kept)
            }
            None => loop {
                let candidate = copies.pop()?;
                let offer = Self::read(candidate);
                if offer.found.is_ok() {
                    break offer;
                }
            },
        };
        Some(offer)
    }

    fn kept_name(&self) -> String {
        self.kept.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    /// Resolve only the copy explicitly offered. Other snapshots are not proven ancestors.
    fn remove_selected(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.kept) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        if let Err(error) = std::fs::remove_file(storage::location_file(&self.kept))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("recovery: a removed copy's location record couldn't be removed: {error}");
        }
        Ok(())
    }
}

/// A recovered copy is released only by an actual save of this adopted document.
pub(super) struct RecoveredWork {
    offer: RecoveryOffer,
    lineage: nodecraft_engine::session::DocumentLineage,
}

/// The person's choice.
enum Choice {
    Recover,
    StartFromLastSave,
    /// Close the notice about a journal that can't be replayed; the file stays.
    Keep,
}

fn quoted(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return "an untitled project".to_owned();
    }
    // A title from an untrusted file stays one readable line.
    let short: String = title.chars().filter(|c| !c.is_control()).take(80).collect();
    if short.chars().count() < title.chars().count() { format!("“{short}…”") } else { format!("“{short}”") }
}

impl NodeCraftApp {
    /// At startup, with storage over `journal`: asks about unsaved work (`kept`, this launch's
    /// copy of it, or work recovered earlier and never saved) and records the open file.
    pub(super) fn attach_recovery(&mut self, journal: PathBuf, kept: Option<PathBuf>) {
        if self.session.file_status().available {
            self.recovery = RecoveryOffer::at_startup(&journal, kept);
            if let Some(offer) = &mut self.recovery {
                offer.offered_over = Some(self.session.document_lineage());
            }
            self.recovery_journal = Some(journal);
        }
    }

    /// The startup question, while one is pending. It's modal: the canvas waits for the answer,
    /// and no key or drop reaches it meanwhile.
    pub(super) fn recovery_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(offer) = &self.recovery else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("recovery_offer")).show(ctx, |ui| {
            ui.set_max_width(t.dialog_width);
            ui.heading("Recover unsaved work?");
            match &offer.found {
                Ok(found) => {
                    let file = offer.location.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                    let title = file.clone().unwrap_or_else(|| quoted(&found.title));
                    let what = match found.changes {
                        0 => format!("Unsaved work on {title} can be recovered."),
                        1 => format!("1 unsaved change to {title} can be recovered."),
                        n => format!("{n} unsaved changes to {title} can be recovered."),
                    };
                    ui.label(format!("NodeCraft didn't close cleanly last time. {what}"));
                    if found.damaged {
                        ui.label(egui::RichText::new("Recovery stopped at an incomplete or damaged record. Only complete changes before that point are included.").color(t.danger));
                    }
                    let explain = match &file {
                        Some(file) => format!("Recover opens that work unsaved, as {file}. Start from last save opens {file} as last saved and discards it."),
                        None => "Recover opens that work as an untitled, unsaved project. Start from last save discards it.".to_owned(),
                    };
                    ui.label(egui::RichText::new(explain).color(t.text_muted));
                    ui.horizontal(|ui| {
                        if ui.button("Recover").clicked() {
                            choice = Some(Choice::Recover);
                        }
                        if ui.button("Start from last save").clicked() {
                            choice = Some(Choice::StartFromLastSave);
                        }
                    });
                }
                Err(why) => {
                    ui.label(format!("NodeCraft didn't close cleanly last time, but its record of unsaved work can't be replayed ({why})."));
                    ui.label(
                        egui::RichText::new(format!(
                            "It was kept as {} in the recovery folder, beside NodeCraft's settings, and won't be offered again. Delete it once you no longer need it.",
                            offer.kept_name()
                        ))
                        .color(t.text_muted),
                    );
                    if ui.button("Continue").clicked() {
                        choice = Some(Choice::Keep);
                    }
                }
            }
        });
        if let Some(choice) = choice {
            self.decide_recovery(choice);
        }
        {
            // Keys, text, scrolling and drops this frame were the modal's; none may edit the
            // document behind it (a paste would land, unseen, in the work about to be replaced).
            ctx.input_mut(|i| {
                i.events.clear();
                i.raw.events.clear();
                i.raw.dropped_files.clear();
                i.keys_down.clear();
                i.modifiers = egui::Modifiers::NONE;
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            });
        }
    }

    fn decide_recovery(&mut self, choice: Choice) {
        let Some(offer) = self.recovery.take() else {
            return;
        };
        let name = offer.kept_name();
        let replaced_unsaved =
            offer.offered_over.as_ref().is_some_and(|lineage| !self.session.is_current_document(lineage)) && self.session.file_status().dirty;
        if matches!(choice, Choice::Recover | Choice::StartFromLastSave) && (self.session.can_undo() || self.session.can_redo() || replaced_unsaved) {
            self.notice =
                Some(format!("The current project has unsaved work. The recovery copy was kept as {name}; save the current work before recovering it."));
            return;
        }

        match (choice, &offer.found) {
            (Choice::Recover, Ok(found)) => match self.session.recover(found.snapshot.clone(), offer.location.clone().map(StorageLocation::Path)) {
                Ok(()) => {
                    log::info!("recovery: recovered {} changes from {name}", found.changes);
                    self.after_document_replaced();
                    let title = offer.location.as_ref().and_then(|p| p.file_name()).map_or_else(|| quoted(&found.title), |n| n.to_string_lossy().into_owned());
                    self.notice = Some(if offer.location.is_some() {
                        format!("Recovered {title}. Save to keep it; until then a copy stays in the recovery folder.")
                    } else {
                        format!("Recovered {title} as an untitled project. Until it's saved, a copy stays in the recovery folder.")
                    });
                    // Kept until the recovered work is saved (`track_recovery`).
                    self.recovered = Some(RecoveredWork { offer, lineage: self.session.document_lineage() });
                }
                Err(error) => {
                    log::error!("recovery: {name} couldn't be recovered: {}", error.message);
                    self.notice = Some(format!("That work couldn't be recovered ({}). It was kept as {name} in the recovery folder.", error.message));
                }
            },
            (Choice::StartFromLastSave, _) => {
                if let Some(file) = &offer.location {
                    let opened = file.to_str().ok_or_else(|| "the path isn't valid UTF-8".to_owned()).and_then(|location| {
                        self.session.dispatch("file.open", &json!({"location":location,"discard_dirty":true})).map(|_| ()).map_err(|error| error.message)
                    });
                    if let Err(error) = opened {
                        self.notice = Some(format!("The last save couldn't be opened ({error}). Unsaved work was kept as {name} in the recovery folder."));
                        return;
                    }
                    self.after_document_replaced();
                }
                self.notice = Some(match offer.remove_selected() {
                    Ok(()) if offer.location.is_some() => "Started from the last save; the selected unsaved recovery copy was discarded.".to_owned(),
                    Ok(()) => "The selected unsaved recovery copy was discarded.".to_owned(),
                    Err(error) => format!("Started from the last save, but the recovery copy couldn't be removed ({error}); it remains as {name}."),
                });
            }
            (Choice::Recover | Choice::Keep, _) => {
                log::warn!("recovery: kept {name}, which can't be replayed");
            }
        }
    }

    /// Every frame: keeps the record of which file this session has open beside its journal
    /// (so a crash recovers to it), and once recovered work is saved, drops its kept copy.
    pub(super) fn track_recovery(&mut self) {
        let Some(journal) = &self.recovery_journal else {
            return;
        };
        let status = self.session.file_status();
        let location = match &status.location {
            Some(StorageLocation::Path(path)) => Some(path.clone()),
            _ => None,
        };
        if self.recorded_location.as_ref() != Some(&location) {
            if let Err(error) = storage::record_location(journal, location.as_deref()) {
                log::warn!("recovery: the open project file wasn't recorded: {error}");
            }
            self.recorded_location = Some(location);
        }
        let Some(recovered) = &self.recovered else {
            return;
        };
        if !self.session.is_current_document(&recovered.lineage) {
            // A replacement is not a save of the work promised protection by this copy.
            self.recovered = None;
        } else if self.session.has_saved_document(&recovered.lineage)
            && let Some(recovered) = self.recovered.take()
            && let Err(error) = recovered.offer.remove_selected()
        {
            let name = recovered.offer.kept_name();
            log::warn!("recovery: saved work's safety copy couldn't be removed: {error}");
            self.notice = Some(format!("The work was saved, but its recovery copy remains as {name}."));
        }
    }

    /// The document was replaced wholesale: show its first canvas, with nothing selected. (At
    /// startup the rest of the view state is still at rest.)
    fn after_document_replaced(&mut self) {
        if let Some(first) = super::sheet_view::home_canvas(self.session.document()) {
            self.canvas = first;
        }
        self.selection.clear();
        self.editors.clear();
        self.centred = false;
    }
}

#[cfg(test)]
mod tests;
