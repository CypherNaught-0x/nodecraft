//! Bounded native acquisition is asynchronous; only accepted bytes reach one engine import.
use super::NodeCraftApp;
#[cfg(not(target_arch = "wasm32"))]
use crate::ids;
use crate::theme::Tokens;
#[cfg(not(target_arch = "wasm32"))]
use egui::Pos2;
use egui::Rect;
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::import::{Incoming, MAX_MEDIA_BYTES, MAX_PROJECT_MEDIA_BYTES};
use nodecraft_graph::Position;
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_DROP_BYTES: usize = MAX_MEDIA_BYTES;
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_DROP_TOTAL_BYTES: usize = MAX_PROJECT_MEDIA_BYTES;
/// Most dropped paths checked for folders: as many as a drop of files reads.
#[cfg(not(target_arch = "wasm32"))]
const MAX_FILES_LOOKED_AT: usize = nodecraft_engine::import::MAX_FILES;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod probe;
#[cfg(any(test, target_arch = "wasm32"))]
pub(super) mod web;
#[cfg(not(target_arch = "wasm32"))]
struct Pending {
    ticket: native::Ticket,
    probe: Option<probe::Pending>,
    identity: std::sync::Arc<crate::navigation::SessionIdentity>,
    project: nodecraft_types::ProjectId,
    canvas: nodecraft_types::CanvasId,
    revision: u64,
    position: Position,
    step: f32,
}
#[cfg(not(target_arch = "wasm32"))]
impl Pending {
    fn current(&self, app: &mut NodeCraftApp) -> bool {
        self.canvas == app.canvas
            && self.project == app.session.document().id()
            && self.revision == app.session.revision()
            && std::sync::Arc::ptr_eq(&self.identity, &crate::navigation::Geometry::identity(&mut app.session))
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct Acquisition {
    service: Result<native::Service, native::Failure>,
    pending: Option<Pending>,
    rect: Rect,
    pointer_owned: bool,
}
#[cfg(not(target_arch = "wasm32"))]
impl Acquisition {
    pub(super) fn new() -> Self {
        Self { service: native::Service::global(), pending: None, rect: Rect::NOTHING, pointer_owned: false }
    }
}
impl NodeCraftApp {
    pub(super) fn drop_pointer_owned(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.drop_acquisition.pointer_owned
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn import_drop(&mut self, batch: native::Batch, pending: &Pending, movies: &[nodecraft_engine::import::ProbedMovie]) {
        if !pending.current(self) || batch.check().is_err() {
            self.notice = Some("The pending drop no longer belongs to this document; nothing was added.".into());
            return;
        }
        let mut incoming = Vec::new();
        let mut new_ids = Vec::new();
        if incoming.try_reserve_exact(batch.files.len()).is_err() || new_ids.try_reserve_exact(batch.files.len()).is_err() {
            self.notice = Some(native::Failure::Allocation.message().into());
            return;
        }
        for (index, file) in batch.files.iter().enumerate() {
            let fresh =
                ids::node_id(self.session.document(), ("drop", self.session.revision(), index, pending.position.x.to_bits(), pending.position.y.to_bits()))
                    .filter(|id| !new_ids.contains(id));
            let Some(id) = fresh else {
                self.notice = Some("Couldn't allocate node ids; try again.".into());
                return;
            };
            new_ids.push(id);
            incoming.push(Incoming { name: &file.name, bytes: &file.bytes });
        }
        if !pending.current(self) || batch.check().is_err() {
            self.notice = Some("The pending drop expired or changed document; nothing was added.".into());
            return;
        }
        match nodecraft_engine::import::import_files_with_movies(&mut self.session, pending.canvas, pending.position, pending.step, &incoming, &new_ids, movies)
        {
            Ok(imported) => {
                if !imported.nodes.is_empty() {
                    self.selection.replace(imported.nodes.iter().copied().collect());
                }
                self.notice = batch.skipped.notice(&imported);
            }
            Err(e) => self.notice = Some(e.message),
        }
        // The raw payload and occupied process slot end only after synchronous import returns.
    }
    /// Poll before canvas input; no backend or filesystem call executes here.
    pub(super) fn poll_drop(&mut self, ui: &mut egui::Ui, t: &Tokens) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = t;
            self.poll_web_drop();
            if self.web_drop.is_some() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }
            false
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Native tests drive the browser drop's import (#319).
            #[cfg(test)]
            self.poll_web_drop();
            if self.drop_acquisition.pending.is_none() && !self.drop_acquisition.pointer_owned {
                self.drop_acquisition.rect = Rect::NOTHING;
            }
            let other_owner = self.drag.is_some()
                || self.wiring.is_some()
                || self.reordering.is_some()
                || self.keys.open
                || self.marquee.is_some()
                || self.navigation.pointer_owned
                || self.palette.is_some()
                || self.turn_into.is_some()
                || egui::Popup::is_any_open(ui.ctx())
                || crate::canvas::text_input_focused(ui.ctx());
            if !ui.input(|i| i.pointer.primary_down() || i.pointer.any_released()) {
                self.drop_acquisition.pointer_owned = false;
            }
            if self.drop_acquisition.pending.is_some() {
                let shown = egui::Area::new(ui.id().with("pending_drop"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::LEFT_BOTTOM, egui::Vec2::new(t.card_padding, -t.card_padding))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::NONE
                            .fill(t.panel_fill)
                            .inner_margin(t.card_padding)
                            .show(ui, |ui| {
                                ui.label("Reading dropped files…");
                                let button = ui.add_enabled(!other_owner, egui::Button::new("Cancel file reading"));
                                // egui can report a click from the previous pointer position
                                // when release precedes PointerMoved in this input frame.
                                // Refuse ambiguous bursts and any actual outside release.
                                let release_inside = ui.input(|i| {
                                    i.events.len() <= 64
                                        && i.events.iter().all(|event| {
                                            !matches!(event,
                                        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. }
                                            if !button.rect.contains(*pos))
                                        })
                                });
                                let cancel = button.clicked() && release_inside;
                                if let Some(notice) = &self.notice {
                                    ui.label(notice);
                                }
                                cancel
                            })
                            .inner
                    });
                self.drop_acquisition.rect = shown.response.rect;
                if shown.inner {
                    self.drop_acquisition.pending = None;
                    self.notice = Some(native::Failure::Cancelled.message().into());
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }
            let rect = self.drop_acquisition.rect;
            if !other_owner
                && ui.input(|i| {
                    i.events.iter().any(|event| {
                        matches!(event,
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } if rect.contains(*pos))
                    })
                })
            {
                self.drop_acquisition.pointer_owned = true;
            }
            if let Some(mut pending) = self.drop_acquisition.pending.take() {
                if !pending.current(self) {
                    self.notice = Some("The document changed while reading files; nothing was added. Drop them again.".into());
                } else if self.drop_acquisition.pointer_owned {
                    // A press belongs to this control through release. Do not remove Cancel
                    // underneath a held gesture just because the worker became ready.
                    match pending.ticket.check() {
                        Ok(()) => self.drop_acquisition.pending = Some(pending),
                        Err(e) => self.notice = Some(e.message().into()),
                    }
                } else {
                    let failed = if pending.probe.is_none() {
                        match pending.ticket.poll() {
                            native::Poll::Pending => false,
                            native::Poll::Failed(e) => {
                                self.notice = Some(e.message().into());
                                true
                            }
                            native::Poll::Ready(batch) => {
                                pending.probe = Some(probe::Pending::new(batch));
                                false
                            }
                        }
                    } else {
                        false
                    };
                    if !failed {
                        let finished = pending.probe.as_mut().is_some_and(|probe| probe.poll(&mut self.videos, ui.ctx()));
                        if finished {
                            if let Some(probe) = pending.probe.take() {
                                self.import_drop(probe.batch, &pending, &probe.movies);
                            }
                        } else {
                            self.drop_acquisition.pending = Some(pending);
                        }
                    }
                }
            }
            if self.drop_acquisition.pending.is_none() && !self.drop_acquisition.pointer_owned {
                self.drop_acquisition.rect = Rect::NOTHING;
            }
            self.drop_acquisition.pointer_owned
                || (self.drop_acquisition.pending.is_some()
                    && ui.input(|i| !i.pointer.primary_down() && i.pointer.hover_pos().is_some_and(|p| rect.contains(p))))
        }
    }
    pub(super) fn drop_pending(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.drop_acquisition.pending.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.web_drop.is_some()
        }
    }
    pub(super) fn drop_files(&mut self, ui: &egui::Ui, viewport: Rect, t: &Tokens) {
        let hovering = ui.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("drop_hint")));
            painter.rect_filled(viewport, 0.0, t.marquee_fill);
            painter.text(viewport.center(), egui::Align2::CENTER_CENTER, "Drop to add as nodes", egui::FontId::proportional(t.body_size * 1.5), t.text);
        }
        if ui.input(|i| i.raw.dropped_files.is_empty()) {
            return;
        }
        if self.comments.drag.is_some() || self.comments.gesture_cancelled {
            self.notice = Some("Finish moving the discussion pin before dropping files.".into());
            return;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let at = ui.input(|i| i.pointer.hover_pos()).filter(|p| viewport.contains(*p)).unwrap_or(viewport.center());
            let world = self.view.to_world(viewport.min, at);
            let position = Position { x: world.x - t.card_width * 0.5, y: world.y - t.card_min_height * 0.5 };
            let files = ui.input(|i| i.raw.dropped_files.clone());
            self.start_web_drop(ui.ctx(), &files, position, t.card_width + t.card_padding * 4.0);
            ui.ctx().request_repaint();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.drop_pointer_owned() {
                self.notice = Some("Drop onto the canvas after releasing the file-reading control.".into());
                return;
            }
            // A dropped folder becomes a Folder card, or refills the one it is dropped on.
            let at: Pos2 = ui.input(|i| i.pointer.hover_pos()).filter(|p| viewport.contains(*p)).unwrap_or(viewport.center());
            let world = self.view.to_world(viewport.min, at);
            let paths: Vec<std::path::PathBuf> = ui.input(|i| i.raw.dropped_files.iter().take(MAX_FILES_LOOKED_AT).map(|f| f.path().to_path_buf()).collect());
            let position = Position { x: world.x - t.card_width * 0.5, y: world.y - t.card_min_height * 0.5 };
            if self.drop_folders(&paths, world, position) {
                ui.ctx().request_repaint();
                return;
            }
            let ticket = match &self.drop_acquisition.service {
                Ok(service) => ui.input(|i| service.start(&i.raw.dropped_files)),
                Err(e) => Err(*e),
            };
            let ticket = match ticket {
                Ok(ticket) => ticket,
                Err(e) => {
                    self.notice = Some(e.message().into());
                    return;
                }
            };
            let identity = crate::navigation::Geometry::identity(&mut self.session);
            self.drop_acquisition.pending = Some(Pending {
                ticket,
                probe: None,
                identity,
                project: self.session.document().id(),
                canvas: self.canvas,
                revision: self.session.revision(),
                position,
                step: t.card_width + t.card_padding * 4.0,
            });
            self.notice = None;
            ui.ctx().request_repaint();
        }
    }
}
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;
