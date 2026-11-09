//! Interface size (#1107): the shell draws its panels, menus, dialogs, top bar and minimap at the
//! size the `view.ui_scale.*` commands set, from 80 % to 200 %, as egui's zoom factor.
//!
//! The session and the shell share one [`UiScale`] (installed in `from_session`). The commands set
//! it; each frame the shell applies what it holds with `Context::set_zoom_factor` and saves a
//! change with the other app settings (`[appearance] interface_size`, or this origin's
//! `localStorage` in the browser). Everything drawn in points grows with it, canvas cards
//! included, the way a browser's page zoom does; canvas zoom stays a separate multiplier on top,
//! and image previews are requested at `pixels_per_point`, so they stay sharp.
//!
//! The shortcuts (⌘⌥= / ⌘⌥- / ⌘⌥0, Ctrl+Alt elsewhere) are taken before the canvas reads its own
//! zoom keys, and never while a text field has focus: Ctrl+Alt is AltGr on many keyboards, and
//! AltGr+0 types a character there.
use super::*;
use nodecraft_engine::settings::InterfaceSize;
pub(super) use nodecraft_engine::ui_scale_cmds::UiScale;

/// Where the browser keeps the size.
#[cfg(target_arch = "wasm32")]
const WEB_KEY: &str = "nodecraft.interface_size";

/// The commands the shortcuts run, in the order they are checked.
const COMMANDS: [&str; 3] = ["view.ui_scale.increase", "view.ui_scale.decrease", "view.ui_scale.reset"];

/// The keys a shortcut spelling names: the engine writes `=`, `Plus`, `Minus` and `0`.
fn shortcut_key(shortcut: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let mut modifiers = egui::Modifiers::NONE;
    let mut key = None;
    for part in shortcut.split('+') {
        match part {
            "Command" => modifiers |= egui::Modifiers::COMMAND,
            "Alt" => modifiers |= egui::Modifiers::ALT,
            "Shift" => modifiers |= egui::Modifiers::SHIFT,
            "=" => key = Some(egui::Key::Equals),
            // `Command+Alt+Plus` is spelled with the word: a bare `+` would split the spelling.
            "Plus" => key = Some(egui::Key::Plus),
            "Minus" => key = Some(egui::Key::Minus),
            "0" => key = Some(egui::Key::Num0),
            _ => return None,
        }
    }
    key.map(|key| (modifiers, key))
}

/// Takes a press of `shortcut` from this frame's input. The key matches by its logical key, which
/// egui-winit already falls back to the physical one for (so macOS ⌥=, typing ≠, arrives as `=`).
/// Only the digit also matches physically, as `consume_shortcut_with_policy` does: a physical match
/// for `=` or `-` would misfire on AltGr layouts (Swedish AltGr++ types `\` on the physical `-`).
/// Shift may be held, since some layouts need it for `=` or `+`.
fn consume(input: &mut egui::InputState, shortcut: &str) -> bool {
    let Some((wanted, key)) = shortcut_key(shortcut) else { return false };
    let mut consumed = false;
    input.events.retain(|event| {
        let matched = matches!(event, egui::Event::Key { key: logical, physical_key, pressed: true, modifiers, .. }
            if modifiers.matches_logically(wanted) && (*logical == key || (key == egui::Key::Num0 && *physical_key == Some(key))));
        consumed |= matched;
        !matched
    });
    consumed
}

impl NodeCraftApp {
    /// The interface size the window is drawn at.
    pub fn interface_size(&self) -> InterfaceSize {
        self.ui_scale.get()
    }

    /// One frame: the shortcuts, then the size applied and, when it changed, saved.
    pub(super) fn ui_scale_frame(&mut self, ctx: &egui::Context) {
        let key_pressed = ctx.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { pressed: true, .. })));
        if key_pressed && !canvas::text_input_focused(ctx) {
            let shortcuts: Vec<(&'static str, &'static str)> = COMMANDS
                .iter()
                .filter_map(|id| self.session.registry().get(id).ok())
                .flat_map(|spec| spec.shortcut.into_iter().chain(spec.alternate_shortcuts.iter().copied()).map(move |shortcut| (spec.id, shortcut)))
                .collect();
            let pressed = ctx.input_mut(|input| shortcuts.iter().find(|(_, shortcut)| consume(input, shortcut)).map(|(id, _)| *id));
            // At either end the key does nothing; it is still taken, so the canvas doesn't zoom.
            if let Some(id) = pressed
                && self.session.enabled(id, &json!({})).is_ok()
                && let Err(error) = self.session.dispatch(id, &json!({}))
            {
                self.notices.report(error);
            }
        }
        let size = self.ui_scale.get();
        if self.ui_scale_applied == Some(size) {
            return;
        }
        ctx.set_zoom_factor(size.zoom_factor());
        let clean = self.save_interface_size(size);
        // Once the clean value is saved, a damaged one is repaired: reset has nothing left to do.
        if clean != size {
            self.ui_scale.set(clean);
        }
        self.ui_scale_applied = Some(clean);
    }

    /// Keeps `size` with the app settings, as a clean value: a damaged one is written as 100 %.
    /// Returns the clean value.
    fn save_interface_size(&mut self, size: InterfaceSize) -> InterfaceSize {
        let clean = InterfaceSize::from_percent(f64::from(size.percent())).unwrap_or_default();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store
            && store.get().appearance.interface_size != clean
        {
            store.update(|settings| settings.appearance.interface_size = clean);
        }
        #[cfg(target_arch = "wasm32")]
        {
            use nodecraft_engine::secrets::OriginStorage;
            let storage = crate::web_storage::LocalStorage;
            let stored = storage.get(WEB_KEY).ok().flatten().map(|text| InterfaceSize::parse(&text));
            let wanted = (clean != InterfaceSize::default()).then_some(clean);
            if stored != wanted {
                let saved = match wanted {
                    Some(size) => storage.set(WEB_KEY, &size.percent().to_string()),
                    None => storage.remove(WEB_KEY),
                };
                if let Err(error) = saved {
                    log::warn!("the interface size wasn't kept in this browser: {error}");
                }
            }
        }
        clean
    }

    /// Takes the size from settings, saying so when the file's value couldn't be used.
    pub(super) fn apply_interface_size(&mut self, size: InterfaceSize) {
        self.ui_scale.set(size);
        if size.damaged() {
            self.notices.warning(InterfaceSize::DAMAGED);
        }
    }

    /// The size this browser kept, if any.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn browser_interface_size() -> InterfaceSize {
        use nodecraft_engine::secrets::OriginStorage;
        match crate::web_storage::LocalStorage.get(WEB_KEY) {
            Ok(Some(text)) => InterfaceSize::parse(&text),
            Ok(None) => InterfaceSize::default(),
            Err(error) => {
                log::warn!("the interface size kept in this browser couldn't be read: {error}");
                InterfaceSize::default()
            }
        }
    }
}

#[cfg(test)]
#[path = "ui_scale_tests.rs"]
mod tests;
