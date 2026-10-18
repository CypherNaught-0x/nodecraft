//! Offering unsaved work back after an unclean exit (#408). At startup a journal holding work
//! is kept aside (`storage`); here it is replayed and the person decides: **Recover** adopts the
//! replayed project (checkpointed at once, untitled and unsaved), **Start from last save** drops
//! the kept copy. Nothing is lost silently: until the decision the copy stays on disk, a damaged
//! journal offers what could be replayed and says so, and one that can't be read is left where
//! it is with its name.
use super::NodeCraftApp;
use crate::theme::Tokens;
use nodecraft_format::{Snapshot, journal};
use std::path::PathBuf;

/// What a kept journal held.
pub struct Found {
    pub snapshot: Snapshot,
    pub title: String,
    /// Edits replayed after its checkpoint.
    pub changes: u64,
    /// Replay stopped at damage: what came after it is lost.
    pub damaged: bool,
}

/// The question put to the person at startup.
pub struct RecoveryOffer {
    /// The kept copy of the journal.
    pub kept: PathBuf,
    /// What it held, or why it can't be replayed.
    pub found: Result<Found, String>,
}

impl RecoveryOffer {
    /// Reads and replays the kept journal at `kept`.
    pub fn read(kept: PathBuf) -> Self {
        let found = std::fs::read(&kept)
            .map_err(|_| "it can't be read".to_owned())
            .and_then(|bytes| journal::recover(&bytes).map_err(|error| error.message))
            .map(|recovered| Found {
                title: recovered.snapshot.project.title().to_owned(),
                changes: recovered.applied,
                damaged: recovered.damage.is_some() || recovered.truncated_tail,
                snapshot: recovered.snapshot,
            });
        Self { kept, found }
    }

    fn kept_name(&self) -> String {
        self.kept.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }
}

/// The person's choice.
enum Choice {
    Recover,
    StartFromLastSave,
    /// Close the notice about a journal that can't be replayed; the file stays.
    Keep,
}

impl NodeCraftApp {
    /// The startup question, while one is pending. It's modal: the canvas waits for the answer.
    pub(super) fn recovery_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(offer) = &self.recovery else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("recovery_offer")).show(ctx, |ui| {
            ui.set_max_width(t.card_width * 1.4);
            ui.heading("Recover unsaved work?");
            match &offer.found {
                Ok(found) => {
                    let title = if found.title.trim().is_empty() { "an untitled project".to_owned() } else { format!("“{}”", found.title) };
                    let changes = match found.changes {
                        0 => "Its unsaved state".to_owned(),
                        1 => "1 unsaved change".to_owned(),
                        n => format!("{n} unsaved changes"),
                    };
                    ui.label(format!("NodeCraft didn't close cleanly last time. {changes} to {title} can be recovered."));
                    if found.damaged {
                        ui.label(egui::RichText::new("Part of the record was damaged: changes after that point can't be recovered.").color(t.danger));
                    }
                    ui.label(egui::RichText::new("Recover opens that work as a new, unsaved project. Start from last save discards it.").color(t.text_muted));
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
                    ui.label(egui::RichText::new(format!("It was kept as {} in the recovery folder.", offer.kept_name())).color(t.text_muted));
                    if ui.button("Continue").clicked() {
                        choice = Some(Choice::Keep);
                    }
                }
            }
        });
        if let Some(choice) = choice {
            self.decide_recovery(choice);
        }
    }

    fn decide_recovery(&mut self, choice: Choice) {
        let Some(offer) = self.recovery.take() else {
            return;
        };
        let name = offer.kept_name();
        match (choice, offer.found) {
            (Choice::Recover, Ok(found)) => match self.session.recover(found.snapshot) {
                Ok(()) => {
                    log::info!("recovery: recovered {} changes from {name}", found.changes);
                    self.after_document_replaced();
                    let _ = std::fs::remove_file(&offer.kept);
                    self.notice = Some("Recovered. Save the project to keep it.".into());
                }
                Err(error) => {
                    log::error!("recovery: {name} couldn't be recovered: {}", error.message);
                    self.notice = Some(format!("That work couldn't be recovered ({}). It was kept as {name} in the recovery folder.", error.message));
                }
            },
            (Choice::StartFromLastSave, _) => {
                log::info!("recovery: discarded {name}");
                let _ = std::fs::remove_file(&offer.kept);
                self.notice = None;
            }
            (Choice::Recover | Choice::Keep, _) => {
                log::warn!("recovery: kept {name}, which can't be replayed");
            }
        }
    }

    /// The document was replaced wholesale: show its first canvas, with nothing selected.
    fn after_document_replaced(&mut self) {
        if let Some(first) = self.session.document().canvases().keys().next() {
            self.canvas = *first;
        }
        self.selection.clear();
        self.editors.clear();
        self.centred = false;
    }
}

#[cfg(test)]
mod tests;
