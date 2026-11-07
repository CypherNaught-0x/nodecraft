//! Small helpers shared by the shell's panels.
//!
//! A disabled control keeps its explanation (#1113): egui hides `on_hover_text` once a widget
//! is disabled, so a control greyed out exactly when a person wants to know why says nothing.
//! [`command_button`] shows the hover text when the control can act and the reason when it
//! can't, and a test below fails on an `add_enabled(..)` that sets hover text with no
//! disabled text.

/// Adds `button`, enabled when `enabled` is `Ok`. Its hover text is `hover` while it can act
/// and the reason in `Err` once it can't. Write reasons as what to do: "Select a row to run".
pub fn command_button(ui: &mut egui::Ui, button: impl egui::Widget, enabled: Result<(), String>, hover: impl Into<egui::WidgetText>) -> egui::Response {
    match enabled {
        Ok(()) => ui.add(button).on_hover_text(hover),
        Err(why) => ui.add_enabled(false, button).on_disabled_hover_text(why),
    }
}

/// Lines in `code` where an `add_enabled(..)` call chains `.on_hover_text(..)` but no
/// `.on_disabled_hover_text(..)`: hover text that disappears exactly when the control greys out.
/// Only the method chain straight after the call counts, which is how the shell writes them.
pub fn hover_lost_when_disabled(code: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    // The index just past the parenthesis that closes the one opened before `from`.
    let close = |from: usize| -> Option<usize> {
        let mut depth = 1usize;
        let mut at = from;
        while depth > 0 {
            match bytes.get(at)? {
                b'(' => depth = depth.saturating_add(1),
                b')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            at = at.saturating_add(1);
        }
        Some(at)
    };
    let mut lines = Vec::new();
    let mut search = 0;
    while let Some(found) = code.get(search..).and_then(|rest| rest.find("add_enabled(")) {
        let start = search.saturating_add(found);
        search = start.saturating_add("add_enabled(".len());
        let Some(mut at) = close(search) else { break };
        let (mut hover, mut disabled) = (false, false);
        loop {
            let rest = code.get(at..).unwrap_or_default();
            let trimmed = rest.trim_start();
            let Some(call) = trimmed.strip_prefix('.') else { break };
            let call = call.trim_start();
            let name: String = call.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let Some(open) = call.get(name.len()..).filter(|after| after.starts_with('(')) else { break };
            hover |= name == "on_hover_text";
            disabled |= name == "on_disabled_hover_text";
            let opened = at.saturating_add(rest.len() - open.len()).saturating_add(1);
            let Some(next) = close(opened) else { break };
            at = next;
        }
        if hover && !disabled {
            lines.push(code.get(..start).map_or(0, |before| before.matches('\n').count()).saturating_add(1));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_finds_a_control_that_loses_its_hover_when_disabled() {
        let bad = "fn f(ui: &mut egui::Ui) {\n    if ui.add_enabled(false, egui::Button::new(\"Go\")).on_hover_text(\"Go there\").clicked() {}\n}";
        assert_eq!(hover_lost_when_disabled(bad), vec![2]);
        let good = "ui.add_enabled(on, b).on_hover_text(\"Go\").on_disabled_hover_text(\"Pick one first\").clicked()";
        assert!(hover_lost_when_disabled(good).is_empty());
        let multiline = "ui\n    .add_enabled(on, b)\n    .on_hover_text(format!(\"{}\", (1)))\n    .clicked()";
        assert_eq!(hover_lost_when_disabled(multiline), vec![2]);
        assert!(hover_lost_when_disabled("ui.add_enabled(on, b).clicked()").is_empty(), "no hover text, nothing lost");
        // Unbalanced input never panics.
        assert!(hover_lost_when_disabled("add_enabled((((").is_empty());
        assert!(hover_lost_when_disabled("x.add_enabled(a, b).on_hover_text(").is_empty());
    }

    /// #1113: no control in the shell loses its explanation when it is disabled.
    #[test]
    fn every_disabled_control_keeps_an_explanation() {
        // Intentional exceptions, as `path:line`; keep this empty unless a reason is written here.
        const ALLOWED: &[&str] = &[];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root.clone()];
        let mut found = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_tests.rs") || path.components().any(|c| c.as_os_str() == "tests") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let code = text.split("#[cfg(test)]").next().unwrap_or_default();
                let relative = path.strip_prefix(&root).unwrap().display().to_string().replace('\\', "/");
                for line in hover_lost_when_disabled(code) {
                    let at = format!("{relative}:{line}");
                    if !ALLOWED.contains(&at.as_str()) {
                        found.push(at);
                    }
                }
            }
        }
        assert!(found.is_empty(), "add_enabled(..).on_hover_text(..) without on_disabled_hover_text: {found:#?}");
    }
}
