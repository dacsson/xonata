//! Prepared full-trace search with typed filters.
use crate::storage::StoreError;
use crate::{Operation, SearchQuery, symbol};
use regex::{Regex, RegexBuilder};

/// Validated search pattern.
pub struct Matcher {
    query: SearchQuery,
    expression: Option<Regex>,
}
impl Matcher {
    /// Compiles a query, returning an inline-displayable error for invalid regex.
    pub fn new(query: SearchQuery) -> Result<Self, StoreError> {
        let expression = if query.text.is_empty() {
            None
        } else {
            let pattern = if query.regex {
                query.text.clone()
            } else {
                regex::escape(&query.text)
            };
            Some(
                RegexBuilder::new(&pattern)
                    .case_insensitive(!query.case_sensitive)
                    .build()
                    .map_err(|e| StoreError::Input(format!("Invalid search pattern: {e}")))?,
            )
        };
        Ok(Self { query, expression })
    }
    /// Returns names and snippets of matching text fields if all filters pass.
    pub fn snippets(&self, op: &Operation, symbols: &[String]) -> Option<Vec<String>> {
        let q = &self.query;
        if !q.op.contains(op.id) || !q.global.contains(op.gid) || !q.thread.contains(op.tid) {
            return None;
        }
        if (q.retired_id.min.is_some() || q.retired_id.max.is_some())
            && !op.rid.is_some_and(|rid| q.retired_id.contains(rid))
        {
            return None;
        }
        if q.cycles
            .min
            .is_some_and(|min| op.end.unwrap_or(op.fetch) < min)
            || q.cycles.max.is_some_and(|max| op.fetch > max)
        {
            return None;
        }
        if !q.stage.is_empty()
            && !op
                .stages
                .iter()
                .any(|s| symbol(symbols, s.name).eq_ignore_ascii_case(&q.stage))
        {
            return None;
        }
        if match q.status.as_str() {
            "" | "all" => false,
            "retired" => op.rid.is_none() || op.flushed,
            "flushed" => !op.flushed,
            "incomplete" => !op.incomplete,
            _ => true,
        } {
            return None;
        }
        let Some(re) = &self.expression else {
            return Some(Vec::new());
        };
        let mut snippets = Vec::new();
        let mut add = |name: &str, value: &str| {
            if let Some(found) = re.find(value) {
                let mut start = found.start().saturating_sub(40);
                while !value.is_char_boundary(start) {
                    start -= 1;
                }
                let mut end = (found.end() + 60).min(value.len());
                while !value.is_char_boundary(end) {
                    end += 1;
                }
                snippets.push(format!("{name}: {}", &value[start..end]));
            }
        };
        add("Disassembly", &op.label);
        add("Metadata", &op.detail);
        for stage in &op.stages {
            add("Stage", symbol(symbols, stage.name));
            add("Stage metadata", &stage.labels);
        }
        (!snippets.is_empty()).then_some(snippets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NumberRange;
    #[test]
    fn invalid_regex_should_report_an_error() {
        let query = SearchQuery {
            text: "[".into(),
            regex: true,
            ..Default::default()
        };
        assert!(Matcher::new(query).is_err());
    }
    #[test]
    fn number_range_should_be_inclusive() {
        let range = NumberRange {
            min: Some(5),
            max: Some(5),
        };
        assert!(range.contains(5));
    }
}
