//! Native file loading and worker transport.
use crate::ui::Transport;
use egui::Context;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};
use xonata_core::storage::NativeBacking;
use xonata_core::{Engine, Event, Request};

enum Command {
    Open(PathBuf),
    Request(Request),
    Analysis(xonata_core::AnalysisRequest, crate::chat::AnalysisReply),
}

/// Native worker handle; the worker owns all trace data.
pub struct NativeTransport {
    commands: Sender<Command>,
    events: Receiver<Event>,
}

impl NativeTransport {
    /// Starts one background worker and a bounded viewport protocol.
    pub fn new(context: Context) -> Self {
        let (commands, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        std::thread::spawn(move || worker(incoming, outgoing, context));
        Self { commands, events }
    }
}

impl Transport for NativeTransport {
    fn open_dialog(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Kanata traces",
                &["log", "txt", "kanata", "gz", "zst", "zstd"],
            )
            .pick_files()
        {
            for path in paths {
                let _ = self.commands.send(Command::Open(path));
            }
        }
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            let _ = self.commands.send(Command::Open(path));
        }
    }

    fn request(&mut self, request: Request) {
        let _ = self.commands.send(Command::Request(request));
    }

    fn analysis(
        &mut self,
        request: xonata_core::AnalysisRequest,
        reply: crate::chat::AnalysisReply,
    ) {
        if let Err(error) = self.commands.send(Command::Analysis(request, reply))
            && let Command::Analysis(_, reply) = error.0
        {
            let _ = reply.send(Err("Trace worker closed".into()));
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        self.events.try_iter().take(256).collect()
    }
}

fn emit(tx: &Sender<Event>, ctx: &Context, event: Event) {
    let _ = tx.send(event);
    ctx.request_repaint();
}

fn worker(rx: Receiver<Command>, tx: Sender<Event>, ctx: Context) {
    let Ok(backing) = NativeBacking::new() else {
        emit(
            &tx,
            &ctx,
            Event::Error {
                trace: 0,
                message: "Cannot create temporary trace storage".into(),
            },
        );
        return;
    };
    let mut engine = Engine::new(Box::new(backing));
    let mut next_id = 1u32;
    let mut files: VecDeque<PathBuf> = VecDeque::new();
    loop {
        if let Some(path) = files.pop_front() {
            let id = next_id;
            next_id = next_id.saturating_add(1);
            let name = path
                .file_name()
                .map_or_else(|| "trace".into(), |s| s.to_string_lossy().into_owned());
            if let Ok(event) = futures_lite::future::block_on(engine.open(id, name)) {
                emit(&tx, &ctx, event);
            }
            if let Err(error) = load_file(&path, id, &mut engine, &rx, &tx, &ctx, &mut files) {
                emit(
                    &tx,
                    &ctx,
                    Event::Error {
                        trace: id,
                        message: error,
                    },
                );
            }
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(3)) {
            Ok(Command::Open(path)) => files.push_back(path),
            Ok(Command::Analysis(request, reply)) => {
                let result = futures_lite::future::block_on(engine.analysis_request(request))
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
                ctx.request_repaint();
            }
            Ok(Command::Request(request)) => {
                let id = request_trace(&request);
                match futures_lite::future::block_on(engine.request(request)) {
                    Ok(Some(event)) => emit(&tx, &ctx, event),
                    Ok(None) => {}
                    Err(error) => emit(
                        &tx,
                        &ctx,
                        Event::Error {
                            trace: id,
                            message: error.to_string(),
                        },
                    ),
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if engine.has_search_work() {
            match futures_lite::future::block_on(engine.tick()) {
                Ok(events) => {
                    for event in events {
                        emit(&tx, &ctx, event);
                    }
                }
                Err(error) => emit(
                    &tx,
                    &ctx,
                    Event::Error {
                        trace: 0,
                        message: error.to_string(),
                    },
                ),
            }
        }
    }
}

fn request_trace(request: &Request) -> u32 {
    match request {
        Request::Close { trace }
        | Request::View { trace, .. }
        | Request::Search { trace, .. }
        | Request::CancelSearch { trace }
        | Request::Results { trace, .. }
        | Request::Jump { trace, .. }
        | Request::JumpRow { trace, .. }
        | Request::Overview { trace, .. }
        | Request::Filter { trace, .. }
        | Request::FilterSuggestions { trace, .. }
        | Request::CancelFilter { trace, .. }
        | Request::FilterResults { trace, .. }
        | Request::FilterSort { trace, .. }
        | Request::SortedFilterResults { trace, .. }
        | Request::RevealFilter { trace, .. }
        | Request::DrawFilter { trace, .. }
        | Request::ClearFilterDrawing { trace }
        | Request::FilterViewport { trace, .. } => *trace,
    }
}

fn load_file(
    path: &PathBuf,
    id: u32,
    engine: &mut Engine,
    rx: &Receiver<Command>,
    tx: &Sender<Event>,
    ctx: &Context,
    files: &mut VecDeque<PathBuf>,
) -> Result<(), String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut data = vec![0u8; 64 * 1024];
    loop {
        while let Ok(command) = rx.try_recv() {
            if let Command::Analysis(request, reply) = command {
                let result = futures_lite::future::block_on(engine.analysis_request(request))
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            } else if let Command::Open(path) = command {
                files.push_back(path);
            } else if let Command::Request(request) = command {
                if matches!(request, Request::Close { trace } if trace == id) {
                    let _ = futures_lite::future::block_on(engine.close(id));
                    return Ok(());
                }
                match futures_lite::future::block_on(engine.request(request)) {
                    Ok(Some(event)) => emit(tx, ctx, event),
                    Ok(None) => {}
                    Err(error) => emit(
                        tx,
                        ctx,
                        Event::Error {
                            trace: id,
                            message: error.to_string(),
                        },
                    ),
                }
            }
        }
        if engine.has_search_work() {
            for event in futures_lite::future::block_on(engine.tick()).map_err(|e| e.to_string())? {
                emit(tx, ctx, event);
            }
        }
        let n = file.read(&mut data).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let event = futures_lite::future::block_on(engine.feed(id, &data[..n]))
            .map_err(|e| e.to_string())?;
        if engine
            .info(id)
            .is_some_and(|info| info.bytes % (1024 * 1024) < n as u64)
        {
            emit(tx, ctx, event);
        }
    }
    emit(
        tx,
        ctx,
        futures_lite::future::block_on(engine.finish(id)).map_err(|e| e.to_string())?,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_analysis_round_trips_without_consuming_viewer_events() {
        let mut transport = NativeTransport::new(Context::default());
        transport.open_paths(vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../core/tests/fixtures/compressed.kanata"),
        ]);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if transport
                .poll()
                .iter()
                .any(|e| matches!(e,Event::Info{info} if info.complete))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native worker did not load fixture"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let (reply, receive) = futures::channel::oneshot::channel();
        transport.analysis(
            xonata_core::AnalysisRequest {
                trace: 1,
                session: 1,
                request: 1,
                action: xonata_core::AnalysisAction::Rows { start: 0, count: 1 },
            },
            reply,
        );
        let value = futures_lite::future::block_on(receive).unwrap().unwrap();
        assert_eq!(value["rows"].as_array().unwrap().len(), 1);
        transport.request(Request::View {
            trace: 1,
            generation: 99,
            start: 0,
            stride: 1,
            count: 1,
            hide_flushed: false,
        });
        loop {
            if transport
                .poll()
                .iter()
                .any(|e| matches!(e, Event::View { generation: 99, .. }))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "analysis stole a viewer reply"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
