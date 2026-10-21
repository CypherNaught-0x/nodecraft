//! The generation card's composer (#83): the prompt with a submit arrow and the variation
//! counter. The arrow, Cmd/Ctrl+Enter in the prompt and Enter on the focused card ask the app to
//! generate; the app quotes the run and asks before anything is spent.
use super::*;
use nodecraft_nodes::{NodeContract, generation::PROMPT};

/// The meaning tag of the parameter counting the images one run makes.
pub const COUNT: &str = "generated_image_count";
/// The meaning tag of a model's output aspect ratio.
pub const ASPECT: &str = "aspect_ratio";
const MAX_RATIO_DIGITS: usize = 5;
/// Shown beside a wired prompt's label.
pub(crate) const WIRED: &str = "from connected text";

/// The parameters a generation card gives their own controls, found by meaning tag in the
/// node's contract rather than by name, so a model's `n` and another's `num_images` both count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    /// How many images one run makes: the variation counter.
    pub count: Option<String>,
    /// The output aspect: tagged `aspect_ratio`, or else a choice whose every option is `W:H`.
    pub aspect: Option<String>,
}

impl Roles {
    pub fn of(contract: &NodeContract) -> Self {
        let mut roles = Self::default();
        let mut ratios = None;
        for group in contract.parameter_groups() {
            for spec in &group.schema.params {
                let meaning = group.rule(&spec.name).and_then(|rule| rule.meaning.clone());
                if roles.count.is_none() && meaning.as_deref() == Some(COUNT) && matches!(spec.control, ParamControl::Unsigned { .. }) {
                    roles.count = Some(spec.name.clone());
                }
                if let ParamControl::Choice { options } = &spec.control {
                    if roles.aspect.is_none() && meaning.as_deref() == Some(ASPECT) {
                        roles.aspect = Some(spec.name.clone());
                    }
                    if ratios.is_none() && options.len() > 1 && options.iter().all(|option| ratio(&option.value).is_some()) {
                        ratios = Some(spec.name.clone());
                    }
                }
            }
        }
        if roles.aspect.is_none() {
            roles.aspect = ratios;
        }
        roles
    }
    /// Bytes this adds to a cached schema entry.
    pub fn bytes(&self) -> usize {
        [&self.count, &self.aspect].into_iter().flatten().fold(0usize, |total, name| total.saturating_add(name.len().saturating_add(48)))
    }
}

/// `W:H` with small positive whole numbers on both sides.
pub fn ratio(value: &str) -> Option<(u32, u32)> {
    let (w, h) = value.split_once(':')?;
    let part = |s: &str| {
        (!s.is_empty() && s.len() <= MAX_RATIO_DIGITS && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u32>().ok()).flatten().filter(|n| *n > 0)
    };
    Some((part(w)?, part(h)?))
}

/// The next count a step away, when the parameter's control accepts it.
pub fn step_count(control: &ParamControl, current: u64, up: bool) -> Option<u64> {
    let ParamControl::Unsigned { step, .. } = control else {
        return None;
    };
    let next = if up { current.checked_add(step.0)? } else { current.checked_sub(step.0)? };
    control.validate_value(&Value::Unsigned(Unsigned(next))).is_ok().then_some(next)
}

/// Whether the card is a generation card that gets a composer.
pub(crate) fn has_composer(node: &Node, descriptor: &NodeDescriptor) -> bool {
    nodecraft_engine::generation_catalog::GenerationCatalog::is_model_selector(&node.kind, nodecraft_nodes::generation::MODEL)
        && descriptor.params.params.iter().any(|spec| spec.name == PROMPT && matches!(spec.control, ParamControl::Text { .. }))
}

/// The answer a text card shows (#84): the first text its active generation made.
pub fn answer_of(node: &Node) -> Option<&str> {
    if node.kind != nodecraft_nodes::generation::TEXT_KIND {
        return None;
    }
    let active = node.active?;
    node.history.iter().find(|g| g.id == active)?.outputs.iter().find_map(|value| match value {
        Value::Text(text) => Some(text.as_str()),
        _ => None,
    })
}

/// What a text card shows of an answer that is still arriving or was cut short (#504). It is
/// view state: only a finished answer is recorded, unless the person keeps a cut-short one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partial<'a> {
    pub text: &'a str,
    /// Why it stopped, once it has; it can then be kept or discarded.
    pub stopped: Option<&'a str>,
}

/// Shown in a text card's answer area before it has an answer.
pub const NO_ANSWER: &str = "The answer appears here as it is written.";
/// Records a cut-short answer on the card.
pub const KEEP: &str = "Keep";
/// Lets a cut-short answer go.
pub const DISCARD: &str = "Discard";

/// Draws a text card's answer area in `area`: the answer streaming in, or one cut short with
/// what can be done with it, or the active answer, or a placeholder. Text is selectable, and
/// scrolled when longer than the area; a streaming answer follows its end.
pub(super) fn answer(ui: &mut egui::Ui, area: Rect, active: Option<&str>, partial: Option<Partial<'_>>, intents: &mut Vec<Intent>, t: &Tokens) {
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("answer").max_rect(area).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(area.intersect(ui.clip_rect()));
    egui::Frame::new().fill(t.prompt_fill).corner_radius(t.card_radius / 2).inner_margin(t.control_gap).show(&mut child, |ui| {
        let width = ui.available_width();
        if let Some(Partial { stopped: Some(why), .. }) = partial {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(why).color(t.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(DISCARD).on_hover_text("Let this partial answer go").clicked() {
                        intents.push(Intent::DiscardPartial);
                    }
                    if ui.button(KEEP).on_hover_text("Add this partial answer to the card's results").clicked() {
                        intents.push(Intent::KeepPartial);
                    }
                });
            });
        }
        let (text, color) = match (partial, active) {
            (Some(partial), _) => (partial.text, if partial.stopped.is_some() { t.text_muted } else { t.text }),
            (None, Some(active)) => (active, t.text),
            (None, None) => (NO_ANSWER, t.text_muted),
        };
        let streaming = partial.is_some_and(|partial| partial.stopped.is_none());
        egui::ScrollArea::vertical().id_salt("answer").max_height(ui.available_height()).auto_shrink([false, false]).stick_to_bottom(streaming).show(
            ui,
            |ui| {
                ui.set_width(width);
                ui.add(egui::Label::new(egui::RichText::new(text).color(color)).selectable(true).wrap());
            },
        );
    });
}

pub(super) struct Composer<'a> {
    pub node: &'a Node,
    /// A wire feeds the prompt port: connected text replaces the prompt, so it is locked and its
    /// draft kept for when the wire goes.
    pub wired: bool,
    pub descriptor: &'a NodeDescriptor,
    pub roles: Option<&'a Roles>,
    pub visible: Option<&'a BTreeSet<String>>,
    pub enabled: Option<&'a BTreeSet<String>>,
    /// How many Elements go by each lowercased name.
    pub elements: &'a std::collections::BTreeMap<String, usize>,
    /// The cards on the canvas, for `@` mentions (#505).
    pub mentions: &'a [super::mention::Candidate],
}

/// Draws the composer at the top of `area` and returns the space left under it. The prompt and
/// the count drawn here are left out of the card's parameter list.
pub(super) fn show(ui: &mut egui::Ui, area: Rect, composer: Composer<'_>, editor: &mut EditorState, intents: &mut Vec<Intent>, t: &Tokens) -> Rect {
    let Composer { node, wired, descriptor, roles, visible, enabled, elements, mentions } = composer;
    let shown = |name: &str| visible.is_some_and(|names| names.contains(name));
    let editable = |name: &str| enabled.is_none_or(|names| names.contains(name));
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("composer").max_rect(area).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(area.intersect(ui.clip_rect()));
    egui::Frame::new().fill(t.prompt_fill).corner_radius(t.card_radius / 2).inner_margin(t.control_gap).show(&mut child, |ui| {
        ui.set_width(ui.available_width());
        if let Some(spec) = descriptor.params.params.iter().find(|spec| spec.name == PROMPT).filter(|spec| shown(&spec.name)) {
            ui.push_id(PROMPT, |ui| {
                // The note shares the label's row, so a wired card is no taller than any other.
                let label = if wired {
                    ui.label(egui::RichText::new(format!("{} · {WIRED}", spec.label)).color(t.text_muted)).on_hover_text(
                        "A wire feeds this prompt: its text, when not empty, is used instead of what you typed. Your prompt is kept for when you disconnect.",
                    )
                } else {
                    ui.label(egui::RichText::new(&spec.label).color(t.text_muted))
                };
                let value = node.params.get(PROMPT).or(spec.default.as_ref());
                // Last frame's focus: a submit in this frame is the field's own key press.
                let focused = editor.text.get(PROMPT).is_some_and(|draft| draft.was_focused);
                let run = focused && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
                let open = editable(PROMPT) && !wired;
                // The text field's id, as `control` makes it: `TextEdit::id_salt` wraps its salt.
                let field = ui.make_persistent_id(egui::IdSalt::new("value"));
                if open {
                    super::mention::keys(ui, field, editor.text.get_mut(PROMPT), mentions, node.id);
                }
                let changed = if open {
                    let changed = control(ui, &spec.control, value, PROMPT, editor, label.id, t);
                    let below = ui.min_rect().bottom();
                    let picked = super::mention::list(ui, field, focused, editor.text.get_mut(PROMPT), mentions, node.id, below, t);
                    // The click on a row ended the field's focus, which commits the search; the
                    // field has its focus back and commits the mention when it is done.
                    changed.filter(|_| !picked)
                } else {
                    ui.add_enabled_ui(false, |ui| control(ui, &spec.control, value, PROMPT, editor, label.id, t)).inner
                };
                if open && let Some(value) = changed {
                    intents.push(Intent::Parameter { name: PROMPT.into(), value });
                }
                // A wired prompt replaces the text, so its mentions feed nothing.
                if let (false, Some(Value::Text(prompt))) = (wired, value) {
                    mention_hint(ui, prompt, elements, t);
                    super::mention::hint(ui, prompt, mentions, node.id, t);
                }
                if run {
                    intents.push(Intent::Generate);
                }
            });
        }
        ui.horizontal(|ui| {
            if let Some(spec) = roles
                .and_then(|roles| roles.count.as_deref())
                .and_then(|name| descriptor.params.params.iter().find(|spec| spec.name == name))
                .filter(|spec| shown(&spec.name))
            {
                counter(ui, node, spec, editable(&spec.name), intents, t);
            }
            // Only as tall as the button, so the row never stretches to the card's height.
            ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), t.submit_size), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if submit(ui, t).on_hover_text("Generate (Enter on the card, or Cmd/Ctrl+Enter in the prompt). Shows the price first.").clicked() {
                    intents.push(Intent::Generate);
                }
            });
        });
    });
    let used = child.min_rect().bottom() + t.control_gap;
    Rect::from_min_max(Pos2::new(area.left(), used.min(area.bottom())), area.max)
}

/// The Elements `prompt` mentions, under it: plain for one Element, in the danger colour for a
/// name more than one Element has (the run is blocked until one is renamed). Tags no Element
/// goes by (`#ff0000`) stay plain text and aren't listed.
fn mention_hint(ui: &mut egui::Ui, prompt: &str, elements: &std::collections::BTreeMap<String, usize>, t: &Tokens) {
    if elements.is_empty() || !prompt.contains('#') {
        return;
    }
    let mut used = Vec::new();
    let mut shared = Vec::new();
    for name in nodecraft_nodes::element::mentions(prompt) {
        match elements.get(&name.to_ascii_lowercase()) {
            Some(1) => used.push(format!("#{name}")),
            Some(_) => shared.push(format!("#{name}")),
            None => {}
        }
    }
    if !used.is_empty() {
        ui.label(egui::RichText::new(format!("Uses {}", used.join(", "))).small().color(t.text_muted))
            .on_hover_text("These Elements' reference images go to the model with the prompt");
    }
    if !shared.is_empty() {
        ui.label(egui::RichText::new(format!("{}: more than one Element has this name", shared.join(", "))).small().color(t.danger));
    }
}

fn counter(ui: &mut egui::Ui, node: &Node, spec: &nodecraft_types::ParamSpec, editable: bool, intents: &mut Vec<Intent>, t: &Tokens) {
    let current = match node.params.get(&spec.name).or(spec.default.as_ref()) {
        Some(Value::Unsigned(n)) => Some(n.0),
        _ => None,
    };
    ui.push_id(&spec.name, |ui| {
        for (text, up, name) in [("-", false, "Fewer"), ("+", true, "More")] {
            let next = current.and_then(|n| step_count(&spec.control, n, up));
            let r = ui.add_enabled(editable && next.is_some(), egui::Button::new(text).small());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, editable && next.is_some(), format!("{name} {}", spec.label.to_lowercase())));
            if r.clicked()
                && let Some(next) = next
            {
                intents.push(Intent::Parameter { name: spec.name.clone(), value: Some(Value::Unsigned(Unsigned(next))) });
            }
            if !up {
                let shown = current.map_or_else(|| "?".to_owned(), |n| n.to_string());
                ui.label(egui::RichText::new(shown).color(t.text)).on_hover_text(&spec.label);
            }
        }
        ui.label(egui::RichText::new(&spec.label).color(t.text_muted));
    });
}

/// The round submit button with a painted up arrow; no font glyph or icon asset.
fn submit(ui: &mut egui::Ui, t: &Tokens) -> egui::Response {
    let size = Vec2::splat(t.submit_size);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Generate"));
    let painter = ui.painter();
    let fill = if response.hovered() { t.accent } else { t.text };
    painter.circle_filled(rect.center(), rect.width() / 2.0, fill);
    let c = rect.center();
    let arm = rect.width() * 0.22;
    let stroke = Stroke::new(t.handle_stroke * 1.5, t.card_fill);
    painter.line_segment([c + Vec2::new(0.0, arm), c - Vec2::new(0.0, arm)], stroke);
    painter.line_segment([c - Vec2::new(0.0, arm), c + Vec2::new(-arm * 0.8, -arm * 0.2)], stroke);
    painter.line_segment([c - Vec2::new(0.0, arm), c + Vec2::new(arm * 0.8, -arm * 0.2)], stroke);
    if response.has_focus() {
        painter.circle_stroke(rect.center(), rect.width() / 2.0 + t.handle_stroke, Stroke::new(t.handle_stroke, t.accent));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_are_small_positive_whole_numbers() {
        assert_eq!(ratio("16:9"), Some((16, 9)));
        assert_eq!(ratio("1:1"), Some((1, 1)));
        for bad in ["", "16", "16:", ":9", "0:1", "1:0", "-1:2", "1.5:1", "999999:1", "16:9:1", "a:b", "１:1"] {
            assert_eq!(ratio(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_counter_steps_only_to_values_the_control_accepts() {
        let control = ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(4)), step: Unsigned(1) };
        assert_eq!(step_count(&control, 2, true), Some(3));
        assert_eq!(step_count(&control, 4, true), None);
        assert_eq!(step_count(&control, 1, false), None);
        assert_eq!(step_count(&control, u64::MAX, true), None);
        assert_eq!(step_count(&ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) }, 0, false), None);
        assert_eq!(step_count(&ParamControl::Toggle, 1, true), None);
    }
}
