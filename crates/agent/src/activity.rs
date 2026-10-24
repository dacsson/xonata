//! Bounded observations of model requests and tool calls, separate from model context.
use futures::{SinkExt, channel::mpsc};
use rig::{
    agent::{AgentHook, DispatchAction, DispatchEvent, HookContext, OutcomeAction, OutcomeEvent},
    effect::EffectKind,
};

/// Maximum UTF-8 bytes in an argument or result preview, including its truncation marker.
pub const MAX_PREVIEW_BYTES: usize = 2048;

/// Ordered lifecycle observations for one analysis run. IDs pair starts with outcomes.
#[derive(Clone, Debug)]
pub enum ActivityEvent {
    Started {
        id: u64,
        turn: usize,
        /// None identifies a model request; tools carry their name and argument preview.
        tool: Option<(String, String)>,
    },
    Finished {
        id: u64,
        failed: bool,
        /// Model requests carry no prompt or response content here.
        output: Option<String>,
    },
}

pub(crate) fn preview(text: &str) -> String {
    const MARKER: &str = "\n[Preview truncated]";
    if text.len() <= MAX_PREVIEW_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_PREVIEW_BYTES - MARKER.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &text[..end])
}

pub(crate) struct ActivityHook(pub mpsc::Sender<ActivityEvent>);

impl AgentHook for ActivityHook {
    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let tool = match event.kind {
            EffectKind::Completion { .. } => None,
            EffectKind::ToolCall { name, args } => Some((preview(name), preview(args))),
            _ => return DispatchAction::Proceed,
        };
        // Backpressure preserves lifecycle events; closing the UI must not fail the analysis.
        let _ = self
            .0
            .clone()
            .send(ActivityEvent::Started {
                id: event.id.as_u64(),
                turn: event.turn,
                tool,
            })
            .await;
        DispatchAction::Proceed
    }

    async fn on_outcome(&self, _: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        let (failed, output) = match event.kind {
            EffectKind::Completion { .. } => (event.outcome.is_err(), None),
            EffectKind::ToolCall { .. } => {
                if let Some(result) = event.tool_result() {
                    (
                        !result.is_success(),
                        Some(preview(&result.output().render())),
                    )
                } else {
                    (
                        true,
                        Some(preview(
                            &event
                                .outcome
                                .as_ref()
                                .err()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "Tool returned no result".into()),
                        )),
                    )
                }
            }
            _ => return OutcomeAction::proceed(),
        };
        let _ = self
            .0
            .clone()
            .send(ActivityEvent::Finished {
                id: event.id.as_u64(),
                failed,
                output,
            })
            .await;
        OutcomeAction::proceed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_should_bound_utf8_without_changing_short_results() {
        assert_eq!(
            preview("{\"id\":\"9007199254740993\"}"),
            "{\"id\":\"9007199254740993\"}"
        );
        let text = preview(&"界".repeat(MAX_PREVIEW_BYTES));
        assert!(text.len() <= MAX_PREVIEW_BYTES);
        assert!(text.ends_with("\n[Preview truncated]"));
    }
}
