//! A single session-only timeline area copied as context for an agent question.
use crate::ui::ACCENT;
use egui::{self, Pos2, Rect, Response, Stroke, StrokeKind, Ui};
use xonata_core::{AreaBounds, Request, TraceInfo};
use xonata_view::{Marker, Viewport};

#[derive(Default)]
pub(crate) struct AgentArea {
    generation: u32,
    selection: Option<Selection>,
    anchor: Option<Marker>,
    cancelled_drag: bool,
}

struct Selection {
    first: Marker,
    last: Marker,
    hide_flushed: bool,
    resolved: Option<AreaBounds>,
    pending: bool,
    copied: bool,
}

pub(crate) struct AreaInput<'a> {
    pub view: &'a Viewport,
    pub rect: Rect,
    pub clip: Rect,
    pub info: &'a TraceInfo,
    pub hide_flushed: bool,
    pub blocked: bool,
}

impl AgentArea {
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.selection = None;
        self.cancelled_drag |= self.anchor.take().is_some();
    }

    pub fn dragging(&self) -> bool {
        self.anchor.is_some() || self.cancelled_drag
    }

    pub fn escape(&mut self, ctx: &egui::Context) {
        if self.selection.is_some()
            && ctx.input_mut(|i| i.consume_key(i.modifiers, egui::Key::Escape))
        {
            self.clear();
        }
    }

    pub fn resolve(&mut self, generation: u32, bounds: Option<AreaBounds>) {
        if generation == self.generation
            && let Some(selection) = &mut self.selection
        {
            selection.resolved = bounds;
            selection.pending = false;
        }
    }

    pub fn interact(
        &mut self,
        ui: &Ui,
        response: &Response,
        input: &AreaInput<'_>,
    ) -> Option<Request> {
        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.hide_flushed != input.hide_flushed)
        {
            self.clear();
        }
        let ctrl = ui.input(|i| i.modifiers.ctrl && !i.modifiers.shift && !i.modifiers.alt);
        if response.drag_started_by(egui::PointerButton::Primary)
            && ctrl
            && !input.blocked
            && input.info.complete
            && input.row_count() > 0
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
            && input.clip.contains(origin)
            && input.view.row_at(origin.y - input.rect.top()) < input.row_count()
        {
            self.clear();
            self.cancelled_drag = false;
            let anchor = input.point(origin);
            self.anchor = Some(anchor);
            self.selection = Some(Selection {
                first: anchor,
                last: anchor,
                hide_flushed: input.hide_flushed,
                resolved: None,
                pending: true,
                copied: false,
            });
        }
        if let Some(anchor) = self.anchor
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(selection) = &mut self.selection
        {
            let end = input.point(input.clip.clamp(pos));
            selection.first = Marker {
                cycle: anchor.cycle.min(end.cycle),
                row: anchor.row.min(end.row),
            };
            selection.last = Marker {
                cycle: anchor.cycle.max(end.cycle),
                row: anchor.row.max(end.row),
            };
        }
        if response.drag_stopped_by(egui::PointerButton::Primary)
            || !ui.input(|i| i.pointer.primary_down())
        {
            self.cancelled_drag = false;
            if self.anchor.take().is_some()
                && let Some(selection) = &self.selection
            {
                return Some(Request::AreaBounds {
                    trace: input.info.id,
                    generation: self.generation,
                    start: selection.first.row,
                    end: selection.last.row,
                    hide_flushed: selection.hide_flushed,
                });
            }
        }
        None
    }

    pub fn show(&mut self, ui: &Ui, input: &AreaInput<'_>) {
        let Some(selection) = &mut self.selection else {
            return;
        };
        let rect = Rect::from_min_max(
            Pos2::new(
                input.rect.left() + input.view.x(selection.first.cycle),
                input.rect.top() + input.view.y(selection.first.row),
            ),
            Pos2::new(
                input.rect.left() + input.view.x(selection.last.cycle.saturating_add(1)),
                input.rect.top() + input.view.y(selection.last.row.saturating_add(1)),
            ),
        )
        .intersect(input.clip);
        let painter = ui.painter().with_clip_rect(input.clip);
        if rect.is_positive() {
            if self.anchor.is_some() {
                painter.rect_filled(rect, 0.0, ACCENT.gamma_multiply(0.16));
            }
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
        }
        if self.anchor.is_some() {
            return;
        }
        let mut clear = false;
        egui::Area::new(egui::Id::new((input.info.id, "agent-area")))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(input.clip.left_bottom() + egui::vec2(8.0, -8.0))
            .movable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                selection.resolved.is_some(),
                                egui::Button::new("Copy for agent"),
                            )
                            .clicked()
                        {
                            ui.ctx().copy_text(selection.context(&input.info.name));
                            selection.copied = true;
                        }
                        clear = ui.button("Clear").clicked();
                        if selection.copied {
                            ui.label("Copied");
                        }
                    });
                    ui.label(format!(
                        "Cycles {}–{} (inclusive)",
                        selection.first.cycle, selection.last.cycle
                    ));
                    if let Some(bounds) = selection.resolved {
                        ui.label(format!(
                            "Operation IDs {}–{}",
                            bounds.first_id, bounds.last_id
                        ));
                    } else {
                        ui.label(if selection.pending {
                            "Resolving operation bounds…"
                        } else {
                            "No operations at the selected rows. Select again."
                        });
                    }
                });
            });
        if clear {
            self.clear();
        }
    }
}

impl AreaInput<'_> {
    fn row_count(&self) -> u64 {
        self.info.count.saturating_sub(if self.hide_flushed {
            self.info.flushed
        } else {
            0
        })
    }

    fn point(&self, pos: Pos2) -> Marker {
        Marker {
            cycle: self.view.cycle_at(pos.x - self.rect.left()).clamp(
                self.info.first_cycle,
                self.info.last_cycle.max(self.info.first_cycle),
            ),
            row: self
                .view
                .row_at(pos.y - self.rect.top())
                .min(self.row_count().saturating_sub(1)),
        }
    }
}

impl Selection {
    fn context(&self, name: &str) -> String {
        let Some(bounds) = self.resolved else {
            return String::new();
        };
        format!(
            "Selected area in trace {name:?}:\nCycles: {}–{} (inclusive).\nOperation IDs: {}–{} (inclusive interval; IDs may have gaps).\nUnfiltered rows: {}–{} (inclusive, zero-based).\nFlushed operations: {}.\nAnalyze these rows within the selected cycles; not every operation necessarily has a stage overlapping this cycle interval.",
            self.first.cycle,
            self.last.cycle,
            bounds.first_id,
            bounds.last_id,
            bounds.first_row,
            bounds.last_row,
            if self.hide_flushed {
                "hidden; exclude them from this area"
            } else {
                "included"
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 9_007_199_254_740_993;

    fn selected() -> AgentArea {
        AgentArea {
            generation: 5,
            selection: Some(Selection {
                first: Marker {
                    cycle: BASE + 1,
                    row: 1,
                },
                last: Marker {
                    cycle: BASE + 3,
                    row: 3,
                },
                hide_flushed: false,
                resolved: None,
                pending: true,
                copied: false,
            }),
            ..Default::default()
        }
    }

    fn bounds() -> AreaBounds {
        AreaBounds {
            first_id: BASE,
            last_id: BASE + 9,
            first_row: 1,
            last_row: 3,
        }
    }

    fn frame(
        ctx: &egui::Context,
        area: &mut AgentArea,
        events: Vec<egui::Event>,
        hide_flushed: bool,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(800.0, 600.0),
                            egui::Sense::click_and_drag(),
                        );
                        let input = AreaInput {
                            view: &Viewport {
                                cycle: BASE,
                                ..Default::default()
                            },
                            rect,
                            clip: rect,
                            info: &TraceInfo {
                                id: 1,
                                name: "area.kanata".into(),
                                count: 8,
                                first_cycle: BASE,
                                last_cycle: BASE + 10,
                                complete: true,
                                ..Default::default()
                            },
                            hide_flushed,
                            blocked: false,
                        };
                        area.interact(ui, &response, &input);
                        area.show(ui, &input);
                    });
            },
        )
    }

    #[test]
    fn copied_context_preserves_exact_bounds_and_visibility() {
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut area = selected();
        area.resolve(4, Some(bounds()));
        assert!(area.selection.as_ref().unwrap().resolved.is_none());
        area.resolve(5, Some(bounds()));
        let _ = frame(&ctx, &mut area, vec![], false);
        let output = frame(&ctx, &mut area, vec![], false);
        let copy = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == "Copy for agent" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("Copy for agent button");
        let _ = frame(
            &ctx,
            &mut area,
            vec![egui::Event::PointerMoved(copy)],
            false,
        );
        let click = |pressed| egui::Event::PointerButton {
            pos: copy,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame(&ctx, &mut area, vec![click(true)], false);
        let output = frame(&ctx, &mut area, vec![click(false)], false);
        let text = output
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text),
                _ => None,
            })
            .expect("clipboard output");
        assert_eq!(
            text,
            &format!(
                "Selected area in trace \"area.kanata\":\nCycles: {}–{} (inclusive).\nOperation IDs: {}–{} (inclusive interval; IDs may have gaps).\nUnfiltered rows: 1–3 (inclusive, zero-based).\nFlushed operations: included.\nAnalyze these rows within the selected cycles; not every operation necessarily has a stage overlapping this cycle interval.",
                BASE + 1,
                BASE + 3,
                BASE,
                BASE + 9
            )
        );
        let selection = area.selection.as_mut().unwrap();
        selection.hide_flushed = true;
        assert!(selection.context("area").contains("hidden; exclude them"));
    }

    #[test]
    fn clearing_and_visibility_changes_ignore_late_bounds() {
        let ctx = egui::Context::default();
        let mut area = selected();
        let _ = frame(&ctx, &mut area, vec![], true);
        area.resolve(5, Some(bounds()));
        assert!(area.selection.is_none());
        let mut area = selected();
        area.clear();
        area.resolve(5, Some(bounds()));
        assert!(area.selection.is_none());
    }

    #[test]
    fn escape_cancels_a_ctrl_drag_without_accepting_its_late_reply() {
        let ctx = egui::Context::default();
        let mut area = selected();
        area.anchor = Some(Marker {
            cycle: BASE,
            row: 0,
        });
        let _ = ctx.run(
            egui::RawInput {
                modifiers: egui::Modifiers::CTRL,
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                }],
                ..Default::default()
            },
            |ctx| area.escape(ctx),
        );
        area.resolve(5, Some(bounds()));
        assert!(area.selection.is_none());
        assert!(
            area.dragging(),
            "Keep suppressing pan until the pointer is released"
        );
        let _ = frame(&ctx, &mut area, vec![], false);
        assert!(!area.dragging());
    }

    #[test]
    fn grid_mapping_clamps_cycles_and_rows_without_losing_integer_precision() {
        let view = Viewport {
            cycle: BASE,
            cycle_fraction: 0.25,
            ..Default::default()
        };
        let input = AreaInput {
            view: &view,
            rect: Rect::from_min_size(Pos2::new(20.0, 32.0), egui::vec2(800.0, 600.0)),
            clip: Rect::EVERYTHING,
            info: &TraceInfo {
                count: 8,
                flushed: 3,
                first_cycle: BASE,
                last_cycle: BASE + 10,
                ..Default::default()
            },
            hide_flushed: true,
            blocked: false,
        };
        assert_eq!(
            input.point(Pos2::new(20.0 + view.cycle_width * 3.0, 32.0)),
            Marker {
                cycle: BASE + 3,
                row: 0
            }
        );
        assert_eq!(
            input.point(Pos2::new(-1000.0, -1000.0)),
            Marker {
                cycle: BASE,
                row: 0
            }
        );
        assert_eq!(
            input.point(Pos2::new(100_000.0, 100_000.0)),
            Marker {
                cycle: BASE + 10,
                row: 4
            }
        );
    }
}
