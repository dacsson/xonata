//! Shared per-tab conversation UI and bounded analysis lifecycle.
use crate::ui::Transport;
use egui::{self, Rect};
use futures::{
    channel::{mpsc, oneshot},
    future::AbortHandle,
};
use std::collections::BTreeMap;
pub use xonata_agent::AgentConfig;
use xonata_agent::{Message, Role, TraceClient};
use xonata_core::{AnalysisAction, AnalysisRequest};

/// Reply channel owned by a worker transport until its bounded request completes.
pub type AnalysisReply = oneshot::Sender<Result<serde_json::Value, String>>;

#[derive(Default)]
pub(crate) struct Chat {
    pub open: bool,
    pub input: String,
    pub focus_input: bool,
    pub messages: Vec<Message>,
    pub running: bool,
    pub status: String,
}

struct Active {
    run: u32,
    cancel: AbortHandle,
    activity: mpsc::Receiver<String>,
}

struct Finished {
    trace: u32,
    run: u32,
    result: Result<String, xonata_agent::AgentError>,
}

pub(crate) struct ChatUpdate {
    pub trace: u32,
    pub running: bool,
    pub status: String,
    pub reply: Option<String>,
}

pub(crate) struct Chats {
    config: Option<AgentConfig>,
    next_run: u32,
    active: BTreeMap<u32, Active>,
    sender: mpsc::Sender<xonata_agent::PendingRequest>,
    requests: xonata_agent::Requests,
    finished: mpsc::UnboundedReceiver<Finished>,
    finish: mpsc::UnboundedSender<Finished>,
    #[cfg(not(target_arch = "wasm32"))]
    runtime: Option<tokio::runtime::Runtime>,
}

impl Default for Chats {
    fn default() -> Self {
        let (sender, requests) = xonata_agent::trace_channel();
        let (finish, finished) = mpsc::unbounded();
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            config: AgentConfig::from_env(),
            #[cfg(target_arch = "wasm32")]
            config: None,
            next_run: 0,
            active: BTreeMap::new(),
            sender,
            requests,
            finished,
            finish,
            #[cfg(not(target_arch = "wasm32"))]
            runtime: None,
        }
    }
}

impl Drop for Chats {
    fn drop(&mut self) {
        for active in self.active.values() {
            active.cancel.abort();
        }
    }
}

impl Chats {
    pub fn available(&self) -> bool {
        self.config.is_some()
    }

    pub fn configure(&mut self, config: Option<AgentConfig>, transport: &mut dyn Transport) {
        let traces: Vec<_> = self.active.keys().copied().collect();
        for trace in traces {
            self.cancel(trace, transport);
        }
        self.config = config;
    }

    pub fn cancel(&mut self, trace: u32, transport: &mut dyn Transport) {
        if let Some(active) = self.active.remove(&trace) {
            active.cancel.abort();
            Self::release(trace, active.run, transport);
        }
    }

    fn release(trace: u32, session: u32, transport: &mut dyn Transport) {
        let (reply, _) = oneshot::channel();
        transport.analysis(
            AnalysisRequest {
                trace,
                session,
                request: u32::MAX,
                action: AnalysisAction::Cancel,
            },
            reply,
        );
    }

    pub fn poll_events(&mut self, transport: &mut dyn Transport) -> Vec<ChatUpdate> {
        while let Ok(pending) = self.requests.try_recv() {
            if self
                .active
                .get(&pending.request.trace)
                .is_some_and(|active| active.run == pending.request.session)
            {
                transport.analysis(pending.request, pending.reply);
            } else {
                let _ = pending.reply.send(Err("Analysis cancelled".into()));
            }
        }
        let mut events = Vec::new();
        for (&trace, active) in &mut self.active {
            let mut latest = None;
            while let Ok(text) = active.activity.try_recv() {
                latest = Some(text);
            }
            if let Some(text) = latest {
                events.push(ChatUpdate {
                    trace,
                    running: true,
                    status: format!("Inspecting {text}…"),
                    reply: None,
                });
            }
        }
        while let Ok(done) = self.finished.try_recv() {
            if !self
                .active
                .get(&done.trace)
                .is_some_and(|active| active.run == done.run)
            {
                continue;
            }
            self.active.remove(&done.trace);
            Self::release(done.trace, done.run, transport);
            events.push(match done.result {
                Ok(text) => ChatUpdate {
                    trace: done.trace,
                    running: false,
                    status: String::new(),
                    reply: Some(text),
                },
                Err(error) => ChatUpdate {
                    trace: done.trace,
                    running: false,
                    status: error.to_string(),
                    reply: None,
                },
            });
        }
        events
    }

    fn start(&mut self, trace: u32, chat: &mut Chat) -> Result<(), String> {
        let config = self
            .config
            .clone()
            .ok_or("No agent connection configured")?;
        if self.active.len() >= 2 {
            return Err("Agent busy: two analyses are already running".into());
        }
        if self.active.contains_key(&trace) {
            return Err("An analysis is already running in this tab".into());
        }
        let text = chat.input.trim().to_owned();
        if text.is_empty() {
            return Ok(());
        }
        if text.len() > 16 * 1024 {
            return Err("Question exceeds 16 KiB".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.runtime.is_none() {
            self.runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?,
            );
        }
        self.next_run = self
            .next_run
            .checked_add(1)
            .ok_or("Analysis session IDs exhausted; reopen the viewer")?;
        let run = self.next_run;
        chat.messages.push(Message {
            role: Role::User,
            text,
        });
        xonata_agent::trim_history(&mut chat.messages);
        let history = chat.messages.clone();
        let client = TraceClient::new(self.sender.clone(), trace, run);
        let (cancel, registration) = AbortHandle::new_pair();
        let (activity, activity_rx) = mpsc::channel(8);
        let finish = self.finish.clone();
        let future = async move {
            let result = xonata_agent::run(config, client, history, activity, registration).await;
            let _ = finish.unbounded_send(Finished { trace, run, result });
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(runtime) = &self.runtime {
            runtime.spawn(future);
        }
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(future);
        self.active.insert(
            trace,
            Active {
                run,
                cancel,
                activity: activity_rx,
            },
        );
        chat.input.clear();
        chat.running = true;
        chat.status = "Analyzing…".into();
        Ok(())
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        trace: u32,
        complete: bool,
        chat: &mut Chat,
        canvas: Rect,
        transport: &mut dyn Transport,
    ) {
        if !chat.open {
            return;
        }
        let mut open = chat.open;
        egui::Window::new("Ask agent")
            .id(egui::Id::new((trace, "agent")))
            .open(&mut open)
            .default_pos(egui::pos2(canvas.left() + 50.0, canvas.top() + 60.0))
            .collapsible(false)
            .default_width(460.0)
            .default_height(400.0)
            .resizable(true)
            .constrain_to(canvas)
            .show(ctx, |ui| {
                if !self.available() {
                    ui.label("Agent connection is not configured.");
                }
                if !complete {
                    ui.label("Wait for trace loading to finish.");
                }
                egui::ScrollArea::vertical()
                    .id_salt((trace, "agent-history"))
                    .max_height((ui.available_height() - 85.0).max(80.0))
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for message in &chat.messages {
                            ui.label(
                                egui::RichText::new(match message.role {
                                    Role::User => "You",
                                    Role::Assistant => "Agent",
                                })
                                .strong(),
                            );
                            ui.add(egui::Label::new(&message.text).wrap().selectable(true));
                            ui.add_space(8.0);
                        }
                    });
                ui.label(&chat.status);
                let input = ui.add_enabled(
                    !chat.running,
                    egui::TextEdit::singleline(&mut chat.input)
                        .desired_width(f32::INFINITY)
                        .hint_text("Ask about this trace"),
                );
                if chat.focus_input {
                    input.request_focus();
                    chat.focus_input = false;
                }
                ui.horizontal(|ui| {
                    let send = ui
                        .add_enabled(
                            self.available()
                                && complete
                                && !chat.running
                                && !chat.input.trim().is_empty(),
                            egui::Button::new("Send"),
                        )
                        .clicked();
                    if (send
                        || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                        && complete
                        && !chat.running
                        && let Err(error) = self.start(trace, chat)
                    {
                        chat.status = error;
                    }
                    if ui
                        .add_enabled(chat.running, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        self.cancel(trace, transport);
                        chat.running = false;
                        chat.status = "Analysis cancelled".into();
                    }
                });
            });
        chat.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use xonata_core::{Event, Request};

    #[derive(Default)]
    struct Host {
        requests: Vec<AnalysisRequest>,
    }

    impl Transport for Host {
        fn open_dialog(&mut self) {}

        fn request(&mut self, _: Request) {}

        fn poll(&mut self) -> Vec<Event> {
            Vec::new()
        }

        fn analysis(&mut self, request: AnalysisRequest, reply: AnalysisReply) {
            self.requests.push(request);
            let _ = reply.send(Ok(json!({})));
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn open_paths(&mut self, _: Vec<std::path::PathBuf>) {}
    }

    fn active(run: u32) -> Active {
        let (cancel, _) = AbortHandle::new_pair();
        let (_, activity) = mpsc::channel(8);
        Active {
            run,
            cancel,
            activity,
        }
    }

    #[test]
    fn replies_keep_tab_identity_and_stale_runs_cannot_complete_a_new_run() {
        let mut chats = Chats::default();
        let mut host = Host::default();
        chats.active.insert(1, active(10));
        chats.active.insert(2, active(11));
        chats
            .finish
            .unbounded_send(Finished {
                trace: 2,
                run: 11,
                result: Ok("tab two".into()),
            })
            .unwrap();
        let updates = chats.poll_events(&mut host);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].trace, 2);
        assert_eq!(updates[0].reply.as_deref(), Some("tab two"));
        assert!(chats.active.contains_key(&1));
        chats.cancel(1, &mut host);
        chats.active.insert(1, active(12));
        chats
            .finish
            .unbounded_send(Finished {
                trace: 1,
                run: 10,
                result: Ok("stale".into()),
            })
            .unwrap();
        assert!(chats.poll_events(&mut host).is_empty());
        assert_eq!(chats.active[&1].run, 12);
        assert!(
            host.requests
                .iter()
                .all(|r| matches!(r.action, AnalysisAction::Cancel))
        );
    }

    #[test]
    fn configuration_replacement_cancels_all_runs_and_rejects_queued_tools() {
        let mut chats = Chats::default();
        let mut host = Host::default();
        chats.active.insert(1, active(10));
        chats.active.insert(2, active(11));
        let (reply, receive) = oneshot::channel();
        chats
            .sender
            .try_send(xonata_agent::PendingRequest {
                request: AnalysisRequest {
                    trace: 1,
                    session: 10,
                    request: 2,
                    action: AnalysisAction::Info,
                },
                reply,
            })
            .unwrap();
        chats.configure(None, &mut host);
        assert!(!chats.available());
        assert!(chats.poll_events(&mut host).is_empty());
        assert!(futures::executor::block_on(receive).unwrap().is_err());
        assert_eq!(host.requests.len(), 2);
        assert!(chats.active.is_empty());
    }

    #[test]
    fn concurrency_limits_report_busy_without_starting_another_run() {
        let mut chats = Chats::default();
        chats.config = Some(AgentConfig {
            api_key: "test".into(),
            base_url: "http://localhost/v1".into(),
            model: "mock".into(),
        });
        chats.active.insert(1, active(1));
        chats.active.insert(2, active(2));
        let mut chat = Chat {
            input: "question".into(),
            ..Default::default()
        };
        assert!(chats.start(3, &mut chat).unwrap_err().contains("busy"));
        assert!(!chat.running);
        assert!(chat.messages.is_empty());
    }
}
