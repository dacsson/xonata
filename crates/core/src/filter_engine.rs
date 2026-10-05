//! Bounded, disk-backed two-pass phase analysis and viewport drawing selection.
use crate::filter::{DrawOptions, FilterBounds, FilterHit, FilterQuery, PhaseEndpoint};
use crate::filter_exclusion::{ExclusionIndex, ExclusionProbe};
use crate::storage::{PageStore, StoreError};
use crate::{Event, TraceInfo};
use lru::LruCache;
use std::collections::VecDeque;
use std::num::NonZeroUsize;

pub(crate) const RESULT_PAGE: u64 = 128;
fn cache<K: std::hash::Hash + Eq, V>(capacity: usize) -> LruCache<K, V> {
    LruCache::new(NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN))
}
#[derive(Default)]
struct TargetWriter {
    count: u64,
    block: Vec<PhaseEndpoint>,
}
pub(crate) struct FilterJob {
    pub generation: u32,
    pub query: FilterQuery,
    skip: u64,
    pages: Vec<u64>,
    cursor: usize,
    row: u64,
    targets_pass: bool,
    writers: LruCache<u64, TargetWriter>,
    targets: LruCache<(u64, u64), Vec<PhaseEndpoint>>,
    pub total: u64,
    pub skipped: u64,
    pub done: bool,
    block: Vec<FilterHit>,
    exclusions: ExclusionIndex,
    source_ids: Vec<u64>,
    op_cursor: usize,
    sources: VecDeque<PhaseEndpoint>,
    pending: Option<(FilterHit, ExclusionProbe)>,
}
impl FilterJob {
    pub fn new(generation: u32, query: FilterQuery, skip: u64, pages: Vec<u64>) -> Self {
        let targets_pass = query.target.is_some();
        Self {
            generation,
            query,
            skip,
            pages,
            cursor: 0,
            row: 0,
            targets_pass,
            writers: cache(32),
            targets: cache(32),
            total: 0,
            skipped: 0,
            done: false,
            block: Vec::new(),
            exclusions: ExclusionIndex::default(),
            source_ids: Vec::new(),
            op_cursor: 0,
            sources: VecDeque::new(),
            pending: None,
        }
    }
    async fn save_writer(
        &self,
        store: &mut PageStore,
        trace: u32,
        thread: u64,
        writer: &TargetWriter,
    ) -> Result<(), StoreError> {
        store
            .write_filter(
                trace,
                self.generation,
                &format!("count-{thread}"),
                &writer.count,
            )
            .await?;
        if !writer.block.is_empty() {
            store
                .write_filter(
                    trace,
                    self.generation,
                    &format!("target-{thread}-{}", writer.count / RESULT_PAGE),
                    &writer.block,
                )
                .await?;
        }
        Ok(())
    }
    async fn append_target(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        hit: PhaseEndpoint,
    ) -> Result<(), StoreError> {
        let thread = hit.thread;
        if !self.writers.contains(&thread) {
            if self.writers.len() == 32
                && let Some((tid, writer)) = self.writers.pop_lru()
            {
                self.save_writer(store, trace, tid, &writer).await?;
            }
            let count = store
                .read_filter(trace, self.generation, &format!("count-{thread}"))
                .await?;
            let block = if count % RESULT_PAGE != 0 {
                store
                    .read_filter(
                        trace,
                        self.generation,
                        &format!("target-{thread}-{}", count / RESULT_PAGE),
                    )
                    .await?
            } else {
                Vec::new()
            };
            self.writers.put(thread, TargetWriter { count, block });
        }
        if let Some(writer) = self.writers.get_mut(&thread) {
            writer.block.push(hit);
            writer.count += 1;
            if writer.block.len() == RESULT_PAGE as usize {
                store
                    .write_filter(
                        trace,
                        self.generation,
                        &format!("target-{thread}-{}", (writer.count - 1) / RESULT_PAGE),
                        &writer.block,
                    )
                    .await?;
                writer.block.clear();
            }
        }
        Ok(())
    }
    async fn target_at(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        thread: u64,
        index: u64,
    ) -> Result<Option<PhaseEndpoint>, StoreError> {
        let page = index / RESULT_PAGE;
        if !self.targets.contains(&(thread, page)) {
            let block = store
                .read_filter(trace, self.generation, &format!("target-{thread}-{page}"))
                .await?;
            self.targets.put((thread, page), block);
        }
        Ok(self
            .targets
            .get(&(thread, page))
            .and_then(|b| b.get((index % RESULT_PAGE) as usize))
            .cloned())
    }
    async fn pair(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        source: &PhaseEndpoint,
    ) -> Result<Option<PhaseEndpoint>, StoreError> {
        let count: u64 = store
            .read_filter(trace, self.generation, &format!("count-{}", source.thread))
            .await?;
        let (mut low, mut high) = (0, count);
        while low < high {
            let mid = low + (high - low) / 2;
            let Some(target) = self.target_at(store, trace, source.thread, mid).await? else {
                return Ok(None);
            };
            if target.row <= source.row {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        match low.checked_add(self.skip).filter(|index| *index < count) {
            Some(index) => self.target_at(store, trace, source.thread, index).await,
            None => Ok(None),
        }
    }
    async fn persist_results(
        &self,
        store: &mut PageStore,
        info: &TraceInfo,
    ) -> Result<(), StoreError> {
        if self.block.is_empty() {
            return Ok(());
        }
        let page = (self.total - 1) / RESULT_PAGE;
        store
            .write_filter(
                info.id,
                self.generation,
                &format!("results-{page}"),
                &self.block,
            )
            .await?;
        let mut bounds = self.block[0].bounds(&self.query.options, info.count);
        for hit in &self.block[1..] {
            let b = hit.bounds(&self.query.options, info.count);
            bounds.cycle_start = bounds.cycle_start.min(b.cycle_start);
            bounds.cycle_end = bounds.cycle_end.max(b.cycle_end);
            bounds.row_start = bounds.row_start.min(b.row_start);
            bounds.row_end = bounds.row_end.max(b.row_end);
        }
        store
            .write_filter(info.id, self.generation, &format!("bounds-{page}"), &bounds)
            .await
    }
    async fn exclusion_sources(
        &mut self,
        store: &mut PageStore,
        info: &TraceInfo,
    ) -> Result<(), StoreError> {
        let mut nodes = 64;
        let mut operations = 256;
        let mut occurrences = 128;
        loop {
            if let Some((hit, mut probe)) = self.pending.take() {
                match probe
                    .advance(
                        &mut self.exclusions,
                        store,
                        info.id,
                        self.generation,
                        &mut nodes,
                    )
                    .await?
                {
                    Some(true) => continue,
                    Some(false) => {
                        self.block.push(hit);
                        self.total += 1;
                        if self.block.len() == RESULT_PAGE as usize {
                            self.persist_results(store, info).await?;
                            self.block.clear();
                        }
                    }
                    None => {
                        self.pending = Some((hit, probe));
                        break;
                    }
                }
            }
            if nodes == 0 || occurrences == 0 {
                break;
            }
            if let Some(source) = self.sources.pop_front() {
                occurrences -= 1;
                let Some(target) = self.pair(store, info.id, &source).await? else {
                    continue;
                };
                if !self.query.accepts(&source, &target) {
                    continue;
                }
                let probe = self
                    .query
                    .exclusion
                    .as_ref()
                    .and_then(|q| self.exclusions.probe(&source, &target, q.scope, info.count));
                let hit = FilterHit {
                    index: self.total,
                    elapsed: i128::from(target.cycle) - i128::from(source.cycle),
                    source,
                    target: Some(target),
                };
                if let Some(probe) = probe {
                    self.pending = Some((hit, probe));
                } else {
                    self.block.push(hit);
                    self.total += 1;
                    if self.block.len() == RESULT_PAGE as usize {
                        self.persist_results(store, info).await?;
                        self.block.clear();
                    }
                }
            } else {
                if operations == 0 {
                    break;
                }
                if self.cursor >= self.pages.len() {
                    self.done = true;
                    break;
                }
                if self.source_ids.is_empty() {
                    self.source_ids = store.page_ids(info.id, self.pages[self.cursor]);
                }
                if self.op_cursor >= self.source_ids.len() {
                    self.source_ids.clear();
                    self.op_cursor = 0;
                    self.cursor += 1;
                    continue;
                }
                let id = self.source_ids[self.op_cursor];
                self.op_cursor += 1;
                operations -= 1;
                if let Some(op) = store.get(info.id, id).await? {
                    let (endpoints, skipped) = self.query.source.endpoints(&op, self.row, info);
                    self.skipped += skipped;
                    self.sources = endpoints.into();
                    self.row += 1;
                }
            }
        }
        Ok(())
    }
    pub async fn tick(
        &mut self,
        store: &mut PageStore,
        info: &TraceInfo,
    ) -> Result<Event, StoreError> {
        // One operation page per tick; all work stays off the rendering thread.
        if !self.done && !self.targets_pass && self.query.exclusion.is_some() {
            self.exclusion_sources(store, info).await?;
        } else if !self.done && self.cursor < self.pages.len() {
            let ids = store.page_ids(info.id, self.pages[self.cursor]);
            for id in ids {
                if let Some(op) = store.get(info.id, id).await? {
                    if self.targets_pass {
                        if let Some(exclusion) = &self.query.exclusion {
                            let (endpoints, skipped) =
                                exclusion.endpoint.endpoints(&op, self.row, info);
                            self.skipped += skipped;
                            for endpoint in endpoints {
                                self.exclusions
                                    .append(
                                        store,
                                        info.id,
                                        self.generation,
                                        endpoint.row,
                                        endpoint.cycle,
                                    )
                                    .await?;
                            }
                        }
                        if let Some(query) = &self.query.target {
                            let (endpoints, skipped) = query.endpoints(&op, self.row, info);
                            self.skipped += skipped;
                            if let Some(hit) =
                                endpoints.into_iter().min_by_key(|h| (h.cycle, h.stage))
                            {
                                self.append_target(store, info.id, hit).await?;
                            }
                        }
                    } else {
                        let (endpoints, skipped) = self.query.source.endpoints(&op, self.row, info);
                        self.skipped += skipped;
                        for source in endpoints {
                            let target = if self.query.target.is_some() {
                                let Some(target) = self.pair(store, info.id, &source).await? else {
                                    continue;
                                };
                                if !self.query.accepts(&source, &target) {
                                    continue;
                                }
                                Some(target)
                            } else {
                                None
                            };
                            let elapsed = target
                                .as_ref()
                                .map_or(source.end as i128 - source.start as i128, |t| {
                                    t.cycle as i128 - source.cycle as i128
                                });
                            self.block.push(FilterHit {
                                index: self.total,
                                source,
                                target,
                                elapsed,
                            });
                            self.total += 1;
                            if self.block.len() == RESULT_PAGE as usize {
                                self.persist_results(store, info).await?;
                                self.block.clear();
                            }
                        }
                    }
                    self.row += 1;
                }
            }
            self.cursor += 1;
        }
        if !self.done && self.cursor >= self.pages.len() {
            if self.targets_pass {
                while let Some((thread, writer)) = self.writers.pop_lru() {
                    self.save_writer(store, info.id, thread, &writer).await?;
                }
                self.exclusions
                    .finish(store, info.id, self.generation)
                    .await?;
                self.targets_pass = false;
                self.cursor = 0;
                self.row = 0;
            } else {
                self.done = true;
            }
        }
        self.persist_results(store, info).await?;
        let fraction = self.cursor as f32 / self.pages.len().max(1) as f32;
        let progress = if self.done {
            1.0
        } else if self.query.target.is_some() {
            if self.targets_pass {
                fraction * 0.5
            } else {
                0.5 + fraction * 0.5
            }
        } else {
            fraction
        };
        Ok(Event::FilterProgress {
            trace: info.id,
            generation: self.generation,
            total: self.total,
            skipped: self.skipped,
            progress,
            done: self.done,
            options: self.query.options.clone(),
        })
    }
}
pub(crate) async fn result_page(
    store: &mut PageStore,
    trace: u32,
    generation: u32,
    start: u64,
    count: u32,
    total: u64,
) -> Result<Vec<FilterHit>, StoreError> {
    let end = start.saturating_add(count.min(128) as u64).min(total);
    let mut hits = Vec::new();
    let mut page = u64::MAX;
    let mut block: Vec<FilterHit> = Vec::new();
    for index in start..end {
        if page != index / RESULT_PAGE {
            page = index / RESULT_PAGE;
            block = store
                .read_filter(trace, generation, &format!("results-{page}"))
                .await?;
        }
        if let Some(hit) = block.get((index % RESULT_PAGE) as usize) {
            hits.push(hit.clone());
        }
    }
    Ok(hits)
}
pub(crate) struct Drawing {
    pub generation: u32,
    pub total: u64,
    pub options: DrawOptions,
}
// Disk-backed merge sort: each tick decodes at most 1,024 compact records.
// Two alternating page namespaces prevent disk use growing with sort toggles.
pub(crate) struct ResultSort {
    pub revision: u32,
    pub done: bool,
    total: u64,
    descending: bool,
    cursor: u64,
    width: u64,
    bank: u8,
    merging: bool,
    left: u64,
    right: u64,
    left_end: u64,
    right_end: u64,
    buffers: [Vec<(i128, u64)>; 2],
    pages: [u64; 2],
    output: Vec<(i128, u64)>,
}
impl ResultSort {
    pub fn new(revision: u32, descending: bool, total: u64) -> Self {
        Self {
            revision,
            done: false,
            total,
            descending,
            cursor: 0,
            width: 1024,
            bank: 0,
            merging: false,
            left: 0,
            right: 0,
            left_end: 0,
            right_end: 0,
            buffers: Default::default(),
            pages: [u64::MAX; 2],
            output: Vec::new(),
        }
    }
    fn compare(descending: bool, a: &(i128, u64), b: &(i128, u64)) -> std::cmp::Ordering {
        let order = a.0.cmp(&b.0);
        (if descending { order.reverse() } else { order }).then_with(|| a.1.cmp(&b.1))
    }
    async fn peek(
        &mut self,
        side: usize,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
    ) -> Result<Option<(i128, u64)>, StoreError> {
        let (position, end) = if side == 0 {
            (self.left, self.left_end)
        } else {
            (self.right, self.right_end)
        };
        if position >= end {
            return Ok(None);
        }
        let page = position / RESULT_PAGE;
        if self.pages[side] != page {
            self.buffers[side] = store
                .read_filter(trace, generation, &format!("sort-{}-{page}", self.bank))
                .await?;
            self.pages[side] = page;
        }
        Ok(self.buffers[side]
            .get((position % RESULT_PAGE) as usize)
            .copied())
    }
    pub async fn tick(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
    ) -> Result<Event, StoreError> {
        if !self.merging {
            let end = self.cursor.saturating_add(self.width).min(self.total);
            let mut run = Vec::with_capacity((end - self.cursor) as usize);
            let mut position = self.cursor;
            while position < end {
                let hits = result_page(store, trace, generation, position, 128, self.total).await?;
                if hits.is_empty() {
                    return Err(StoreError::Codec("Missing elapsed-sort source page".into()));
                }
                position += hits.len() as u64;
                run.extend(hits.into_iter().map(|h| (h.elapsed, h.index)));
            }
            run.sort_unstable_by(|a, b| Self::compare(self.descending, a, b));
            for (offset, block) in run.chunks(RESULT_PAGE as usize).enumerate() {
                store
                    .write_filter(
                        trace,
                        generation,
                        &format!("sort-0-{}", self.cursor / RESULT_PAGE + offset as u64),
                        &block,
                    )
                    .await?;
            }
            self.cursor = end;
            if self.cursor >= self.total {
                self.done = self.total <= self.width;
                self.merging = true;
                self.cursor = 0;
            }
        } else {
            for _ in 0..1024 {
                if self.cursor >= self.total {
                    break;
                }
                if self.left >= self.left_end && self.right >= self.right_end {
                    self.left = self.cursor;
                    self.left_end = self.left.saturating_add(self.width).min(self.total);
                    self.right = self.left_end;
                    self.right_end = self.right.saturating_add(self.width).min(self.total);
                }
                let a = self.peek(0, store, trace, generation).await?;
                let b = self.peek(1, store, trace, generation).await?;
                let take_left = match (a, b) {
                    (Some(a), Some(b)) => Self::compare(self.descending, &a, &b).is_le(),
                    (Some(_), None) => true,
                    _ => false,
                };
                if let Some(value) = if take_left {
                    self.left += 1;
                    a
                } else {
                    self.right += 1;
                    b
                } {
                    self.output.push(value);
                    self.cursor += 1;
                } else {
                    return Err(StoreError::Codec("Missing elapsed-sort record".into()));
                }
                if self.output.len() == RESULT_PAGE as usize || self.cursor == self.total {
                    store
                        .write_filter(
                            trace,
                            generation,
                            &format!("sort-{}-{}", 1 - self.bank, (self.cursor - 1) / RESULT_PAGE),
                            &self.output,
                        )
                        .await?;
                    self.output.clear();
                }
            }
            if self.cursor >= self.total {
                self.bank = 1 - self.bank;
                self.width = self.width.saturating_mul(2);
                self.done = self.width >= self.total;
                self.cursor = 0;
                self.left = 0;
                self.right = 0;
                self.left_end = 0;
                self.right_end = 0;
                self.pages = [u64::MAX; 2];
            }
        }
        Ok(Event::FilterSortProgress {
            trace,
            generation,
            revision: self.revision,
            total: self.total,
            progress: if self.done {
                1.0
            } else {
                self.cursor as f32 / self.total.max(1) as f32
            },
            done: self.done,
        })
    }
    pub async fn page(
        &self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
        start: u64,
        count: u32,
    ) -> Result<Vec<FilterHit>, StoreError> {
        let end = start.saturating_add(count.min(128) as u64).min(self.total);
        let mut page = u64::MAX;
        let mut records: Vec<(i128, u64)> = Vec::new();
        let mut source_page = u64::MAX;
        let mut hits: Vec<FilterHit> = Vec::new();
        let mut result = Vec::new();
        for position in start..end {
            if page != position / RESULT_PAGE {
                page = position / RESULT_PAGE;
                records = store
                    .read_filter(trace, generation, &format!("sort-{}-{page}", self.bank))
                    .await?;
            }
            if let Some((_, original)) = records.get((position % RESULT_PAGE) as usize) {
                if source_page != original / RESULT_PAGE {
                    source_page = original / RESULT_PAGE;
                    hits = store
                        .read_filter(trace, generation, &format!("results-{source_page}"))
                        .await?;
                }
                if let Some(hit) = hits.get((original % RESULT_PAGE) as usize) {
                    result.push(hit.clone());
                }
            }
        }
        Ok(result)
    }
}
pub(crate) struct DrawingScan {
    pub request: u32,
    pub generation: u32,
    bounds: FilterBounds,
    cursor: u64,
    total: u64,
    selected: Option<u64>,
    visible: u64,
    hits: Vec<FilterHit>,
}
impl DrawingScan {
    pub fn new(
        request: u32,
        drawing: &Drawing,
        bounds: FilterBounds,
        selected: Option<u64>,
    ) -> Self {
        Self {
            request,
            generation: drawing.generation,
            bounds,
            cursor: 0,
            total: drawing.total,
            selected,
            visible: 0,
            hits: Vec::new(),
        }
    }
    pub async fn tick(
        &mut self,
        store: &mut PageStore,
        info: &TraceInfo,
        drawing: &Drawing,
    ) -> Result<Option<Event>, StoreError> {
        let pages = self.total.div_ceil(RESULT_PAGE);
        for _ in 0..8 {
            if self.cursor >= pages {
                break;
            }
            let bounds: FilterBounds = store
                .read_filter(info.id, self.generation, &format!("bounds-{}", self.cursor))
                .await?;
            if bounds.intersects(self.bounds) {
                let block: Vec<FilterHit> = store
                    .read_filter(
                        info.id,
                        self.generation,
                        &format!("results-{}", self.cursor),
                    )
                    .await?;
                for hit in block {
                    if hit
                        .bounds(&drawing.options, info.count)
                        .intersects(self.bounds)
                    {
                        self.visible += 1;
                        if self.hits.len() < 2048 {
                            self.hits.push(hit);
                        } else if self.selected == Some(hit.index) {
                            self.hits.pop();
                            self.hits.push(hit);
                        }
                    }
                }
            }
            self.cursor += 1;
        }
        Ok((self.cursor >= pages).then(|| Event::FilterDrawing {
            trace: info.id,
            generation: self.generation,
            request: self.request,
            visible: self.visible,
            hits: std::mem::take(&mut self.hits),
        }))
    }
}
