use super::*;
use futures::StreamExt;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn answer(message: Value, reason: &str) -> Value {
    json!({"id":"mock","object":"chat.completion","created":0,"model":"mock","choices":[{"index":0,"message":message,"finish_reason":reason}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})
}
type RecordedRequests = Arc<Mutex<Vec<Value>>>;

fn provider(
    responses: Vec<(u16, Value)>,
) -> (AgentConfig, RecordedRequests, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = AgentConfig {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        api_key: "test-secret".into(),
        model: "mock".into(),
    };
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let records = recorded.clone();
    let thread = std::thread::spawn(move || {
        for (status, response) in responses {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "mock model request timed out");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            let (header_end, length) = loop {
                let n = socket.read(&mut chunk).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..pos]);
                    assert!(
                        headers.starts_with("POST /v1/chat/completions "),
                        "{headers}"
                    );
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (pos + 4, length);
                }
            };
            while bytes.len() < header_end + length {
                let n = socket.read(&mut chunk).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&chunk[..n]);
            }
            records
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap());
            let body = response.to_string();
            write!(socket,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    (config, recorded, thread)
}

async fn turn(config: AgentConfig, history: Vec<Message>) -> Result<String, AgentError> {
    observed_turn(config, history).await.0
}

async fn observed_turn(
    config: AgentConfig,
    history: Vec<Message>,
) -> (Result<String, AgentError>, Vec<ActivityEvent>) {
    let (sender, mut requests) = trace_channel();
    let client = TraceClient::new(sender, 7, 42);
    let (activity, activity_rx) = mpsc::channel(8);
    let (_cancel, registration) = futures::future::AbortHandle::new_pair();
    let task = run(config, client, history, activity, registration);
    let host = async {
        while let Some(request) = requests.next().await {
            assert_eq!(request.request.trace, 7);
            assert_eq!(request.request.session, 42);
            assert!(matches!(request.request.action, AnalysisAction::Info));
            let _ = request
                .reply
                .send(Ok(json!({"count":"9007199254740993","complete":true})));
        }
    };
    let (result, (), events) = futures::join!(task, host, activity_rx.collect());
    (result, events)
}

#[test]
fn compatible_model_executes_tools_and_receives_role_aware_followup_context() {
    let (config, recorded, server) = provider(vec![
        (
            200,
            answer(
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"trace_info","arguments":"{}"}}]}),
                "tool_calls",
            ),
        ),
        (
            200,
            answer(
                json!({"role":"assistant","content":"Observed count 9007199254740993."}),
                "stop",
            ),
        ),
        (
            200,
            answer(
                json!({"role":"assistant","content":"That count came from trace_info."}),
                "stop",
            ),
        ),
    ]);
    runtime().block_on(async {
        let (first, activity) = observed_turn(
            config.clone(),
            vec![Message {
                role: Role::User,
                text: "Count instructions".into(),
            }],
        )
        .await;
        let first = first.unwrap();
        assert_eq!(activity.len(), 6);
        assert!(matches!(&activity[0], ActivityEvent::Started { turn: 1, tool: None, .. }));
        assert!(matches!(&activity[2], ActivityEvent::Started { turn: 1, tool: Some((name, args)), .. }
            if name == "trace_info" && args == "{}"));
        assert!(matches!(&activity[3], ActivityEvent::Finished { failed: false, output: Some(text), .. }
            if text.contains("9007199254740993")));
        assert!(matches!(&activity[4], ActivityEvent::Started { turn: 2, tool: None, .. }));
        for pair in activity.as_chunks::<2>().0 {
            assert!(matches!((&pair[0], &pair[1]),
                (ActivityEvent::Started { id: start, .. }, ActivityEvent::Finished { id: end, failed: false, .. }) if start == end));
        }
        let second = turn(
            config,
            vec![
                Message {
                    role: Role::User,
                    text: "Count instructions".into(),
                },
                Message {
                    role: Role::Assistant,
                    text: first,
                },
                Message {
                    role: Role::User,
                    text: "Where did that count come from?".into(),
                },
            ],
        )
        .await
        .unwrap();
        assert_eq!(second, "That count came from trace_info.");
    });
    server.join().unwrap();
    let requests = recorded.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests.iter() {
        let messages = request["messages"].to_string();
        assert!(
            !messages.contains(
                "https://github.com/shioyadan/Konata/blob/master/docs/kanata-log-format.md"
            ),
            "Unused specifications must not enter model context"
        );
    }
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["content"].to_string().contains("9007199254740993"))
    );
    let messages = requests[2]["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(
            |m| m["role"] == "assistant" && m["content"].to_string().contains("Observed count")
        )
    );
    assert_eq!(requests[0]["model"], "mock");
}

#[test]
fn specifications_are_read_on_demand_without_retaining_pages_in_followups() {
    let (config, recorded, server) = provider(vec![
        (
            200,
            answer(
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"spec-1","type":"function","function":{"name":"read_specification","arguments":"{\"id\":\"kanata-trace\"}"}}]}),
                "tool_calls",
            ),
        ),
        (
            200,
            answer(
                json!({"role":"assistant","content":"Kanata uses tab-delimited records."}),
                "stop",
            ),
        ),
        (
            200,
            answer(
                json!({"role":"assistant","content":"The specification describes the format."}),
                "stop",
            ),
        ),
    ]);
    runtime().block_on(async {
        let question = Message {
            role: Role::User,
            text: "Explain the Kanata format".into(),
        };
        let first = turn(config.clone(), vec![question.clone()]).await.unwrap();
        turn(
            config,
            vec![
                question,
                Message {
                    role: Role::Assistant,
                    text: first,
                },
                Message {
                    role: Role::User,
                    text: "Continue".into(),
                },
            ],
        )
        .await
        .unwrap();
    });
    server.join().unwrap();
    let requests = recorded.lock().unwrap();
    let source = "https://github.com/shioyadan/Konata/blob/master/docs/kanata-log-format.md";
    assert!(!requests[0]["messages"].to_string().contains(source));
    let tools = requests[0]["tools"].to_string();
    assert!(tools.contains("read_specification") && tools.contains("Kanata trace"));
    assert!(!tools.contains(source));
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["content"].to_string().contains(source))
    );
    assert!(!requests[2]["messages"].to_string().contains(source));
}

#[test]
fn failed_tools_should_report_arguments_and_the_returned_error() {
    let (config, _, server) = provider(vec![
        (
            200,
            answer(
                json!({"role":"assistant","content":null,"tool_calls":[{
                    "id":"bad-spec","type":"function","function":{
                        "name":"read_specification","arguments":"{\"id\":\"unknown\"}"
                    }
                }]}),
                "tool_calls",
            ),
        ),
        (
            200,
            answer(
                json!({"role":"assistant","content":"The specification was not found."}),
                "stop",
            ),
        ),
    ]);
    let (result, activity) = runtime().block_on(observed_turn(
        config,
        vec![Message {
            role: Role::User,
            text: "Read a specification".into(),
        }],
    ));
    assert!(result.is_ok());
    assert!(activity.iter().any(|event| matches!(event,
        ActivityEvent::Started { tool: Some((name, args)), .. }
            if name == "read_specification" && args.contains("unknown"))));
    assert!(activity.iter().any(|event| matches!(event,
        ActivityEvent::Finished { failed: true, output: Some(output), .. }
            if output.contains("Unknown specification ID"))));
    server.join().unwrap();
}

#[test]
fn provider_errors_are_terminal() {
    let (config, _, server) = provider(vec![(
        401,
        json!({"error":{"message":"Invalid credentials","type":"authentication_error"}}),
    )]);
    let (result, activity) = runtime().block_on(observed_turn(
        config,
        vec![Message {
            role: Role::User,
            text: "Count".into(),
        }],
    ));
    assert!(matches!(result, Err(AgentError::Failed(_))));
    assert!(matches!(
        activity.last(),
        Some(ActivityEvent::Finished { failed: true, .. })
    ));
    server.join().unwrap();
}

#[test]
fn cancelled_or_expired_runs_terminate_while_waiting_for_a_tool() {
    runtime().block_on(async {
        let (cancel, registration) = futures::future::AbortHandle::new_pair();
        cancel.abort();
        assert!(matches!(
            bounded::<()>(futures::future::pending(), registration, 1000).await,
            Err(AgentError::Cancelled)
        ));
        let (_, registration) = futures::future::AbortHandle::new_pair();
        assert!(matches!(
            bounded::<()>(futures::future::pending(), registration, 1).await,
            Err(AgentError::Timeout)
        ));
    });
}

#[test]
fn history_limits_remove_complete_exchanges() {
    let mut history = Vec::new();
    for i in 0..40 {
        history.push(Message {
            role: if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            text: format!("{i}:{}", "x".repeat(4096)),
        });
    }
    trim_history(&mut history);
    assert_eq!(history.first().unwrap().role, Role::User);
    assert!(history.len() <= MAX_MESSAGES);
    assert!(history.iter().map(|m| m.text.len()).sum::<usize>() <= MAX_HISTORY_BYTES);
    assert!(history.last().unwrap().text.starts_with("39:"));
}
