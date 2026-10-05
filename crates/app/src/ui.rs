//! Shared viewer UI. It requests only the visible trace rows from a worker.
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use eframe::egui::{self, Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use serde::{Deserialize, Serialize};
use xonata_core::symbol;
use xonata_core::{
    Event, Operation, OverviewRaster, Request, Row, SearchHit, SearchQuery, TraceInfo,
};
use xonata_view::{DEFAULT_CYCLE_WIDTH, DEFAULT_ROW_HEIGHT, Marker, Viewport};

const BG: Color32 = Color32::from_rgb(19, 23, 29);
const PANEL: Color32 = Color32::from_rgb(28, 34, 43);
const BORDER: Color32 = Color32::from_rgb(56, 67, 79);
const TEXT: Color32 = Color32::from_rgb(218, 226, 235);
const MUTED: Color32 = Color32::from_rgb(143, 159, 174);
const ACCENT: Color32 = Color32::from_rgb(104, 183, 187);
const FLUSHED_OPACITY: f32 = 0.22;
const RULER_HEIGHT: f32 = 32.0;
const COLORS: [Color32; 8] = [
    Color32::from_rgb(93, 165, 180),
    Color32::from_rgb(133, 132, 191),
    Color32::from_rgb(184, 142, 107),
    Color32::from_rgb(116, 169, 126),
    Color32::from_rgb(168, 115, 139),
    Color32::from_rgb(94, 135, 172),
    Color32::from_rgb(164, 168, 104),
    Color32::from_rgb(130, 155, 171),
];

/// Platform file picker, worker requests, and event polling.
pub trait Transport {
    /// Opens one or more traces through the platform picker.
    fn open_dialog(&mut self);
    /// Sends a bounded worker request.
    fn request(&mut self, request: Request);
    /// Takes pending worker events without blocking a frame.
    fn poll(&mut self) -> Vec<Event>;
    /// Notifies an embedding page about a selected instruction.
    fn selection(&mut self, _trace: u32, _op: u64) {}
    /// Whether the platform requests reduced animation.
    fn reduced_motion(&self) -> bool {
        false
    }
    /// Opens files dropped on a native window.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_paths(&mut self, paths: Vec<PathBuf>);
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    sidebar: bool,
    sidebar_width: f32,
    hide_flushed: bool,
    overview: bool,
    overview_width: f32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            sidebar: true,
            sidebar_width: 320.0,
            hide_flushed: false,
            overview: true,
            overview_width: 120.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum InspectorSection {
    Metadata,
    Phases,
    Markers,
}

struct Tab {
    filters: crate::filters::Filters,
    info: TraceInfo,
    view: Viewport,
    rows: Vec<Row>,
    view_gen: u32,
    auto_center: bool,
    last_view: Option<(u64, u64, u32, bool)>,
    sidebar: bool,
    sidebar_width: f32,
    details: bool,
    inspector_section: InspectorSection,
    canvas_rect: Option<Rect>,
    overview: bool,
    overview_width: f32,
    overview_requested: bool,
    overview_raster: Option<OverviewRaster>,
    overview_texture: Option<egui::TextureHandle>,
    overview_last_jump: Option<u64>,
    hide_flushed: bool,
    selected: Option<Operation>,
    selected_row: Option<u64>,
    markers: Vec<Marker>,
    marker_drag: Option<usize>,
    wrap_result: Option<bool>,
    pending_reveal: Option<u64>,
    search_open: bool,
    focus_search: bool,
    query: SearchQuery,
    query_dirty: Option<f64>,
    search_gen: u32,
    results: BTreeMap<u64, SearchHit>,
    result_requested: BTreeSet<u64>,
    result_total: u64,
    search_progress: f32,
    search_done: bool,
    current_result: Option<u64>,
    search_error: String,
}
impl Tab {
    fn new(info: TraceInfo, prefs: &Preferences) -> Self {
        Self {
            info,
            filters: crate::filters::Filters::default(),
            view: Viewport::default(),
            rows: Vec::new(),
            view_gen: 0,
            auto_center: true,
            last_view: None,
            sidebar: prefs.sidebar,
            sidebar_width: prefs.sidebar_width,
            details: false,
            inspector_section: InspectorSection::Metadata,
            canvas_rect: None,
            overview: prefs.overview,
            overview_width: prefs.overview_width,
            overview_requested: false,
            overview_raster: None,
            overview_texture: None,
            overview_last_jump: None,
            hide_flushed: prefs.hide_flushed,
            selected: None,
            selected_row: None,
            markers: Vec::new(),
            marker_drag: None,
            wrap_result: None,
            pending_reveal: None,
            search_open: false,
            focus_search: false,
            query: SearchQuery::default(),
            query_dirty: None,
            search_gen: 0,
            results: BTreeMap::new(),
            result_requested: BTreeSet::new(),
            result_total: 0,
            search_progress: 0.0,
            search_done: false,
            current_result: None,
            search_error: String::new(),
        }
    }
    fn request_search(&mut self, transport: &mut dyn Transport) {
        self.search_gen = self.search_gen.wrapping_add(1);
        self.results.clear();
        self.result_requested.clear();
        self.pending_reveal = None;
        self.wrap_result = None;
        self.result_total = 0;
        self.search_done = false;
        self.current_result = None;
        self.search_error.clear();
        transport.request(Request::Search {
            trace: self.info.id,
            generation: self.search_gen,
            query: Box::new(self.query.clone()),
        });
        self.query_dirty = None;
    }
    fn ensure_view(&mut self, transport: &mut dyn Transport, size: Vec2) {
        let stride = self.view.stride();
        let start = self.view.row.max(0.0) as u64;
        let count = ((size.y / self.view.row_height / stride as f32).ceil() as u32)
            .saturating_add(4)
            .min(1024);
        let key = (start, stride, count, self.hide_flushed);
        if self.last_view == Some(key) {
            return;
        }
        self.view_gen = self.view_gen.wrapping_add(1);
        self.last_view = Some(key);
        transport.request(Request::View {
            trace: self.info.id,
            generation: self.view_gen,
            start,
            stride,
            count,
            hide_flushed: self.hide_flushed,
        });
    }
    fn request_results(&mut self, transport: &mut dyn Transport, start: u64) {
        let page = start / 64 * 64;
        if !self.result_requested.insert(page) {
            return;
        }
        transport.request(Request::Results {
            trace: self.info.id,
            generation: self.search_gen,
            start: page,
            count: 64,
        });
    }
    fn reveal_hit(&mut self, index: u64, transport: &mut dyn Transport) {
        self.current_result = Some(index);
        self.pending_reveal = Some(index);
        self.auto_center = false;
        if let Some(hit) = self.results.get(&index).cloned() {
            self.hide_flushed = false;
            self.view.row = hit.row.saturating_sub(4) as f64;
            self.position_at_operation(&hit.op);
            self.pending_reveal = None;
            let op_id = hit.op.id;
            self.selected = Some(hit.op);
            self.selected_row = Some(hit.row);
            self.last_view = None;
            transport.selection(self.info.id, op_id);
        } else {
            self.request_results(transport, index);
        }
    }
    fn position_at_operation(&mut self, op: &Operation) {
        let first = op
            .stages
            .iter()
            .map(|stage| stage.start)
            .min()
            .unwrap_or(op.fetch);
        let inset = if self.sidebar {
            self.sidebar_width + 24.0
        } else {
            24.0
        };
        self.view.cycle = first;
        self.view.cycle_fraction = 0.0;
        self.view.pan(inset, 0.0);
    }
    fn step_result(&mut self, previous: bool, transport: &mut dyn Transport) {
        if self.result_total == 0 || self.wrap_result.is_some() {
            return;
        }
        let index = match self.current_result {
            None => {
                if previous {
                    self.result_total - 1
                } else {
                    0
                }
            }
            Some(0) if previous => {
                self.wrap_result = Some(true);
                return;
            }
            Some(index) if !previous && index + 1 >= self.result_total => {
                if self.search_done {
                    self.wrap_result = Some(false);
                }
                return;
            }
            Some(index) => {
                if previous {
                    index - 1
                } else {
                    index + 1
                }
            }
        };
        self.reveal_hit(index, transport);
    }
    fn zoom_anchor(&self) -> Vec2 {
        self.canvas_rect.map_or(Vec2::ZERO, |rect| {
            Vec2::new(
                rect.width() * 0.5,
                (rect.height() - RULER_HEIGHT).max(0.0) * 0.5,
            )
        })
    }
}

/// Cross-platform application state.
pub struct Viewer {
    transport: Box<dyn Transport>,
    tabs: Vec<Tab>,
    active: Option<u32>,
    split: Option<u32>,
    error: Option<String>,
    jump_text: String,
    jump_retired: bool,
    show_help: bool,
    prefs: Preferences,
}
impl Viewer {
    fn apply_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "mono".into(),
            egui::FontData::from_static(include_bytes!(
                "../assets/fonts/LiberationMono-Regular.ttf"
            ))
            .into(),
        );
        fonts.font_data.insert(
            "mono-bold".into(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/LiberationMono-Bold.ttf"))
                .into(),
        );
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "mono".into());
        let mut bold = fonts.families[&egui::FontFamily::Monospace].clone();
        bold.insert(0, "mono-bold".into());
        fonts
            .families
            .insert(egui::FontFamily::Name("mono-bold".into()), bold);
        ctx.set_fonts(fonts);
    }
    fn apply_visuals(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        visuals.window_corner_radius = egui::CornerRadius::ZERO;
        visuals.menu_corner_radius = egui::CornerRadius::ZERO;
        visuals.widgets.noninteractive.bg_fill = PANEL;
        visuals.widgets.noninteractive.weak_bg_fill = PANEL;
        visuals.widgets.noninteractive.fg_stroke.color = TEXT;
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 45, 56);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(36, 45, 56);
        visuals.widgets.inactive.fg_stroke.color = TEXT;
        visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(54, 69, 79);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(54, 69, 79);
        visuals.widgets.hovered.fg_stroke.color = TEXT;
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
        visuals.widgets.active.bg_fill = Color32::from_rgb(47, 89, 97);
        visuals.widgets.active.weak_bg_fill = Color32::from_rgb(47, 89, 97);
        visuals.widgets.active.fg_stroke.color = TEXT;
        visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;
        visuals.widgets.open.corner_radius = egui::CornerRadius::ZERO;
        visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
        visuals.selection.bg_fill = Color32::from_rgb(43, 108, 118);
        ctx.set_visuals(visuals);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(8.0, 5.0);
            for (text_style, size) in [
                (egui::TextStyle::Small, 11.0),
                (egui::TextStyle::Body, 14.0),
                (egui::TextStyle::Button, 13.0),
                (egui::TextStyle::Monospace, 14.0),
            ] {
                style
                    .text_styles
                    .insert(text_style, egui::FontId::monospace(size));
            }
            style
                .text_styles
                .insert(egui::TextStyle::Heading, bold_font(18.0));
        });
    }
    /// Creates the viewer with muted visuals and native/browser transport.
    pub fn new(cc: &eframe::CreationContext<'_>, transport: Box<dyn Transport>) -> Self {
        Self::apply_fonts(&cc.egui_ctx);
        Self::apply_visuals(&cc.egui_ctx);
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        cc.egui_ctx.style_mut(|style| {
            style.animation_time = if transport.reduced_motion() {
                0.0
            } else {
                0.14
            }
        });
        let prefs = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, "xonata-prefs"))
            .unwrap_or_default();
        Self {
            transport,
            tabs: Vec::new(),
            active: None,
            split: None,
            error: None,
            jump_text: String::new(),
            jump_retired: false,
            show_help: false,
            prefs,
        }
    }
    fn ingest(&mut self, event: Event) {
        match event {
            Event::Info { info } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == info.id) {
                    if tab.info.count == 0 && info.count > 0 && tab.auto_center {
                        tab.view.cycle = info.first_cycle;
                        tab.view.cycle_fraction = 0.0;
                    }
                    if tab.info.count != info.count || tab.info.complete != info.complete {
                        tab.last_view = None;
                    }
                    if !tab.info.complete
                        && info.complete
                        && tab.search_open
                        && tab.query_dirty.is_some()
                    {
                        tab.query_dirty = Some(f64::NEG_INFINITY);
                    }
                    tab.info = info;
                } else {
                    let id = info.id;
                    self.tabs.push(Tab::new(info, &self.prefs));
                    self.active = Some(id);
                }
            }
            Event::View {
                trace,
                generation,
                rows,
            } => {
                if let Some(tab) = self.tab_mut(trace)
                    && tab.view_gen == generation
                {
                    tab.rows = rows;
                }
            }
            Event::Overview {
                trace,
                generation,
                raster,
            } => {
                if generation == 1
                    && let Some(tab) = self.tab_mut(trace)
                {
                    tab.overview_raster = Some(raster);
                    tab.overview_texture = None;
                }
            }
            Event::SearchProgress {
                trace,
                generation,
                total,
                progress,
                done,
            } => {
                if let Some(tab) = self.tab_mut(trace)
                    && tab.search_gen == generation
                {
                    tab.result_total = total;
                    tab.search_progress = progress;
                    tab.search_done = done;
                }
            }
            Event::Results {
                trace,
                generation,
                start,
                hits,
            } => {
                if let Some(tab) = self.tab_mut(trace)
                    && tab.search_gen == generation
                {
                    tab.result_requested.remove(&start);
                    for (offset, hit) in hits.into_iter().enumerate() {
                        tab.results.insert(start + offset as u64, hit);
                    }
                    while tab.results.len() > 512 {
                        let first = tab.results.first_key_value().map(|(key, _)| *key);
                        let last = tab.results.last_key_value().map(|(key, _)| *key);
                        let remove = match (first, last) {
                            (Some(first), Some(last))
                                if start.saturating_sub(first) > last.saturating_sub(start) =>
                            {
                                first
                            }
                            (_, Some(last)) => last,
                            _ => break,
                        };
                        tab.results.remove(&remove);
                    }
                    if let Some(index) = tab.pending_reveal
                        && tab.results.contains_key(&index)
                    {
                        let transport = &mut *self.transport;
                        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == trace) {
                            tab.reveal_hit(index, transport);
                        }
                    }
                }
            }
            Event::Jump { trace, row } => {
                let mut selection = None;
                if let Some(tab) = self.tab_mut(trace) {
                    if let Some(row) = row {
                        tab.auto_center = false;
                        tab.hide_flushed = false;
                        tab.view.row = row.index.saturating_sub(4) as f64;
                        tab.position_at_operation(&row.op);
                        tab.selected_row = Some(row.index);
                        selection = Some(row.op.id);
                        tab.selected = Some(row.op);
                        tab.last_view = None;
                    } else {
                        self.error = Some("Operation was not found".into());
                    }
                }
                if let Some(op) = selection {
                    self.transport.selection(trace, op);
                }
            }
            Event::FilterSelection {
                trace,
                generation,
                hit,
                operation,
            } => {
                let mut selection = None;
                if let Some(tab) = self.tab_mut(trace)
                    && tab.filters.generation == generation
                    && tab.filters.selected == Some(hit.index)
                    && let Ok(op) = serde_json::from_str::<Operation>(&operation)
                {
                    tab.auto_center = false;
                    tab.hide_flushed = false;
                    let inset = if tab.sidebar {
                        tab.sidebar_width + 24.0
                    } else {
                        24.0
                    };
                    let first = if let Some(target) = &hit.target {
                        let span = hit.source.cycle.abs_diff(target.cycle) as f64
                            * tab.view.cycle_width as f64;
                        if span
                            < tab
                                .canvas_rect
                                .map_or(0.0, |r| (r.width() - inset - 24.0) as f64)
                        {
                            hit.source.cycle.min(target.cycle)
                        } else {
                            hit.source.cycle
                        }
                    } else {
                        hit.source.start
                    };
                    tab.view.cycle = first;
                    tab.view.cycle_fraction = 0.0;
                    tab.view.pan(inset, 0.0);
                    let top = hit.target.as_ref().map_or(hit.source.row, |t| {
                        if t.row.abs_diff(hit.source.row).saturating_add(8) as f64
                            * (tab.view.row_height as f64)
                            < tab.canvas_rect.map_or(0.0, |r| r.height() as f64)
                        {
                            t.row.min(hit.source.row)
                        } else {
                            hit.source.row
                        }
                    });
                    tab.view.row = top.saturating_sub(4) as f64;
                    tab.selected_row = Some(hit.source.row);
                    selection = Some(hit.source.op);
                    tab.selected = Some(op);
                    tab.last_view = None;
                }
                if let Some(op) = selection {
                    self.transport.selection(trace, op);
                }
            }
            event @ (Event::FilterSuggestions { trace, .. }
            | Event::FilterProgress { trace, .. }
            | Event::FilterResults { trace, .. }
            | Event::FilterSortProgress { trace, .. }
            | Event::SortedFilterResults { trace, .. }
            | Event::FilterError { trace, .. }
            | Event::FilterDrawn { trace, .. }
            | Event::FilterDrawing { trace, .. }) => {
                if let Some(tab) = self.tab_mut(trace) {
                    if matches!(&event, Event::FilterDrawn { generation, .. } if *generation == tab.filters.generation)
                    {
                        tab.hide_flushed = false;
                        tab.last_view = None;
                    }
                    tab.filters.ingest(event);
                }
            }
            Event::Error { trace, message } => {
                if message.contains("search pattern") {
                    if let Some(tab) = self.tab_mut(trace) {
                        tab.search_error = message;
                    }
                } else {
                    self.error = Some(message);
                }
            }
            Event::Metrics { .. } => {}
        }
    }
    fn tab_mut(&mut self, id: u32) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.info.id == id)
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let search = ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::F))
            || (!typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F)));
        let next = !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::N));
        let previous = !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::P));
        let canvas_shortcuts = !typing
            && !self.show_help
            && self.error.is_none()
            && !self
                .active
                .and_then(|id| self.tabs.iter().find(|tab| tab.info.id == id))
                .is_some_and(|tab| tab.search_open || tab.wrap_result.is_some());
        let zoom_x_in =
            canvas_shortcuts && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::ArrowRight));
        let zoom_x_out =
            canvas_shortcuts && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::ArrowLeft));
        let zoom_y_in =
            canvas_shortcuts && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::ArrowUp));
        let zoom_y_out =
            canvas_shortcuts && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::ArrowDown));
        let overview =
            canvas_shortcuts && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::M));
        let open = ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::O));
        let help = !typing
            && ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(text) if text == "?"))
            });
        if open {
            self.transport.open_dialog();
        }
        if help {
            self.show_help = true;
        }
        if let Some(id) = self.active {
            let transport = &mut *self.transport;
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == id) {
                if zoom_x_in || zoom_x_out || zoom_y_in || zoom_y_out {
                    let anchor = tab.zoom_anchor();
                    tab.view.zoom_axes(
                        if zoom_x_in {
                            1.25
                        } else if zoom_x_out {
                            0.8
                        } else {
                            1.0
                        },
                        if zoom_y_in {
                            1.25
                        } else if zoom_y_out {
                            0.8
                        } else {
                            1.0
                        },
                        anchor.x,
                        anchor.y,
                    );
                    tab.auto_center = false;
                    tab.last_view = None;
                }
                if overview {
                    tab.overview = !tab.overview;
                }
                if search {
                    tab.search_open = true;
                    tab.focus_search = true;
                }
                if next || previous {
                    tab.step_result(previous, transport);
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let paths = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>()
            });
            if !paths.is_empty() {
                self.transport.open_paths(paths);
            }
        }
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("xonata")
                    .font(bold_font(18.0))
                    .color(ACCENT),
            );
            ui.separator();
            if ui
                .button("Open")
                .on_hover_text("Open one or more traces (Ctrl+O)")
                .clicked()
            {
                self.transport.open_dialog();
            }
            if let Some(id) = self.active {
                let mut search = false;
                let mut sidebar = false;
                let mut inspector = false;
                let active_tab = self.tabs.iter().find(|tab| tab.info.id == id);
                let search_active = active_tab.is_some_and(|tab| tab.search_open);
                let sidebar_active = active_tab.is_some_and(|tab| tab.sidebar);
                let inspector_active = active_tab.is_some_and(|tab| tab.details);
                let can_inspect =
                    active_tab.is_some_and(|tab| tab.selected.is_some() || !tab.markers.is_empty());
                if ui
                    .selectable_label(search_active, "Search")
                    .on_hover_text("Search this trace (F)")
                    .clicked()
                {
                    search = true;
                }
                let filters_active = self
                    .tabs
                    .iter()
                    .find(|t| t.info.id == id)
                    .is_some_and(|t| t.filters.open);
                if ui
                    .selectable_label(filters_active, "Filters")
                    .on_hover_text("Phase and interval constraints with result drawings")
                    .clicked()
                    && let Some(tab) = self.tab_mut(id)
                {
                    tab.filters.open = !tab.filters.open;
                }
                if ui.selectable_label(sidebar_active, "Disassembly").clicked() {
                    sidebar = true;
                }
                if ui
                    .add_enabled(
                        can_inspect,
                        egui::Button::new("Inspector").selected(inspector_active),
                    )
                    .clicked()
                {
                    inspector = true;
                }
                if (search || sidebar || inspector)
                    && let Some(tab) = self.tab_mut(id)
                {
                    if search {
                        tab.search_open = !tab.search_open;
                        tab.focus_search = tab.search_open;
                    }
                    if sidebar {
                        tab.sidebar = !tab.sidebar;
                    }
                    if inspector {
                        tab.details = !tab.details;
                    }
                }
                if let Some(tab) = self.tab_mut(id)
                    && ui
                        .selectable_label(tab.overview, "Overview")
                        .on_hover_text("Whole-trace minimap (M)")
                        .clicked()
                {
                    tab.overview = !tab.overview;
                }
                ui.menu_button("View", |ui| {
                    if ui
                        .add_enabled(
                            self.tabs.len() > 1 || self.split.is_some(),
                            egui::Button::new("Split view").selected(self.split.is_some()),
                        )
                        .clicked()
                    {
                        self.split = if self.split.is_some() {
                            None
                        } else {
                            self.tabs
                                .iter()
                                .find(|tab| tab.info.id != id)
                                .map(|tab| tab.info.id)
                        };
                        ui.close();
                    }
                    if let Some(tab) = self.tab_mut(id)
                        && ui
                            .selectable_label(tab.hide_flushed, "Hide flushed operations")
                            .clicked()
                    {
                        tab.hide_flushed = !tab.hide_flushed;
                        tab.last_view = None;
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Jump to instruction").color(MUTED));
                    ui.horizontal(|ui| {
                        let input = ui.add_sized(
                            [100.0, 26.0],
                            egui::TextEdit::singleline(&mut self.jump_text).hint_text("ID"),
                        );
                        egui::ComboBox::from_id_salt("jump-kind")
                            .selected_text(if self.jump_retired { "RID" } else { "Op" })
                            .width(55.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.jump_retired, false, "Op");
                                ui.selectable_value(&mut self.jump_retired, true, "RID");
                            });
                        if ui.button("Go").clicked()
                            || (input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))
                        {
                            if let Ok(value) = self.jump_text.parse() {
                                self.transport.request(Request::Jump {
                                    trace: id,
                                    id: value,
                                    retired: self.jump_retired,
                                });
                                ui.close();
                            } else {
                                self.error = Some("Enter a nonnegative integer ID".into());
                            }
                        }
                    });
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("?").clicked() {
                    self.show_help = true;
                }
                if let Some(id) = self.active
                    && let Some(tab) = self.tab_mut(id)
                {
                    let anchor = tab.zoom_anchor();
                    let mut changed = false;
                    for (label, horizontal) in [("Y", false), ("X", true)] {
                        ui.label(label);
                        if ui.small_button("+").clicked() {
                            tab.view.zoom_axes(
                                if horizontal { 1.25 } else { 1.0 },
                                if horizontal { 1.0 } else { 1.25 },
                                anchor.x,
                                anchor.y,
                            );
                            changed = true;
                        }
                        let scale = if horizontal {
                            tab.view.cycle_width / DEFAULT_CYCLE_WIDTH
                        } else {
                            tab.view.row_height / DEFAULT_ROW_HEIGHT
                        };
                        if ui
                            .button(format!("{:.0}%", 100.0 * scale))
                            .on_hover_text("Reset this axis")
                            .clicked()
                        {
                            tab.view.zoom_axes(
                                if horizontal { 1.0 / scale } else { 1.0 },
                                if horizontal { 1.0 } else { 1.0 / scale },
                                anchor.x,
                                anchor.y,
                            );
                            changed = true;
                        }
                        if ui.small_button("−").clicked() {
                            tab.view.zoom_axes(
                                if horizontal { 0.8 } else { 1.0 },
                                if horizontal { 1.0 } else { 0.8 },
                                anchor.x,
                                anchor.y,
                            );
                            changed = true;
                        }
                    }
                    if changed {
                        tab.auto_center = false;
                        tab.last_view = None;
                    }
                    ui.label(egui::RichText::new(format!("{} ops", tab.info.count)).color(MUTED))
                        .on_hover_text(format!(
                            "{} cycles · {} flushed · {} warnings\n{}",
                            tab.info.last_cycle.saturating_sub(tab.info.first_cycle),
                            tab.info.flushed,
                            tab.info.warning_count,
                            tab.info.warnings.join("\n")
                        ));
                }
            });
        });
    }
    fn tabs(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::horizontal()
            .id_salt("trace-tabs")
            .max_height(34.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut close = None;
                    for tab in &self.tabs {
                        let id = tab.info.id;
                        let title = format!(
                            "{}{}",
                            tab.info.name,
                            if tab.info.complete { "" } else { " …" }
                        );
                        let selected = self.active == Some(id);
                        let group = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if ui
                                .selectable_label(selected, title)
                                .on_hover_text(format!("{} operations", tab.info.count))
                                .clicked()
                            {
                                self.active = Some(id);
                            }
                            if ui.small_button("×").on_hover_text("Close trace").clicked() {
                                close = Some(id);
                            }
                        });
                        if selected {
                            ui.painter().hline(
                                group.response.rect.x_range(),
                                group.response.rect.bottom() + 2.0,
                                Stroke::new(1.0_f32, ACCENT),
                            );
                        }
                    }
                    if let Some(id) = close {
                        self.transport.request(Request::Close { trace: id });
                        self.tabs.retain(|tab| tab.info.id != id);
                        if self.active == Some(id) {
                            self.active = self.tabs.first().map(|t| t.info.id);
                        }
                        if self.split == Some(id) {
                            self.split = None;
                        }
                    }
                });
            });
    }
}

impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        Self::apply_visuals(ctx);
        for event in self.transport.poll() {
            self.ingest(event);
        }
        self.shortcuts(ctx);
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                self.toolbar(ui);
                ui.separator();
                self.tabs(ui);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::ZERO))
            .show(ctx, |ui| {
                if self.tabs.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.35);
                        ui.heading(
                            egui::RichText::new("Open a Kanata trace")
                                .size(28.0)
                                .color(TEXT),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Drop a .log, .gz, or .zst file here, or use Open.",
                            )
                            .color(MUTED),
                        );
                        ui.add_space(12.0);
                        if ui.button("Open traces").clicked() {
                            self.transport.open_dialog();
                        }
                    });
                } else if let Some(id) = self.active {
                    let split = self.split.filter(|other| {
                        *other != id && self.tabs.iter().any(|t| t.info.id == *other)
                    });
                    if let Some(other) = split {
                        ui.columns(2, |columns| {
                            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == id) {
                                draw_tab(&mut columns[0], tab, &mut *self.transport);
                            }
                            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == other)
                            {
                                draw_tab(&mut columns[1], tab, &mut *self.transport);
                            }
                        });
                    } else if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == id) {
                        draw_tab(ui, tab, &mut *self.transport);
                    }
                }
            });
        if let Some(tab) = self
            .active
            .and_then(|id| self.tabs.iter().find(|t| t.info.id == id))
        {
            self.prefs.sidebar = tab.sidebar;
            self.prefs.sidebar_width = tab.sidebar_width;
            self.prefs.hide_flushed = tab.hide_flushed;
            self.prefs.overview = tab.overview;
            self.prefs.overview_width = tab.overview_width;
        }
        if let Some(id) = self.active
            && let Some(tab) = self.tabs.iter_mut().find(|t| t.info.id == id)
            && let Some(canvas) = tab.canvas_rect
        {
            tab.filters
                .show(ctx, &tab.info, canvas, &mut *self.transport);
        }
        let search_open = self.active.is_some_and(|id| {
            self.tabs
                .iter()
                .any(|tab| tab.info.id == id && tab.search_open)
        });
        if search_open
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            && let Some(id) = self.active
            && let Some(tab) = self.tab_mut(id)
        {
            tab.search_open = false;
        }
        let wrapping = self
            .active
            .and_then(|id| self.tabs.iter().find(|tab| tab.info.id == id))
            .is_some_and(|tab| tab.wrap_result.is_some());
        if wrapping
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            && let Some(id) = self.active
            && let Some(tab) = self.tab_mut(id)
        {
            tab.wrap_result = None;
        }
        let modal = self.error.is_some() || self.show_help || search_open || wrapping;
        let fade = if self.transport.reduced_motion() {
            if modal { 1.0 } else { 0.0 }
        } else {
            ctx.animate_bool_with_time(egui::Id::new("modal-dim"), modal, 0.14)
        };
        if fade > 0.0 {
            // Areas can rise on click. Keep the backdrop above the canvas and its
            // overlays, but in a lower order than every modal window.
            let backdrop = egui::Area::new(egui::Id::new("modal-backdrop"))
                .order(egui::Order::Middle)
                .movable(false)
                .fixed_pos(ctx.screen_rect().min)
                .interactable(modal)
                .show(ctx, |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(ctx.screen_rect().size(), Sense::click());
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        Color32::from_black_alpha((125.0 * fade) as u8),
                    );
                });
            ctx.move_to_top(backdrop.response.layer_id);
        }
        if search_open
            && let Some(id) = self.active
            && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == id)
        {
            egui::Window::new(format!("Search · {}", tab.info.name))
                .id(egui::Id::new("search-modal"))
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .title_bar(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(PANEL)
                        .corner_radius(egui::CornerRadius::ZERO)
                        .inner_margin(egui::Margin::same(20))
                        .stroke(Stroke::new(1.0_f32, BORDER)),
                )
                .show(ctx, |ui| {
                    ui.set_width((ctx.available_rect().width().min(760.0) - 60.0).max(260.0));
                    draw_search(ui, tab, &mut *self.transport);
                });
        }
        if wrapping
            && let Some(id) = self.active
            && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == id)
        {
            egui::Window::new("End of search results")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let previous = tab.wrap_result == Some(true);
                    ui.label(if previous {
                        "Reached the first match. Wrap to the last?"
                    } else {
                        "Reached the last match. Wrap to the first?"
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Wrap").clicked() {
                            tab.wrap_result = None;
                            tab.reveal_hit(
                                if previous { tab.result_total - 1 } else { 0 },
                                &mut *self.transport,
                            );
                        }
                        if ui.button("Cancel").clicked() {
                            tab.wrap_result = None;
                        }
                    });
                });
        }
        if let Some(message) = self.error.clone() {
            egui::Window::new("Notice")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(message);
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
        }
        if self.show_help {
            egui::Window::new("Keyboard and mouse")
                .order(egui::Order::Foreground)
                .open(&mut self.show_help)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_width((ctx.screen_rect().width() - 60.0).clamp(260.0, 560.0));
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height((ctx.screen_rect().height() - 100.0).max(100.0))
                        .show(ui, |ui| {
                            egui::Grid::new("controls_guide")
                                .num_columns(2)
                                .striped(false)
                                .spacing(Vec2::new(32.0, 7.0))
                                .show(ui, |ui| {
                                    for (feature, control) in [
                                        ("Open trace", "Ctrl+O"),
                                        ("Search", "F / Ctrl+F"),
                                        ("Filter builder / suggestions", "Filters → Query → ▾"),
                                        (
                                            "Sort results by elapsed cycles",
                                            "Filters → Results → Elapsed cycles",
                                        ),
                                        ("Draw filter results", "Filters → Draw"),
                                        ("Next / previous match", "n / p"),
                                        ("Show this guide", "?"),
                                        ("Pan trace", "Drag"),
                                        ("Scroll trace", "Wheel"),
                                        ("Zoom both axes", "Ctrl+wheel / Pinch"),
                                        ("Horizontal zoom in / out", "Ctrl+Right / Ctrl+Left"),
                                        ("Vertical zoom in / out", "Ctrl+Up / Ctrl+Down"),
                                        ("Reset axis zoom", "Click X / Y percentage"),
                                        ("Inspect metadata", "Click instruction / stage"),
                                        ("Place marker", "Shift+click"),
                                        ("Move marker", "Drag marker"),
                                        ("Remove marker", "Hover marker + Delete"),
                                        ("Clear markers", "Inspector → Clear markers"),
                                        ("Toggle overview", "M"),
                                        ("Jump to pipeline row", "Click / drag overview"),
                                        ("Resize overlay", "Drag panel edge"),
                                        ("Expand / compact overlay", "Click > / <"),
                                    ] {
                                        ui.label(egui::RichText::new(feature).color(MUTED));
                                        ui.label(
                                            egui::RichText::new(control)
                                                .font(bold_font(14.0))
                                                .color(TEXT),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });
                });
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "xonata-prefs", &self.prefs);
    }
}

fn bold_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("mono-bold".into()))
}

fn trace_rows_rect(canvas: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(canvas.left(), canvas.top() + RULER_HEIGHT),
        canvas.max,
    )
}

fn overlay_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .inner_margin(12)
}

fn draw_tab(ui: &mut egui::Ui, tab: &mut Tab, transport: &mut dyn Transport) {
    let size = ui.available_size().max(Vec2::new(100.0, 100.0));
    if tab.auto_center
        && let Some(first_stage) = tab
            .rows
            .iter()
            .flat_map(|row| row.op.stages.iter().map(|stage| stage.start))
            .min()
    {
        let padding = if tab.sidebar {
            tab.sidebar_width.min(size.x * 0.8) + 32.0
        } else {
            32.0
        };
        tab.view.cycle = first_stage.saturating_sub((padding / tab.view.cycle_width).ceil() as u64);
        tab.view.cycle_fraction = 0.0;
        tab.auto_center = false;
    }
    let rect = draw_pipeline(ui, tab, transport, size);
    tab.canvas_rect = Some(rect);
    draw_disassembly(ui.ctx(), tab, transport, rect);
    draw_overview(ui.ctx(), tab, transport, rect);
    if tab.details {
        let mut open = true;
        egui::Window::new("Inspector")
            .id(egui::Id::new((tab.info.id, "inspector")))
            .open(&mut open)
            .collapsible(false)
            .default_size([360.0, 340.0])
            .min_size([260.0, 160.0])
            .default_pos(Pos2::new(
                (rect.right()
                    - if tab.overview {
                        tab.overview_width
                    } else {
                        0.0
                    }
                    - 390.0)
                    .max(rect.left()),
                rect.top() + 48.0,
            ))
            .constrain_to(rect)
            .frame(overlay_frame())
            .show(ui.ctx(), |ui| draw_details(ui, tab));
        tab.details = open;
    }
}

fn overview_row_at(tab: &Tab, body: Rect, pos: Pos2) -> u64 {
    (((pos.y - body.top()) / body.height()).clamp(0.0, 1.0) as f64 * tab.info.count as f64)
        .min(tab.info.count.saturating_sub(1) as f64) as u64
}

fn draw_overview(ctx: &egui::Context, tab: &mut Tab, transport: &mut dyn Transport, canvas: Rect) {
    if !tab.overview {
        return;
    }
    if tab.info.complete && !tab.overview_requested {
        tab.overview_requested = true;
        transport.request(Request::Overview {
            trace: tab.info.id,
            generation: 1,
            width: 256,
            height: 1024,
        });
    }
    if let Some(raster) = tab.overview_raster.take() {
        let pixels = raster
            .pixels
            .iter()
            .map(|index| {
                if *index == 0 {
                    BG
                } else {
                    let color = COLORS[((index & 127).saturating_sub(1) as usize) % COLORS.len()];
                    if index & 128 != 0 {
                        color.gamma_multiply(FLUSHED_OPACITY)
                    } else {
                        color
                    }
                }
            })
            .collect();
        tab.overview_texture = Some(ctx.load_texture(
            format!("overview-{}", tab.info.id),
            egui::ColorImage::new([raster.width as usize, raster.height as usize], pixels),
            egui::TextureOptions::NEAREST,
        ));
    }
    let max_width = (canvas.width() * 0.45).clamp(88.0, 512.0);
    tab.overview_width = tab.overview_width.clamp(88.0, max_width);
    let width = tab.overview_width;
    let left = canvas.right() - width;
    egui::Area::new(egui::Id::new((tab.info.id, "overview")))
        .order(egui::Order::Middle)
        .fixed_pos(Pos2::new(left, canvas.top()))
        .constrain(false)
        .movable(false)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            let (header, _) =
                ui.allocate_exact_size(Vec2::new(width, RULER_HEIGHT), Sense::hover());
            ui.painter().rect_filled(header, 0.0, PANEL);
            ui.scope_builder(egui::UiBuilder::new().max_rect(header.shrink(4.0)), |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Map").small().color(MUTED));
                    if ui
                        .small_button(if width >= max_width - 1.0 { "<" } else { ">" })
                        .on_hover_text("Expand / compact overview")
                        .clicked()
                    {
                        tab.overview_width = if width >= max_width - 1.0 {
                            120.0_f32.min(max_width)
                        } else {
                            max_width
                        };
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Hide overview (M)")
                        .clicked()
                    {
                        tab.overview = false;
                    }
                });
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            let body = Rect::from_min_max(Pos2::new(left, canvas.top() + RULER_HEIGHT), canvas.max);
            let response = ui.allocate_rect(body, Sense::click_and_drag());
            let painter = ui.painter_at(body);
            painter.rect_filled(body, 0.0, BG);
            if let Some(texture) = &tab.overview_texture {
                painter.image(
                    texture.id(),
                    body,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let span = tab.info.last_cycle.saturating_sub(tab.info.first_cycle) as f64 + 1.0;
                let visible = trace_rows_rect(canvas);
                let x = |cycle: u64| {
                    body.left()
                        + (cycle.saturating_sub(tab.info.first_cycle) as f64 / span).clamp(0.0, 1.0)
                            as f32
                            * body.width()
                };
                let y = |row: f64| {
                    body.top()
                        + (row / tab.info.count.max(1) as f64).clamp(0.0, 1.0) as f32
                            * body.height()
                };
                let viewport = Rect::from_min_max(
                    Pos2::new(x(tab.view.cycle), y(tab.view.row)),
                    Pos2::new(
                        x(tab.view.cycle_at(visible.width())),
                        y(tab.view.row + visible.height() as f64 / tab.view.row_height as f64),
                    ),
                );
                let viewport = Rect::from_center_size(
                    viewport.center(),
                    viewport.size().max(Vec2::splat(3.0)),
                )
                .intersect(body);
                painter.rect_stroke(
                    viewport,
                    0.0,
                    Stroke::new(1.5_f32, Color32::WHITE),
                    StrokeKind::Inside,
                );
                // A full-width row band remains visible even when the horizontal viewport is subpixel.
                painter.line_segment(
                    [
                        Pos2::new(body.left(), viewport.center().y),
                        Pos2::new(body.right(), viewport.center().y),
                    ],
                    Stroke::new(0.7_f32, Color32::WHITE.gamma_multiply(0.45)),
                );
            } else {
                painter.text(
                    body.center(),
                    egui::Align2::CENTER_CENTER,
                    if tab.info.complete {
                        "Building…"
                    } else {
                        "Loading…"
                    },
                    egui::FontId::monospace(11.0),
                    MUTED,
                );
            }
            if response.clicked() || response.dragged() {
                if let Some(pos) = response.interact_pointer_pos()
                    && tab.info.complete
                    && tab.info.count > 0
                {
                    let row = overview_row_at(tab, body, pos);
                    if tab.overview_last_jump != Some(row) {
                        tab.overview_last_jump = Some(row);
                        transport.request(Request::JumpRow {
                            trace: tab.info.id,
                            row,
                        });
                    }
                }
            } else {
                tab.overview_last_jump = None;
            }
            if let Some(pos) = response.hover_pos() {
                response.on_hover_text(format!(
                    "Pipeline row {} / {}\nClick or drag to jump; drag left edge to resize",
                    overview_row_at(tab, body, pos) + 1,
                    tab.info.count
                ));
            }
            let edge = Rect::from_min_max(
                Pos2::new(left - 4.0, canvas.top()),
                Pos2::new(left + 4.0, canvas.bottom()),
            );
            let resize = ui
                .interact(
                    edge,
                    egui::Id::new((tab.info.id, "overview-resize")),
                    Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if resize.dragged()
                && let Some(pos) = resize.interact_pointer_pos()
            {
                tab.overview_width = (canvas.right() - pos.x).clamp(88.0, max_width);
            }
        });
}

fn navigate_wheel(ui: &egui::Ui, tab: &mut Tab, rect: Rect, hovered: bool, horizontal: bool) {
    if !hovered {
        return;
    }
    let (zoom, scroll, pointer, modifiers) = ui.input(|input| {
        (
            input.zoom_delta(),
            input.raw_scroll_delta,
            input.pointer.hover_pos().unwrap_or(rect.center()),
            input.modifiers,
        )
    });
    if zoom != 1.0 {
        tab.view
            .zoom(zoom, pointer.x - rect.left(), pointer.y - rect.top());
    } else if scroll != Vec2::ZERO && !modifiers.ctrl && !modifiers.command {
        tab.view
            .pan(if horizontal { scroll.x } else { 0.0 }, scroll.y);
    } else {
        return;
    }
    tab.auto_center = false;
    tab.last_view = None;
}

fn draw_disassembly(
    ctx: &egui::Context,
    tab: &mut Tab,
    transport: &mut dyn Transport,
    canvas: Rect,
) {
    let fade = if transport.reduced_motion() {
        if tab.sidebar { 1.0 } else { 0.0 }
    } else {
        ctx.animate_bool_with_time(
            egui::Id::new((tab.info.id, "disassembly-fade")),
            tab.sidebar,
            0.12,
        )
    };
    if fade <= 0.0 {
        return;
    }
    let max_width = (canvas.width() * 0.8).max(80.0);
    let width = tab.sidebar_width.clamp(180.0_f32.min(max_width), max_width);
    egui::Area::new(egui::Id::new((tab.info.id, "disassembly-overlay")))
        .order(egui::Order::Middle)
        .fixed_pos(canvas.min)
        .movable(false)
        .constrain(false)
        .interactable(tab.sidebar)
        .show(ctx, |ui| {
            ui.set_opacity(fade);
            ui.set_clip_rect(canvas);
            let (panel, _) =
                ui.allocate_exact_size(Vec2::new(width, canvas.height()), Sense::hover());
            let painter = ui.painter_at(panel);
            painter.rect_filled(panel, 0.0, Color32::from_rgba_unmultiplied(22, 27, 34, 238));
            painter.vline(panel.right(), panel.y_range(), Stroke::new(1.0_f32, BORDER));
            let body = trace_rows_rect(panel);
            let header = Rect::from_min_max(panel.min, Pos2::new(panel.right(), body.top()));
            painter.hline(panel.x_range(), body.top(), Stroke::new(1.0_f32, BORDER));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(header.shrink2(Vec2::new(8.0, 3.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.label(egui::RichText::new("DISASSEMBLY").small().color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_text("Hide disassembly")
                            .clicked()
                        {
                            tab.sidebar = false;
                        }
                        if ui
                            .small_button(if width < max_width - 1.0 { ">" } else { "<" })
                            .on_hover_text("Expand / compact disassembly; drag its edge to resize")
                            .clicked()
                        {
                            tab.sidebar_width = if width < max_width - 1.0 {
                                max_width
                            } else {
                                320.0_f32.min(max_width * 0.5).max(180.0_f32.min(max_width))
                            };
                        }
                    });
                },
            );
            let response = ui.interact(body, ui.id().with("rows"), Sense::click_and_drag());
            for row in &tab.rows {
                let center = body.top() + tab.view.row_center(row.index);
                let row_rect = Rect::from_min_max(
                    Pos2::new(body.left(), center - tab.view.row_height * 0.5),
                    Pos2::new(body.right(), center + tab.view.row_height * 0.5),
                )
                .intersect(body);
                if !row_rect.is_positive() {
                    continue;
                }
                let selected = tab.selected.as_ref().is_some_and(|op| op.id == row.op.id);
                if selected {
                    painter.rect_filled(
                        row_rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(104, 183, 187, 24),
                    );
                }
                if tab.view.row_height >= 18.0 {
                    let label = row.op.label.lines().next().unwrap_or("");
                    painter
                        .with_clip_rect(row_rect.shrink2(Vec2::new(8.0, 0.0)))
                        .text(
                            Pos2::new(body.left() + 12.0, center),
                            egui::Align2::LEFT_CENTER,
                            format!("{:>5}  {label}", row.op.id),
                            egui::FontId::monospace((tab.view.row_height * 0.4).clamp(11.0, 15.0)),
                            if selected {
                                ACCENT
                            } else if row.op.flushed {
                                TEXT.gamma_multiply(FLUSHED_OPACITY)
                            } else {
                                TEXT
                            },
                        );
                }
            }
            if let Some(pos) = response.hover_pos() {
                let row_index = tab.view.row_at(pos.y - body.top());
                if let Some(row) = tab.rows.iter().find(|row| row.index == row_index) {
                    response
                        .clone()
                        .on_hover_text(row.op.metadata(&tab.info.symbols));
                    if response.clicked() {
                        transport.selection(tab.info.id, row.op.id);
                        tab.selected_row = Some(row.index);
                        tab.selected = Some(row.op.clone());
                        tab.inspector_section = InspectorSection::Metadata;
                        tab.details = true;
                    }
                }
            }
            // Apply overlay navigation after painting so its rows and the
            // already-painted canvas always use the same transform this frame.
            navigate_wheel(ui, tab, trace_rows_rect(canvas), response.hovered(), false);
            if response.dragged() {
                tab.view.pan(0.0, ui.input(|input| input.pointer.delta().y));
                tab.auto_center = false;
                tab.last_view = None;
            }
            let edge = Rect::from_min_max(
                Pos2::new(panel.right() - 4.0, panel.top()),
                Pos2::new(panel.right() + 4.0, panel.bottom()),
            );
            let resize = ui
                .interact(edge, ui.id().with("resize"), Sense::drag())
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if resize.dragged()
                && let Some(pointer) = resize.interact_pointer_pos()
            {
                tab.sidebar_width =
                    (pointer.x - canvas.left()).clamp(180.0_f32.min(max_width), max_width);
            }
        });
}

fn draw_pipeline(
    ui: &mut egui::Ui,
    tab: &mut Tab,
    transport: &mut dyn Transport,
    size: Vec2,
) -> Rect {
    let (canvas, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let rect = trace_rows_rect(canvas);
    if tab.filters.drawn.is_some() && tab.filters.drawing_visible && tab.hide_flushed {
        tab.hide_flushed = false;
        tab.last_view = None;
    }
    tab.ensure_view(transport, rect.size());
    let painter = ui.painter_at(rect);
    ui.painter_at(canvas).rect_filled(canvas, 0.0, BG);
    navigate_wheel(ui, tab, rect, response.hovered(), true);
    let cursor = response
        .hover_pos()
        .or_else(|| response.interact_pointer_pos());
    if response.hovered()
        && !ui.ctx().wants_keyboard_input()
        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete))
        && let Some(pos) = cursor
        && let Some(index) = tab.markers.iter().position(|marker| {
            Pos2::new(
                rect.left() + tab.view.x(marker.cycle),
                rect.top() + tab.view.y(marker.row),
            )
            .distance(pos)
                <= 10.0
        })
    {
        tab.markers.remove(index);
        tab.marker_drag = None;
    }
    let marker_pos = |marker: Marker| {
        Pos2::new(
            rect.left() + tab.view.x(marker.cycle),
            rect.top() + tab.view.y(marker.row),
        )
    };
    let marker_clicked = response.clicked()
        && cursor.is_some_and(|pos| {
            tab.markers
                .iter()
                .any(|marker| marker_pos(*marker).distance(pos) <= 10.0)
        });
    if response.drag_started() {
        tab.marker_drag = ui.input(|i| i.pointer.press_origin()).and_then(|pos| {
            tab.markers
                .iter()
                .position(|marker| marker_pos(*marker).distance(pos) <= 10.0)
        });
    }
    if response.dragged() {
        tab.auto_center = false;
        if let (Some(index), Some(pos)) = (tab.marker_drag, cursor) {
            tab.markers[index] = Marker {
                cycle: tab
                    .view
                    .cycle_at(pos.x - rect.left() + tab.view.cycle_width * 0.5),
                row: tab
                    .view
                    .row_at(pos.y - rect.top() + tab.view.row_height * 0.5),
            };
        } else {
            let delta = ui.input(|i| i.pointer.delta());
            tab.view.pan(delta.x, delta.y);
            tab.last_view = None;
        }
    }
    if response.drag_stopped() {
        tab.marker_drag = None;
    }
    if marker_clicked {
        tab.details = true;
        tab.inspector_section = InspectorSection::Markers;
    }
    let placing_marker = response.clicked() && ui.input(|i| i.modifiers.shift);
    if placing_marker
        && let Some(pos) = cursor
        && rect.contains(pos)
    {
        tab.markers.push(Marker {
            cycle: tab
                .view
                .cycle_at(pos.x - rect.left() + tab.view.cycle_width * 0.5),
            row: tab
                .view
                .row_at(pos.y - rect.top() + tab.view.row_height * 0.5),
        });
    }
    draw_grid(&ui.painter_at(canvas), canvas, &tab.view);
    let mut clicked = None;
    let mut lanes = Vec::new();
    for row in &tab.rows {
        let y = rect.top() + tab.view.y(row.index);
        if y + tab.view.row_height < rect.top() || y > rect.bottom() {
            continue;
        }
        let op = &row.op;
        lanes.clear();
        for stage in &op.stages {
            if !lanes.contains(&stage.lane) {
                lanes.push(stage.lane);
            }
        }
        for stage in &op.stages {
            let start = rect.left() + tab.view.x(stage.start);
            let end = rect.left()
                + tab
                    .view
                    .x(stage.end.unwrap_or(tab.info.last_cycle.saturating_add(1)));
            if end < rect.left() || start > rect.right() {
                continue;
            }
            let lane = lanes
                .iter()
                .position(|lane| *lane == stage.lane)
                .unwrap_or(0);
            let gap = (tab.view.row_height * 0.1).min(4.0);
            let h = ((tab.view.row_height - gap * 2.0) / lanes.len().max(1) as f32).max(0.5);
            let top = y + gap + lane as f32 * h;
            let block = Rect::from_min_max(
                Pos2::new(start + 1.0, top),
                Pos2::new(
                    (end - 1.0).max(start + 2.0),
                    top + h - if lanes.len() > 1 { 1.0 } else { 0.0 },
                ),
            )
            .intersect(rect);
            if !block.is_positive() {
                continue;
            }
            let name = symbol(&tab.info.symbols, stage.name);
            let color = COLORS[stage.name as usize % COLORS.len()];
            painter.rect_filled(
                block,
                0.0,
                if op.flushed {
                    color.gamma_multiply(FLUSHED_OPACITY)
                } else {
                    color
                },
            );
            let font = bold_font((h - 12.0).clamp(12.0, 17.0));
            let label = painter.layout_no_wrap(name.to_owned(), font.clone(), BG);
            let first_cell = Rect::from_min_max(
                Pos2::new(start + 1.0, block.top()),
                Pos2::new(start + tab.view.cycle_width - 1.0, block.bottom()),
            )
            .intersect(block);
            // Prefer the first cycle, but keep the name readable across the slab when
            // zoom makes individual cycles too narrow. Measure the actual glyphs.
            let name_area = if label.size().x + 8.0 <= first_cell.width() {
                first_cell
            } else {
                block
            };
            let name_rect = (label.size().x + 4.0 <= name_area.width()
                && label.size().y <= name_area.height())
            .then(|| Rect::from_center_size(name_area.center(), label.size()));
            // Visit visible cycles only: a long phase must not cost work proportional to its duration.
            if tab.view.cycle_width >= 12.0 {
                let visible_start = stage.start.max(tab.view.cycle_at(0.0));
                let visible_end = stage
                    .end
                    .unwrap_or(tab.info.last_cycle.saturating_add(1))
                    .min(tab.view.cycle_at(rect.width()).saturating_add(1));
                for cycle in visible_start..visible_end {
                    let left = rect.left() + tab.view.x(cycle);
                    let cell = Rect::from_min_max(
                        Pos2::new(left + 1.0, block.top()),
                        Pos2::new(left + tab.view.cycle_width - 1.0, block.bottom()),
                    )
                    .intersect(block);
                    if cycle > stage.start {
                        painter.line_segment(
                            [
                                Pos2::new(left, block.top()),
                                Pos2::new(left, block.bottom()),
                            ],
                            Stroke::new(0.7_f32, BG.gamma_multiply(0.28)),
                        );
                    }
                    if cycle > stage.start
                        && cell.width() > 24.0
                        && cell.height() >= 22.0
                        && !name_rect.is_some_and(|name| name.expand(2.0).intersects(cell))
                    {
                        let text = (cycle - stage.start + 1).to_string();
                        let text_color = BG.gamma_multiply(0.38);
                        let label = painter.layout_no_wrap(text, font.clone(), text_color);
                        if label.size().x + 12.0 <= cell.width() {
                            painter.with_clip_rect(cell).galley(
                                Pos2::new(left + 8.0, top + h * 0.5 - label.size().y * 0.5),
                                label,
                                text_color,
                            );
                        }
                    }
                }
            }
            if let Some(name_rect) = name_rect {
                painter
                    .with_clip_rect(block)
                    .galley(name_rect.min, label, BG);
            }
            if cursor.is_some_and(|pos| block.contains(pos)) {
                let end = stage.end.unwrap_or(tab.info.last_cycle.saturating_add(1));
                response.clone().on_hover_text(format!(
                    "{} / {} · {}–{}\nElapsed: {} cycles{}\n{}\n{}",
                    symbol(&tab.info.symbols, stage.lane),
                    name,
                    stage.start,
                    end,
                    end.saturating_sub(stage.start),
                    if stage.end.is_none() { " (open)" } else { "" },
                    stage.labels,
                    op.metadata(&tab.info.symbols)
                ));
                if response.clicked() && !placing_marker && !marker_clicked {
                    clicked = Some((row.index, op.clone()));
                }
            }
        }
        if tab.selected.as_ref().is_some_and(|s| s.id == op.id) {
            painter.rect_stroke(
                Rect::from_min_size(
                    Pos2::new(rect.left(), y),
                    Vec2::new(rect.width(), tab.view.row_height.max(2.0)),
                ),
                0.0,
                Stroke::new(1.0_f32, ACCENT),
                StrokeKind::Inside,
            );
            for dependency in &op.dependencies {
                if let Some(producer) = tab.rows.iter().find(|r| r.op.id == dependency.producer) {
                    let py = rect.top() + tab.view.row_center(producer.index);
                    let cy = rect.top() + tab.view.row_center(row.index);
                    let x = rect.left() + tab.view.x(dependency.cycle);
                    painter.line_segment(
                        [Pos2::new(x, py), Pos2::new(x, cy)],
                        Stroke::new(1.3_f32, ACCENT),
                    );
                    painter.circle_filled(Pos2::new(x, py), 2.5, ACCENT);
                    painter.circle_filled(Pos2::new(x, cy), 3.5, ACCENT);
                }
            }
        }
    }
    if response.clicked()
        && !placing_marker
        && !marker_clicked
        && clicked.is_none()
        && let Some(pos) = cursor
        && rect.contains(pos)
    {
        let row_index = tab.view.row_at(pos.y - rect.top());
        if let Some(row) = tab.rows.iter().find(|r| r.index == row_index) {
            clicked = Some((row.index, row.op.clone()));
        }
    }
    if let Some((index, op)) = clicked {
        transport.selection(tab.info.id, op.id);
        tab.selected_row = Some(index);
        tab.selected = Some(op);
        tab.inspector_section = InspectorSection::Metadata;
        tab.details = true;
    }
    let label_clip = Rect::from_min_max(
        Pos2::new(
            rect.left() + if tab.sidebar { tab.sidebar_width } else { 0.0 },
            rect.top(),
        ),
        Pos2::new(
            rect.right()
                - if tab.overview {
                    tab.overview_width
                } else {
                    0.0
                },
            rect.bottom(),
        ),
    );
    tab.filters
        .drawings(ui, &tab.info, &tab.view, rect, label_clip, transport);
    for pair in tab.markers.windows(2) {
        let a = Pos2::new(
            rect.left() + tab.view.x(pair[0].cycle),
            rect.top() + tab.view.y(pair[0].row),
        );
        let b = Pos2::new(
            rect.left() + tab.view.x(pair[1].cycle),
            rect.top() + tab.view.y(pair[1].row),
        );
        if !Rect::from_two_pos(a, b).expand(6.0).intersects(rect) {
            continue;
        }
        painter.line_segment([a, b], Stroke::new(2.0_f32, TEXT));
        let (cycles, rows) = pair[0].distance(pair[1]);
        let text = format!("{cycles} cycles · {rows} pipelines");
        let label = painter.layout_no_wrap(text, egui::FontId::monospace(13.0), TEXT);
        let center = rect.clamp((a + b.to_vec2()) * 0.5);
        let pos = center - label.size() * 0.5;
        painter.rect_filled(
            Rect::from_min_size(pos - Vec2::splat(4.0), label.size() + Vec2::splat(8.0)),
            0.0,
            PANEL,
        );
        painter.galley(pos, label, TEXT);
    }
    for (index, marker) in tab.markers.iter().enumerate() {
        let pos = Pos2::new(
            rect.left() + tab.view.x(marker.cycle),
            rect.top() + tab.view.y(marker.row),
        );
        painter.circle_filled(pos, 5.0, ACCENT);
        painter.text(
            pos + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!("M{}", index + 1),
            egui::FontId::monospace(12.0),
            TEXT,
        );
    }
    if tab.rows.is_empty() && tab.info.count == 0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Waiting for instructions…",
            egui::FontId::monospace(16.0),
            MUTED,
        );
    }
    canvas
}

fn draw_grid(painter: &egui::Painter, rect: Rect, view: &Viewport) {
    let rows = trace_rows_rect(rect);
    painter.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(rect.right(), rows.top())),
        0.0,
        PANEL,
    );
    painter.hline(rect.x_range(), rows.top(), Stroke::new(1.0_f32, BORDER));
    let step = (80.0 / view.cycle_width).ceil().max(1.0) as u64;
    let step = 10u64.pow((step as f64).log10().ceil().clamp(0.0, 18.0) as u32);
    let first = view.cycle / step * step;
    for i in 0_u64..100 {
        let cycle = first.saturating_add(i.saturating_mul(step));
        let x = rect.left() + view.x(cycle);
        if x > rect.right() {
            break;
        }
        if x < rect.left() {
            continue;
        }
        painter.line_segment(
            [Pos2::new(x, rows.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(0.6_f32, BORDER),
        );
        painter.text(
            Pos2::new(x + 8.0, rect.top() + RULER_HEIGHT * 0.5),
            egui::Align2::LEFT_CENTER,
            cycle.to_string(),
            egui::FontId::monospace(12.0),
            MUTED,
        );
    }
}

fn draw_details(ui: &mut egui::Ui, tab: &mut Tab) {
    if let Some(op) = &tab.selected {
        ui.add(
            egui::Label::new(
                egui::RichText::new(op.label.lines().next().unwrap_or("Instruction"))
                    .font(bold_font(15.0))
                    .color(TEXT),
            )
            .wrap(),
        );
        ui.label(
            egui::RichText::new(format!(
                "Op {} · C{}–{} · {}",
                op.id,
                op.fetch,
                op.end.unwrap_or(tab.info.last_cycle),
                if op.flushed {
                    "flushed"
                } else if op.incomplete {
                    "incomplete"
                } else if op.rid.is_none() {
                    "active"
                } else {
                    "retired"
                },
            ))
            .small()
            .color(MUTED),
        );
    } else {
        ui.label(egui::RichText::new("Grid markers").font(bold_font(15.0)));
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(tab.selected.is_some(), egui::Button::new("Copy metadata"))
            .clicked()
            && let Some(op) = &tab.selected
        {
            ui.ctx().copy_text(op.metadata(&tab.info.symbols));
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(tab.selected.is_some(), |ui| {
            ui.selectable_value(
                &mut tab.inspector_section,
                InspectorSection::Metadata,
                "Metadata",
            );
            ui.selectable_value(
                &mut tab.inspector_section,
                InspectorSection::Phases,
                "Phases",
            );
        });
        ui.selectable_value(
            &mut tab.inspector_section,
            InspectorSection::Markers,
            "Markers",
        );
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt((tab.info.id, "inspector-content"))
        .max_height(ui.available_height().clamp(140.0, 420.0))
        .show(ui, |ui| match tab.inspector_section {
            InspectorSection::Metadata => {
                if let Some(op) = &tab.selected {
                    egui::Grid::new((tab.info.id, "metadata-grid"))
                        .num_columns(2)
                        .spacing([20.0, 8.0])
                        .show(ui, |ui| {
                            for (name, value) in [
                                ("Global ID", op.gid.to_string()),
                                ("Thread", op.tid.to_string()),
                                (
                                    "Retired ID",
                                    op.rid.map_or_else(|| "—".into(), |id| id.to_string()),
                                ),
                                ("Source line", op.line.to_string()),
                            ] {
                                ui.label(egui::RichText::new(name).color(MUTED));
                                ui.label(value);
                                ui.end_row();
                            }
                        });
                    if !op.detail.is_empty() {
                        ui.separator();
                        ui.add(egui::Label::new(&op.detail).wrap());
                    }
                }
            }
            InspectorSection::Phases => {
                if let Some(op) = &tab.selected {
                    egui::Grid::new((tab.info.id, "phase-grid"))
                        .num_columns(4)
                        .spacing([16.0, 8.0])
                        .show(ui, |ui| {
                            for title in ["Phase", "Lane", "Cycles", "Elapsed"] {
                                ui.label(egui::RichText::new(title).color(MUTED));
                            }
                            ui.end_row();
                            for stage in &op.stages {
                                let end =
                                    stage.end.unwrap_or(tab.info.last_cycle.saturating_add(1));
                                ui.label(
                                    egui::RichText::new(symbol(&tab.info.symbols, stage.name))
                                        .font(bold_font(14.0)),
                                )
                                .on_hover_text(&stage.labels);
                                ui.label(symbol(&tab.info.symbols, stage.lane));
                                ui.label(format!("{}–{end}", stage.start));
                                ui.label(format!(
                                    "{} cycles{}",
                                    end.saturating_sub(stage.start),
                                    if stage.end.is_none() { " (open)" } else { "" },
                                ));
                                ui.end_row();
                            }
                        });
                    if !op.dependencies.is_empty() {
                        ui.separator();
                        egui::CollapsingHeader::new(format!(
                            "Dependencies ({})",
                            op.dependencies.len()
                        ))
                        .show(ui, |ui| {
                            for dep in &op.dependencies {
                                ui.label(format!(
                                    "Op {} · type {} · C{}",
                                    dep.producer, dep.kind, dep.cycle
                                ));
                            }
                        });
                    }
                }
            }
            InspectorSection::Markers => draw_markers(ui, tab),
        });
}

fn draw_markers(ui: &mut egui::Ui, tab: &mut Tab) {
    ui.label("Shift+click to place · Drag to move · Hover + Delete to remove");
    let mut remove = None;
    for (index, marker) in tab.markers.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("M{}", index + 1));
            ui.add(egui::DragValue::new(&mut marker.cycle).prefix("C "));
            ui.add(egui::DragValue::new(&mut marker.row).prefix("Row "));
            if ui
                .small_button("×")
                .on_hover_text("Remove marker (or hover it on the trace and press Delete)")
                .clicked()
            {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        tab.markers.remove(index);
    }
    if !tab.markers.is_empty() && ui.button("Clear markers").clicked() {
        tab.markers.clear();
    }
}

fn draw_search(ui: &mut egui::Ui, tab: &mut Tab, transport: &mut dyn Transport) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("Search trace").color(TEXT));
            ui.label(egui::RichText::new(&tab.info.name).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("×").on_hover_text("Close search (Esc)").clicked() {
                    tab.search_open = false;
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let input_width = (ui.available_width() - 110.0).max(120.0);
            let response = ui.add_sized(
                [input_width, 34.0],
                egui::TextEdit::singleline(&mut tab.query.text)
                    .id_salt((tab.info.id, "search-input"))
                    .hint_text("Search disassembly and metadata"),
            );
            if tab.focus_search {
                response.request_focus();
                tab.focus_search = false;
            }
            let changed = response.changed();
            if changed {
                tab.query_dirty = Some(ui.input(|i| i.time));
            }
            if ui
                .selectable_label(tab.query.regex, ".*")
                .on_hover_text("Use regular expression")
                .clicked()
            {
                tab.query.regex = !tab.query.regex;
                tab.query_dirty = Some(ui.input(|i| i.time));
            }
            if ui
                .selectable_label(tab.query.case_sensitive, "Aa")
                .on_hover_text("Match case")
                .clicked()
            {
                tab.query.case_sensitive = !tab.query.case_sensitive;
                tab.query_dirty = Some(ui.input(|i| i.time));
            }
        });
        egui::CollapsingHeader::new("Filters")
            .id_salt((tab.info.id, "search-filters"))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let mut changed = false;
                    changed |= number_filter(ui, "Op", &mut tab.query.op);
                    changed |= number_filter(ui, "Global", &mut tab.query.global);
                    changed |= number_filter(ui, "RID", &mut tab.query.retired_id);
                    changed |= number_filter(ui, "Thread", &mut tab.query.thread);
                    changed |= number_filter(ui, "Cycles", &mut tab.query.cycles);
                    ui.label("Stage");
                    changed |= ui
                        .add_sized(
                            [70.0, 20.0],
                            egui::TextEdit::singleline(&mut tab.query.stage),
                        )
                        .changed();
                    egui::ComboBox::from_id_salt((tab.info.id, "status"))
                        .selected_text(if tab.query.status.is_empty() {
                            "All"
                        } else {
                            &tab.query.status
                        })
                        .show_ui(ui, |ui| {
                            for (value, title) in [
                                ("", "All"),
                                ("retired", "Retired"),
                                ("flushed", "Flushed"),
                                ("incomplete", "Incomplete"),
                            ] {
                                changed |= ui
                                    .selectable_value(&mut tab.query.status, value.into(), title)
                                    .changed();
                            }
                        });
                    if changed {
                        tab.query_dirty = Some(ui.input(|i| i.time));
                    }
                });
            });
        if tab.info.complete
            && tab
                .query_dirty
                .is_some_and(|at| ui.input(|i| i.time) - at >= 0.18)
        {
            tab.request_search(transport);
        }
        ui.separator();
        ui.horizontal(|ui| {
            let suffix = if tab.search_done {
                "matches"
            } else {
                "found so far"
            };
            ui.label(format!("{} {suffix}", tab.result_total));
            if !tab.info.complete {
                ui.label("Search starts after loading");
            } else if !tab.search_done && tab.search_gen != 0 {
                ui.add(egui::ProgressBar::new(tab.search_progress).desired_width(95.0));
            }
            if let Some(current) = tab.current_result {
                ui.label(format!("{} / {}", current + 1, tab.result_total));
            }
            if ui
                .add_enabled(tab.result_total > 0, egui::Button::new("Prev"))
                .on_hover_text("Previous match (p)")
                .clicked()
            {
                tab.step_result(true, transport);
            }
            if ui
                .add_enabled(tab.result_total > 0, egui::Button::new("Next"))
                .on_hover_text("Next match (n)")
                .clicked()
            {
                tab.step_result(false, transport);
            }
            if !tab.search_error.is_empty() {
                ui.colored_label(Color32::from_rgb(231, 140, 137), &tab.search_error);
            }
        });
        egui::ScrollArea::vertical().max_height(350.0).show_rows(
            ui,
            42.0,
            tab.result_total.min(usize::MAX as u64) as usize,
            |ui, range| {
                for index in range {
                    let index = index as u64;
                    if let Some(hit) = tab.results.get(&index) {
                        let label = hit.op.label.lines().next().unwrap_or("");
                        let text = format!(
                            "{:>6}  Op {} · C{}  {}  {}",
                            index + 1,
                            hit.op.id,
                            hit.op.fetch,
                            if hit.op.flushed { "[flushed]" } else { "" },
                            label
                        );
                        let snippet = hit.snippets.first().cloned().unwrap_or_default();
                        let clicked = ui
                            .add(
                                egui::Button::selectable(tab.current_result == Some(index), text)
                                    .truncate(),
                            )
                            .on_hover_text(hit.snippets.join("\n"))
                            .clicked();
                        ui.add(
                            egui::Label::new(egui::RichText::new(snippet).small().color(MUTED))
                                .truncate(),
                        );
                        if clicked {
                            tab.reveal_hit(index, transport);
                        }
                    } else {
                        tab.request_results(transport, index);
                        ui.label(
                            egui::RichText::new(format!("{:>6}  Loading…", index + 1)).color(MUTED),
                        );
                    }
                }
            },
        );
    });
}
fn number_filter(ui: &mut egui::Ui, title: &str, range: &mut xonata_core::NumberRange) -> bool {
    let mut min_text = range.min.map_or_else(String::new, |v| v.to_string());
    let mut max_text = range.max.map_or_else(String::new, |v| v.to_string());
    let mut changed = false;
    ui.label(title);
    if ui
        .add_sized(
            [61.0, 20.0],
            egui::TextEdit::singleline(&mut min_text).hint_text("min"),
        )
        .changed()
        && (min_text.is_empty() || min_text.parse::<u64>().is_ok())
    {
        range.min = min_text.parse().ok();
        changed = true;
    }
    if ui
        .add_sized(
            [61.0, 20.0],
            egui::TextEdit::singleline(&mut max_text).hint_text("max"),
        )
        .changed()
        && (max_text.is_empty() || max_text.parse::<u64>().is_ok())
    {
        range.max = max_text.parse().ok();
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use xonata_core::Stage;

    struct TestTransport;

    impl Transport for TestTransport {
        fn open_dialog(&mut self) {}
        fn request(&mut self, _request: Request) {}
        fn poll(&mut self) -> Vec<Event> {
            Vec::new()
        }
        fn reduced_motion(&self) -> bool {
            true
        }
        #[cfg(not(target_arch = "wasm32"))]
        fn open_paths(&mut self, _paths: Vec<PathBuf>) {}
    }

    fn context_and_tab() -> (egui::Context, Tab) {
        let ctx = egui::Context::default();
        Viewer::apply_fonts(&ctx);
        Viewer::apply_visuals(&ctx);
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut tab = Tab::new(
            TraceInfo {
                id: 1,
                name: "alignment.kanata".into(),
                count: 8,
                first_cycle: 100,
                last_cycle: 110,
                complete: true,
                symbols: vec!["0".into(), "F".into()],
                ..Default::default()
            },
            &Preferences::default(),
        );
        tab.auto_center = false;
        tab.view.cycle = 100;
        tab.rows.push(Row {
            index: 5,
            op: Operation {
                id: 1,
                gid: 1,
                tid: 0,
                rid: Some(1),
                fetch: 100,
                end: Some(102),
                flushed: false,
                incomplete: false,
                line: 3,
                label: "add x1,x2,x3".into(),
                detail: "Instruction metadata".into(),
                stages: vec![Stage {
                    name: 1,
                    lane: 0,
                    start: 100,
                    end: Some(102),
                    labels: String::new(),
                }],
                dependencies: Vec::new(),
            },
        });
        (ctx, tab)
    }

    fn render(ctx: &egui::Context, tab: &mut Tab, events: Vec<egui::Event>) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        draw_tab(ui, tab, &mut TestTransport);
                    });
            },
        )
    }

    #[derive(Default)]
    struct RecordingTransport(Vec<Request>);
    impl Transport for RecordingTransport {
        fn open_dialog(&mut self) {}
        fn request(&mut self, request: Request) {
            self.0.push(request);
        }
        fn poll(&mut self) -> Vec<Event> {
            Vec::new()
        }
        #[cfg(not(target_arch = "wasm32"))]
        fn open_paths(&mut self, _paths: Vec<PathBuf>) {}
    }

    #[test]
    fn result_requests_should_deduplicate_in_flight_pages_and_retry_partial_pages() {
        let (_, mut tab) = context_and_tab();
        let mut transport = RecordingTransport::default();
        tab.request_results(&mut transport, 0);
        tab.request_results(&mut transport, 1);
        tab.request_results(&mut transport, 64);
        assert_eq!(transport.0.len(), 2);
        // A partial reply is no longer in flight; future matches on that page must be fetched.
        tab.result_requested.remove(&0);
        tab.request_results(&mut transport, 10);
        assert_eq!(transport.0.len(), 3);
    }

    #[test]
    fn revealing_a_match_should_position_the_first_phase_beyond_the_disassembly_overlay() {
        let (_, mut tab) = context_and_tab();
        let mut op = tab.rows[0].op.clone();
        op.fetch = 0;
        op.stages[0].start = 863;
        tab.results.insert(
            0,
            SearchHit {
                op,
                row: 5,
                snippets: Vec::new(),
            },
        );
        tab.reveal_hit(0, &mut TestTransport);
        assert!((tab.view.x(863) - tab.sidebar_width - 24.0).abs() < 0.01);
    }

    #[test]
    fn search_navigation_should_prompt_before_wrapping_in_either_direction() {
        let (_, mut tab) = context_and_tab();
        tab.result_total = 3;
        tab.search_done = true;
        tab.current_result = Some(2);
        tab.step_result(false, &mut TestTransport);
        assert_eq!(
            (tab.current_result, tab.wrap_result),
            (Some(2), Some(false))
        );
        tab.wrap_result = None;
        tab.current_result = Some(0);
        tab.step_result(true, &mut TestTransport);
        assert_eq!((tab.current_result, tab.wrap_result), (Some(0), Some(true)));
    }

    #[test]
    fn long_phases_should_show_the_phase_then_number_each_remaining_cycle() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.rows[0].op.stages[0].end = Some(104);
        let output = render(&ctx, &mut tab, Vec::new());
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            ["F", "2", "3", "4"]
                .iter()
                .all(|label| labels.contains(label)),
            "{labels:?}"
        );
    }

    #[test]
    fn marker_drag_should_snap_to_the_grid_without_panning_the_trace() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.markers.push(Marker { cycle: 102, row: 2 });
        let start = Pos2::new(144.0, 120.0);
        let _ = render(&ctx, &mut tab, vec![egui::Event::PointerMoved(start)]);
        let _ = render(
            &ctx,
            &mut tab,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
        );
        let _ = render(
            &ctx,
            &mut tab,
            vec![egui::Event::PointerMoved(Pos2::new(288.0, 208.0))],
        );
        assert_eq!(
            (tab.markers[0], tab.view.cycle, tab.view.row),
            (Marker { cycle: 104, row: 4 }, 100, 0.0)
        );
    }

    #[test]
    fn marker_connections_should_render_both_distances() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.markers = vec![Marker { cycle: 101, row: 2 }, Marker { cycle: 104, row: 6 }];
        let output = render(&ctx, &mut tab, Vec::new());
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::epaint::Shape::Text(text) if text.galley.job.text == "3 cycles · 4 pipelines")));
    }

    #[test]
    fn partial_result_replies_should_allow_retry_without_repositioning_an_existing_selection() {
        let (_, mut tab) = context_and_tab();
        tab.result_requested.insert(0);
        tab.current_result = Some(0);
        let hit = SearchHit {
            op: tab.rows[0].op.clone(),
            row: 5,
            snippets: Vec::new(),
        };
        tab.results.insert(0, hit.clone());
        let mut viewer = Viewer {
            transport: Box::new(TestTransport),
            tabs: vec![tab],
            active: Some(1),
            split: None,
            error: None,
            jump_text: String::new(),
            jump_retired: false,
            show_help: false,
            prefs: Preferences::default(),
        };
        viewer.ingest(Event::Results {
            trace: 1,
            generation: 0,
            start: 0,
            hits: vec![hit],
        });
        let tab = &mut viewer.tabs[0];
        assert!(tab.result_requested.is_empty());
        assert_eq!((tab.view.row, tab.view.cycle), (0.0, 100));
        let mut transport = RecordingTransport::default();
        tab.request_results(&mut transport, 1);
        assert_eq!(transport.0.len(), 1);
    }

    #[test]
    fn delete_should_remove_only_the_hovered_marker() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.markers = vec![Marker { cycle: 102, row: 2 }, Marker { cycle: 104, row: 4 }];
        let _ = render(
            &ctx,
            &mut tab,
            vec![egui::Event::PointerMoved(Pos2::new(144.0, 120.0))],
        );
        let _ = render(
            &ctx,
            &mut tab,
            vec![egui::Event::Key {
                key: Key::Delete,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(tab.markers, vec![Marker { cycle: 104, row: 4 }]);
    }

    #[test]
    fn zoom_shortcuts_should_change_only_the_requested_axis() {
        for (key, x_factor, y_factor) in [
            (Key::ArrowRight, 1.25, 1.0),
            (Key::ArrowLeft, 0.8, 1.0),
            (Key::ArrowUp, 1.0, 1.25),
            (Key::ArrowDown, 1.0, 0.8),
        ] {
            let (ctx, tab) = context_and_tab();
            let mut viewer = Viewer {
                transport: Box::new(TestTransport),
                tabs: vec![tab],
                active: Some(1),
                split: None,
                error: None,
                jump_text: String::new(),
                jump_retired: false,
                show_help: false,
                prefs: Preferences::default(),
            };
            let _ = ctx.run(
                egui::RawInput {
                    modifiers: Modifiers::CTRL,
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::CTRL,
                    }],
                    ..Default::default()
                },
                |ctx| viewer.shortcuts(ctx),
            );
            assert_eq!(
                (
                    viewer.tabs[0].view.cycle_width,
                    viewer.tabs[0].view.row_height
                ),
                (
                    DEFAULT_CYCLE_WIDTH * x_factor,
                    DEFAULT_ROW_HEIGHT * y_factor
                )
            );
        }
    }

    fn filter_hit(tab: &Tab, index: u64) -> xonata_core::filter::FilterHit {
        let query = xonata_core::filter::FilterQuery::parse("phase=F").unwrap();
        let source = query
            .source
            .endpoints(&tab.rows[0].op, tab.rows[0].index, &tab.info)
            .0
            .remove(0);
        xonata_core::filter::FilterHit {
            index,
            elapsed: 2,
            source,
            target: None,
        }
    }

    #[test]
    fn graphical_filters_should_quote_user_text_and_default_to_nonnegative_gaps() {
        let (_, mut tab) = context_and_tab();
        let mut op = tab.rows[0].op.clone();
        op.label = "add \"quoted\" & operands -> result;".into();
        op.detail = "prefix Free RQU resource=36 suffix".into();
        tab.filters.builder.source.phase = "F".into();
        tab.filters.builder.source.instruction = op.label.clone();
        tab.filters.builder.source.metadata = "rqu *=36".into();
        tab.filters.builder.interval = true;
        tab.filters.builder.target.phase = "F".into();
        let query = xonata_core::filter::FilterQuery::parse(&tab.filters.builder.query()).unwrap();
        let source = query.source.endpoints(&op, 5, &tab.info).0.remove(0);
        let mut target = source.clone();
        target.cycle = source.cycle - 1;
        assert!(!query.accepts(&source, &target));
        target.cycle = source.cycle;
        assert!(query.accepts(&source, &target));
        tab.filters.builder.kind = crate::filter_builder::ResultKind::Both;
        let query = xonata_core::filter::FilterQuery::parse(&tab.filters.builder.query()).unwrap();
        target.cycle = source.cycle - 1;
        assert!(query.accepts(&source, &target));
    }

    #[test]
    fn filter_window_should_keep_its_width_across_frames_and_sort_with_one_button() {
        let (ctx, mut tab) = context_and_tab();
        tab.filters.open = true;
        tab.filters.done = true;
        tab.filters.total = 1;
        tab.filters.results.insert(0, filter_hit(&tab, 7));
        let mut transport = RecordingTransport::default();
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0));
        let id = egui::Id::new((tab.info.id, "filters-window"));
        let render = |tab: &mut Tab, transport: &mut RecordingTransport, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(canvas),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |_| {});
                    tab.filters.show(ctx, &tab.info, canvas, transport);
                },
            )
        };
        for (results, exclusion) in [(false, false), (false, true), (true, true)] {
            tab.filters.results_tab = results;
            tab.filters.builder.interval = exclusion;
            tab.filters.builder.exclude_enabled = exclusion;
            for _ in 0..5 {
                let _ = render(&mut tab, &mut transport, Vec::new());
            }
            let width = ctx.memory(|m| m.area_rect(id).unwrap().width());
            assert!(width < 650.0, "initial window unexpectedly wide: {width}");
            for _ in 0..100 {
                let _ = render(&mut tab, &mut transport, Vec::new());
                let current = ctx.memory(|m| m.area_rect(id).unwrap().width());
                assert!(
                    (current - width).abs() < 1.0,
                    "window grew from {width} to {current}"
                );
            }
        }
        let original = ctx.memory(|m| m.area_rect(id).unwrap());
        let corner = original.right_bottom() - Vec2::splat(3.0);
        let resized_corner = corner + Vec2::new(100.0, 50.0);
        for (pos, pressed) in [
            (corner, true),
            (resized_corner, true),
            (resized_corner, false),
        ] {
            let _ = render(
                &mut tab,
                &mut transport,
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
        for _ in 0..5 {
            let _ = render(&mut tab, &mut transport, Vec::new());
        }
        let resized = ctx.memory(|m| m.area_rect(id).unwrap());
        assert!(
            resized.width() > original.width() + 60.0,
            "window remains manually resizable: {original:?} -> {resized:?}"
        );
        // Read button positions from the rendered UI, independent of font metrics.
        for descending in [true, false] {
            let output = render(&mut tab, &mut transport, Vec::new());
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.job.text.starts_with("Elapsed cycles") =>
                    {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("single sort button visible");
            for pressed in [true, false] {
                let _ = render(
                    &mut tab,
                    &mut transport,
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
            assert_eq!(tab.filters.descending, Some(descending));
            assert!(
                matches!(transport.0.last(),Some(Request::FilterSort {descending:order,..}) if *order==descending)
            );
            tab.filters.ingest(Event::FilterSortProgress {
                trace: tab.info.id,
                generation: tab.filters.generation,
                revision: tab.filters.result_revision,
                total: 1,
                progress: 1.0,
                done: true,
            });
        }
    }

    #[test]
    fn filter_result_window_should_align_columns_and_remain_clickable() {
        let (ctx, mut tab) = context_and_tab();
        tab.filters.open = true;
        tab.filters.results_tab = true;
        tab.filters.generation = 1;
        tab.filters.done = true;
        tab.filters.total = 1;
        let mut hit = filter_hit(&tab, 7);
        hit.source.instruction.push_str(&" operand".repeat(30));
        tab.filters.results.insert(0, hit);
        let mut transport = RecordingTransport::default();
        let mut render = |tab: &mut Tab, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |_| {});
                    tab.filters.show(
                        ctx,
                        &tab.info,
                        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0)),
                        &mut transport,
                    );
                },
            )
        };
        for _ in 0..3 {
            let _ = render(&mut tab, Vec::new());
        }
        let output = render(&mut tab, Vec::new());
        let column = |prefix: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text.starts_with(prefix) => {
                        Some((
                            text.pos + text.galley.size() * 0.5,
                            shape.clip_rect,
                            text.galley.rows.len(),
                            text.galley.elided,
                        ))
                    }
                    _ => None,
                })
                .expect("filter result column is visible")
        };
        let instruction = column("add x1,x2,x3");
        let endpoint = column("#8  Op 1 / F");
        let elapsed = column("2 cycles");
        assert!((instruction.0.y - endpoint.0.y).abs() < 0.01);
        assert!((endpoint.0.y - elapsed.0.y).abs() < 0.01);
        assert!(instruction.1.right() <= endpoint.1.left());
        assert!(endpoint.1.right() <= elapsed.1.left());
        assert_eq!(instruction.2, 1);
        assert!(
            instruction.3,
            "long instruction must truncate within its column"
        );
        // Clicking either side of the row reveals the same stable result ID.
        for pos in [instruction.0, endpoint.0] {
            for pressed in [true, false] {
                let _ = render(
                    &mut tab,
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
        }
        assert_eq!(tab.filters.selected, Some(7));
        assert!(transport.0.iter().any(|r| matches!(
            r,
            Request::RevealFilter {
                generation: 1,
                index: 7,
                ..
            }
        )));
    }

    #[test]
    fn filter_navigation_should_reveal_the_endpoint_and_ignore_stale_selections() {
        let (_, mut tab) = context_and_tab();
        let mut hit = filter_hit(&tab, 0);
        hit.source.cycle = (1_u64 << 60) + 17;
        hit.source.start = hit.source.cycle;
        hit.source.end = hit.source.cycle + 2;
        tab.filters.generation = 2;
        tab.filters.selected = Some(0);
        tab.view.cycle_width = 37.0;
        tab.view.row_height = 29.0;
        let operation = serde_json::to_string(&tab.rows[0].op).unwrap();
        let mut viewer = Viewer {
            transport: Box::<RecordingTransport>::default(),
            tabs: vec![tab],
            active: Some(1),
            split: None,
            error: None,
            jump_text: String::new(),
            jump_retired: false,
            show_help: false,
            prefs: Preferences::default(),
        };
        let original = viewer.tabs[0].view.cycle;
        viewer.ingest(Event::FilterSelection {
            trace: 1,
            generation: 1,
            hit: hit.clone(),
            operation: operation.clone(),
        });
        assert_eq!(viewer.tabs[0].view.cycle, original);
        viewer.ingest(Event::FilterSelection {
            trace: 1,
            generation: 2,
            hit: hit.clone(),
            operation,
        });
        let tab = &viewer.tabs[0];
        assert!((tab.view.x(hit.source.start) - tab.sidebar_width - 24.0).abs() < 0.01);
        assert_eq!((tab.view.cycle_width, tab.view.row_height), (37.0, 29.0));
        assert_eq!(tab.selected_row, Some(hit.source.row));
    }

    #[test]
    fn filter_cache_should_ignore_stale_generations_and_keep_at_most_512_results() {
        let (_, mut tab) = context_and_tab();
        tab.filters.generation = 2;
        let hit = filter_hit(&tab, 0);
        tab.filters.ingest(Event::FilterResults {
            trace: 1,
            generation: 1,
            start: 0,
            hits: vec![hit.clone()],
        });
        assert!(tab.filters.results.is_empty());
        for start in (0..1024).step_by(128) {
            let hits = (start..start + 128)
                .map(|index| {
                    let mut hit = hit.clone();
                    hit.index = index;
                    hit
                })
                .collect();
            tab.filters.ingest(Event::FilterResults {
                trace: 1,
                generation: 2,
                start,
                hits,
            });
        }
        assert_eq!(tab.filters.results.len(), 512);
        assert!(tab.filters.results.contains_key(&1023));
        tab.filters.ingest(Event::FilterDrawn {
            trace: 1,
            generation: 2,
            options: Default::default(),
        });
        tab.filters.ingest(Event::FilterDrawing {
            trace: 1,
            generation: 1,
            request: 0,
            visible: 1,
            hits: vec![hit],
        });
        assert!(tab.filters.drawing_hits.is_empty());
    }

    #[test]
    fn drawing_numbers_should_remain_visible_beside_the_disassembly_overlay() {
        let (ctx, mut tab) = context_and_tab();
        let mut hit = filter_hit(&tab, 0);
        hit.source.start = tab.view.cycle;
        hit.source.end = tab.view.cycle + 20;
        tab.filters.generation = 1;
        tab.filters.drawn = Some(1);
        tab.filters.drawing_options.label = "example".into();
        tab.filters.drawing_hits = vec![hit];
        let output = render(&ctx, &mut tab, Vec::new());
        let label = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.job.text == "#1 · 2 cycles · example" =>
                {
                    Some((text.pos, shape.clip_rect))
                }
                _ => None,
            })
            .expect("numbered drawing label");
        let canvas = trace_rows_rect(tab.canvas_rect.unwrap());
        assert!(label.0.x >= canvas.left() + tab.sidebar_width);
        assert!(label.1.left() >= canvas.left() + tab.sidebar_width);
    }

    #[test]
    fn single_phase_drawings_should_follow_lane_geometry_after_pan_and_independent_zoom() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.filters.generation = 1;
        tab.filters.drawn = Some(1);
        tab.filters.drawing_hits = vec![filter_hit(&tab, 0)];
        let color = Color32::from_rgb(225, 187, 105).gamma_multiply(0.30);
        for (width, height, row) in [(72.0, 44.0, 0.0), (36.0, 32.0, 2.35), (20.0, 26.0, 3.2)] {
            tab.view.cycle_width = width;
            tab.view.row_height = height;
            tab.view.row = row;
            let output = render(&ctx, &mut tab, Vec::new());
            let block = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::epaint::Shape::Rect(r) if r.fill == color => Some(r.rect),
                    _ => None,
                })
                .unwrap();
            let canvas = trace_rows_rect(tab.canvas_rect.unwrap());
            assert!((block.width() - width * 2.0).abs() < 0.5);
            assert!((block.center().y - (canvas.top() + tab.view.row_center(5))).abs() < 0.5);
        }
    }

    #[test]
    fn animated_search_backdrop_should_not_cover_or_block_result_clicks() {
        let (ctx, mut tab) = context_and_tab();
        tab.result_total = 1;
        tab.search_done = true;
        tab.results.insert(
            0,
            SearchHit {
                op: tab.rows[0].op.clone(),
                row: 5,
                snippets: vec!["add x1,x2,x3".into()],
            },
        );
        let mut viewer = Viewer {
            transport: Box::<RecordingTransport>::default(),
            tabs: vec![tab],
            active: Some(1),
            split: None,
            error: None,
            jump_text: String::new(),
            jump_retired: false,
            show_help: false,
            prefs: Preferences::default(),
        };
        let mut frame = eframe::Frame::_new_kittest();
        let mut time = 0.0;
        let mut render = |viewer: &mut Viewer, events| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| eframe::App::update(viewer, ctx, &mut frame),
            )
        };
        for _ in 0..3 {
            let _ = render(&mut viewer, Vec::new());
        }
        viewer.tabs[0].search_open = true;
        for _ in 0..6 {
            let _ = render(&mut viewer, Vec::new());
        }
        // A click in the dimmed background must not raise it over the dialog.
        let outside = Pos2::new(20.0, 600.0);
        for pressed in [true, false] {
            let _ = render(
                &mut viewer,
                vec![
                    egui::Event::PointerMoved(outside),
                    egui::Event::PointerButton {
                        pos: outside,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
        }
        let output = render(&mut viewer, Vec::new());
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text.contains("Op 1 · C100") => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("search result is rendered");
        assert_eq!(
            ctx.layer_id_at(pos),
            Some(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("search-modal")
            ))
        );
        for pressed in [true, false] {
            let _ = render(
                &mut viewer,
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
        assert_eq!(viewer.tabs[0].current_result, Some(0));
    }

    #[test]
    fn phase_inspector_should_report_elapsed_cycles_for_closed_and_open_phases() {
        let (ctx, mut tab) = context_and_tab();
        tab.inspector_section = InspectorSection::Phases;
        // Large absolute cycle numbers must not lose precision when computing duration.
        let start = (1_u64 << 60) + 100;
        tab.info.last_cycle = start + 10;
        for (end, expected) in [(Some(start + 2), "2 cycles"), (None, "11 cycles (open)")] {
            let mut op = tab.rows[0].op.clone();
            op.stages[0].start = start;
            op.stages[0].end = end;
            tab.selected = Some(op);
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw_details(ui, &mut tab));
            });
            assert!(output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == expected)
            }));
        }
    }

    #[test]
    fn flushed_instructions_should_be_much_dimmer_in_trace_and_disassembly() {
        let (ctx, mut tab) = context_and_tab();
        let colors = |output: egui::FullOutput| {
            let mut slab = None;
            let mut disassembly = None;
            for shape in output.shapes {
                match shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.fill == COLORS[1]
                            || rect.fill == COLORS[1].gamma_multiply(FLUSHED_OPACITY) =>
                    {
                        slab = Some(rect.fill.a());
                    }
                    egui::epaint::Shape::Text(text)
                        if text.galley.job.text.ends_with("add x1,x2,x3") =>
                    {
                        disassembly = Some(text.galley.job.sections[0].format.color.a());
                    }
                    _ => {}
                }
            }
            (slab.unwrap(), disassembly.unwrap())
        };
        let _ = render(&ctx, &mut tab, Vec::new());
        let normal = colors(render(&ctx, &mut tab, Vec::new()));
        tab.rows[0].op.flushed = true;
        let flushed = colors(render(&ctx, &mut tab, Vec::new()));
        assert!(flushed.0 < normal.0 / 3 && flushed.1 < normal.1 / 3);
    }

    #[test]
    fn phase_names_should_use_the_whole_slab_when_cycle_cells_are_too_narrow() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.info.symbols[1] = "vE".into();
        tab.rows[0].op.stages[0].end = Some(110);
        for width in [32.0, 24.0, 12.0, 6.0] {
            tab.view.cycle_width = width;
            tab.view.row_height = 24.0;
            let output = render(&ctx, &mut tab, Vec::new());
            let labels: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == "vE" => Some(text),
                    _ => None,
                })
                .collect();
            assert_eq!(labels.len(), 1, "cycle width {width}");
            if width <= 12.0 {
                let center = labels[0].pos.x + labels[0].galley.size().x * 0.5;
                assert!((center - (tab.canvas_rect.unwrap().left() + width * 5.0)).abs() < 0.5);
            }
        }
    }

    #[test]
    fn phase_names_should_hide_when_the_slab_is_too_narrow_or_short() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        tab.info.symbols[1] = "vE".into();
        tab.rows[0].op.stages[0].end = Some(110);
        for (width, height) in [(1.0, 32.0), (12.0, 8.0)] {
            tab.view.cycle_width = width;
            tab.view.row_height = height;
            let output = render(&ctx, &mut tab, Vec::new());
            assert!(!output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == "vE")
            }));
        }
    }

    #[test]
    fn cycle_numbers_should_have_less_opacity_than_the_phase_name() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        let output = render(&ctx, &mut tab, Vec::new());
        let opacity = |label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.galley.job.sections[0].format.color.a())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert!(opacity("2") < opacity("F"));
    }

    #[test]
    fn overview_click_should_jump_to_a_row_without_resizing_the_canvas() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        let _ = render(&ctx, &mut tab, Vec::new());
        let pos = Pos2::new(940.0, 340.0);
        let _ = render(&ctx, &mut tab, vec![egui::Event::PointerMoved(pos)]);
        let _ = render(&ctx, &mut tab, Vec::new());
        let canvas = tab.canvas_rect;
        let mut transport = RecordingTransport::default();
        for pressed in [true, false] {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| draw_tab(ui, &mut tab, &mut transport));
                },
            );
        }
        assert!(
            transport
                .0
                .iter()
                .any(|request| matches!(request, Request::JumpRow { row: 4, .. })),
            "requests: {:?}",
            transport.0
        );
        assert_eq!(tab.canvas_rect, canvas);
    }

    #[test]
    fn overview_row_mapping_should_clamp_edges_and_include_the_last_row() {
        let (_, mut tab) = context_and_tab();
        tab.info.count = 28_621;
        let body = Rect::from_min_size(Pos2::new(880.0, 32.0), Vec2::new(120.0, 608.0));
        assert_eq!(
            (
                overview_row_at(&tab, body, body.min),
                overview_row_at(&tab, body, body.max)
            ),
            (0, 28_620)
        );
    }

    #[test]
    fn expanding_the_overview_should_preserve_the_canvas_extent() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        let _ = render(&ctx, &mut tab, Vec::new());
        let canvas = tab.canvas_rect;
        tab.overview_width = 440.0;
        let _ = render(&ctx, &mut tab, Vec::new());
        assert_eq!((tab.canvas_rect, tab.overview_width), (canvas, 440.0));
    }

    #[test]
    fn dragging_the_overview_edge_should_resize_without_panning_the_canvas() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        for _ in 0..3 {
            let _ = render(&ctx, &mut tab, Vec::new());
        }
        let canvas = tab.canvas_rect;
        let start = Pos2::new(880.0, 300.0);
        let _ = render(
            &ctx,
            &mut tab,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        for x in [840.0, 780.0, 700.0] {
            let _ = render(
                &ctx,
                &mut tab,
                vec![egui::Event::PointerMoved(Pos2::new(x, 300.0))],
            );
        }
        assert_eq!(
            (
                tab.overview_width,
                tab.canvas_rect,
                tab.view.cycle,
                tab.view.row
            ),
            (300.0, canvas, 100, 0.0)
        );
    }

    #[test]
    fn opening_and_expanding_overlays_should_leave_the_canvas_size_unchanged() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        let _ = render(&ctx, &mut tab, Vec::new());
        let canvas = tab.canvas_rect;
        tab.sidebar = true;
        tab.details = true;
        tab.selected = Some(tab.rows[0].op.clone());
        for width in [320.0, 700.0] {
            tab.sidebar_width = width;
            let _ = render(&ctx, &mut tab, Vec::new());
            let _ = render(&ctx, &mut tab, Vec::new());
            assert_eq!(tab.canvas_rect, canvas);
        }
    }

    #[test]
    fn phase_and_disassembly_text_should_align_after_fractional_pan_and_zoom() {
        let (ctx, mut tab) = context_and_tab();
        for (row, zoom) in [(0.0, 1.0), (2.35, 1.4), (3.2, 0.8)] {
            tab.view = Viewport {
                cycle: 100,
                row,
                ..Default::default()
            };
            tab.view.zoom(zoom, 0.0, 0.0);
            let _ = render(&ctx, &mut tab, Vec::new());
            let output = render(&ctx, &mut tab, Vec::new());
            let mut centers = Vec::new();
            for shape in output.shapes {
                if let egui::epaint::Shape::Text(text) = shape.shape
                    && (text.galley.job.text == "F"
                        || text.galley.job.text.ends_with("add x1,x2,x3"))
                {
                    centers.push(text.pos.y + text.galley.size().y * 0.5);
                }
            }
            assert!(
                centers.len() == 2 && (centers[0] - centers[1]).abs() <= 0.5,
                "text centers: {centers:?}"
            );
        }
    }

    #[test]
    fn pinch_should_zoom_the_trace_while_preserving_the_canvas_extent() {
        let (ctx, mut tab) = context_and_tab();
        tab.sidebar = false;
        let _ = render(
            &ctx,
            &mut tab,
            vec![egui::Event::PointerMoved(Pos2::new(650.0, 300.0))],
        );
        let canvas = tab.canvas_rect;
        let _ = render(&ctx, &mut tab, vec![egui::Event::Zoom(2.0)]);
        assert_eq!(
            (tab.view.cycle_width, tab.canvas_rect),
            (DEFAULT_CYCLE_WIDTH * 2.0, canvas)
        );
    }

    #[test]
    fn dragging_the_disassembly_edge_should_follow_the_pointer_without_moving_the_canvas() {
        let (ctx, mut tab) = context_and_tab();
        let _ = render(&ctx, &mut tab, Vec::new());
        let _ = render(&ctx, &mut tab, Vec::new());
        let canvas = tab.canvas_rect;
        let start = Pos2::new(319.0, 300.0);
        let _ = render(
            &ctx,
            &mut tab,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        for x in [380.0, 440.0, 580.0] {
            let _ = render(
                &ctx,
                &mut tab,
                vec![egui::Event::PointerMoved(Pos2::new(x, 300.0))],
            );
        }
        assert_eq!((tab.sidebar_width, tab.canvas_rect), (580.0, canvas));
    }
}
