//! General phase predicates, endpoint pairing, and compact filter records.
use crate::storage::StoreError;
use crate::{Operation, Stage, TraceInfo, symbol};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// Exact decimal serialization through JavaScript and compact numeric binary storage.
macro_rules! decimal {
    ($module:ident, $ty:ty) => {
        pub(crate) mod $module {
            use serde::{Deserialize, Deserializer, Serializer};
            pub fn serialize<S: Serializer>(value: &$ty, s: S) -> Result<S::Ok, S::Error> {
                if s.is_human_readable() {
                    s.serialize_str(&value.to_string())
                } else {
                    serde::Serialize::serialize(value, s)
                }
            }
            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<$ty, D::Error> {
                if d.is_human_readable() {
                    String::deserialize(d)?
                        .parse()
                        .map_err(serde::de::Error::custom)
                } else {
                    <$ty>::deserialize(d)
                }
            }
        }
    };
}
decimal!(decimal_u64, u64);
decimal!(decimal_i128, i128);

/// Vertical extent of an interval drawing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawSpan {
    /// Source and target rows, including intervening rows.
    #[default]
    Both,
    /// Source row only.
    Source,
    /// Target row only.
    Target,
    /// Every pipeline row.
    All,
}
/// Query-provided drawing defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DrawOptions {
    /// Optional text accompanying the stable result number.
    pub label: String,
    /// Interval vertical extent.
    pub span: DrawSpan,
    /// Additional rows on each side.
    #[serde(with = "decimal_u64")]
    pub row_pad: u64,
}
/// A matching phase endpoint, without a decoded operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhaseEndpoint {
    /// Operation ID.
    #[serde(with = "decimal_u64")]
    pub op: u64,
    /// Unfiltered pipeline row.
    #[serde(with = "decimal_u64")]
    pub row: u64,
    /// Thread ID.
    #[serde(with = "decimal_u64")]
    pub thread: u64,
    /// Phase occurrence within the operation.
    pub stage: u32,
    /// Lane position among this operation's lanes.
    pub lane_index: u32,
    /// Number of lanes in the operation.
    pub lane_count: u32,
    /// Matched endpoint cycle.
    #[serde(with = "decimal_u64")]
    pub cycle: u64,
    /// Phase start.
    #[serde(with = "decimal_u64")]
    pub start: u64,
    /// Phase exclusive end (latest observed boundary for open phases).
    #[serde(with = "decimal_u64")]
    pub end: u64,
    /// Whether the phase has no recorded end.
    pub open: bool,
    /// Bounded disassembly excerpt.
    pub instruction: String,
    /// Phase name.
    pub phase: String,
    /// Lane name.
    pub lane: String,
    /// Bounded metadata excerpt.
    pub metadata: String,
}
/// One stable filter result, suitable for paging and drawing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilterHit {
    /// Zero-based stable result index.
    #[serde(with = "decimal_u64")]
    pub index: u64,
    /// Source or single matching phase.
    pub source: PhaseEndpoint,
    /// Paired target, absent for a single-phase query.
    pub target: Option<PhaseEndpoint>,
    /// Signed interval gap or single-phase duration.
    #[serde(with = "decimal_i128")]
    pub elapsed: i128,
}
/// Exact viewport bounds sent to the drawing worker.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct FilterBounds {
    /// First cycle.
    #[serde(with = "decimal_u64")]
    pub cycle_start: u64,
    /// Last cycle.
    #[serde(with = "decimal_u64")]
    pub cycle_end: u64,
    /// First row.
    #[serde(with = "decimal_u64")]
    pub row_start: u64,
    /// Last row.
    #[serde(with = "decimal_u64")]
    pub row_end: u64,
}
impl FilterHit {
    /// Drawing bounds in integer cycle and row coordinates.
    pub fn bounds(&self, options: &DrawOptions, rows: u64) -> FilterBounds {
        let s = &self.source;
        if let Some(t) = &self.target {
            let (a, b) = match options.span {
                DrawSpan::Both => (s.row.min(t.row), s.row.max(t.row)),
                DrawSpan::Source => (s.row, s.row),
                DrawSpan::Target => (t.row, t.row),
                DrawSpan::All => (0, rows.saturating_sub(1)),
            };
            FilterBounds {
                cycle_start: s.cycle.min(t.cycle),
                cycle_end: s.cycle.max(t.cycle),
                row_start: a.saturating_sub(options.row_pad),
                row_end: b
                    .saturating_add(options.row_pad)
                    .min(rows.saturating_sub(1)),
            }
        } else {
            FilterBounds {
                cycle_start: s.start,
                cycle_end: s.end,
                row_start: s.row,
                row_end: s.row,
            }
        }
    }
}
impl FilterBounds {
    /// Includes zero-width boundaries and partially visible rectangles.
    pub fn intersects(self, other: Self) -> bool {
        self.cycle_start <= other.cycle_end
            && self.cycle_end >= other.cycle_start
            && self.row_start <= other.row_end
            && self.row_end >= other.row_start
    }
}
#[derive(Clone, Copy, Debug)]
enum Position {
    Start,
    End,
}
#[derive(Clone, Copy, Debug)]
enum Compare {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
impl Compare {
    fn test(self, a: i128, b: i128) -> bool {
        match self {
            Self::Eq => a == b,
            Self::Ne => a != b,
            Self::Lt => a < b,
            Self::Le => a <= b,
            Self::Gt => a > b,
            Self::Ge => a >= b,
        }
    }
}
#[derive(Debug)]
enum Predicate {
    Text(String, Regex),
    Number(String, Compare, i128),
    Status(String),
}
/// Compiled predicates for one phase endpoint.
#[derive(Debug)]
pub struct EndpointQuery {
    position: Position,
    predicates: Vec<Predicate>,
}
/// Rows considered when checking for a forbidden phase boundary between endpoints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExcludeScope {
    /// Strictly intervening instruction rows within the cycle interval.
    #[default]
    BetweenRows,
    /// Any instruction row within the cycle interval, including other threads.
    AllRows,
}
/// Optional forbidden endpoint predicates, using the same fields as FROM and TO.
#[derive(Debug)]
pub struct ExclusionQuery {
    /// Compiled forbidden phase boundary conditions.
    pub endpoint: EndpointQuery,
    /// Rows searched within the open cycle interval.
    pub scope: ExcludeScope,
}
/// Validated query compiled exclusively by the worker.
#[derive(Debug)]
pub struct FilterQuery {
    /// Source predicates.
    pub source: EndpointQuery,
    /// Optional target predicates.
    pub target: Option<EndpointQuery>,
    /// Optional post-pair exclusion condition.
    pub exclusion: Option<ExclusionQuery>,
    /// Drawing defaults.
    pub options: DrawOptions,
    comparisons: Vec<(String, Compare, i128)>,
}
fn error(offset: usize, message: impl std::fmt::Display) -> StoreError {
    StoreError::Input(format!("Filter at byte {}: {message}", offset + 1))
}
fn segments<'a>(
    text: &'a str,
    delimiter: &str,
    origin: usize,
) -> Result<Vec<(&'a str, usize)>, StoreError> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    let mut i = 0;
    while i < text.len() {
        let c = text.as_bytes()[i];
        if quoted && escaped {
            escaped = false;
        } else if quoted && c == b'\\' {
            escaped = true;
        } else if c == b'"' {
            quoted = !quoted;
        } else if !quoted && text[i..].starts_with(delimiter) {
            parts.push((&text[start..i], origin + start));
            i += delimiter.len();
            start = i;
            continue;
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
    if quoted {
        return Err(error(origin + start, "Unclosed quoted value"));
    }
    parts.push((&text[start..], origin + start));
    Ok(parts)
}
fn clause(text: &str, offset: usize) -> Result<(String, Compare, String), StoreError> {
    let text = text.trim();
    let i = text
        .find(['=', '!', '<', '>'])
        .ok_or_else(|| error(offset, "Expected field=value or numeric comparison"))?;
    let field = text[..i].trim().to_ascii_lowercase();
    let tail = &text[i..];
    let (cmp, n) = if tail.starts_with("!=") {
        (Compare::Ne, 2)
    } else if tail.starts_with("<=") {
        (Compare::Le, 2)
    } else if tail.starts_with(">=") {
        (Compare::Ge, 2)
    } else {
        match tail.as_bytes()[0] {
            b'=' => (Compare::Eq, 1),
            b'<' => (Compare::Lt, 1),
            b'>' => (Compare::Gt, 1),
            _ => return Err(error(offset + i, "Invalid comparison")),
        }
    };
    let raw = tail[n..].trim();
    if raw.is_empty() {
        return Err(error(offset + i + n, "Missing value"));
    }
    let value = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).map_err(|e| error(offset + i + n, e))?
    } else {
        if raw.contains('"') {
            return Err(error(offset + i + n, "Quote the entire value"));
        }
        raw.to_owned()
    };
    Ok((field, cmp, value))
}
pub(crate) fn expression(
    value: &str,
    raw_regex: bool,
    substring: bool,
    offset: usize,
) -> Result<Regex, StoreError> {
    let pattern = if raw_regex {
        value.to_owned()
    } else {
        let mut p = String::new();
        if !substring {
            p.push('^');
        }
        for c in value.chars() {
            match c {
                '*' => p.push_str(".*"),
                '?' => p.push('.'),
                _ => p.push_str(&regex::escape(&c.to_string())),
            }
        }
        if !substring {
            p.push('$');
        }
        p
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .size_limit(1024 * 1024)
        .build()
        .map_err(|e| error(offset, e))
}
fn number(value: &str, offset: usize) -> Result<i128, StoreError> {
    value
        .parse()
        .map_err(|_| error(offset, "Expected a decimal integer"))
}
impl EndpointQuery {
    fn parse(
        text: &str,
        offset: usize,
        position: Position,
        side: &str,
    ) -> Result<Self, StoreError> {
        let mut query = Self {
            position,
            predicates: Vec::new(),
        };
        let mut phase = false;
        let mut position_seen = false;
        for (part, at) in segments(text, "&", offset)? {
            let (mut field, cmp, value) = clause(part, at)?;
            if field == side {
                field = "phase".into();
            }
            match field.as_str() {
                "op" | "global" | "thread" | "rid" | "cycle" | "duration" => {
                    let n = number(&value, at)?;
                    if !(0..=u64::MAX as i128).contains(&n) {
                        return Err(error(at, "Endpoint integers must fit u64"));
                    }
                    query.predicates.push(Predicate::Number(field, cmp, n));
                }
                _ if !matches!(cmp, Compare::Eq) => {
                    return Err(error(at, "Text predicates require ="));
                }
                "phase_pos" => {
                    if position_seen {
                        return Err(error(at, "Duplicate phase_pos"));
                    }
                    position_seen = true;
                    query.position = match value.to_ascii_uppercase().as_str() {
                        "START" => Position::Start,
                        "END" => Position::End,
                        _ => return Err(error(at, "phase_pos must be START or END")),
                    };
                }
                "status" => {
                    let value = value.to_ascii_lowercase();
                    if !["all", "retired", "flushed", "incomplete"].contains(&value.as_str()) {
                        return Err(error(at, "Unknown instruction status"));
                    }
                    query.predicates.push(Predicate::Status(value));
                }
                "phase"
                | "lane"
                | "instr"
                | "meta"
                | "op_meta"
                | "phase_meta"
                | "instr_re"
                | "meta_re"
                | "op_meta_re"
                | "phase_meta_re"
                | "meta_contains"
                | "op_meta_contains"
                | "phase_meta_contains" => {
                    if field == "phase" {
                        phase = true;
                    }
                    let re = expression(
                        &value,
                        field.ends_with("_re"),
                        field.starts_with("instr") || field.ends_with("_contains"),
                        at,
                    )?;
                    query.predicates.push(Predicate::Text(field, re));
                }
                _ => return Err(error(at, format!("Unknown endpoint field {field}"))),
            }
        }
        if !phase {
            return Err(error(offset, "A phase/from/to constraint is required"));
        }
        Ok(query)
    }
    fn matches(&self, op: &Operation, stage: &Stage, cycle: u64, info: &TraceInfo) -> bool {
        self.predicates.iter().all(|p| match p {
            Predicate::Status(v) => match v.as_str() {
                "all" => true,
                "retired" => op.rid.is_some() && !op.flushed,
                "flushed" => op.flushed,
                "incomplete" => op.incomplete,
                _ => false,
            },
            Predicate::Text(field, re) => match field.as_str() {
                "phase" => re.is_match(symbol(&info.symbols, stage.name)),
                "lane" => re.is_match(symbol(&info.symbols, stage.lane)),
                "instr" | "instr_re" => re.is_match(&op.label),
                "op_meta" | "op_meta_re" | "op_meta_contains" => {
                    op.detail.lines().any(|l| re.is_match(l))
                }
                "phase_meta" | "phase_meta_re" | "phase_meta_contains" => {
                    stage.labels.lines().any(|l| re.is_match(l))
                }
                _ => op
                    .detail
                    .lines()
                    .chain(stage.labels.lines())
                    .any(|l| re.is_match(l)),
            },
            Predicate::Number(field, cmp, value) => {
                let actual = match field.as_str() {
                    "op" => Some(op.id),
                    "global" => Some(op.gid),
                    "thread" => Some(op.tid),
                    "rid" => op.rid,
                    "cycle" => Some(cycle),
                    "duration" => Some(
                        stage
                            .end
                            .unwrap_or(info.last_cycle.saturating_add(1))
                            .saturating_sub(stage.start),
                    ),
                    _ => None,
                };
                actual.is_some_and(|n| cmp.test(n as i128, *value))
            }
        })
    }
    /// Resolves matching phase occurrences, counting unknown requested ENDs.
    pub fn endpoints(
        &self,
        op: &Operation,
        row: u64,
        info: &TraceInfo,
    ) -> (Vec<PhaseEndpoint>, u64) {
        let mut hits = Vec::new();
        let mut skipped = 0;
        let mut lanes = Vec::new();
        for stage in &op.stages {
            if !lanes.contains(&stage.lane) {
                lanes.push(stage.lane);
            }
        }
        for (i, stage) in op.stages.iter().enumerate() {
            let end = stage.end.unwrap_or(info.last_cycle.saturating_add(1));
            let cycle = match self.position {
                Position::Start => stage.start,
                Position::End => end,
            };
            if !self.matches(op, stage, cycle, info) {
                continue;
            }
            // Parser finish gives unfinished phases a synthetic EOF boundary.
            let open = stage.end.is_none()
                || (op.incomplete
                    && stage.end == op.end
                    && stage.end == Some(info.last_cycle.saturating_add(1)));
            if matches!(self.position, Position::End) && open {
                skipped += 1;
                continue;
            }
            hits.push(PhaseEndpoint {
                op: op.id,
                row,
                thread: op.tid,
                stage: i as u32,
                lane_index: lanes.iter().position(|l| *l == stage.lane).unwrap_or(0) as u32,
                lane_count: lanes.len() as u32,
                cycle,
                start: stage.start,
                end,
                open,
                instruction: excerpt(op.label.lines().next().unwrap_or("")),
                phase: excerpt(symbol(&info.symbols, stage.name)),
                lane: excerpt(symbol(&info.symbols, stage.lane)),
                metadata: op
                    .detail
                    .chars()
                    .chain(std::iter::once('\n'))
                    .chain(stage.labels.chars())
                    .take(160)
                    .collect(),
            });
        }
        (hits, skipped)
    }
}
fn excerpt(text: &str) -> String {
    text.chars().take(160).collect()
}
impl FilterQuery {
    /// Parses the textual DSL; errors include byte locations. No trace-specific fields exist.
    pub fn parse(text: &str) -> Result<Self, StoreError> {
        if text.len() > 16384 {
            return Err(error(0, "Query exceeds 16 KiB"));
        }
        let sections = segments(text, ";", 0)?;
        let pairs = segments(sections[0].0, "->", 0)?;
        if pairs.len() > 2 {
            return Err(error(0, "Only one -> is allowed"));
        }
        let interval = pairs.len() == 2;
        let source = EndpointQuery::parse(
            pairs[0].0,
            pairs[0].1,
            if interval {
                Position::End
            } else {
                Position::Start
            },
            "from",
        )?;
        let target = if interval {
            Some(EndpointQuery::parse(
                pairs[1].0,
                pairs[1].1,
                Position::Start,
                "to",
            )?)
        } else {
            None
        };
        let mut query = Self {
            source,
            target,
            exclusion: None,
            options: DrawOptions::default(),
            comparisons: Vec::new(),
        };
        let mut options_seen = std::collections::BTreeSet::new();
        let mut exclusion = None;
        let mut exclude_scope = ExcludeScope::default();
        for (part, at) in sections.into_iter().skip(1) {
            let (field, cmp, value) = clause(part, at)?;
            match field.as_str() {
                "exclude" | "exclude_scope" if interval && matches!(cmp, Compare::Eq) => {
                    if !options_seen.insert(field.clone()) {
                        return Err(error(at, "Duplicate exclusion option"));
                    }
                    if field == "exclude" {
                        exclusion = Some((value, at));
                    } else {
                        exclude_scope = match value.as_str() {
                            "rows" => ExcludeScope::BetweenRows,
                            "all" => ExcludeScope::AllRows,
                            _ => return Err(error(at, "exclude_scope must be rows or all")),
                        };
                    }
                }
                "gap" | "row_distance" if interval => {
                    let n = number(&value, at)?;
                    if field == "row_distance" && n < 0 {
                        return Err(error(at, "Row distance cannot be negative"));
                    }
                    query.comparisons.push((field, cmp, n));
                }
                "label" | "span" | "row_pad" if matches!(cmp, Compare::Eq) => {
                    if !options_seen.insert(field.clone()) {
                        return Err(error(at, "Duplicate drawing option"));
                    }
                    match field.as_str() {
                        "label" => {
                            if value.chars().count() > 80 {
                                return Err(error(at, "Label exceeds 80 characters"));
                            }
                            query.options.label = value;
                        }
                        "span" => {
                            query.options.span = match value.as_str() {
                                "both" => DrawSpan::Both,
                                "source" => DrawSpan::Source,
                                "target" => DrawSpan::Target,
                                "all" => DrawSpan::All,
                                _ => {
                                    return Err(error(
                                        at,
                                        "span must be both, source, target, or all",
                                    ));
                                }
                            }
                        }
                        _ => {
                            query.options.row_pad = value
                                .parse()
                                .map_err(|_| error(at, "row_pad must fit u64"))?
                        }
                    }
                }
                _ => return Err(error(at, format!("Unknown or invalid option {field}"))),
            }
        }
        if let Some((text, at)) = exclusion {
            query.exclusion = Some(ExclusionQuery {
                endpoint: EndpointQuery::parse(&text, at, Position::Start, "exclude")?,
                scope: exclude_scope,
            });
        } else if options_seen.contains("exclude_scope") {
            return Err(error(0, "exclude_scope requires an exclusion condition"));
        }
        Ok(query)
    }
    /// Applies interval constraints after pairing; never substitutes another target.
    pub fn accepts(&self, source: &PhaseEndpoint, target: &PhaseEndpoint) -> bool {
        self.comparisons.iter().all(|(field, cmp, value)| {
            cmp.test(
                if field == "gap" {
                    target.cycle as i128 - source.cycle as i128
                } else {
                    target.row.abs_diff(source.row) as i128
                },
                *value,
            )
        })
    }
}
