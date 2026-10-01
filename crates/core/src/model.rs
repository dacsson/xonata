//! Serializable trace records and bounded worker protocol.
use serde::{Deserialize, Serialize};

/// One named pipeline interval. End is exclusive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stage {
    /// Interned stage name.
    pub name: u32,
    /// Interned lane name.
    pub lane: u32,
    /// First cycle.
    pub start: u64,
    /// End cycle; absent while the interval is open.
    pub end: Option<u64>,
    /// Stage metadata.
    pub labels: String,
}

/// Consumer-to-producer dependency.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dependency {
    /// Producer operation ID.
    pub producer: u64,
    /// Dependency type supplied by the trace.
    pub kind: u64,
    /// Cycle at which the dependency was recorded.
    pub cycle: u64,
}

/// One instruction, including all associated metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    /// File-local ID.
    pub id: u64,
    /// Simulator ID.
    pub gid: u64,
    /// Thread ID.
    pub tid: u64,
    /// Retirement ID, including speculative IDs on flushed operations.
    pub rid: Option<u64>,
    /// First observed cycle.
    pub fetch: u64,
    /// Retirement, flush, or EOF cycle.
    pub end: Option<u64>,
    /// Whether the operation was flushed.
    pub flushed: bool,
    /// Whether it reached EOF without retirement.
    pub incomplete: bool,
    /// Source line of the I command.
    pub line: u64,
    /// Disassembly label.
    pub label: String,
    /// Instruction metadata.
    pub detail: String,
    /// Pipeline intervals in insertion order.
    pub stages: Vec<Stage>,
    /// Dependencies recorded on this consumer.
    pub dependencies: Vec<Dependency>,
}

impl Operation {
    /// Approximate owned size, including allocated collections.
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.label.capacity()
            + self.detail.capacity()
            + self.stages.capacity() * std::mem::size_of::<Stage>()
            + self
                .stages
                .iter()
                .map(|s| s.labels.capacity())
                .sum::<usize>()
            + self.dependencies.capacity() * std::mem::size_of::<Dependency>()
    }

    /// Produces plain text suitable for pinning or copying.
    pub fn metadata(&self, symbols: &[String]) -> String {
        let mut text = format!(
            "{}\nOp {} · Global {} · Thread {}\nRetired ID: {:?}\nCycles: {}–{} · Source line {}\nStatus: {}\n{}",
            self.label,
            self.id,
            self.gid,
            self.tid,
            self.rid,
            self.fetch,
            self.end.map_or_else(|| "active".into(), |v| v.to_string()),
            self.line,
            if self.flushed {
                "flushed"
            } else if self.incomplete {
                "incomplete"
            } else if self.rid.is_some() {
                "retired"
            } else {
                "active"
            },
            self.detail
        );
        for stage in &self.stages {
            text.push_str(&format!(
                "\n{} / {}: {}–{} {}",
                symbol(symbols, stage.lane),
                symbol(symbols, stage.name),
                stage.start,
                stage.end.map_or_else(|| "active".into(), |v| v.to_string()),
                stage.labels
            ));
        }
        for dep in &self.dependencies {
            text.push_str(&format!(
                "\nDepends on {} · type {} · cycle {}",
                dep.producer, dep.kind, dep.cycle
            ));
        }
        text
    }
}

/// Resolves a symbol without panicking on malformed external data.
pub fn symbol(symbols: &[String], index: u32) -> &str {
    symbols.get(index as usize).map_or("?", String::as_str)
}

/// Inclusive numeric search interval.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct NumberRange {
    /// Optional inclusive lower bound.
    pub min: Option<u64>,
    /// Optional inclusive upper bound.
    pub max: Option<u64>,
}
impl NumberRange {
    /// Tests whether a value lies inside the interval.
    pub fn contains(&self, value: u64) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

/// Structured search query; empty text matches every instruction allowed by filters.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Literal or regular-expression text.
    pub text: String,
    /// Interpret text as a regular expression.
    pub regex: bool,
    /// Match case exactly.
    pub case_sensitive: bool,
    /// Operation ID filter.
    pub op: NumberRange,
    /// Global ID filter.
    pub global: NumberRange,
    /// Retirement ID filter.
    pub retired_id: NumberRange,
    /// Thread ID filter.
    pub thread: NumberRange,
    /// Cycle interval overlap filter.
    pub cycles: NumberRange,
    /// Literal stage-name filter.
    pub stage: String,
    /// Status: all, retired, flushed, or incomplete.
    pub status: String,
}

/// A result row; snippets are fetched lazily with the operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    /// Matching instruction.
    pub op: Operation,
    /// Display row including flushed operations.
    pub row: u64,
    /// Names and snippets of the matching textual fields.
    pub snippets: Vec<String>,
}

/// A visible instruction row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    /// Display row in the requested filter.
    pub index: u64,
    /// Instruction data.
    pub op: Operation,
}

/// Trace loading and storage status.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraceInfo {
    /// Application-assigned trace ID.
    pub id: u32,
    /// Display file name.
    pub name: String,
    /// Number of instructions.
    pub count: u64,
    /// Flushed instruction count.
    pub flushed: u64,
    /// First absolute cycle.
    pub first_cycle: u64,
    /// Last observed cycle.
    pub last_cycle: u64,
    /// Parsing finished successfully.
    pub complete: bool,
    /// Count of recoverable parser warnings.
    pub warning_count: u64,
    /// Bounded sample of warnings.
    pub warnings: Vec<String>,
    /// Stage and lane symbol table.
    pub symbols: Vec<String>,
    /// Bytes received from the source file.
    pub bytes: u64,
}

/// Compact whole-trace image built off the UI thread.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverviewRaster {
    /// Image width in pixels (at most 256).
    pub width: u16,
    /// Image height in pixels (at most 1024).
    pub height: u16,
    /// Row-major palette indices: zero is empty, 1–8 are phase colors;
    /// the high bit indicates a flushed instruction.
    pub pixels: Vec<u8>,
}

/// Commands shared by native and browser workers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Releases all trace-owned resources.
    Close {
        /// Trace ID.
        trace: u32,
    },
    /// Retrieves only visible rows, sampling when zoomed out.
    View {
        /// Trace ID.
        trace: u32,
        /// View generation.
        generation: u32,
        /// First row.
        start: u64,
        /// Row stride.
        stride: u64,
        /// Maximum rows.
        count: u32,
        /// Exclude flushed instructions.
        hide_flushed: bool,
    },
    /// Builds a bounded overview after parsing finishes.
    Overview {
        /// Trace ID.
        trace: u32,
        /// Overview generation.
        generation: u32,
        /// Requested raster width.
        width: u16,
        /// Requested raster height.
        height: u16,
    },
    /// Finds an instruction by its unfiltered pipeline row.
    JumpRow {
        /// Trace ID.
        trace: u32,
        /// Unfiltered row index.
        row: u64,
    },
    /// Starts or replaces a search.
    Search {
        /// Trace ID.
        trace: u32,
        /// Query generation.
        generation: u32,
        /// Query.
        query: Box<SearchQuery>,
    },
    /// Stops scanning and removes the search index.
    CancelSearch {
        /// Trace ID.
        trace: u32,
    },
    /// Retrieves a bounded window of search results.
    Results {
        /// Trace ID.
        trace: u32,
        /// Query generation.
        generation: u32,
        /// First result.
        start: u64,
        /// Maximum results.
        count: u32,
    },
    /// Finds an operation or retirement ID.
    Jump {
        /// Trace ID.
        trace: u32,
        /// Target ID.
        id: u64,
        /// Interpret ID as retirement ID.
        retired: bool,
    },
}

/// Worker responses. UI ignores responses for obsolete generations.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Updated trace information.
    Info {
        /// Trace status.
        info: TraceInfo,
    },
    /// Viewport data.
    View {
        /// Trace ID.
        trace: u32,
        /// View generation.
        generation: u32,
        /// Visible rows.
        rows: Vec<Row>,
    },
    /// Completed compact overview image.
    Overview {
        /// Trace ID.
        trace: u32,
        /// Overview generation.
        generation: u32,
        /// Phase palette raster.
        raster: OverviewRaster,
    },
    /// Search scan progress; total is provisional until done.
    SearchProgress {
        /// Trace ID.
        trace: u32,
        /// Query generation.
        generation: u32,
        /// Matching instruction count.
        total: u64,
        /// Fraction scanned.
        progress: f32,
        /// Scan finished.
        done: bool,
    },
    /// Requested search rows.
    Results {
        /// Trace ID.
        trace: u32,
        /// Query generation.
        generation: u32,
        /// First result index.
        start: u64,
        /// Results.
        hits: Vec<SearchHit>,
    },
    /// Navigation destination.
    Jump {
        /// Trace ID.
        trace: u32,
        /// Instruction and unfiltered row.
        row: Option<Row>,
    },
    /// Recoverable application error.
    Error {
        /// Trace ID, if known.
        trace: u32,
        /// User-readable explanation.
        message: String,
    },
    /// Global cache metrics.
    Metrics {
        /// Decoded-page bytes.
        decoded_bytes: usize,
        /// Resident page count.
        pages: usize,
    },
}
