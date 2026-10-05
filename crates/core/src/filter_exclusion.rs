//! Compact disk-backed spatial index for forbidden phase boundaries.
use crate::filter::{ExcludeScope, FilterBounds, PhaseEndpoint};
use crate::storage::{PageStore, StoreError};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

const LEAF_SIZE: usize = 128;
const FANOUT: usize = 32;
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Node {
    bounds: FilterBounds,
    level: Option<u32>,
    page: u64,
}
#[derive(Default)]
struct Level {
    nodes: Vec<Node>,
    written: u64,
}
pub(crate) struct ExclusionIndex {
    points: Vec<(u64, u64)>,
    leaves: u64,
    levels: Vec<Level>,
    root: Option<Node>,
    nodes: LruCache<(u32, u64), Vec<Node>>,
    cache: LruCache<u64, Vec<(u64, u64)>>,
}
impl Default for ExclusionIndex {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            leaves: 0,
            levels: Vec::new(),
            root: None,
            nodes: LruCache::new(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN)),
            cache: LruCache::new(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN)),
        }
    }
}
fn union(a: FilterBounds, b: FilterBounds) -> FilterBounds {
    FilterBounds {
        cycle_start: a.cycle_start.min(b.cycle_start),
        cycle_end: a.cycle_end.max(b.cycle_end),
        row_start: a.row_start.min(b.row_start),
        row_end: a.row_end.max(b.row_end),
    }
}
impl ExclusionIndex {
    async fn group(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
        level: usize,
    ) -> Result<Node, StoreError> {
        let buffer = &mut self.levels[level];
        let bounds = buffer
            .nodes
            .iter()
            .skip(1)
            .fold(buffer.nodes[0].bounds, |b, n| union(b, n.bounds));
        let page = buffer.written;
        store
            .write_filter(
                trace,
                generation,
                &format!("exclude-node-{level}-{page}"),
                &buffer.nodes,
            )
            .await?;
        buffer.written += 1;
        buffer.nodes.clear();
        Ok(Node {
            bounds,
            level: Some(level as u32),
            page,
        })
    }
    async fn push(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
        mut level: usize,
        mut node: Node,
    ) -> Result<(), StoreError> {
        loop {
            if level == self.levels.len() {
                self.levels.push(Level::default());
            }
            self.levels[level].nodes.push(node);
            if self.levels[level].nodes.len() < FANOUT {
                return Ok(());
            }
            node = self.group(store, trace, generation, level).await?;
            level += 1;
        }
    }
    async fn leaf(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
    ) -> Result<(), StoreError> {
        let (row, cycle) = self.points[0];
        let bounds = self.points.iter().skip(1).fold(
            FilterBounds {
                cycle_start: cycle,
                cycle_end: cycle,
                row_start: row,
                row_end: row,
            },
            |b, &(row, cycle)| {
                union(
                    b,
                    FilterBounds {
                        cycle_start: cycle,
                        cycle_end: cycle,
                        row_start: row,
                        row_end: row,
                    },
                )
            },
        );
        let node = Node {
            bounds,
            level: None,
            page: self.leaves,
        };
        store
            .write_filter(
                trace,
                generation,
                &format!("exclude-leaf-{}", self.leaves),
                &self.points,
            )
            .await?;
        self.points.clear();
        self.leaves += 1;
        self.push(store, trace, generation, 0, node).await
    }
    pub async fn append(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
        row: u64,
        cycle: u64,
    ) -> Result<(), StoreError> {
        self.points.push((row, cycle));
        if self.points.len() == LEAF_SIZE {
            self.leaf(store, trace, generation).await?;
        }
        Ok(())
    }
    pub async fn finish(
        &mut self,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
    ) -> Result<(), StoreError> {
        if !self.points.is_empty() {
            self.leaf(store, trace, generation).await?;
        }
        let mut level = 0;
        while level < self.levels.len() {
            let buffer = &self.levels[level];
            if !buffer.nodes.is_empty() {
                if buffer.nodes.len() == 1
                    && self.levels[level + 1..].iter().all(|b| b.nodes.is_empty())
                {
                    self.root = buffer.nodes.first().copied();
                    break;
                }
                let node = self.group(store, trace, generation, level).await?;
                self.push(store, trace, generation, level + 1, node).await?;
            }
            level += 1;
        }
        // The completed index only retains bounded decoded caches and its root.
        self.levels.clear();
        Ok(())
    }
    pub fn probe(
        &self,
        source: &PhaseEndpoint,
        target: &PhaseEndpoint,
        scope: ExcludeScope,
        rows: u64,
    ) -> Option<ExclusionProbe> {
        let cycle_start = source.cycle.min(target.cycle).checked_add(1)?;
        let cycle_end = source.cycle.max(target.cycle).checked_sub(1)?;
        let (row_start, row_end) = match scope {
            ExcludeScope::BetweenRows => (
                source.row.min(target.row).checked_add(1)?,
                source.row.max(target.row).checked_sub(1)?,
            ),
            ExcludeScope::AllRows => (0, rows.checked_sub(1)?),
        };
        if cycle_start > cycle_end || row_start > row_end {
            return None;
        }
        let bounds = FilterBounds {
            cycle_start,
            cycle_end,
            row_start,
            row_end,
        };
        let root = self.root.filter(|n| n.bounds.intersects(bounds))?;
        Some(ExclusionProbe {
            bounds,
            stack: vec![root],
        })
    }
}
pub(crate) struct ExclusionProbe {
    bounds: FilterBounds,
    stack: Vec<Node>,
}
impl ExclusionProbe {
    // Yield after a shared per-tick node budget, including cache hits. The stack
    // holds at most FANOUT siblings per tree level, never all matching points.
    pub async fn advance(
        &mut self,
        index: &mut ExclusionIndex,
        store: &mut PageStore,
        trace: u32,
        generation: u32,
        budget: &mut usize,
    ) -> Result<Option<bool>, StoreError> {
        while *budget > 0 {
            let Some(node) = self.stack.pop() else {
                return Ok(Some(false));
            };
            *budget -= 1;
            if let Some(level) = node.level {
                let key = (level, node.page);
                if !index.nodes.contains(&key) {
                    let nodes = store
                        .read_filter(
                            trace,
                            generation,
                            &format!("exclude-node-{level}-{}", node.page),
                        )
                        .await?;
                    index.nodes.put(key, nodes);
                }
                if let Some(nodes) = index.nodes.get(&key) {
                    self.stack.extend(
                        nodes
                            .iter()
                            .rev()
                            .filter(|n| n.bounds.intersects(self.bounds))
                            .copied(),
                    );
                }
            } else {
                if !index.cache.contains(&node.page) {
                    let points = store
                        .read_filter(trace, generation, &format!("exclude-leaf-{}", node.page))
                        .await?;
                    index.cache.put(node.page, points);
                }
                if index.cache.get(&node.page).is_some_and(|points| {
                    points.iter().any(|&(row, cycle)| {
                        self.bounds.row_start <= row
                            && row <= self.bounds.row_end
                            && self.bounds.cycle_start <= cycle
                            && cycle <= self.bounds.cycle_end
                    })
                }) {
                    return Ok(Some(true));
                }
            }
        }
        Ok(self.stack.is_empty().then_some(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::NativeBacking;
    use futures_lite::future::block_on;

    #[test]
    fn tree_should_find_late_points_across_multiple_levels_with_bounded_resumable_work() {
        block_on(async {
            let mut store = PageStore::new(Box::new(NativeBacking::new().unwrap()), 1024);
            let mut index = ExclusionIndex::default();
            let origin = 1_u64 << 60;
            for row in 0..(LEAF_SIZE * FANOUT + 1) as u64 {
                index
                    .append(&mut store, 1, 1, row, origin + (row % 2) * 100)
                    .await
                    .unwrap();
            }
            index
                .append(&mut store, 1, 1, 99999, origin + 50)
                .await
                .unwrap();
            index.finish(&mut store, 1, 1).await.unwrap();
            for (end, expected) in [(u64::MAX, true), (99998, false)] {
                let mut probe = ExclusionProbe {
                    bounds: FilterBounds {
                        cycle_start: origin + 49,
                        cycle_end: origin + 51,
                        row_start: 0,
                        row_end: end,
                    },
                    stack: vec![index.root.unwrap()],
                };
                let mut ticks = 0;
                let found = loop {
                    let mut budget = 1;
                    ticks += 1;
                    if let Some(found) = probe
                        .advance(&mut index, &mut store, 1, 1, &mut budget)
                        .await
                        .unwrap()
                    {
                        break found;
                    }
                    assert_eq!(budget, 0);
                    assert!(probe.stack.len() <= FANOUT * 3);
                };
                assert_eq!(found, expected);
                assert!(ticks > FANOUT);
                assert!(index.nodes.len() <= 32 && index.cache.len() <= 32);
            }
        });
    }
}
