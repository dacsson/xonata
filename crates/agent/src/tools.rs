use crate::{MAX_TOOL_BYTES, TraceClient, delay};
use futures::channel::mpsc;
use rig::message::ToolName;
use rig::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use xonata_core::{AnalysisAction, SearchQuery};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    text: Option<String>,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    case_sensitive: bool,
    id: Option<String>,
    start: Option<String>,
    count: Option<u32>,
    descending: Option<bool>,
}

fn decimal(value: Option<String>) -> Result<u64, String> {
    value
        .unwrap_or_else(|| "0".into())
        .parse()
        .map_err(|_| "Expected an unsigned decimal integer string".into())
}
const HELP: &str = "Phase syntax: phase=E & instr=vfirst & meta_contains=RQU* & thread=0 & duration>=3. Intervals: from=E & instr=vfirst -> to=E & instr=vmsne; gap>=0. Add phase_pos=START or phase_pos=END to an endpoint (End must be known). Each source pairs with the next later matching row in the same thread, then signed gap constraints apply without substituting a later target. Optional exclusion: ; exclude=\"phase=X & instr=barrier\"; exclude_scope=rows (strictly intervening rows) or all (any row/thread), boundaries strictly inside the paired interval. Field contains matching is case-insensitive with wildcards. Inspect trace_info symbols and operation stages first; phase names vary between traces. Results use exact decimal strings and retain stable original indexes under elapsed sorting.";

pub(crate) fn build(
    client: TraceClient,
    activity: mpsc::Sender<String>,
) -> Result<Vec<DynamicTool>, String> {
    let calls = Arc::new(AtomicU32::new(0));
    let sequence = Arc::new(AtomicU32::new(0));
    let definitions = [
        (
            "trace_info",
            "Loaded trace counts, cycles, warnings, and stage symbols.",
            json!({}),
        ),
        (
            "operation_details",
            "Inspect a specific operation with stage intervals, metadata, and dependencies. id is an exact decimal string.",
            json!({"id":{"type":"string"}}),
        ),
        (
            "peek_rows",
            "Inspect unfiltered rows. start is an exact decimal string; count defaults to 20, maximum 64.",
            json!({"start":{"type":"string"},"count":{"type":"integer"}}),
        ),
        (
            "search_trace",
            "Start a search with text, or omit text to page the current search. Returns observed rows and exact total. start is a decimal string.",
            json!({"text":{"type":"string"},"regex":{"type":"boolean"},"case_sensitive":{"type":"boolean"},"start":{"type":"string"},"count":{"type":"integer"}}),
        ),
        (
            "phase_help",
            "Read phase-query syntax and pairing/exclusion semantics before constructing a query.",
            json!({}),
        ),
        (
            "query_phases",
            "Start a phase query with text, or omit text to page current results. Optional descending sorts by signed elapsed cycles. Read phase_help first.",
            json!({"text":{"type":"string"},"start":{"type":"string"},"count":{"type":"integer"},"descending":{"type":"boolean"}}),
        ),
    ];
    definitions
        .into_iter()
        .map(|(name, description, properties)| {
            let client = client.clone();
            let calls = calls.clone();
            let sequence = sequence.clone();
            let activity = activity.clone();
            let tool = DynamicTool::new(
                ToolName::new(name).map_err(|e| e.to_string())?,
                description,
                json!({"type":"object", "properties":properties, "additionalProperties":false}),
                move |args| {
                    let client = client.clone();
                    let calls = calls.clone();
                    let sequence = sequence.clone();
                    let mut activity = activity.clone();
                    Box::pin(async move {
                        let result: Result<Value, String> = async {
                            if calls.fetch_add(1, Ordering::Relaxed) >= 32 {
                                return Err("Tool call budget exhausted (32)".into());
                            }
                            let _ = activity.try_send(name.to_owned());
                            let args: Args = serde_json::from_value(args)
                                .map_err(|e| format!("Invalid tool arguments: {e}"))?;
                            if args.text.as_ref().is_some_and(|s| s.len() > 16 * 1024) {
                                return Err("Query exceeds 16 KiB".into());
                            }
                            let start = decimal(args.start)?;
                            let count = args.count.unwrap_or(20).clamp(1, 64);
                            let call = |action| {
                                client.call(sequence.fetch_add(1, Ordering::Relaxed), action)
                            };
                            let data = match name {
                                "trace_info" => call(AnalysisAction::Info).await?,
                                "operation_details" => {
                                    call(AnalysisAction::Operation {
                                        id: decimal(Some(args.id.ok_or("id is required")?))?,
                                    })
                                    .await?
                                }
                                "peek_rows" => call(AnalysisAction::Rows { start, count }).await?,
                                "phase_help" => json!({"syntax":HELP}),
                                "search_trace" | "query_phases" => {
                                    let phases = name == "query_phases";
                                    if let Some(text) = args.text {
                                        call(if phases {
                                            AnalysisAction::Filter { text }
                                        } else {
                                            AnalysisAction::Search {
                                                query: Box::new(SearchQuery {
                                                    text,
                                                    regex: args.regex,
                                                    case_sensitive: args.case_sensitive,
                                                    ..Default::default()
                                                }),
                                            }
                                        })
                                        .await?;
                                    }
                                    let mut status;
                                    loop {
                                        status = call(AnalysisAction::Status).await?;
                                        if status[if phases { "has_filter" } else { "has_search" }]
                                            != true
                                        {
                                            return Err(
                                                "Start a query with text before requesting pages"
                                                    .into(),
                                            );
                                        }
                                        if status
                                            [if phases { "filter_done" } else { "search_done" }]
                                            == true
                                        {
                                            break;
                                        }
                                        delay(30).await;
                                    }
                                    if phases && let Some(descending) = args.descending {
                                        call(AnalysisAction::Sort { descending }).await?;
                                        loop {
                                            status = call(AnalysisAction::Status).await?;
                                            if status["sort_done"] == true {
                                                break;
                                            }
                                            delay(30).await;
                                        }
                                    }
                                    let result = call(if phases {
                                        AnalysisAction::FilterResults {
                                            start,
                                            count,
                                            sorted: args.descending.is_some(),
                                        }
                                    } else {
                                        AnalysisAction::SearchResults { start, count }
                                    })
                                    .await?;
                                    json!({"status":status, "page":result})
                                }
                                _ => return Err("Unknown tool".into()),
                            };
                            Ok(bounded(data, start))
                        }
                        .await;
                        result
                            .map(ToolOutput::json)
                            .map_err(ToolExecutionError::provider)
                    })
                },
            );
            Ok(tool)
        })
        .collect()
}

// A hostile trace can contain megabytes of labels on one operation. Apply both
// structural and byte budgets before anything is sent to the model.
fn crop(value: &mut Value, array_limit: usize, truncated: &mut bool) {
    match value {
        Value::Number(n) => {
            *value = Value::String(n.to_string());
        }
        Value::String(s) if s.len() > 2048 => {
            let mut end = 2048;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            *truncated = true;
        }
        Value::Array(items) => {
            if items.len() > array_limit {
                items.truncate(array_limit);
                *truncated = true;
            }
            for item in items {
                crop(item, array_limit, truncated);
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                crop(item, array_limit, truncated);
            }
        }
        _ => {}
    }
}

fn bounded(mut data: Value, start: u64) -> Value {
    let mut truncated = false;
    crop(&mut data, 64, &mut truncated);
    let mut limit = 32;
    while data.to_string().len() > MAX_TOOL_BYTES - 1024 && limit > 0 {
        crop(&mut data, limit, &mut truncated);
        limit /= 2;
    }
    let page = data.get("page").unwrap_or(&data);
    let returned = page
        .get("hits")
        .or_else(|| page.get("rows"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    json!({"data":data,"truncated":truncated,"returned":returned,"next_start":start.saturating_add(returned as u64).to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_arguments_and_tool_budgets_do_not_reach_the_worker() {
        futures::executor::block_on(async {
            let (sender, mut requests) = crate::trace_channel();
            let (activity, _) = mpsc::channel(8);
            let tools = build(TraceClient::new(sender, 1, 1), activity).unwrap();
            assert!(tools[1].run(json!({"id":"-1"})).await.is_err());
            assert!(tools[2].run(json!({"unknown":1})).await.is_err());
            for _ in 0..30 {
                tools[4].run(json!({})).await.unwrap();
            }
            assert!(tools[4].run(json!({})).await.is_err());
            assert!(requests.try_recv().is_err());
        });
    }

    #[test]
    fn tool_payload_preserves_integers_and_bounds_hostile_metadata() {
        let rows = (0..64)
            .map(|_| json!({"id":u64::MAX,"label":"Ж".repeat(100_000),"stages":[1,2,3]}))
            .collect::<Vec<_>>();
        let value = bounded(json!({"rows":rows}), 10);
        assert!(value.to_string().len() <= MAX_TOOL_BYTES);
        assert_eq!(value["data"]["rows"][0]["id"], u64::MAX.to_string());
        assert_eq!(value["truncated"], true);
        let returned = value["returned"].as_u64().unwrap();
        assert_eq!(value["next_start"], (10 + returned).to_string());
    }
}
