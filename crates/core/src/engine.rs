//! Single-owner trace worker with interleaved viewport and search commands.
use crate::compression::Decoder;
use crate::filter::FilterQuery;
use crate::filter_engine::{Drawing, DrawingScan, FilterJob, ResultSort, result_page};
use crate::filter_suggestions::Suggestions;
use crate::parser::Parser;
use crate::search::Matcher;
use crate::storage::{BackingStore, CACHE_BUDGET, PageStore, StoreError};
use crate::{Event, OverviewRaster, Request, Row, SearchHit, TraceInfo};
use std::collections::BTreeMap;

struct Trace {
    parser: Parser,
    decoder: Decoder,
    search: Option<Search>,
    overview: Option<Overview>,
    filter: Option<FilterJob>,
    sorting: Option<ResultSort>,
    drawing: Option<Drawing>,
    drawing_scan: Option<DrawingScan>,
    suggestions: Option<Suggestions>,
}
struct Search {
    generation: u32,
    matcher: Matcher,
    pages: Vec<u64>,
    cursor: usize,
    row: u64,
    total: u64,
    block: Vec<(u64, u64)>,
}

struct Overview {
    generation: u32,
    pages: Vec<u64>,
    cursor: usize,
    row: u64,
    raster: OverviewRaster,
}

fn overview_column(cycle: u64, info: &TraceInfo, width: usize) -> usize {
    let span = (info.last_cycle.saturating_sub(info.first_cycle) as u128 + 1).max(1);
    ((cycle.saturating_sub(info.first_cycle) as u128 * width as u128 / span) as usize)
        .min(width - 1)
}

/// State and cache owned by one native or browser worker.
pub struct Engine {
    store: PageStore,
    traces: BTreeMap<u32, Trace>,
}
impl Engine {
    /// Constructs a worker with a shared 128 MiB decoded-page budget.
    pub fn new(backing: Box<dyn BackingStore>) -> Self {
        Self {
            store: PageStore::new(backing, CACHE_BUDGET),
            traces: BTreeMap::new(),
        }
    }
    /// Starts a trace, replacing any previous use of the same ID.
    pub async fn open(&mut self, id: u32, name: String) -> Result<Event, StoreError> {
        if self.traces.contains_key(&id) {
            self.close(id).await?;
        }
        let parser = Parser::new(id, name.clone());
        self.traces.insert(
            id,
            Trace {
                parser,
                decoder: Decoder::from_name(&name),
                search: None,
                overview: None,
                filter: None,
                sorting: None,
                drawing: None,
                drawing_scan: None,
                suggestions: None,
            },
        );
        self.store.directories.insert(id, BTreeMap::new());
        Ok(Event::Info {
            info: self.traces[&id].parser.info.clone(),
        })
    }
    /// Appends at most 256 KiB of original file bytes.
    pub async fn feed(&mut self, id: u32, bytes: &[u8]) -> Result<Event, StoreError> {
        let trace = self
            .traces
            .get_mut(&id)
            .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
        for chunk in trace.decoder.feed(bytes)? {
            trace.parser.feed(&mut self.store, &chunk).await?;
        }
        trace.parser.info.bytes += bytes.len() as u64;
        Ok(Event::Info {
            info: trace.parser.info.clone(),
        })
    }
    /// Validates compression and marks unretired instructions incomplete.
    pub async fn finish(&mut self, id: u32) -> Result<Event, StoreError> {
        let trace = self
            .traces
            .get_mut(&id)
            .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
        let final_bytes = trace.decoder.finish()?;
        if !final_bytes.is_empty() {
            trace.parser.feed(&mut self.store, &final_bytes).await?;
        }
        trace.parser.finish(&mut self.store).await?;
        Ok(Event::Info {
            info: trace.parser.info.clone(),
        })
    }
    /// Drops one trace and its worker-owned storage.
    pub async fn close(&mut self, id: u32) -> Result<(), StoreError> {
        self.traces.remove(&id);
        self.store.remove(id).await
    }
    /// Executes one bounded UI request.
    pub async fn request(&mut self, request: Request) -> Result<Option<Event>, StoreError> {
        match request {
            Request::Close { trace } => {
                self.close(trace).await?;
                Ok(None)
            }
            Request::View {
                trace,
                generation,
                start,
                stride,
                count,
                hide_flushed,
            } => {
                let ids = self
                    .store
                    .visible_ids(trace, start, stride, count, hide_flushed);
                let mut rows = Vec::with_capacity(ids.len());
                for (index, id) in ids {
                    if let Some(op) = self.store.get(trace, id).await? {
                        rows.push(Row { index, op });
                    }
                }
                Ok(Some(Event::View {
                    trace,
                    generation,
                    rows,
                }))
            }
            Request::Overview {
                trace,
                generation,
                width,
                height,
            } => {
                let pages = self
                    .store
                    .directories
                    .get(&trace)
                    .map_or_else(Vec::new, |directory| directory.keys().copied().collect());
                let target = self
                    .traces
                    .get_mut(&trace)
                    .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
                if !target.parser.info.complete {
                    return Ok(None);
                }
                let width = width.clamp(1, 256);
                let height = height
                    .clamp(1, 1024)
                    .min(target.parser.info.count.clamp(1, 1024) as u16);
                target.overview = Some(Overview {
                    generation,
                    pages,
                    cursor: 0,
                    row: 0,
                    raster: OverviewRaster {
                        width,
                        height,
                        pixels: vec![0; width as usize * height as usize],
                    },
                });
                Ok(None)
            }
            Request::JumpRow { trace, row } => {
                let target = self
                    .store
                    .visible_ids(trace, row, 1, 1, false)
                    .first()
                    .copied();
                let row = if let Some((index, id)) = target {
                    self.store.get(trace, id).await?.map(|op| Row { index, op })
                } else {
                    None
                };
                Ok(Some(Event::Jump { trace, row }))
            }
            Request::Search {
                trace,
                generation,
                query,
            } => {
                if let Some(target) = self.traces.get_mut(&trace) {
                    target.search = None;
                }
                self.store.clear_search(trace).await?;
                let matcher = Matcher::new(*query)?;
                let pages = self
                    .store
                    .directories
                    .get(&trace)
                    .map_or_else(Vec::new, |d| d.keys().copied().collect());
                let target = self
                    .traces
                    .get_mut(&trace)
                    .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
                target.search = Some(Search {
                    generation,
                    matcher,
                    pages,
                    cursor: 0,
                    row: 0,
                    total: 0,
                    block: Vec::new(),
                });
                let empty = target.search.as_ref().is_some_and(|s| s.pages.is_empty());
                Ok(Some(Event::SearchProgress {
                    trace,
                    generation,
                    total: 0,
                    progress: if empty { 1.0 } else { 0.0 },
                    done: empty,
                }))
            }
            Request::CancelSearch { trace } => {
                if let Some(target) = self.traces.get_mut(&trace) {
                    target.search = None;
                }
                self.store.clear_search(trace).await?;
                Ok(None)
            }
            Request::Results {
                trace,
                generation,
                start,
                count,
            } => {
                let target = self
                    .traces
                    .get(&trace)
                    .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
                let search = target
                    .search
                    .as_ref()
                    .ok_or_else(|| StoreError::Input("Search is closed".into()))?;
                if generation != search.generation {
                    return Ok(None);
                }
                let mut hits = Vec::new();
                let end = start
                    .saturating_add(count.min(256) as u64)
                    .min(search.total);
                let mut loaded_page = u64::MAX;
                let mut block = Vec::new();
                for n in start..end {
                    if loaded_page != n / 1024 {
                        loaded_page = n / 1024;
                        block = self.store.read_hits(trace, generation, loaded_page).await?;
                    }
                    if let Some(&(row, id)) = block.get((n % 1024) as usize)
                        && let Some(op) = self.store.get(trace, id).await?
                    {
                        let snippets = search
                            .matcher
                            .snippets(&op, &target.parser.info.symbols)
                            .unwrap_or_default();
                        hits.push(SearchHit { op, row, snippets });
                    }
                }
                Ok(Some(Event::Results {
                    trace,
                    generation,
                    start,
                    hits,
                }))
            }
            Request::FilterSuggestions {
                trace,
                generation,
                field,
                text,
            } => {
                let Some(target) = self.traces.get_mut(&trace) else {
                    return Ok(None);
                };
                if !target.parser.info.complete {
                    return Ok(None);
                }
                let pages = self
                    .store
                    .directories
                    .get(&trace)
                    .map_or_else(Vec::new, |d| d.keys().copied().collect());
                target.suggestions = Some(Suggestions::new(generation, field, &text, pages)?);
                Ok(None)
            }
            Request::Filter {
                trace,
                generation,
                text,
                skip,
            } => {
                let target = self
                    .traces
                    .get_mut(&trace)
                    .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
                if !target.parser.info.complete {
                    return Ok(Some(Event::FilterError {
                        trace,
                        generation,
                        message: "Wait for trace loading to finish".into(),
                    }));
                }
                target.sorting = None;
                if let Some(old) = target.filter.take()
                    && !target
                        .drawing
                        .as_ref()
                        .is_some_and(|d| d.generation == old.generation)
                {
                    self.store.clear_filter(trace, old.generation).await?;
                }
                let query = match FilterQuery::parse(&text) {
                    Ok(query) => query,
                    Err(error) => {
                        return Ok(Some(Event::FilterError {
                            trace,
                            generation,
                            message: error.to_string(),
                        }));
                    }
                };
                let pages = self
                    .store
                    .directories
                    .get(&trace)
                    .map_or_else(Vec::new, |d| d.keys().copied().collect());
                target.filter = Some(FilterJob::new(generation, query, skip, pages));
                Ok(None)
            }
            Request::CancelFilter { trace, generation } => {
                if let Some(target) = self.traces.get_mut(&trace)
                    && target
                        .filter
                        .as_ref()
                        .is_some_and(|f| f.generation == generation)
                {
                    target.sorting = None;
                }
                if let Some(target) = self.traces.get_mut(&trace)
                    && target
                        .filter
                        .as_ref()
                        .is_some_and(|f| f.generation == generation)
                    && let Some(old) = target.filter.take()
                    && !target
                        .drawing
                        .as_ref()
                        .is_some_and(|d| d.generation == old.generation)
                {
                    self.store.clear_filter(trace, old.generation).await?;
                }
                Ok(None)
            }
            Request::FilterResults {
                trace,
                generation,
                start,
                count,
            } => {
                let Some(job) = self
                    .traces
                    .get(&trace)
                    .and_then(|t| t.filter.as_ref())
                    .filter(|f| f.generation == generation)
                else {
                    return Ok(None);
                };
                let hits = result_page(&mut self.store, trace, generation, start, count, job.total)
                    .await?;
                Ok(Some(Event::FilterResults {
                    trace,
                    generation,
                    start,
                    hits,
                }))
            }
            Request::FilterSort {
                trace,
                generation,
                revision,
                descending,
            } => {
                let Some(target) = self.traces.get_mut(&trace) else {
                    return Ok(None);
                };
                let Some(job) = target
                    .filter
                    .as_ref()
                    .filter(|f| f.generation == generation && f.done)
                else {
                    return Ok(None);
                };
                if target
                    .sorting
                    .as_ref()
                    .is_some_and(|old| revision.wrapping_sub(old.revision) >= (1 << 31))
                {
                    return Ok(None);
                }
                target.sorting = Some(ResultSort::new(revision, descending, job.total));
                Ok(None)
            }
            Request::SortedFilterResults {
                trace,
                generation,
                revision,
                start,
                count,
            } => {
                let Some(target) = self.traces.get(&trace).filter(|t| {
                    t.filter
                        .as_ref()
                        .is_some_and(|f| f.generation == generation)
                }) else {
                    return Ok(None);
                };
                let Some(view) = target
                    .sorting
                    .as_ref()
                    .filter(|r| r.revision == revision && r.done)
                else {
                    return Ok(None);
                };
                let hits = view
                    .page(&mut self.store, trace, generation, start, count)
                    .await?;
                Ok(Some(Event::SortedFilterResults {
                    trace,
                    generation,
                    revision,
                    start,
                    hits,
                }))
            }
            Request::RevealFilter {
                trace,
                generation,
                index,
            } => {
                let Some(job) = self
                    .traces
                    .get(&trace)
                    .and_then(|t| t.filter.as_ref())
                    .filter(|f| f.generation == generation)
                else {
                    return Ok(None);
                };
                let mut hits =
                    result_page(&mut self.store, trace, generation, index, 1, job.total).await?;
                let Some(hit) = hits.pop() else {
                    return Ok(None);
                };
                let Some(op) = self.store.get(trace, hit.source.op).await? else {
                    return Ok(None);
                };
                let operation =
                    serde_json::to_string(&op).map_err(|e| StoreError::Codec(e.to_string()))?;
                Ok(Some(Event::FilterSelection {
                    trace,
                    generation,
                    hit,
                    operation,
                }))
            }
            Request::DrawFilter {
                trace,
                generation,
                label,
            } => {
                let Some(target) = self.traces.get_mut(&trace) else {
                    return Ok(None);
                };
                let Some(job) = target
                    .filter
                    .as_ref()
                    .filter(|f| f.generation == generation && f.done)
                else {
                    return Ok(None);
                };
                if label.chars().count() > 80 {
                    return Ok(Some(Event::FilterError {
                        trace,
                        generation,
                        message: "Drawing label exceeds 80 characters".into(),
                    }));
                }
                let mut options = job.query.options.clone();
                if !label.trim().is_empty() {
                    options.label = label;
                }
                let total = job.total;
                if let Some(old) = target.drawing.take()
                    && old.generation != generation
                {
                    self.store.clear_filter(trace, old.generation).await?;
                }
                target.drawing = Some(Drawing {
                    generation,
                    total,
                    options: options.clone(),
                });
                target.drawing_scan = None;
                Ok(Some(Event::FilterDrawn {
                    trace,
                    generation,
                    options,
                }))
            }
            Request::ClearFilterDrawing { trace } => {
                if let Some(target) = self.traces.get_mut(&trace) {
                    if let Some(old) = target.drawing.take()
                        && !target
                            .filter
                            .as_ref()
                            .is_some_and(|f| f.generation == old.generation)
                    {
                        self.store.clear_filter(trace, old.generation).await?;
                    }
                    target.drawing_scan = None;
                }
                Ok(None)
            }
            Request::FilterViewport {
                trace,
                generation,
                request,
                bounds,
                selected,
            } => {
                if let Some(target) = self.traces.get_mut(&trace)
                    && let Some(drawing) = target
                        .drawing
                        .as_ref()
                        .filter(|d| d.generation == generation)
                {
                    target.drawing_scan = Some(DrawingScan::new(
                        request,
                        drawing,
                        bounds,
                        selected.and_then(|s| s.parse().ok()),
                    ));
                }
                Ok(None)
            }
            Request::Jump { trace, id, retired } => {
                let mut found = None;
                if retired {
                    let pages: Vec<_> = self
                        .store
                        .directories
                        .get(&trace)
                        .map_or_else(Vec::new, |d| d.keys().copied().collect());
                    for page in pages {
                        for op_id in self.store.page_ids(trace, page) {
                            if let Some(op) = self.store.get(trace, op_id).await?
                                && op.rid == Some(id)
                                && !op.flushed
                            {
                                found = Some(op);
                                break;
                            }
                        }
                        if found.is_some() {
                            break;
                        }
                    }
                } else {
                    found = self.store.get(trace, id).await?;
                }
                let row = found.and_then(|op| {
                    self.store
                        .row_of(trace, op.id)
                        .map(|index| Row { index, op })
                });
                Ok(Some(Event::Jump { trace, row }))
            }
        }
    }
    /// Advances bounded overview and search batches during worker idle periods.
    pub async fn tick(&mut self) -> Result<Vec<Event>, StoreError> {
        let ids: Vec<u32> = self.traces.keys().copied().collect();
        let mut events = Vec::new();
        for id in ids {
            let Some(trace) = self.traces.get_mut(&id) else {
                continue;
            };
            if let Some(overview) = trace.overview.as_mut() {
                let width = overview.raster.width as usize;
                let height = overview.raster.height as usize;
                // Small batches let view and search requests interleave with raster construction.
                for _ in 0..4 {
                    if overview.cursor >= overview.pages.len() {
                        break;
                    }
                    for op_id in self.store.page_ids(id, overview.pages[overview.cursor]) {
                        if let Some(op) = self.store.get(id, op_id).await? {
                            let y = (overview.row as u128 * height as u128
                                / trace.parser.info.count.max(1) as u128)
                                as usize;
                            let y = y.min(height - 1);
                            for stage in &op.stages {
                                let first = overview_column(stage.start, &trace.parser.info, width);
                                let last = overview_column(
                                    stage
                                        .end
                                        .unwrap_or(trace.parser.info.last_cycle.saturating_add(1))
                                        .saturating_sub(1)
                                        .max(stage.start),
                                    &trace.parser.info,
                                    width,
                                );
                                // Keep subpixel phases visible when the raster is narrowed on screen.
                                let last = last.max((first + 2).min(width - 1));
                                let color = 1 + (stage.name % 8) as u8;
                                overview.raster.pixels[y * width + first..=y * width + last]
                                    .fill(color | if op.flushed { 128 } else { 0 });
                            }
                            overview.row += 1;
                        }
                    }
                    overview.cursor += 1;
                }
                if overview.cursor >= overview.pages.len()
                    && let Some(overview) = trace.overview.take()
                {
                    events.push(Event::Overview {
                        trace: id,
                        generation: overview.generation,
                        raster: overview.raster,
                    });
                }
            }
            if let Some(suggestions) = trace.suggestions.as_mut() {
                let event = suggestions
                    .tick(&mut self.store, &trace.parser.info)
                    .await?;
                if matches!(&event, Event::FilterSuggestions { done: true, .. }) {
                    trace.suggestions = None;
                }
                events.push(event);
            }
            if let Some(view) = trace.sorting.as_mut().filter(|r| !r.done)
                && let Some(filter) = &trace.filter
            {
                events.push(view.tick(&mut self.store, id, filter.generation).await?);
            }
            if let Some(filter) = trace.filter.as_mut().filter(|f| !f.done) {
                match filter.tick(&mut self.store, &trace.parser.info).await {
                    Ok(event) => events.push(event),
                    Err(error) => {
                        events.push(Event::FilterError {
                            trace: id,
                            generation: filter.generation,
                            message: error.to_string(),
                        });
                        let generation = filter.generation;
                        trace.filter = None;
                        if !trace
                            .drawing
                            .as_ref()
                            .is_some_and(|d| d.generation == generation)
                        {
                            self.store.clear_filter(id, generation).await?;
                        }
                    }
                }
            }
            if let (Some(scan), Some(drawing)) =
                (trace.drawing_scan.as_mut(), trace.drawing.as_ref())
                && let Some(event) = scan
                    .tick(&mut self.store, &trace.parser.info, drawing)
                    .await?
            {
                events.push(event);
                trace.drawing_scan = None;
            }
            let Some(search) = trace.search.as_mut() else {
                continue;
            };
            if search.cursor >= search.pages.len() {
                continue;
            }
            for _ in 0..16 {
                if search.cursor >= search.pages.len() {
                    break;
                }
                let page = search.pages[search.cursor];
                let page_ids = self.store.page_ids(id, page);
                for op_id in page_ids {
                    if let Some(op) = self.store.get(id, op_id).await? {
                        if search
                            .matcher
                            .snippets(&op, &trace.parser.info.symbols)
                            .is_some()
                        {
                            search.block.push((search.row, op_id));
                            search.total += 1;
                            if search.block.len() == 1024 {
                                self.store
                                    .write_hits(
                                        id,
                                        search.generation,
                                        (search.total - 1) / 1024,
                                        &search.block,
                                    )
                                    .await?;
                                search.block.clear();
                            }
                        }
                        search.row += 1;
                    }
                }
                search.cursor += 1;
            }
            if !search.block.is_empty() {
                self.store
                    .write_hits(id, search.generation, search.total / 1024, &search.block)
                    .await?;
            }
            events.push(Event::SearchProgress {
                trace: id,
                generation: search.generation,
                total: search.total,
                progress: search.cursor as f32 / search.pages.len() as f32,
                done: search.cursor == search.pages.len(),
            });
        }
        Ok(events)
    }
    /// Whether at least one search or overview still needs worker time.
    pub fn has_search_work(&self) -> bool {
        self.traces.values().any(|trace| {
            trace
                .search
                .as_ref()
                .is_some_and(|s| s.cursor < s.pages.len())
                || trace.overview.is_some()
                || trace.filter.as_ref().is_some_and(|f| !f.done)
                || trace.drawing_scan.is_some()
                || trace.sorting.as_ref().is_some_and(|r| !r.done)
                || trace.suggestions.is_some()
        })
    }
    /// Returns shared cache use.
    pub fn metrics(&self) -> (usize, usize) {
        self.store.metrics()
    }
    /// Returns status for one trace.
    pub fn info(&self, id: u32) -> Option<&TraceInfo> {
        self.traces.get(&id).map(|trace| &trace.parser.info)
    }
}
