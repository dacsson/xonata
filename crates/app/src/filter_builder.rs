//! Plain-language filter forms and worker-backed value suggestions.
use crate::ui::Transport;
use eframe::egui::{self, Ui};
use xonata_core::filter::{DrawSpan, ExcludeScope};
use xonata_core::{Event, Request, SuggestionField};

#[derive(Default)]
pub(crate) struct Suggestions {
    desired: Option<(SuggestionField, String)>,
    sent: Option<(SuggestionField, String)>,
    generation: u32,
    changed_at: f64,
    values: Vec<String>,
    done: bool,
}
impl Suggestions {
    fn want(&mut self, field: SuggestionField, text: &str, time: f64) {
        if !self
            .desired
            .as_ref()
            .is_some_and(|(f, t)| *f == field && t == text)
        {
            self.desired = Some((field, text.to_owned()));
            self.changed_at = time;
            self.values.clear();
            self.done = false;
            self.sent = None;
            // Invalidate replies immediately, including during the typing debounce.
            self.generation = self.generation.wrapping_add(1);
        }
    }
    pub fn tick(&mut self, ui: &Ui, trace: u32, complete: bool, transport: &mut dyn Transport) {
        if complete
            && self.desired != self.sent
            && ui.input(|i| i.time) - self.changed_at >= 0.15
            && let Some((field, text)) = &self.desired
        {
            transport.request(Request::FilterSuggestions {
                trace,
                generation: self.generation,
                field: *field,
                text: text.clone(),
            });
            self.sent = self.desired.clone();
        }
        if self.desired != self.sent || (self.desired.is_some() && !self.done) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    pub fn ingest(&mut self, event: Event) {
        if let Event::FilterSuggestions {
            generation,
            field,
            values,
            done,
            ..
        } = event
            && generation == self.generation
            && self.desired.as_ref().is_some_and(|(f, _)| *f == field)
        {
            self.values = values;
            self.done = done;
        }
    }
    fn edit(&mut self, ui: &mut Ui, value: &mut String, field: SuggestionField, width: f32) {
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(value)
                    .desired_width(width)
                    .char_limit(512)
                    .hint_text("Any"),
            );
            if response.has_focus() {
                self.want(field, value, ui.input(|i| i.time));
            }
            egui::ComboBox::from_id_salt(response.id.with("suggestions"))
                .width(28.0)
                .selected_text("")
                .show_ui(ui, |ui| {
                    self.want(field, value, ui.input(|i| i.time));
                    ui.label("Up to 16 suggestions · type to narrow");
                    let mut choice = None;
                    for example in &self.values {
                        if ui.add(egui::Button::new(example).truncate()).clicked() {
                            choice = Some(example.clone());
                        }
                    }
                    if self.values.is_empty() {
                        ui.label(if self.done {
                            "No examples found. You can enter your own value."
                        } else {
                            "Looking for examples…"
                        });
                    }
                    if let Some(choice) = choice {
                        *value = choice;
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Choose a value from the loaded trace. Type to narrow suggestions.");
        });
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum ResultKind {
    #[default]
    Delays,
    Overlaps,
    Both,
}
impl ResultKind {
    fn label(self) -> &'static str {
        match self {
            Self::Delays => "Delays only (≥ 0 cycles)",
            Self::Overlaps => "Overlaps only (< 0 cycles)",
            Self::Both => "Delays and overlaps",
        }
    }
}
pub(crate) struct Endpoint {
    pub instruction: String,
    pub phase: String,
    pub metadata: String,
    pub end: bool,
    lane: String,
    status: &'static str,
    conditions: Vec<Condition>,
}
impl Default for Endpoint {
    fn default() -> Self {
        Self {
            instruction: String::new(),
            phase: String::new(),
            metadata: String::new(),
            end: false,
            lane: String::new(),
            status: "all",
            conditions: Vec::new(),
        }
    }
}
struct Condition {
    field: &'static str,
    compare: &'static str,
    value: String,
}
const CONDITIONS: &[(&str, &str)] = &[
    ("duration", "Phase duration"),
    ("cycle", "Endpoint cycle"),
    ("op", "Operation ID"),
    ("global", "Global ID"),
    ("thread", "Thread"),
    ("rid", "Retired ID"),
    ("op_meta_contains", "Instruction metadata"),
    ("phase_meta_contains", "Phase metadata"),
    ("instr_re", "Instruction regex"),
    ("meta_re", "Metadata regex"),
];
fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}
impl Endpoint {
    fn query(&self) -> String {
        let mut fields = vec![
            format!(
                "phase={}",
                quote(if self.phase.trim().is_empty() {
                    "*"
                } else {
                    self.phase.trim()
                })
            ),
            format!("phase_pos={}", if self.end { "END" } else { "START" }),
        ];
        for (field, value) in [
            ("instr", &self.instruction),
            ("meta_contains", &self.metadata),
            ("lane", &self.lane),
        ] {
            if !value.trim().is_empty() {
                fields.push(format!("{field}={}", quote(value.trim())));
            }
        }
        if self.status != "all" {
            fields.push(format!("status={}", self.status));
        }
        for c in &self.conditions {
            if !c.value.trim().is_empty() {
                fields.push(format!(
                    "{}{}{}",
                    c.field,
                    if numeric(c.field) { c.compare } else { "=" },
                    quote(c.value.trim())
                ));
            }
        }
        fields.join(" & ")
    }
    fn show(&mut self, ui: &mut Ui, id: &str, interval: bool, suggestions: &mut Suggestions) {
        // Measure outside the grid: grid cells inherit previous content widths.
        let edit_width = (ui.available_width() - 238.0).max(80.0);
        egui::Grid::new(id)
            .num_columns(3)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Instruction");
                ui.label("contains");
                suggestions.edit(
                    ui,
                    &mut self.instruction,
                    SuggestionField::Instruction,
                    edit_width,
                );
                ui.end_row();
                ui.label("Phase");
                ui.label("is");
                suggestions.edit(ui, &mut self.phase, SuggestionField::Phase, edit_width);
                ui.end_row();
                if interval {
                    ui.label("Boundary");
                    ui.label("");
                    egui::ComboBox::from_id_salt((id, "boundary"))
                        .selected_text(if self.end { "End" } else { "Start" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.end, false, "Start");
                            ui.selectable_value(&mut self.end, true, "End");
                        });
                    ui.end_row();
                }
                ui.label("Metadata");
                ui.label("contains");
                suggestions.edit(
                    ui,
                    &mut self.metadata,
                    SuggestionField::Metadata,
                    edit_width,
                );
                ui.end_row();
            });
        egui::CollapsingHeader::new("More conditions")
            .id_salt((id, "more"))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Lane");
                    suggestions.edit(ui, &mut self.lane, SuggestionField::Lane, edit_width);
                });
                ui.horizontal(|ui| {
                    ui.label("Status");
                    egui::ComboBox::from_id_salt((id, "status"))
                        .selected_text(self.status)
                        .show_ui(ui, |ui| {
                            for s in ["all", "retired", "flushed", "incomplete"] {
                                ui.selectable_value(&mut self.status, s, s);
                            }
                        });
                });
                let mut remove = None;
                for (index, condition) in self.conditions.iter_mut().enumerate() {
                    ui.push_id((id, index), |ui| {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("field")
                                .selected_text(
                                    CONDITIONS
                                        .iter()
                                        .find(|(f, _)| *f == condition.field)
                                        .map_or("Condition", |(_, label)| *label),
                                )
                                .show_ui(ui, |ui| {
                                    for (field, label) in CONDITIONS {
                                        ui.selectable_value(&mut condition.field, *field, *label);
                                    }
                                });
                            if numeric(condition.field) {
                                egui::ComboBox::from_id_salt("compare")
                                    .width(45.0)
                                    .selected_text(condition.compare)
                                    .show_ui(ui, |ui| {
                                        for c in ["=", "!=", ">=", ">", "<=", "<"] {
                                            ui.selectable_value(&mut condition.compare, c, c);
                                        }
                                    });
                                ui.add(
                                    egui::TextEdit::singleline(&mut condition.value)
                                        .desired_width(140.0)
                                        .char_limit(24)
                                        .hint_text("Decimal integer"),
                                );
                            } else if condition.field.ends_with("_contains") {
                                ui.label("contains");
                                suggestions.edit(
                                    ui,
                                    &mut condition.value,
                                    if condition.field == "op_meta_contains" {
                                        SuggestionField::OperationMetadata
                                    } else {
                                        SuggestionField::PhaseMetadata
                                    },
                                    (edit_width - 90.0).max(80.0),
                                );
                            } else {
                                ui.label("regex");
                                ui.add(
                                    egui::TextEdit::singleline(&mut condition.value)
                                        .desired_width(180.0)
                                        .char_limit(512),
                                );
                            }
                            if ui.button("×").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
                if let Some(index) = remove {
                    self.conditions.remove(index);
                }
                if ui
                    .add_enabled(
                        self.conditions.len() < 16,
                        egui::Button::new("Add condition"),
                    )
                    .clicked()
                {
                    self.conditions.push(Condition {
                        field: "duration",
                        compare: ">=",
                        value: String::new(),
                    });
                }
            });
    }
}
fn numeric(field: &str) -> bool {
    matches!(
        field,
        "duration" | "cycle" | "op" | "global" | "thread" | "rid"
    )
}

#[derive(Default)]
pub(crate) struct Builder {
    pub interval: bool,
    pub source: Endpoint,
    pub target: Endpoint,
    pub exclude_enabled: bool,
    pub exclude: Endpoint,
    pub exclude_scope: ExcludeScope,
    pub kind: ResultKind,
    pub skip: u64,
    span: DrawSpan,
    row_pad: u64,
    max_rows: String,
    min_gap: String,
    max_gap: String,
}
impl Builder {
    pub fn validation_error(&self) -> Option<String> {
        for (side, endpoint) in [
            ("From", &self.source),
            ("To", &self.target),
            ("Exclude between", &self.exclude),
        ] {
            if (side == "To" && !self.interval)
                || (side == "Exclude between" && (!self.interval || !self.exclude_enabled))
            {
                continue;
            }
            for c in &endpoint.conditions {
                if numeric(c.field)
                    && !c.value.trim().is_empty()
                    && c.value.trim().parse::<u64>().is_err()
                {
                    let label = CONDITIONS
                        .iter()
                        .find(|(field, _)| *field == c.field)
                        .map_or(c.field, |(_, label)| *label);
                    return Some(format!(
                        "{side}: {label} needs a nonnegative decimal integer."
                    ));
                }
            }
        }
        if self.interval {
            if !self.max_rows.trim().is_empty() && self.max_rows.trim().parse::<u64>().is_err() {
                return Some("Maximum row distance needs a nonnegative decimal integer.".into());
            }
            for (label, value) in [
                ("Minimum signed gap", &self.min_gap),
                ("Maximum signed gap", &self.max_gap),
            ] {
                if !value.trim().is_empty() && value.trim().parse::<i128>().is_err() {
                    return Some(format!(
                        "{label} needs a decimal integer; negative values mean overlap."
                    ));
                }
            }
        }
        None
    }
    pub fn query(&self) -> String {
        let mut text = self.source.query();
        if self.interval {
            text.push_str(&format!(" -> {}", self.target.query()));
            if self.exclude_enabled {
                text.push_str(&format!(
                    "; exclude={}; exclude_scope={}",
                    quote(&self.exclude.query()),
                    if self.exclude_scope == ExcludeScope::BetweenRows {
                        "rows"
                    } else {
                        "all"
                    }
                ));
            }
            match self.kind {
                ResultKind::Delays => text.push_str("; gap>=0"),
                ResultKind::Overlaps => text.push_str("; gap<0"),
                ResultKind::Both => {}
            }
            for (field, value) in [
                ("row_distance<=", &self.max_rows),
                ("gap>=", &self.min_gap),
                ("gap<=", &self.max_gap),
            ] {
                if !value.trim().is_empty() {
                    text.push_str(&format!("; {field}{}", quote(value.trim())));
                }
            }
        }
        text.push_str(&format!(
            "; span={}; row_pad={}",
            match self.span {
                DrawSpan::Both => "both",
                DrawSpan::Source => "source",
                DrawSpan::Target => "target",
                DrawSpan::All => "all",
            },
            self.row_pad
        ));
        text
    }
    pub fn show(&mut self, ui: &mut Ui, suggestions: &mut Suggestions) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Find");
            let was_interval = self.interval;
            egui::ComboBox::from_id_salt("mode")
                .selected_text(if self.interval {
                    "Intervals between phases"
                } else {
                    "Single phases"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.interval, false, "Single phases");
                    ui.selectable_value(&mut self.interval, true, "Intervals between phases");
                });
            if !was_interval && self.interval {
                self.source.end = true;
                self.target.end = false;
            }
            if was_interval && !self.interval {
                self.source.end = false;
            }
            if self.interval {
                egui::ComboBox::from_id_salt("result-kind").selected_text(self.kind.label()).show_ui(ui, |ui| {
                    for k in [ResultKind::Delays, ResultKind::Overlaps, ResultKind::Both] { ui.selectable_value(&mut self.kind, k, k.label()); }
                }).response.on_hover_text("Target boundary minus source boundary: nonnegative delays by default; negative values are overlaps.");
            }
        });
        ui.add_space(8.0);
        ui.strong(if self.interval { "FROM" } else { "PHASE" });
        self.source.show(ui, "source", self.interval, suggestions);
        if self.interval {
            ui.add_space(10.0);
            ui.strong("TO");
            self.target.show(ui, "target", true, suggestions);
            ui.add_space(10.0);
            ui.checkbox(&mut self.exclude_enabled, "Exclude between (optional)");
            if self.exclude_enabled {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Check");
                    egui::ComboBox::from_id_salt("exclude-scope")
                        .selected_text(if self.exclude_scope == ExcludeScope::BetweenRows {
                            "Intervening rows"
                        } else {
                            "Any pipeline row"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.exclude_scope,
                                ExcludeScope::BetweenRows,
                                "Intervening rows",
                            );
                            ui.selectable_value(
                                &mut self.exclude_scope,
                                ExcludeScope::AllRows,
                                "Any pipeline row",
                            );
                        });
                });
                self.exclude.show(ui, "exclude", true, suggestions);
                ui.label("Reject the pair if a matching Start / End lies strictly inside its cycle interval. Intervening rows excludes FROM / TO rows; Any pipeline row includes other threads. A rejected pair never chooses a later TO.");
            }
        }
        egui::CollapsingHeader::new("Advanced pairing and drawing").show(ui, |ui| {
            if self.interval {
                ui.horizontal(|ui| { ui.label("Skip matching targets"); ui.add(egui::DragValue::new(&mut self.skip).range(0..=u64::MAX)); });
                for (label, value) in [("Maximum row distance", &mut self.max_rows), ("Minimum signed gap", &mut self.min_gap), ("Maximum signed gap", &mut self.max_gap)] { ui.horizontal(|ui| { ui.label(label); ui.add(egui::TextEdit::singleline(value).desired_width(120.0).char_limit(24).hint_text("No limit")); }); }
                ui.label("Pair with the next matching later instruction in the same thread, then check the gap. Excluded overlaps do not choose another target.");
                ui.horizontal(|ui| { ui.label("Drawing spans"); egui::ComboBox::from_id_salt("span").selected_text(span_label(self.span)).show_ui(ui, |ui| { for span in [DrawSpan::Both, DrawSpan::Source, DrawSpan::Target, DrawSpan::All] { ui.selectable_value(&mut self.span, span, span_label(span)); } }); });
                ui.horizontal(|ui| { ui.label("Extra rows around drawing"); ui.add(egui::DragValue::new(&mut self.row_pad).range(0..=u64::MAX)); });
            } else { ui.label("Single-phase drawings follow the matching phase slab."); }
        });
        ui.add_space(6.0);
        ui.label("Leave fields empty for any value. Contains ignores case; * matches any text within one line (e.g. FREE RQU *=36). Instruction and phase must be on the same row.");
        if self.interval {
            ui.label(format!("For each {} phase {} in {}, find the next matching {} phase {} in {} in the same thread. {}.", any(&self.source.phase), if self.source.end { "end" } else { "start" }, any(&self.source.instruction), any(&self.target.phase), if self.target.end { "end" } else { "start" }, any(&self.target.instruction), self.kind.label()));
        }
    }
}
fn any(text: &str) -> &str {
    if text.trim().is_empty() { "any" } else { text }
}
fn span_label(span: DrawSpan) -> &'static str {
    match span {
        DrawSpan::Both => "Both endpoint rows",
        DrawSpan::Source => "Source row",
        DrawSpan::Target => "Target row",
        DrawSpan::All => "All rows",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xonata_core::filter::FilterQuery;

    #[test]
    fn exclusion_controls_should_toggle_the_form_and_allow_scope_selection() {
        let ctx = egui::Context::default();
        let mut builder = Builder {
            interval: true,
            ..Default::default()
        };
        builder.exclude.phase = "X".into();
        let mut suggestions = Suggestions::default();
        let render = |builder: &mut Builder, suggestions: &mut Suggestions, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::Vec2::new(700.0, 1500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| builder.show(ui, suggestions));
                },
            )
        };
        let text_position = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("control text visible")
        };
        for _ in 0..3 {
            let _ = render(&mut builder, &mut suggestions, Vec::new());
        }
        for label in [
            "Exclude between (optional)",
            "Intervening rows",
            "Any pipeline row",
            "Exclude between (optional)",
        ] {
            let output = render(&mut builder, &mut suggestions, Vec::new());
            let pos = text_position(&output, label);
            for pressed in [true, false] {
                let _ = render(
                    &mut builder,
                    &mut suggestions,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            if label == "Any pipeline row" {
                assert_eq!(builder.exclude_scope, ExcludeScope::AllRows);
                assert!(
                    FilterQuery::parse(&builder.query())
                        .unwrap()
                        .exclusion
                        .is_some()
                );
            }
        }
        assert!(!builder.exclude_enabled);
        assert_eq!(builder.exclude.phase, "X");
        assert!(
            FilterQuery::parse(&builder.query())
                .unwrap()
                .exclusion
                .is_none()
        );
    }
    #[test]
    fn exclusion_should_be_optional_reuse_all_fields_and_preserve_disabled_values() {
        let mut builder = Builder {
            interval: true,
            ..Default::default()
        };
        builder.source.phase = "E".into();
        builder.target.phase = "E".into();
        builder.exclude.phase = "X".into();
        builder.exclude.instruction = "branch \"label\"; & ->".into();
        builder.exclude.metadata = "resource *=36".into();
        builder.exclude.conditions.push(Condition {
            field: "cycle",
            compare: ">=",
            value: "bad".into(),
        });
        assert!(builder.validation_error().is_none());
        assert!(
            xonata_core::filter::FilterQuery::parse(&builder.query())
                .unwrap()
                .exclusion
                .is_none()
        );
        builder.exclude_enabled = true;
        assert_eq!(
            builder.validation_error().as_deref(),
            Some("Exclude between: Endpoint cycle needs a nonnegative decimal integer.")
        );
        builder.exclude.conditions[0].value = "100".into();
        builder.exclude_scope = ExcludeScope::AllRows;
        let parsed = xonata_core::filter::FilterQuery::parse(&builder.query()).unwrap();
        assert_eq!(parsed.exclusion.unwrap().scope, ExcludeScope::AllRows);
        builder.exclude_enabled = false;
        assert_eq!(builder.exclude.phase, "X");
        assert!(
            xonata_core::filter::FilterQuery::parse(&builder.query())
                .unwrap()
                .exclusion
                .is_none()
        );
        builder.interval = false;
        builder.exclude_enabled = true;
        builder.exclude.conditions[0].value = "bad".into();
        assert!(builder.validation_error().is_none());
        assert!(
            xonata_core::filter::FilterQuery::parse(&builder.query())
                .unwrap()
                .exclusion
                .is_none()
        );
    }

    #[test]
    fn changing_suggestion_fields_should_invalidate_replies_during_debounce() {
        let mut suggestions = Suggestions::default();
        suggestions.want(SuggestionField::Phase, "E", 0.0);
        let generation = suggestions.generation;
        suggestions.sent = suggestions.desired.clone();
        suggestions.want(SuggestionField::Metadata, "free *36", 0.05);
        suggestions.ingest(Event::FilterSuggestions {
            trace: 1,
            generation,
            field: SuggestionField::Phase,
            values: vec!["E".into()],
            done: true,
        });
        assert!(suggestions.values.is_empty());
        assert!(!suggestions.done);
        // Returning to an earlier value must issue a fresh request, rather than
        // retaining an invalidated generation with an empty suggestion list.
        suggestions.want(SuggestionField::Phase, "E", 0.1);
        assert!(suggestions.sent.is_none());
        assert_ne!(suggestions.generation, generation);
    }

    #[test]
    fn invalid_numeric_conditions_should_have_field_names_without_syntax_offsets() {
        let mut builder = Builder::default();
        builder.source.conditions.push(Condition {
            field: "duration",
            compare: ">=",
            value: "-1".into(),
        });
        assert_eq!(
            builder.validation_error().as_deref(),
            Some("From: Phase duration needs a nonnegative decimal integer.")
        );
        builder.source.conditions[0].value = u64::MAX.to_string();
        assert!(builder.validation_error().is_none());
    }
}
