//! Copying a history result to the OS clipboard (#831). `history.copy` reads and decodes the
//! result in the engine and hands it to the host registered here, which keeps it until the frame
//! ends and then gives it to egui: the desktop and browser backends own the clipboard itself.
use super::NodeCraftApp;
use nodecraft_engine::Session;
use nodecraft_engine::copy_cmds::Clip;
use nodecraft_types::{Error, ErrorKind};
use std::{cell::RefCell, rc::Rc};

/// Registers the clipboard host on `session`; the returned slot holds what it was last given.
pub(super) fn register(session: &mut Session) -> Result<Rc<RefCell<Option<Clip>>>, Error> {
    let copied = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&copied);
    session.register_clipboard_host(move |clip| {
        *slot.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Busy, "the clipboard is busy"))? = Some(clip);
        Ok(())
    })?;
    Ok(copied)
}

impl NodeCraftApp {
    /// Hands this frame's copy, if any, to egui and says it was copied.
    pub(super) fn deliver_copy(&mut self, ctx: &egui::Context) {
        let Some(clip) = self.copied.try_borrow_mut().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        match clip {
            Clip::Text(text) => {
                ctx.copy_text(text);
                self.notice = Some("Copied the text.".into());
            }
            Clip::Image { width, height, rgba } => {
                let size = [usize::try_from(width).unwrap_or(0), usize::try_from(height).unwrap_or(0)];
                // `from_rgba_unmultiplied` asserts the length; a mismatch is reported instead.
                if size[0].checked_mul(size[1]).and_then(|pixels| pixels.checked_mul(4)) != Some(rgba.len()) {
                    self.notice = Some("The image couldn't be copied.".into());
                    return;
                }
                ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(size, &rgba));
                self.notice = Some(format!("Copied the {width}×{height} image."));
            }
        }
    }
}
