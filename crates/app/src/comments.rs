//! Session-only comments anchored to the trace grid.
use std::sync::Arc;

use egui::{Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use xonata_view::{Marker, Viewport};

use crate::ui::{ACCENT, BORDER, MUTED, PANEL, TEXT};

#[derive(Default)]
pub(crate) struct Comments {
    entries: Vec<Comment>,
    next_id: u64,
    pub placing: bool,
    editor: Option<Editor>,
}

struct Comment {
    id: u64,
    anchor: Marker,
    text: String,
    preview: String,
}

struct Editor {
    id: Option<u64>,
    anchor: Marker,
    text: String,
    focus: bool,
}

pub(crate) struct Bubbles {
    items: Vec<Bubble>,
    pub hovered: bool,
}

struct Bubble {
    id: u64,
    anchor: Pos2,
    rect: Rect,
    text: Arc<egui::Galley>,
}

impl Comments {
    pub fn editing(&self) -> bool {
        self.editor.is_some()
    }

    pub fn begin(&mut self, anchor: Marker) {
        self.placing = false;
        self.editor = Some(Editor {
            id: None,
            anchor,
            text: String::new(),
            focus: true,
        });
    }

    pub fn prepare(
        &mut self,
        ui: &mut egui::Ui,
        view: &Viewport,
        rows: Rect,
        clip: Rect,
        trace: u32,
    ) -> Bubbles {
        let mut bubbles = Bubbles {
            items: Vec::new(),
            hovered: false,
        };
        if clip.width() < 40.0 || clip.height() < 40.0 {
            return bubbles;
        }
        let interactive = !self.editing() && !self.placing;
        let mut edit = None;
        let mut remove = None;
        for comment in &self.entries {
            let anchor = Pos2::new(
                rows.left() + view.x(comment.anchor.cycle),
                rows.top() + view.y(comment.anchor.row),
            );
            if !clip.contains(anchor) {
                continue;
            }
            // Layout only visible comments, with bounded previews even for long pasted text.
            let mut job = egui::text::LayoutJob::simple(
                comment.preview.clone(),
                egui::FontId::monospace(13.0),
                TEXT,
                (clip.width() - 32.0).min(240.0),
            );
            job.wrap.max_rows = 6;
            let text = ui.painter().layout_job(job);
            let size = (text.size() + Vec2::splat(16.0)).min(clip.size() - Vec2::splat(8.0));
            let offset = |value: f32, size: f32, min: f32, max: f32| {
                let wanted = if value + 18.0 + size > max - 4.0 {
                    value - size - 18.0
                } else {
                    value + 18.0
                };
                wanted.clamp(min + 4.0, max - size - 4.0)
            };
            let min = Pos2::new(
                offset(anchor.x, size.x, clip.left(), clip.right()),
                offset(anchor.y, size.y, clip.top(), clip.bottom()),
            );
            let rect = Rect::from_min_size(min, size);
            if interactive {
                let response = ui
                    .interact(
                        rect,
                        egui::Id::new((trace, "comment", comment.id)),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!(
                        "Cycle {} · row {}\nClick to edit · Delete removes",
                        comment.anchor.cycle, comment.anchor.row
                    ));
                if response.clicked() {
                    edit = Some(comment.id);
                }
                if response.hovered() {
                    bubbles.hovered = true;
                    remove = Some(comment.id);
                }
            }
            bubbles.items.push(Bubble {
                id: comment.id,
                anchor,
                rect,
                text,
            });
        }
        if let Some(id) = remove
            && !ui.ctx().wants_keyboard_input()
            && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Delete))
        {
            self.entries.retain(|comment| comment.id != id);
            bubbles.items.retain(|bubble| bubble.id != id);
        } else if let Some(comment) = edit.and_then(|id| self.entries.iter().find(|c| c.id == id)) {
            self.editor = Some(Editor {
                id: Some(comment.id),
                anchor: comment.anchor,
                text: comment.text.clone(),
                focus: true,
            });
        }
        bubbles
    }

    pub fn show_editor(&mut self, ctx: &egui::Context, trace: u32) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let mut remove = false;
        egui::Window::new("Comment")
            .id(egui::Id::new((trace, "comment-editor")))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width((ctx.screen_rect().width() - 80.0).clamp(180.0, 440.0));
                ui.label(
                    egui::RichText::new(format!(
                        "Cycle {} · row {}",
                        editor.anchor.cycle, editor.anchor.row
                    ))
                    .color(MUTED),
                );
                let input = egui::ScrollArea::vertical()
                    .id_salt((trace, "comment-scroll"))
                    .max_height((ctx.screen_rect().height() - 220.0).clamp(80.0, 240.0))
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut editor.text)
                                .id(egui::Id::new((trace, "comment-text")))
                                .font(egui::TextStyle::Monospace)
                                .hint_text("Write or paste a comment…")
                                .desired_width(ui.available_width())
                                .desired_rows(5)
                                .char_limit(16_384)
                                .return_key(egui::KeyboardShortcut::new(
                                    Modifiers::SHIFT,
                                    Key::Enter,
                                )),
                        )
                    })
                    .inner;
                if editor.focus {
                    input.request_focus();
                    editor.focus = false;
                }
                // Consume after TextEdit so pasted text in this frame is included in the save.
                save = ui.input_mut(|input| {
                    // consume_key ignores extra Shift/Alt modifiers; only plain Enter saves.
                    let enter = input.events.iter().position(|event| {
                        matches!(event,
                        egui::Event::Key { key: Key::Enter, pressed: true, modifiers, .. }
                            if *modifiers == Modifiers::NONE)
                    });
                    if let Some(index) = enter {
                        input.events.remove(index);
                        true
                    } else {
                        false
                    }
                });
                cancel = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                ui.label(
                    egui::RichText::new("Enter saves · Shift+Enter newline · Esc cancels")
                        .small()
                        .color(MUTED),
                );
                ui.horizontal(|ui| {
                    save |= ui
                        .add_enabled(!editor.text.trim().is_empty(), egui::Button::new("Save"))
                        .clicked();
                    cancel |= ui.button("Cancel").clicked();
                    if editor.id.is_some() {
                        remove = ui.button("Delete").clicked();
                    }
                });
            });
        if cancel || remove {
            if remove {
                let id = editor.id;
                self.entries.retain(|comment| Some(comment.id) != id);
            }
            self.editor = None;
        } else if save && !editor.text.trim().is_empty() {
            let id = editor.id.unwrap_or_else(|| {
                let id = self.next_id;
                self.next_id += 1;
                id
            });
            let text = std::mem::take(&mut editor.text);
            let mut preview: String = text.chars().take(1024).collect();
            if text.chars().count() > 1024 {
                preview.push('…');
            }
            let comment = Comment {
                id,
                anchor: editor.anchor,
                text,
                preview,
            };
            if let Some(existing) = self.entries.iter_mut().find(|comment| comment.id == id) {
                *existing = comment;
            } else {
                self.entries.push(comment);
            }
            self.editor = None;
        }
    }
}

impl Bubbles {
    pub fn paint(&self, painter: &egui::Painter, clip: Rect) {
        let painter = painter.with_clip_rect(clip);
        for bubble in &self.items {
            painter.line_segment(
                [bubble.anchor, bubble.rect.clamp(bubble.anchor)],
                Stroke::new(1.5_f32, ACCENT),
            );
            painter.circle_filled(bubble.anchor, 3.0, ACCENT);
            painter.rect_filled(
                bubble.rect,
                0.0,
                Color32::from_rgba_unmultiplied(PANEL.r(), PANEL.g(), PANEL.b(), 245),
            );
            painter.rect_stroke(
                bubble.rect,
                0.0,
                Stroke::new(1.0_f32, BORDER),
                StrokeKind::Inside,
            );
            painter.with_clip_rect(bubble.rect.shrink(4.0)).galley(
                bubble.rect.min + Vec2::splat(8.0),
                bubble.text.clone(),
                TEXT,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn frame(
        ctx: &egui::Context,
        comments: &mut Comments,
        view: &Viewport,
        events: Vec<egui::Event>,
    ) -> Bubbles {
        let mut bubbles = Bubbles {
            items: Vec::new(),
            hovered: false,
        };
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let rows = ui.max_rect();
                    bubbles = comments.prepare(ui, view, rows, rows, 1);
                    bubbles.paint(ui.painter(), rows);
                });
                comments.show_editor(ctx, 1);
            },
        );
        bubbles
    }

    fn save(ctx: &egui::Context, comments: &mut Comments, view: &Viewport, text: &str) {
        for _ in 0..3 {
            frame(ctx, comments, view, Vec::new());
        }
        frame(ctx, comments, view, vec![egui::Event::Paste(text.into())]);
        frame(ctx, comments, view, vec![key(Key::Enter, Modifiers::NONE)]);
    }

    #[test]
    fn pasted_comment_should_save_on_enter_and_keep_newlines() {
        let ctx = egui::Context::default();
        let mut comments = Comments::default();
        let view = Viewport::default();
        let anchor = Marker { cycle: 3, row: 4 };
        comments.begin(anchor);
        for _ in 0..3 {
            frame(&ctx, &mut comments, &view, Vec::new());
        }
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![egui::Event::Paste("vfirst → vmsne\nFREE RQU *=36".into())],
        );
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![key(Key::Enter, Modifiers::SHIFT)],
        );
        assert!(comments.editing(), "Shift+Enter must keep editing");
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![egui::Event::Text("Delay: 4 cycles".into())],
        );
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![key(Key::Enter, Modifiers::NONE)],
        );
        assert!(!comments.editing());
        assert_eq!(comments.entries.len(), 1);
        assert_eq!(comments.entries[0].anchor, anchor);
        assert_eq!(
            comments.entries[0].text,
            "vfirst → vmsne\nFREE RQU *=36\nDelay: 4 cycles"
        );
        assert_eq!(frame(&ctx, &mut comments, &view, Vec::new()).items.len(), 1);
    }

    #[test]
    fn bubble_click_should_edit_in_place_and_escape_should_preserve_original() {
        let ctx = egui::Context::default();
        let mut comments = Comments::default();
        let view = Viewport::default();
        comments.begin(Marker { cycle: 2, row: 2 });
        save(&ctx, &mut comments, &view, "Original");
        frame(&ctx, &mut comments, &view, Vec::new());
        let pos = frame(&ctx, &mut comments, &view, Vec::new()).items[0]
            .rect
            .center();
        for pressed in [true, false] {
            frame(
                &ctx,
                &mut comments,
                &view,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(comments.editor.as_ref().unwrap().text, "Original");
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![egui::Event::Paste(" changed".into())],
        );
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![key(Key::Escape, Modifiers::NONE)],
        );
        assert_eq!(comments.entries[0].text, "Original");
        // Editing preserves the anchor and identity instead of creating another comment.
        comments.editor = Some(Editor {
            id: Some(0),
            anchor: comments.entries[0].anchor,
            text: String::new(),
            focus: true,
        });
        save(&ctx, &mut comments, &view, "Updated");
        assert_eq!(comments.entries.len(), 1);
        assert_eq!(comments.entries[0].text, "Updated");
        assert_eq!(comments.entries[0].anchor, Marker { cycle: 2, row: 2 });
    }

    #[test]
    fn hovered_bubble_delete_should_remove_only_that_comment() {
        let ctx = egui::Context::default();
        let mut comments = Comments::default();
        let view = Viewport::default();
        for anchor in [Marker { cycle: 1, row: 1 }, Marker { cycle: 6, row: 6 }] {
            comments.begin(anchor);
            save(&ctx, &mut comments, &view, "Comment");
        }
        frame(&ctx, &mut comments, &view, Vec::new());
        let pos = frame(&ctx, &mut comments, &view, Vec::new()).items[0]
            .rect
            .center();
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![egui::Event::PointerMoved(pos)],
        );
        frame(
            &ctx,
            &mut comments,
            &view,
            vec![key(Key::Delete, Modifiers::NONE)],
        );
        assert_eq!(comments.entries.len(), 1);
        assert_eq!(comments.entries[0].anchor, Marker { cycle: 6, row: 6 });
    }

    #[test]
    fn bubbles_should_follow_precise_grid_origins_and_only_layout_visible_previews() {
        let ctx = egui::Context::default();
        let mut comments = Comments::default();
        let mut view = Viewport {
            cycle: u64::MAX - 100,
            ..Default::default()
        };
        let anchor = Marker {
            cycle: view.cycle + 2,
            row: 3,
        };
        comments.begin(anchor);
        let full = "測試\n".repeat(4000);
        save(&ctx, &mut comments, &view, &full);
        let a = frame(&ctx, &mut comments, &view, Vec::new());
        assert_eq!(comments.entries[0].text, full);
        assert!(a.items[0].text.rows.len() <= 6);
        assert!(a.items[0].text.job.text.chars().count() <= 1025);
        view.cycle += 1;
        view.cycle_width /= 2.0;
        view.row = 1.0;
        view.row_height /= 2.0;
        let b = frame(&ctx, &mut comments, &view, Vec::new());
        assert_eq!(
            a.items[0].anchor - b.items[0].anchor,
            Vec2::new(108.0, 88.0)
        );
        assert!(!b.items[0].rect.contains(b.items[0].anchor));
        view.row = 10.0;
        assert!(
            frame(&ctx, &mut comments, &view, Vec::new())
                .items
                .is_empty()
        );
    }
}
