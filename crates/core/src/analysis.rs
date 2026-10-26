//! Isolated read-only query contexts sharing the worker's trace pages.
use super::{Engine, QueryContext};
use crate::{Event, Request, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Correlated request from one analysis run. Session IDs are allocated by the host.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalysisRequest {
    /// Loaded trace ID.
    pub trace: u32,
    /// Run-scoped query/storage namespace.
    pub session: u32,
    /// Per-run request identity.
    pub request: u32,
    /// Inspection or query operation.
    pub action: AnalysisAction,
}

/// Read-only analysis capabilities; viewport and drawing mutations are excluded.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnalysisAction {
    /// Trace metadata.
    Info,
    /// Full operation with stage and dependency evidence.
    Operation {
        /// Exact operation ID.
        #[serde(with = "crate::filter::decimal_u64")]
        id: u64,
    },
    /// Unfiltered pipeline rows.
    Rows {
        /// First row.
        #[serde(with = "crate::filter::decimal_u64")]
        start: u64,
        /// Requested page size, capped at 64.
        count: u32,
    },
    /// Replaces only this session's ordinary search.
    Search {
        /// Existing typed search semantics.
        query: Box<crate::SearchQuery>,
    },
    /// Replaces only this session's phase query.
    Filter {
        /// Existing phase-filter expression.
        text: String,
    },
    /// Starts elapsed sorting after the phase query completes.
    Sort {
        /// Largest elapsed counts first.
        descending: bool,
    },
    /// Current search, phase query, and sorting progress.
    Status,
    /// One search result page.
    SearchResults {
        /// First result.
        #[serde(with = "crate::filter::decimal_u64")]
        start: u64,
        /// Page size capped at 64.
        count: u32,
    },
    /// One original or elapsed-sorted phase result page.
    FilterResults {
        /// First result.
        #[serde(with = "crate::filter::decimal_u64")]
        start: u64,
        /// Page size capped at 64.
        count: u32,
        /// Read the completed elapsed-sorted view.
        sorted: bool,
    },
    /// Releases all query state and temporary indexes for this run.
    Cancel,
}

impl Engine {
    // The single-owner worker serializes these context switches. Only query state
    // is exchanged: operation pages and the bounded decoded cache stay shared.
    // Always restore the viewer context, including when a query returns an error.
    fn enter_analysis(&mut self, trace: u32, session: u32) -> Result<QueryContext, StoreError> {
        let original = self
            .traces
            .get_mut(&trace)
            .ok_or_else(|| StoreError::Input("Trace is closed".into()))?;
        if !original.parser.info.complete {
            return Err(StoreError::Input("Wait for trace loading to finish".into()));
        }
        if !self.analysis.contains_key(&(trace, session)) && self.analysis.len() >= 2 {
            return Err(StoreError::Input("Analysis worker is busy".into()));
        }
        let context = self.analysis.remove(&(trace, session)).unwrap_or_default();
        self.store.auxiliary_prefix = format!("analysis-{session}-");
        Ok(std::mem::replace(&mut original.queries, context))
    }

    fn leave_analysis(&mut self, trace: u32, session: u32, original: QueryContext) {
        if let Some(target) = self.traces.get_mut(&trace) {
            self.analysis.insert(
                (trace, session),
                std::mem::replace(&mut target.queries, original),
            );
        }
        self.store.auxiliary_prefix.clear();
    }

    /// Executes a bounded, correlated analysis request independently of the UI.
    pub async fn analysis_request(
        &mut self,
        request: AnalysisRequest,
    ) -> Result<Value, StoreError> {
        let AnalysisRequest {
            trace,
            session,
            action,
            ..
        } = request;
        if matches!(action, AnalysisAction::Cancel) {
            self.analysis_errors.remove(&(trace, session));
            self.analysis.remove(&(trace, session));
            self.store.clear_analysis(trace, session).await?;
            return Ok(json!({"cancelled": true}));
        }
        if let Some(error) = self.analysis_errors.get(&(trace, session)) {
            return Err(StoreError::Input(error.clone()));
        }
        let original = self.enter_analysis(trace, session)?;
        let result = self.analysis_action(trace, action).await;
        self.leave_analysis(trace, session, original);
        result
    }

    async fn analysis_action(
        &mut self,
        trace: u32,
        action: AnalysisAction,
    ) -> Result<Value, StoreError> {
        let request = match action {
            AnalysisAction::Info => return Ok(json!(self.info(trace))),
            AnalysisAction::Operation { id } => {
                let op = self
                    .store
                    .get(trace, id)
                    .await?
                    .ok_or_else(|| StoreError::Input("Operation not found".into()))?;
                let symbols = &self.traces[&trace].parser.info.symbols;
                let stages: Vec<_> = op.stages.iter().map(|stage| json!({
                    "phase": crate::symbol(symbols,stage.name), "lane": crate::symbol(symbols,stage.lane),
                    "start": stage.start.to_string(), "end": stage.end.map(|c| c.to_string()), "metadata": stage.labels,
                })).collect();
                let mut operation = json!(op);
                operation["stages"] = json!(stages);
                return Ok(json!({"operation":operation}));
            }
            AnalysisAction::Status => {
                let target = &self.traces[&trace];
                return Ok(json!({
                    "has_search": target.queries.search.is_some(),
                    "has_filter": target.queries.filter.is_some(),
                    "search_done": target.queries.search.as_ref().is_none_or(|s| s.cursor >= s.pages.len()),
                    "search_total": target.queries.search.as_ref().map_or(0, |s| s.total).to_string(),
                    "filter_done": target.queries.filter.as_ref().is_none_or(|f| f.done),
                    "filter_total": target.queries.filter.as_ref().map_or(0, |f| f.total).to_string(),
                    "sort_done": target.queries.sorting.as_ref().is_none_or(|s| s.done),
                }));
            }
            AnalysisAction::Rows { start, count } => Request::View {
                trace,
                generation: 1,
                start,
                stride: 1,
                count: count.clamp(1, 64),
                hide_flushed: false,
            },
            AnalysisAction::Search { query } => Request::Search {
                trace,
                generation: 1,
                query,
            },
            AnalysisAction::SearchResults { start, count } => Request::Results {
                trace,
                generation: 1,
                start,
                count: count.clamp(1, 64),
            },
            AnalysisAction::Filter { text } => Request::Filter {
                trace,
                generation: 1,
                text,
                skip: 0,
            },
            AnalysisAction::Sort { descending } => {
                if !self.traces[&trace]
                    .queries
                    .filter
                    .as_ref()
                    .is_some_and(|f| f.done)
                {
                    return Err(StoreError::Input("Phase query is not complete".into()));
                }
                Request::FilterSort {
                    trace,
                    generation: 1,
                    revision: 1,
                    descending,
                }
            }
            AnalysisAction::FilterResults {
                start,
                count,
                sorted,
            } => {
                if sorted {
                    Request::SortedFilterResults {
                        trace,
                        generation: 1,
                        revision: 1,
                        start,
                        count: count.clamp(1, 64),
                    }
                } else {
                    Request::FilterResults {
                        trace,
                        generation: 1,
                        start,
                        count: count.clamp(1, 64),
                    }
                }
            }
            AnalysisAction::Cancel => return Ok(Value::Null),
        };
        let event = self.request(request).await?;
        if let Some(Event::FilterError { message, .. }) = event {
            return Err(StoreError::Input(message));
        }
        Ok(json!(event))
    }

    pub(super) async fn tick_analysis(&mut self) -> Result<(), StoreError> {
        let sessions: Vec<_> = self.analysis.keys().copied().collect();
        for (trace, session) in sessions {
            let original = self.enter_analysis(trace, session)?;
            let result = self.tick_traces([trace]).await;
            self.leave_analysis(trace, session, original);
            let failure = match result {
                Err(error) => Some(error.to_string()),
                Ok(events) => events.into_iter().find_map(|e| match e {
                    Event::FilterError { message, .. } => Some(message),
                    _ => None,
                }),
            };
            if let Some(message) = failure {
                self.analysis
                    .insert((trace, session), QueryContext::default());
                self.analysis_errors.insert((trace, session), message);
            }
        }
        Ok(())
    }
}
