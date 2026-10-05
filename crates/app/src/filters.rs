//! Session-only filter editor, paged results, and query-generated drawings.
use crate::filter_builder::{Builder, ResultKind, Suggestions};
use crate::ui::Transport;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use std::collections::{BTreeMap, BTreeSet};
use xonata_core::filter::{DrawOptions, FilterBounds, FilterHit};
use xonata_core::{Event, Request, TraceInfo};
use xonata_view::Viewport;

pub(crate) struct Filters {
    pub open: bool,
    pub results_tab: bool,
    pub builder: Builder,
    suggestions: Suggestions,
    ran_delays: bool,
    pub label: String,
    pub generation: u32,
    pub running: bool,
    pub done: bool,
    pub total: u64,
    pub skipped: u64,
    pub progress: f32,
    pub result_revision: u32,
    pub descending: Option<bool>,
    pub sorting: bool,
    pub error: String,
    pub results: BTreeMap<u64, FilterHit>,
    requested: BTreeSet<u64>,
    pub selected: Option<u64>,
    pub drawn: Option<u32>,
    pub drawing_options: DrawOptions,
    pub drawing_visible: bool,
    pub drawing_hits: Vec<FilterHit>,
    pub visible_count: u64,
    viewport: Option<(u32, FilterBounds, Option<u64>)>,
    desired: Option<FilterBounds>,
    changed_at: f64,
    request: u32,
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            open: false,
            results_tab: false,
            builder: Builder::default(),
            suggestions: Suggestions::default(),
            ran_delays: false,
            label: String::new(),
            generation: 0,
            running: false,
            done: false,
            total: 0,
            skipped: 0,
            progress: 0.0,
            result_revision: 0,
            descending: None,
            sorting: false,
            error: String::new(),
            results: BTreeMap::new(),
            requested: BTreeSet::new(),
            selected: None,
            drawn: None,
            drawing_options: DrawOptions::default(),
            drawing_visible: true,
            drawing_hits: Vec::new(),
            visible_count: 0,
            viewport: None,
            desired: None,
            changed_at: 0.0,
            request: 0,
        }
    }
}
impl Filters {
    pub fn ingest(&mut self, event: Event) {
        match event {
            event @ Event::FilterSuggestions { .. } => self.suggestions.ingest(event),
            Event::FilterProgress {
                generation,
                total,
                skipped,
                progress,
                done,
                ..
            } if generation == self.generation => {
                self.total = total;
                self.skipped = skipped;
                self.progress = progress;
                self.running = !done;
                self.done = done;
            }
            Event::FilterError {
                generation,
                message,
                ..
            } if generation == self.generation => {
                self.error = if message.starts_with("Filter at byte ") {
                    message
                        .split_once(": ")
                        .map_or_else(|| message.clone(), |(_, detail)| detail.to_owned())
                } else {
                    message
                };
                self.running = false;
                self.done = false;
            }
            Event::FilterResults {
                generation,
                start,
                hits,
                ..
            } if generation == self.generation && self.descending.is_none() => {
                self.cache_results(start, hits);
            }
            Event::SortedFilterResults {
                generation,
                revision,
                start,
                hits,
                ..
            } if generation == self.generation
                && revision == self.result_revision
                && self.descending.is_some() =>
            {
                self.cache_results(start, hits);
            }
            Event::FilterSortProgress {
                generation,
                revision,
                total,
                done,
                ..
            } if generation == self.generation && revision == self.result_revision => {
                self.total = total;
                self.sorting = !done;
            }
            Event::FilterDrawn {
                generation,
                options,
                ..
            } if generation == self.generation => {
                self.drawn = Some(generation);
                self.drawing_options = options;
                self.drawing_visible = true;
                self.drawing_hits.clear();
                self.viewport = None;
            }
            Event::FilterDrawing {
                generation,
                request,
                hits,
                visible,
                ..
            } if self.drawn == Some(generation) && request == self.request => {
                self.drawing_hits = hits;
                self.visible_count = visible;
            }
            _ => {}
        }
    }
    fn cache_results(&mut self, start: u64, hits: Vec<FilterHit>) {
        self.requested.remove(&start);
        for (offset, hit) in hits.into_iter().enumerate() {
            self.results.insert(start + offset as u64, hit);
        }
        while self.results.len() > 512 {
            let first = self.results.first_key_value().map(|(k, _)| *k);
            let last = self.results.last_key_value().map(|(k, _)| *k);
            let key = match (first, last) {
                (Some(a), Some(b)) if start.abs_diff(a) > start.abs_diff(b) => a,
                (_, Some(b)) => b,
                _ => break,
            };
            self.results.remove(&key);
        }
    }
    fn displayed_total(&self) -> u64 {
        if self.sorting { 0 } else { self.total }
    }
    pub fn sort(&mut self, trace: u32, transport: &mut dyn Transport) {
        self.result_revision = self.result_revision.wrapping_add(1);
        let descending = !self.descending.unwrap_or(false);
        self.descending = Some(descending);
        self.sorting = true;
        self.results.clear();
        self.requested.clear();
        transport.request(Request::FilterSort {
            trace,
            generation: self.generation,
            revision: self.result_revision,
            descending,
        });
    }
    pub fn run(&mut self, trace: u32, transport: &mut dyn Transport) {
        if let Some(error) = self.builder.validation_error() {
            self.error = error;
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.results.clear();
        self.requested.clear();
        self.selected = None;
        self.total = 0;
        self.skipped = 0;
        self.progress = 0.0;
        self.done = false;
        self.running = true;
        self.error.clear();
        self.result_revision = self.result_revision.wrapping_add(1);
        self.descending = None;
        self.sorting = false;
        self.results_tab = true;
        self.ran_delays = self.builder.interval && self.builder.kind == ResultKind::Delays;
        transport.request(Request::Filter {
            trace,
            generation: self.generation,
            text: self.builder.query(),
            skip: self.builder.skip,
        });
    }
    fn request_page(&mut self, trace: u32, index: u64, transport: &mut dyn Transport) {
        let start = index / 128 * 128;
        if self.requested.insert(start) {
            if self.descending.is_some() {
                transport.request(Request::SortedFilterResults {
                    trace,
                    generation: self.generation,
                    revision: self.result_revision,
                    start,
                    count: 128,
                });
            } else {
                transport.request(Request::FilterResults {
                    trace,
                    generation: self.generation,
                    start,
                    count: 128,
                });
            }
        }
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        info: &TraceInfo,
        canvas: Rect,
        transport: &mut dyn Transport,
    ) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new(format!("Filters · {}", info.name))
            .id(egui::Id::new((info.id, "filters-window")))
            .open(&mut open).collapsible(false)
            .default_size([600.0, 420.0]).min_size([400.0, 220.0])
            .default_pos(Pos2::new((canvas.right() - 650.0).max(canvas.left()), canvas.top() + 48.0))
            .constrain_to(canvas)
            .show(ctx, |ui| {
                let results_tab = self.results_tab;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.results_tab, false, "Query");
                    ui.selectable_value(&mut self.results_tab, true, "Results");
                });
                ui.add_space(6.0);
                if !results_tab {
                    egui::ScrollArea::vertical().id_salt((info.id, "filter-builder"))
                        .auto_shrink([false, true])
                        .max_height((canvas.height() - 170.0).clamp(100.0, 360.0)).show(ui, |ui| {
                        self.builder.show(ui, &mut self.suggestions);
                    });
                }
                self.suggestions.tick(ui, info.id, info.complete, transport);
                ui.horizontal(|ui| {
                    if ui.add_enabled(info.complete && !self.running, egui::Button::new("Run")).clicked() { self.run(info.id, transport); }
                    if ui.add_enabled(self.running, egui::Button::new("Cancel")).clicked() {
                        transport.request(Request::CancelFilter { trace: info.id, generation: self.generation });
                        self.generation = self.generation.wrapping_add(1); self.running = false; self.done = false;
                        self.total = 0; self.results.clear(); self.requested.clear(); self.selected = None;
                    }
                    if !info.complete { ui.label("Wait for loading to finish"); }
                    if self.running { ui.add(egui::ProgressBar::new(self.progress).desired_width(100.0)); }
                });
                if !self.error.is_empty() { ui.colored_label(Color32::from_rgb(231, 140, 137), &self.error); }
                ui.horizontal(|ui| {
                    ui.label("Label");
                    ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(180.0).hint_text("Optional Draw label"));
                    if ui.add_enabled(self.done && !self.running, egui::Button::new("Draw")).clicked() {
                        transport.request(Request::DrawFilter { trace: info.id, generation: self.generation, label: self.label.clone() });
                    }
                });
                if self.drawn.is_some() {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut self.drawing_visible, "Show drawings").changed() { self.viewport = None; }
                        if ui.button("Clear drawings").clicked() {
                            transport.request(Request::ClearFilterDrawing { trace: info.id });
                            self.drawn = None; self.drawing_hits.clear(); self.visible_count = 0; self.viewport = None;
                        }
                    });
                    if self.visible_count > 2048 { ui.label(format!("Showing 2,048 of {} visible rectangles; zoom in for more", self.visible_count)); }
                }
                if results_tab {
                    ui.horizontal(|ui| {
                        ui.label(format!("{} results{}", self.total, if self.done { "" } else { " so far" }));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let text = if self.descending == Some(false) { "Elapsed cycles ↑" } else { "Elapsed cycles ↓" };
                            if ui.add_enabled(self.done && !self.running, egui::Button::new(text))
                                .on_hover_text(if self.descending == Some(true) { "Sort by elapsed cycles: lowest first" } else { "Sort by elapsed cycles: highest first" }).clicked() {
                                self.sort(info.id, transport);
                            }
                        });
                    });
                    if self.skipped > 0 { ui.label(format!("{} unknown END endpoints skipped", self.skipped)); }
                    if self.sorting { ui.label("Sorting elapsed cycles…"); }
                    if self.done && self.total == 0 {
                        ui.label(if self.ran_delays { "No nonnegative delays found. Select Delays and overlaps to check overlapping pairs, or revise the conditions." }
                            else { "No matches. Check instruction / phase suggestions and try fewer conditions. Phase and instruction must belong to the same row." });
                    }
                    ui.separator();
                    egui::ScrollArea::vertical().id_salt((info.id, "filter-results", self.generation, self.result_revision))
                        .auto_shrink([false, false])
                        .max_height(ui.available_height().max(100.0)).show_rows(ui, 26.0,
                            self.displayed_total().min(usize::MAX as u64) as usize, |ui, range| {
                            for index in range {
                                let index = index as u64;
                                if let Some(hit) = self.results.get(&index) {
                                    let pair = hit.target.as_ref().map_or_else(|| format!("Op {} / {}", hit.source.op, hit.source.phase),
                                        |t| format!("Op {} / {} → Op {} / {}", hit.source.op, hit.source.phase, t.op, t.phase));
                                    let value = if hit.target.is_some() && hit.elapsed < 0 { format!("{} cycles overlap", -hit.elapsed) }
                                        else { format!("{} cycles{}", hit.elapsed, if hit.source.open && hit.target.is_none() { " (open)" } else { "" }) };
                                    let tooltip = description(hit);
                                    if result_row(ui, self.selected == Some(hit.index), &hit.source.instruction, &format!("#{}  {pair}", hit.index + 1), &value)
                                        .on_hover_text(tooltip).clicked() {
                                        self.selected = Some(hit.index);
                                        self.viewport = None;
                                        transport.request(Request::RevealFilter { trace: info.id, generation: self.generation, index: hit.index });
                                    }
                                } else {
                                    self.request_page(info.id, index, transport);
                                    ui.add_sized([ui.available_width(), 26.0], egui::Label::new(format!("#{}  Loading…", index + 1)));
                                }
                            }
                        });
                }
            });
        self.open = open;
    }
    pub fn drawings(
        &mut self,
        ui: &egui::Ui,
        trace: &TraceInfo,
        view: &Viewport,
        rect: Rect,
        label_clip: Rect,
        transport: &mut dyn Transport,
    ) {
        let Some(generation) = self.drawn.filter(|_| self.drawing_visible) else {
            return;
        };
        let bounds = FilterBounds {
            cycle_start: view.cycle_at(0.0),
            cycle_end: view.cycle_at(rect.width()).saturating_add(1),
            row_start: view.row_at(0.0),
            row_end: view.row_at(rect.height()).saturating_add(1),
        };
        let time = ui.input(|i| i.time);
        if self.desired.is_none_or(|b| !same_bounds(b, bounds)) {
            self.desired = Some(bounds);
            self.changed_at = time;
        }
        let selected = if generation == self.generation {
            self.selected
        } else {
            None
        };
        if (self
            .viewport
            .is_none_or(|(g, b, s)| g != generation || !same_bounds(b, bounds) || s != selected))
            && (self.viewport.is_none() || time - self.changed_at >= 0.06)
        {
            self.request = self.request.wrapping_add(1);
            self.viewport = Some((generation, bounds, selected));
            transport.request(Request::FilterViewport {
                trace: trace.id,
                generation,
                request: self.request,
                bounds,
                selected: selected.map(|s| s.to_string()),
            });
        }
        let painter = ui.painter_at(rect);
        for hit in &self.drawing_hits {
            let b = hit.bounds(&self.drawing_options, trace.count);
            let mut top = rect.top() + view.y(b.row_start);
            let mut bottom = rect.top() + view.y(b.row_end) + view.row_height;
            if hit.target.is_none() {
                let gap = (view.row_height * 0.1).min(4.0);
                let h =
                    ((view.row_height - gap * 2.0) / hit.source.lane_count.max(1) as f32).max(0.5);
                top += gap + hit.source.lane_index as f32 * h;
                bottom = top + h - if hit.source.lane_count > 1 { 1.0 } else { 0.0 };
            }
            let left = rect.left() + view.x(b.cycle_start);
            let right = (rect.left() + view.x(b.cycle_end)).max(left + 1.0);
            let block =
                Rect::from_min_max(Pos2::new(left, top), Pos2::new(right, bottom)).intersect(rect);
            if !block.is_positive() {
                continue;
            }
            let selected = selected == Some(hit.index);
            let color = Color32::from_rgb(225, 187, 105);
            painter.rect_filled(
                block,
                0.0,
                color.gamma_multiply(if selected { 0.45 } else { 0.30 }),
            );
            painter.rect_stroke(
                block,
                0.0,
                Stroke::new(
                    if selected { 2.0_f32 } else { 1.0_f32 },
                    color.gamma_multiply(0.8),
                ),
                StrokeKind::Inside,
            );
            let label = format!(
                "#{} · {} cycles{}{}",
                hit.index + 1,
                hit.elapsed,
                if hit.elapsed < 0 { " (overlap)" } else { "" },
                if self.drawing_options.label.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", self.drawing_options.label)
                }
            );
            let label_block = block.intersect(label_clip);
            if label_block.is_positive() {
                let text_pos = label_block.min + Vec2::new(3.0, 2.0);
                let galley = painter.layout_no_wrap(
                    label,
                    egui::FontId::monospace(12.0),
                    Color32::from_rgb(250, 228, 183),
                );
                let badge = Rect::from_min_size(
                    text_pos - Vec2::new(2.0, 1.0),
                    galley.size() + Vec2::new(4.0, 2.0),
                )
                .intersect(label_clip);
                let labels = painter.with_clip_rect(label_clip);
                labels.rect_filled(badge, 0.0, Color32::from_rgba_unmultiplied(19, 23, 29, 205));
                labels.galley(text_pos, galley, Color32::from_rgb(250, 228, 183));
            }
            // Drawings do not capture clicks; phase metadata remains inspectable.
            ui.interact(
                block,
                egui::Id::new((trace.id, "filter-rectangle", generation, hit.index)),
                Sense::hover(),
            )
            .on_hover_text(description(hit));
        }
        if self.visible_count > 2048 {
            painter.text(
                rect.right_top() + Vec2::new(-140.0, 10.0),
                egui::Align2::RIGHT_TOP,
                format!("2,048 / {} rectangles · zoom in", self.visible_count),
                egui::FontId::monospace(12.0),
                Color32::LIGHT_GRAY,
            );
        }
    }
}
fn result_row(
    ui: &mut egui::Ui,
    selected: bool,
    instruction: &str,
    endpoint: &str,
    elapsed: &str,
) -> egui::Response {
    let response = ui.add_sized(
        [ui.available_width(), 26.0],
        egui::Button::selectable(selected, ""),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            selected,
            format!("{instruction} · {endpoint} · {elapsed}"),
        )
    });
    let rect = response.rect.shrink2(Vec2::new(6.0, 0.0));
    let font = egui::FontId::monospace(12.0);
    let color = ui.visuals().text_color();
    let elapsed_width = ui
        .painter()
        .layout_no_wrap(elapsed.to_owned(), font.clone(), color)
        .size()
        .x
        .min(rect.width() * 0.4);
    let instruction_end = rect.left() + rect.width() * 0.42;
    let elapsed_start = rect.right() - elapsed_width;
    // Paint within one clickable row; independent clips prevent long text from
    // spilling into adjacent columns or increasing the window's minimum width.
    for (text, left, right) in [
        (instruction, rect.left(), instruction_end - 8.0),
        (endpoint, instruction_end, elapsed_start - 8.0),
        (elapsed, elapsed_start, rect.right()),
    ] {
        let column = Rect::from_min_max(
            Pos2::new(left, rect.top()),
            Pos2::new(right.max(left), rect.bottom()),
        );
        let mut job =
            egui::text::LayoutJob::simple(text.to_owned(), font.clone(), color, column.width());
        job.wrap = egui::text::TextWrapping::truncate_at_width(column.width());
        job.break_on_newline = false;
        let galley = ui.painter().layout_job(job);
        let pos = Pos2::new(left, rect.center().y - galley.size().y * 0.5);
        ui.painter()
            .with_clip_rect(column.intersect(ui.clip_rect()))
            .galley(pos, galley, color);
    }
    response
}
fn same_bounds(a: FilterBounds, b: FilterBounds) -> bool {
    (a.cycle_start, a.cycle_end, a.row_start, a.row_end)
        == (b.cycle_start, b.cycle_end, b.row_start, b.row_end)
}
fn description(hit: &FilterHit) -> String {
    let s = &hit.source;
    let mut text = format!(
        "#{} · {} cycles{}\nSource: Op {} · {} / {} · cycle {}\n{}\n{}",
        hit.index + 1,
        hit.elapsed,
        if hit.elapsed < 0 {
            " (overlap)"
        } else if s.open && hit.target.is_none() {
            " (open)"
        } else {
            ""
        },
        s.op,
        s.lane,
        s.phase,
        s.cycle,
        s.instruction,
        s.metadata
    );
    if let Some(t) = &hit.target {
        text.push_str(&format!(
            "\nTarget: Op {} · {} / {} · cycle {}\n{}\n{}",
            t.op, t.lane, t.phase, t.cycle, t.instruction, t.metadata
        ));
    }
    text
}
