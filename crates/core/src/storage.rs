//! A shared byte-bounded page cache over asynchronous platform backing stores.
use crate::Operation;
use async_trait::async_trait;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Operations per independently compressed page.
pub const PAGE_SIZE: u64 = 256;
/// Default shared decoded page budget.
pub const CACHE_BUDGET: usize = 128 * 1024 * 1024;

/// Storage, parser, or codec failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Platform storage failure.
    #[error("Temporary trace storage failed: {0}. Free space or enable browser storage and retry.")]
    Storage(String),
    /// Invalid input.
    #[error("{0}")]
    Input(String),
    /// Internal page decoding failure.
    #[error("Trace page is unreadable: {0}")]
    Codec(String),
}

/// Key-value storage owned exclusively by a worker. Missing keys return `None`.
#[async_trait(?Send)]
pub trait BackingStore {
    /// Reads an independently stored object.
    async fn read(&mut self, trace: u32, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    /// Atomically replaces an object.
    async fn write(&mut self, trace: u32, key: &str, bytes: &[u8]) -> Result<(), StoreError>;
    /// Removes all objects belonging to a trace.
    async fn remove(&mut self, trace: u32) -> Result<(), StoreError>;
    /// Removes search objects for a trace.
    async fn clear_search(&mut self, trace: u32) -> Result<(), StoreError>;
}

#[derive(Default, Serialize, Deserialize)]
struct Page {
    ops: Vec<Option<Operation>>,
}
impl Page {
    fn empty() -> Self {
        Self {
            ops: (0..PAGE_SIZE).map(|_| None).collect(),
        }
    }
    fn bytes(&self) -> usize {
        self.ops.capacity() * std::mem::size_of::<Option<Operation>>()
            + self
                .ops
                .iter()
                .flatten()
                .map(|o| o.memory_bytes() - std::mem::size_of::<Operation>())
                .sum::<usize>()
    }
}
struct Cached {
    page: Page,
    dirty: bool,
    bytes: usize,
}

/// Small per-page directory. It contains no instruction labels or stages.
#[derive(Clone, Default)]
pub struct PageSummary {
    /// Present instructions in this page.
    pub present: [u64; 4],
    /// Flushed instructions in this page.
    pub flushed: [u64; 4],
}
impl PageSummary {
    /// Number of visible instructions.
    pub fn count(&self, hide_flushed: bool) -> u64 {
        self.present
            .iter()
            .zip(self.flushed)
            .map(|(p, f)| (if hide_flushed { *p & !f } else { *p }).count_ones() as u64)
            .sum()
    }
    /// Iterates occupied slots without expanding the page.
    pub fn slots(&self, hide_flushed: bool) -> impl Iterator<Item = u64> + '_ {
        (0..PAGE_SIZE).filter(move |i| {
            let mask = 1 << (i % 64);
            self.present[(i / 64) as usize] & mask != 0
                && (!hide_flushed || self.flushed[(i / 64) as usize] & mask == 0)
        })
    }
}

/// Worker-wide page cache and trace directories.
pub struct PageStore {
    backing: Box<dyn BackingStore>,
    cache: LruCache<(u32, u64), Cached>,
    bytes: usize,
    budget: usize,
    /// Per-trace page directories, ordered by operation ID.
    pub directories: BTreeMap<u32, BTreeMap<u64, PageSummary>>,
}
impl PageStore {
    /// Creates a cache with a global byte budget shared by all traces.
    pub fn new(backing: Box<dyn BackingStore>, budget: usize) -> Self {
        Self {
            backing,
            cache: LruCache::unbounded(),
            bytes: 0,
            budget,
            directories: BTreeMap::new(),
        }
    }
    async fn ensure(&mut self, trace: u32, page: u64) -> Result<(), StoreError> {
        if self.cache.contains(&(trace, page)) {
            return Ok(());
        }
        let value = match self.backing.read(trace, &format!("page-{page}")).await? {
            Some(bytes) => {
                let raw = lz4_flex::decompress_size_prepended(&bytes)
                    .map_err(|e| StoreError::Codec(e.to_string()))?;
                postcard::from_bytes(&raw).map_err(|e| StoreError::Codec(e.to_string()))?
            }
            None => Page::empty(),
        };
        let bytes = value.bytes();
        self.bytes += bytes;
        self.cache.put(
            (trace, page),
            Cached {
                page: value,
                dirty: false,
                bytes,
            },
        );
        Ok(())
    }
    /// Reads one operation; the returned copy can be sent to a different thread.
    pub async fn get(&mut self, trace: u32, id: u64) -> Result<Option<Operation>, StoreError> {
        let page = id / PAGE_SIZE;
        if !self
            .directories
            .get(&trace)
            .is_some_and(|d| d.contains_key(&page))
        {
            return Ok(None);
        }
        self.ensure(trace, page).await?;
        let result = self
            .cache
            .get(&(trace, page))
            .and_then(|p| p.page.ops[(id % PAGE_SIZE) as usize].clone());
        self.trim().await?;
        Ok(result)
    }
    /// Inserts or updates an operation, preserving post-retirement labels.
    pub async fn set(&mut self, trace: u32, op: Operation) -> Result<(), StoreError> {
        if op.memory_bytes() > 8 * 1024 * 1024 {
            return Err(StoreError::Input(
                "One instruction exceeds the 8 MiB metadata budget".into(),
            ));
        }
        let id = op.id;
        let page = id / PAGE_SIZE;
        let slot = id % PAGE_SIZE;
        self.ensure(trace, page).await?;
        let summary = self
            .directories
            .entry(trace)
            .or_default()
            .entry(page)
            .or_default();
        summary.present[(slot / 64) as usize] |= 1 << (slot % 64);
        if op.flushed {
            summary.flushed[(slot / 64) as usize] |= 1 << (slot % 64);
        } else {
            summary.flushed[(slot / 64) as usize] &= !(1 << (slot % 64));
        }
        if let Some(cached) = self.cache.get_mut(&(trace, page)) {
            self.bytes -= cached.bytes;
            let old = cached.page.ops[slot as usize].replace(op);
            cached.dirty = true;
            cached.bytes = cached.bytes
                - old.as_ref().map_or(0, |old| {
                    old.memory_bytes() - std::mem::size_of::<Operation>()
                })
                + cached.page.ops[slot as usize].as_ref().map_or(0, |new| {
                    new.memory_bytes() - std::mem::size_of::<Operation>()
                });
            self.bytes += cached.bytes;
        }
        self.trim().await
    }
    async fn persist(&mut self, key: (u32, u64), value: &Cached) -> Result<(), StoreError> {
        if value.dirty {
            let raw =
                postcard::to_allocvec(&value.page).map_err(|e| StoreError::Codec(e.to_string()))?;
            let bytes = lz4_flex::compress_prepend_size(&raw);
            self.backing
                .write(key.0, &format!("page-{}", key.1), &bytes)
                .await?;
        }
        Ok(())
    }
    async fn trim(&mut self) -> Result<(), StoreError> {
        while self.bytes > self.budget {
            let Some((key, value)) = self.cache.pop_lru() else {
                break;
            };
            if let Err(error) = self.persist(key, &value).await {
                self.cache.put(key, value);
                return Err(error);
            }
            self.bytes -= value.bytes;
        }
        Ok(())
    }
    /// Returns IDs and row indexes for a bounded viewport query.
    pub fn visible_ids(
        &self,
        trace: u32,
        start: u64,
        stride: u64,
        count: u32,
        hide: bool,
    ) -> Vec<(u64, u64)> {
        let mut result = Vec::new();
        let mut row = 0;
        let mut target = start;
        if let Some(directory) = self.directories.get(&trace) {
            for (page, summary) in directory {
                let next = row + summary.count(hide);
                if next <= target {
                    row = next;
                    continue;
                }
                for slot in summary.slots(hide) {
                    if row == target {
                        result.push((row, page * PAGE_SIZE + slot));
                        target = target.saturating_add(stride.max(1));
                        if result.len() >= count.min(1024) as usize {
                            return result;
                        }
                    }
                    row += 1;
                }
            }
        }
        result
    }
    /// Finds the unfiltered row of an operation without decoding pages.
    pub fn row_of(&self, trace: u32, id: u64) -> Option<u64> {
        let directory = self.directories.get(&trace)?;
        let mut row = 0;
        for (page, summary) in directory.range(..=id / PAGE_SIZE) {
            if *page == id / PAGE_SIZE {
                return summary
                    .slots(false)
                    .position(|slot| slot == id % PAGE_SIZE)
                    .map(|n| row + n as u64);
            }
            row += summary.count(false);
        }
        None
    }
    /// Lists bounded IDs from one page.
    pub fn page_ids(&self, trace: u32, page: u64) -> Vec<u64> {
        self.directories
            .get(&trace)
            .and_then(|d| d.get(&page))
            .map_or_else(Vec::new, |s| {
                s.slots(false).map(|slot| page * PAGE_SIZE + slot).collect()
            })
    }
    /// Cache metrics.
    pub fn metrics(&self) -> (usize, usize) {
        (self.bytes, self.cache.len())
    }
    /// Stores a bounded block of search result IDs.
    pub async fn write_hits(
        &mut self,
        trace: u32,
        generation: u32,
        page: u64,
        ids: &[(u64, u64)],
    ) -> Result<(), StoreError> {
        let bytes = postcard::to_allocvec(ids).map_err(|e| StoreError::Codec(e.to_string()))?;
        self.backing
            .write(trace, &format!("search-{generation}-{page}"), &bytes)
            .await
    }
    /// Reads a bounded block of search result IDs.
    pub async fn read_hits(
        &mut self,
        trace: u32,
        generation: u32,
        page: u64,
    ) -> Result<Vec<(u64, u64)>, StoreError> {
        match self
            .backing
            .read(trace, &format!("search-{generation}-{page}"))
            .await?
        {
            Some(bytes) => {
                postcard::from_bytes(&bytes).map_err(|e| StoreError::Codec(e.to_string()))
            }
            None => Ok(Vec::new()),
        }
    }
    /// Clears disk-backed result indexes.
    pub async fn clear_search(&mut self, trace: u32) -> Result<(), StoreError> {
        self.backing.clear_search(trace).await
    }
    /// Releases cached pages, directory, and backing objects for one trace.
    pub async fn remove(&mut self, trace: u32) -> Result<(), StoreError> {
        let keys: Vec<_> = self
            .cache
            .iter()
            .filter(|(k, _)| k.0 == trace)
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            if let Some(value) = self.cache.pop(&key) {
                self.bytes -= value.bytes;
            }
        }
        self.directories.remove(&trace);
        self.backing.remove(trace).await
    }
}

/// Native backing storage removed automatically when its worker exits.
#[cfg(not(target_arch = "wasm32"))]
pub struct NativeBacking {
    directory: tempfile::TempDir,
}
#[cfg(not(target_arch = "wasm32"))]
impl NativeBacking {
    /// Creates a private temporary trace directory.
    pub fn new() -> Result<Self, StoreError> {
        tempfile::Builder::new()
            .prefix("xonata-")
            .tempdir()
            .map(|directory| Self { directory })
            .map_err(|e| StoreError::Storage(e.to_string()))
    }
    fn dir(&self, trace: u32) -> std::path::PathBuf {
        self.directory.path().join(trace.to_string())
    }
}
#[cfg(not(target_arch = "wasm32"))]
#[async_trait(?Send)]
impl BackingStore for NativeBacking {
    async fn read(&mut self, trace: u32, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(self.dir(trace).join(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Storage(e.to_string())),
        }
    }
    async fn write(&mut self, trace: u32, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let directory = self.dir(trace);
        std::fs::create_dir_all(&directory).map_err(|e| StoreError::Storage(e.to_string()))?;
        let temp = directory.join("pending");
        std::fs::write(&temp, bytes).map_err(|e| StoreError::Storage(e.to_string()))?;
        std::fs::rename(temp, directory.join(key)).map_err(|e| StoreError::Storage(e.to_string()))
    }
    async fn remove(&mut self, trace: u32) -> Result<(), StoreError> {
        match std::fs::remove_dir_all(self.dir(trace)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Storage(e.to_string())),
        }
    }
    async fn clear_search(&mut self, trace: u32) -> Result<(), StoreError> {
        if self.dir(trace).exists() {
            for entry in std::fs::read_dir(self.dir(trace))
                .map_err(|e| StoreError::Storage(e.to_string()))?
            {
                let entry = entry.map_err(|e| StoreError::Storage(e.to_string()))?;
                if entry.file_name().to_string_lossy().starts_with("search-") {
                    std::fs::remove_file(entry.path())
                        .map_err(|e| StoreError::Storage(e.to_string()))?;
                }
            }
        }
        Ok(())
    }
}
