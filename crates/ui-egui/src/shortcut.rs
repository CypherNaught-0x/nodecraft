//! One spelling for keyboard shortcuts (#1117).
//!
//! The engine writes a command's shortcut as `Command+Shift+Z`, where `Command` is the platform's
//! primary modifier. Everything the shell shows a person (the quick-add palette, tooltips, menus,
//! notices) formats it here, so macOS reads `Shift+⌘Z` and Windows, Linux and the browser
//! elsewhere read `Ctrl+Shift+Z`. Modifiers come in a fixed order for each platform: on macOS
//! Ctrl, Option, Shift, then ⌘, as Apple orders them. Only ⌘ is a symbol: egui's default fonts
//! have no ⇧, ⌥ or ⌃, so those are written out.

/// The shortcut as a person on this platform reads it. On the web this follows the browser's
/// operating system, as egui reports it.
pub fn format_shortcut(ctx: &egui::Context, shortcut: &str) -> String {
    format_for(shortcut, ctx.os() == egui::os::OperatingSystem::Mac)
}

/// [`format_shortcut`] for an explicit platform: `mac` writes Command as `⌘` and the others as
/// names (`Shift+⌘Z`), otherwise names joined by `+` (`Ctrl+Shift+Z`). A part this can't read
/// is kept as written, so an unknown key still shows rather than vanishing.
pub fn format_for(shortcut: &str, mac: bool) -> String {
    let (mut command, mut ctrl, mut shift, mut alt) = (false, false, false, false);
    let mut keys = Vec::new();
    for part in shortcut.split('+').map(str::trim).filter(|part| !part.is_empty()) {
        match part {
            "Command" | "Cmd" => command = true,
            "Ctrl" | "Control" => ctrl = true,
            "Shift" => shift = true,
            "Alt" | "Option" => alt = true,
            key => keys.push(match egui::Key::from_name(key) {
                Some(key) => key.name(),
                None => key,
            }),
        }
    }
    let mut parts: Vec<&str> = Vec::new();
    if mac {
        parts.extend([(ctrl, "Ctrl"), (alt, "Option"), (shift, "Shift")].into_iter().filter(|(on, _)| *on).map(|(_, name)| name));
    } else {
        parts.extend([(ctrl || command, "Ctrl"), (alt, "Alt"), (shift, "Shift")].into_iter().filter(|(on, _)| *on).map(|(_, name)| name));
    }
    let key = keys.join("+");
    let key = if mac && command { format!("⌘{key}") } else { key };
    if !key.is_empty() {
        parts.push(&key);
    }
    parts.join("+")
}

/// `label (shortcut)`, the way tooltips and accessible names show a control's shortcut.
pub fn with_shortcut(ctx: &egui::Context, label: &str, shortcut: &str) -> String {
    format!("{label} ({})", format_shortcut(ctx, shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_shift_z_reads_right_on_each_platform() {
        assert_eq!(format_for("Command+Shift+Z", true), "Shift+⌘Z");
        assert_eq!(format_for("Command+Shift+Z", false), "Ctrl+Shift+Z");
        // Order is fixed whatever order the engine wrote.
        assert_eq!(format_for("Shift+Command+Z", false), "Ctrl+Shift+Z");
        assert_eq!(format_for("Command+Alt+0", false), "Ctrl+Alt+0");
        assert_eq!(format_for("Command+Alt+0", true), "Option+⌘0");
        assert_eq!(format_for("Ctrl+Command+F", true), "Ctrl+⌘F", "Ctrl stays Ctrl on macOS");
    }

    #[test]
    fn plain_keys_and_named_keys() {
        assert_eq!(format_for("B", false), "B");
        assert_eq!(format_for("3", true), "3");
        assert_eq!(format_for("Command+Enter", false), "Ctrl+Enter");
        assert_eq!(format_for("Command+F", true), "⌘F");
        assert_eq!(format_for("Escape", false), "Escape");
    }

    #[test]
    fn hostile_strings_never_panic_and_keep_what_they_cannot_read() {
        assert_eq!(format_for("", false), "");
        assert_eq!(format_for("+++", true), "");
        assert_eq!(format_for("Command+Mystery", false), "Ctrl+Mystery");
        assert_eq!(format_for("ü+ß", true), "ü+ß");
    }

    #[test]
    fn the_command_symbol_is_in_the_default_fonts() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        for font in [egui::FontId::proportional(13.0), egui::FontId::monospace(10.5)] {
            assert!(ctx.fonts_mut(|fonts| fonts.has_glyph(&font, '⌘')), "⌘ missing from the default {font:?}");
        }
    }
}
