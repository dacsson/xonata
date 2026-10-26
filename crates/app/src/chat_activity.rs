//! Compact latest-run activity. Details never enter the conversation sent to the model.
use crate::ui::{ACCENT, MUTED};
use xonata_agent::ActivityEvent;

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 64 * 1024;
pub(super) const DETAILS_HEIGHT: f32 = 144.0;

#[derive(Default)]
pub(super) struct Activity {
    entries: Vec<Entry>,
    pub expanded: bool,
    omitted: bool,
}

struct Entry {
    id: u64,
    turn: usize,
    tool: Option<String>,
    arguments: Option<String>,
    output: Option<String>,
    state: State,
    started: f64,
    ended: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Completed,
    Failed,
    Interrupted,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.tool.as_ref().map_or(0, String::len)
            + self.arguments.as_ref().map_or(0, String::len)
            + self.output.as_ref().map_or(0, String::len)
    }

    fn elapsed(&self, now: f64) -> f64 {
        (self.ended.unwrap_or(now) - self.started).max(0.0)
    }

    fn description(&self) -> String {
        let state = match self.state {
            State::Running if self.tool.is_none() => "waiting for model…",
            State::Running => "running…",
            State::Completed => "completed",
            State::Failed => "failed",
            State::Interrupted => "interrupted",
        };
        format!(
            "Turn {} · {} · {state}",
            self.turn,
            self.tool.as_deref().unwrap_or("Model"),
        )
    }

    fn label(&self, now: f64) -> String {
        format!("{} · {:.1}s", self.description(), self.elapsed(now))
    }
}

impl Activity {
    pub fn reset(&mut self) {
        self.entries.clear();
        self.omitted = false;
    }

    pub fn push(&mut self, event: ActivityEvent, now: f64) {
        match event {
            ActivityEvent::Started { id, turn, tool } => {
                let (tool, arguments) = match tool {
                    Some((name, args)) => (Some(name), Some(args)),
                    None => (None, None),
                };
                self.entries.push(Entry {
                    id,
                    turn,
                    tool,
                    arguments,
                    output: None,
                    state: State::Running,
                    started: now,
                    ended: None,
                });
            }
            ActivityEvent::Finished { id, failed, output } => {
                if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                    entry.output = output;
                    entry.state = if failed {
                        State::Failed
                    } else {
                        State::Completed
                    };
                    entry.ended = Some(now);
                }
            }
        }
        let mut bytes: usize = self.entries.iter().map(Entry::bytes).sum();
        while self.entries.len() > MAX_ENTRIES || bytes > MAX_BYTES {
            bytes -= self.entries.remove(0).bytes();
            self.omitted = true;
        }
    }

    pub fn settle(&mut self, now: f64) {
        for entry in &mut self.entries {
            if entry.state == State::Running {
                entry.state = State::Interrupted;
                entry.ended = Some(now);
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        trace: u32,
        status: &str,
        running: bool,
        details_height: f32,
    ) {
        let now = ui.input(|input| input.time);
        if !running {
            self.settle(now);
        }
        let current = self
            .entries
            .iter()
            .rev()
            .find(|entry| running && entry.state == State::Running);
        if current.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        let summary = if let Some(entry) = current {
            entry.description()
        } else if !status.is_empty() {
            status.to_owned()
        } else {
            self.entries
                .last()
                .map_or_else(String::new, |entry| entry.label(now))
        };
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.entries.is_empty(),
                    egui::Button::new(if self.expanded { "▾" } else { "▸" }).small(),
                )
                .on_hover_text("Show or hide agent activity")
                .clicked()
            {
                self.expanded = !self.expanded;
            }
            if let Some(entry) = current {
                ui.label(
                    egui::RichText::new(format!("{:.1}s", entry.elapsed(now)))
                        .small()
                        .monospace()
                        .color(ACCENT),
                );
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&summary)
                        .small()
                        .italics()
                        .color(if running { ACCENT } else { MUTED }),
                )
                .truncate(),
            )
            .on_hover_text(&summary);
        });
        // The parent reserves space on the frame following expansion.
        if self.expanded && details_height > 0.0 {
            egui::ScrollArea::vertical()
                .id_salt((trace, "agent-activity"))
                .max_height(details_height)
                .show(ui, |ui| {
                    if self.omitted {
                        ui.label(
                            egui::RichText::new("Earlier activity omitted (display limit)")
                                .small()
                                .color(MUTED),
                        );
                    }
                    for entry in &self.entries {
                        if let Some(args) = &entry.arguments {
                            egui::CollapsingHeader::new(entry.label(now))
                                .id_salt((trace, entry.id))
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new("Arguments").small().color(MUTED));
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(args).monospace())
                                            .wrap()
                                            .selectable(true),
                                    );
                                    if let Some(output) = &entry.output {
                                        ui.label(
                                            egui::RichText::new("Result (preview)")
                                                .small()
                                                .color(MUTED),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(output).monospace(),
                                            )
                                            .wrap()
                                            .selectable(true),
                                        );
                                    }
                                });
                        } else {
                            ui.label(egui::RichText::new(entry.label(now)).small().color(MUTED));
                        }
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: u64) -> ActivityEvent {
        ActivityEvent::Started {
            id,
            turn: 1,
            tool: Some(("trace_info".into(), "{}".into())),
        }
    }

    #[test]
    fn compact_timer_should_tick_reset_for_tools_and_freeze_on_completion() {
        let ctx = egui::Context::default();
        let mut activity = Activity::default();
        activity.push(
            ActivityEvent::Started {
                id: 1,
                turn: 1,
                tool: None,
            },
            1.0,
        );
        let render = |activity: &mut Activity, time| {
            ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        activity.show(ui, 1, "Analyzing…", true, 0.0);
                    });
                },
            )
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
        };
        assert!(render(&mut activity, 3.5).iter().any(|text| text == "2.5s"));
        assert!(render(&mut activity, 8.0).iter().any(|text| text == "7.0s"));
        activity.push(
            ActivityEvent::Finished {
                id: 1,
                failed: false,
                output: None,
            },
            8.0,
        );
        activity.push(started(2), 8.0);
        let labels = render(&mut activity, 9.2);
        assert!(labels.iter().any(|text| text == "1.2s"));
        assert!(labels.iter().any(|text| text.contains("trace_info")));
        activity.push(
            ActivityEvent::Finished {
                id: 2,
                failed: false,
                output: Some("{}".into()),
            },
            9.2,
        );
        assert!((activity.entries[1].elapsed(30.0) - 1.2).abs() < 1e-9);
    }

    #[test]
    fn outcomes_should_match_ids_and_terminal_states_should_interrupt_pending_work() {
        let mut activity = Activity::default();
        activity.push(started(1), 1.0);
        activity.push(started(2), 2.0);
        activity.push(
            ActivityEvent::Finished {
                id: 1,
                failed: true,
                output: Some("Invalid arguments".into()),
            },
            3.0,
        );
        activity.settle(4.0);
        assert!(activity.entries[0].state == State::Failed);
        assert_eq!(
            activity.entries[0].output.as_deref(),
            Some("Invalid arguments")
        );
        assert!(activity.entries[1].state == State::Interrupted);
        assert_eq!(activity.entries[1].ended, Some(4.0));
        activity.reset();
        assert!(activity.entries.is_empty());
    }

    #[test]
    fn retained_activity_should_bound_both_count_and_bytes() {
        let mut activity = Activity::default();
        for id in 0..200 {
            activity.push(started(id), 0.0);
        }
        assert_eq!(activity.entries.len(), MAX_ENTRIES);
        assert!(activity.omitted);
        for id in 200..300 {
            activity.push(
                ActivityEvent::Started {
                    id,
                    turn: 1,
                    tool: Some(("peek_rows".into(), "a".repeat(2048))),
                },
                0.0,
            );
            activity.push(
                ActivityEvent::Finished {
                    id,
                    failed: false,
                    output: Some("b".repeat(2048)),
                },
                1.0,
            );
        }
        assert!(activity.entries.iter().map(Entry::bytes).sum::<usize>() <= MAX_BYTES);
        assert_eq!(activity.entries.last().unwrap().id, 299);
    }

    #[test]
    fn activity_should_stay_compact_until_expanded_and_show_selectable_tool_details() {
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut activity = Activity::default();
        activity.push(started(1), 0.0);
        activity.push(
            ActivityEvent::Finished {
                id: 1,
                failed: false,
                output: Some("{\"count\":\"9007199254740993\"}".into()),
            },
            1.0,
        );
        let frame = |activity: &mut Activity, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        activity.show(ui, 1, "Analysis complete", false, DETAILS_HEIGHT)
                    });
                },
            )
        };
        let texts = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::epaint::Shape::Text(text) = &shape.shape {
                        Some((
                            text.galley.job.text.clone(),
                            text.pos + text.galley.size() * 0.5,
                        ))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let collapsed = frame(&mut activity, vec![]);
        assert!(
            !texts(&collapsed)
                .iter()
                .any(|(text, _)| text.contains("trace_info"))
        );
        let expand = texts(&collapsed)
            .into_iter()
            .find(|(text, _)| text == "▸")
            .unwrap()
            .1;
        let click = |pos, pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame(
            &mut activity,
            vec![egui::Event::PointerMoved(expand), click(expand, true)],
        );
        let _ = frame(&mut activity, vec![click(expand, false)]);
        let expanded = frame(&mut activity, vec![]);
        assert!(activity.expanded);
        let tool = texts(&expanded)
            .into_iter()
            .find(|(text, _)| text.contains("trace_info"))
            .unwrap()
            .1;
        let _ = frame(
            &mut activity,
            vec![egui::Event::PointerMoved(tool), click(tool, true)],
        );
        let _ = frame(&mut activity, vec![click(tool, false)]);
        let details = texts(&frame(&mut activity, vec![]));
        assert!(details.iter().any(|(text, _)| text == "Arguments"));
        assert!(details.iter().any(|(text, _)| text == "{}"));
        assert!(
            details
                .iter()
                .any(|(text, _)| text.contains("9007199254740993"))
        );
    }
}
