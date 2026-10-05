//! Platform-independent Kanata parsing, paged storage, and incremental search.
#![deny(missing_docs)]

/// Incremental gzip and Zstandard input.
pub mod compression;
/// Worker-owned trace engine.
pub mod engine;
/// General phase filtering and interval queries.
pub mod filter;
mod filter_engine;
mod filter_exclusion;
mod filter_suggestions;
/// Trace models and worker messages.
pub mod model;
/// Incremental Kanata parser.
pub mod parser;
/// Search matching and typed filters.
pub mod search;
/// Bounded page cache and backing-store interface.
pub mod storage;

pub use engine::Engine;
pub use model::*;
pub use storage::{BackingStore, StoreError};

/// Parses a synthetic trace for native release benchmarks.
pub async fn benchmark_trace(count: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(backing) = storage::NativeBacking::new() {
        let mut engine = Engine::new(Box::new(backing));
        let _ = engine.open(1, "benchmark.kanata".into()).await;
        let _ = engine.feed(1, b"Kanata\t0004\n").await;
        for id in 0..count {
            let text = format!("I\t{id}\t{id}\t0\nS\t{id}\t0\tF\nC\t1\nR\t{id}\t{id}\t0\n");
            let _ = engine.feed(1, text.as_bytes()).await;
        }
        let _ = engine.finish(1).await;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = count;
}
