//! Incremental Kanata v4 parser with recoverable command warnings.
use crate::storage::{PageStore, StoreError};
use crate::{Dependency, Operation, Stage, TraceInfo};
use memchr::memchr;
use std::collections::{HashMap, HashSet};

const MAX_LINE: usize = 8 * 1024 * 1024;
const MAX_SYMBOLS: usize = 65_536;

/// Bounded parser state; completed operations live in [`PageStore`].
pub struct Parser {
    pending: Vec<u8>,
    line: u64,
    header: bool,
    cycle: u64,
    symbols: HashMap<String, u32>,
    last_stage: HashMap<u64, usize>,
    active: HashSet<u64>,
    /// Public trace summary updated during loading.
    pub info: TraceInfo,
}
impl Parser {
    /// Creates a parser for one trace.
    pub fn new(id: u32, name: String) -> Self {
        Self {
            pending: Vec::new(),
            line: 1,
            header: false,
            cycle: 0,
            symbols: HashMap::new(),
            last_stage: HashMap::new(),
            active: HashSet::new(),
            info: TraceInfo {
                id,
                name,
                ..Default::default()
            },
        }
    }
    fn warning(&mut self, message: String) {
        self.info.warning_count += 1;
        if self.info.warnings.len() < 10 {
            self.info
                .warnings
                .push(format!("Line {}: {message}", self.line));
        }
    }
    fn number(&self, text: Option<&str>, command: &str) -> Result<u64, StoreError> {
        text.and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| StoreError::Input(format!("{command} has an invalid number")))
    }
    fn symbol(&mut self, text: &str) -> Result<u32, StoreError> {
        let name = text.trim();
        if let Some(id) = self.symbols.get(name) {
            return Ok(*id);
        }
        if self.symbols.len() >= MAX_SYMBOLS || name.len() > 256 {
            return Err(StoreError::Input("Stage symbol budget exceeded".into()));
        }
        let id = self.info.symbols.len() as u32;
        self.info.symbols.push(name.into());
        self.symbols.insert(name.into(), id);
        Ok(id)
    }
    /// Feeds decoded bytes; line boundaries may fall anywhere.
    pub async fn feed(&mut self, store: &mut PageStore, bytes: &[u8]) -> Result<(), StoreError> {
        let mut tail = bytes;
        while let Some(index) = memchr(b'\n', tail) {
            let line = &tail[..index];
            if self.pending.is_empty() {
                self.parse_line(store, line).await?;
            } else {
                self.pending.extend_from_slice(line);
                let joined = std::mem::take(&mut self.pending);
                self.parse_line(store, &joined).await?;
            }
            self.line += 1;
            tail = &tail[index + 1..];
        }
        if self.pending.len() + tail.len() > MAX_LINE {
            return Err(StoreError::Input("Trace line exceeds 8 MiB".into()));
        }
        self.pending.extend_from_slice(tail);
        Ok(())
    }
    async fn parse_line(&mut self, store: &mut PageStore, raw: &[u8]) -> Result<(), StoreError> {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw)
            .map_err(|_| StoreError::Input(format!("Line {} is not UTF-8", self.line)))?;
        if !self.header {
            if !line.starts_with("Kanata\t") {
                return Err(StoreError::Input(
                    "The selected file is not a Kanata trace".into(),
                ));
            }
            if line.split('\t').nth(1) != Some("0004") {
                return Err(StoreError::Input(
                    "Only Kanata version 0004 is supported".into(),
                ));
            }
            self.header = true;
            return Ok(());
        }
        match self.command(store, line).await {
            Err(StoreError::Input(message)) => {
                self.warning(message);
                Ok(())
            }
            other => other,
        }
    }
    async fn command(&mut self, store: &mut PageStore, line: &str) -> Result<(), StoreError> {
        let parts: Vec<&str> = line.split('\t').collect();
        let cmd = parts[0];
        if cmd == "C=" {
            let cycle = self.number(parts.get(1).copied(), cmd)?;
            if cycle < self.cycle {
                return Err(StoreError::Input("Cycle moved backwards".into()));
            }
            self.cycle = cycle;
            if self.info.count == 0 {
                self.info.first_cycle = cycle;
            }
            self.info.last_cycle = cycle;
            return Ok(());
        }
        if cmd == "C" {
            self.cycle = self
                .cycle
                .checked_add(self.number(parts.get(1).copied(), cmd)?)
                .ok_or_else(|| StoreError::Input("Cycle overflow".into()))?;
            self.info.last_cycle = self.cycle;
            return Ok(());
        }
        if cmd.is_empty() {
            return Ok(());
        }
        // Some producers interleave resource occupancy snapshots as standalone
        // key=value lines. They are trace extensions, not malformed commands.
        if parts.len() == 1 && cmd.contains('=') {
            return Ok(());
        }
        if !matches!(cmd, "I" | "L" | "S" | "E" | "R" | "W") {
            self.warning(format!("Unknown command: {cmd}"));
            return Ok(());
        }
        let id = self.number(parts.get(1).copied(), cmd)?;
        let trace = self.info.id;
        if cmd == "I" {
            if store.get(trace, id).await?.is_some() {
                return Err(StoreError::Input(format!("Operation {id} redefined")));
            }
            let gid = self.number(parts.get(2).copied(), cmd)?;
            let tid = self.number(parts.get(3).copied(), cmd)?;
            let op = Operation {
                id,
                gid,
                tid,
                rid: None,
                fetch: self.cycle,
                end: None,
                flushed: false,
                incomplete: false,
                line: self.line,
                label: String::new(),
                detail: String::new(),
                stages: Vec::new(),
                dependencies: Vec::new(),
            };
            store.set(trace, op).await?;
            self.active.insert(id);
            self.info.count += 1;
            return Ok(());
        }
        let Some(mut op) = store.get(trace, id).await? else {
            return Err(StoreError::Input(format!(
                "{cmd} refers to unknown operation {id}"
            )));
        };
        if op.rid.is_some() && cmd != "L" {
            self.warning(format!(
                "{cmd} appears after operation {id} retired or flushed"
            ));
        }
        match cmd {
            "L" => {
                let kind = self.number(parts.get(2).copied(), cmd)?;
                let text = parts
                    .get(3..)
                    .ok_or_else(|| StoreError::Input("L is missing text".into()))?
                    .join("\t");
                match kind {
                    0 => op.label.push_str(&text),
                    1 => op.detail.push_str(&text),
                    2 => {
                        let index = self
                            .last_stage
                            .get(&id)
                            .copied()
                            .or_else(|| op.stages.len().checked_sub(1))
                            .ok_or_else(|| StoreError::Input("Stage label has no stage".into()))?;
                        let stage = op
                            .stages
                            .get_mut(index)
                            .ok_or_else(|| StoreError::Input("Stage label has no stage".into()))?;
                        if !stage.labels.is_empty() {
                            stage.labels.push('\n');
                        }
                        stage.labels.push_str(&text);
                    }
                    _ => return Err(StoreError::Input(format!("Unknown label type {kind}"))),
                }
                if op.end.is_some() {
                    unescape(&mut op);
                }
            }
            "S" => {
                let lane_name = parts
                    .get(2)
                    .ok_or_else(|| StoreError::Input("S is missing lane".into()))?;
                let stage_name = parts
                    .get(3)
                    .ok_or_else(|| StoreError::Input("S is missing stage".into()))?;
                let lane = self.symbol(lane_name)?;
                let name = self.symbol(stage_name)?;
                if let Some(previous) = op
                    .stages
                    .iter_mut()
                    .rev()
                    .find(|s| s.lane == lane && s.end.is_none())
                {
                    previous.end = Some(self.cycle);
                }
                op.stages.push(Stage {
                    lane,
                    name,
                    start: self.cycle,
                    end: None,
                    labels: String::new(),
                });
                self.last_stage.insert(id, op.stages.len() - 1);
            }
            "E" => {
                let lane_name = parts
                    .get(2)
                    .ok_or_else(|| StoreError::Input("E is missing lane".into()))?
                    .trim();
                let stage_name = parts
                    .get(3)
                    .ok_or_else(|| StoreError::Input("E is missing stage".into()))?
                    .trim();
                let lane = self
                    .symbols
                    .get(lane_name)
                    .copied()
                    .ok_or_else(|| StoreError::Input("Unknown lane".into()))?;
                let name = self
                    .symbols
                    .get(stage_name)
                    .copied()
                    .ok_or_else(|| StoreError::Input("Unknown stage".into()))?;
                if let Some(stage) = op
                    .stages
                    .iter_mut()
                    .rev()
                    .find(|s| s.lane == lane && s.name == name)
                {
                    stage.end = Some(self.cycle);
                }
            }
            "R" => {
                if op.rid.is_some() {
                    return Err(StoreError::Input(format!(
                        "Operation {id} was already retired or flushed"
                    )));
                }
                let rid = self.number(parts.get(2).copied(), cmd)?;
                let kind = self.number(parts.get(3).copied(), cmd)?;
                if kind > 1 {
                    return Err(StoreError::Input("R requires status 0 or 1".into()));
                }
                op.rid = Some(rid);
                op.end = Some(self.cycle);
                op.flushed = kind == 1;
                if op.flushed {
                    self.info.flushed += 1;
                }
                for stage in &mut op.stages {
                    if stage.end.is_none() {
                        stage.end = Some(self.cycle);
                    }
                }
                unescape(&mut op);
                self.last_stage.remove(&id);
                self.active.remove(&id);
            }
            "W" => {
                let producer = self.number(parts.get(2).copied(), cmd)?;
                let kind = self.number(parts.get(3).copied(), cmd)?;
                if producer > id {
                    self.warning(format!("Dependency refers to future producer {producer}"));
                }
                op.dependencies.push(Dependency {
                    producer,
                    kind,
                    cycle: self.cycle,
                });
            }
            _ => {}
        }
        store.set(trace, op).await
    }
    /// Completes a trace, exposing operations with no retirement event.
    pub async fn finish(&mut self, store: &mut PageStore) -> Result<(), StoreError> {
        if !self.pending.is_empty() {
            let tail = std::mem::take(&mut self.pending);
            self.parse_line(store, &tail).await?;
            self.line += 1;
        }
        if !self.header {
            return Err(StoreError::Input("The selected file is empty".into()));
        }
        for id in self.active.drain() {
            if let Some(mut op) = store.get(self.info.id, id).await? {
                op.end = Some(self.cycle.saturating_add(1));
                op.incomplete = true;
                for stage in &mut op.stages {
                    if stage.end.is_none() {
                        stage.end = op.end;
                    }
                }
                unescape(&mut op);
                store.set(self.info.id, op).await?;
            }
        }
        self.info.complete = true;
        Ok(())
    }
}
fn unescape(op: &mut Operation) {
    if op.label.contains("\\n") {
        op.label = op.label.replace("\\n", "\n");
    }
    if op.detail.contains("\\n") {
        op.detail = op.detail.replace("\\n", "\n");
    }
    for stage in &mut op.stages {
        if stage.labels.contains("\\n") {
            stage.labels = stage.labels.replace("\\n", "\n");
        }
    }
}
