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
        | Request::Overview { trace, .. } => *trace,
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
            if let Command::Open(path) = command {
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
