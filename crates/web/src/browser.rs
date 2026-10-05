//! Browser rendering handle, worker adapter, and origin-private trace storage.
use async_trait::async_trait;
use eframe::egui;
use js_sys::{Object, Reflect, Uint8Array};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use web_sys::{
    CustomEvent, CustomEventInit, DragEvent, Event, File, HtmlCanvasElement, HtmlInputElement,
    MessageEvent, Worker, WorkerOptions, WorkerType,
};
use xonata_app::ui::{Transport, Viewer};
use xonata_core::storage::{BackingStore, StoreError};
use xonata_core::{Engine, Event as CoreEvent, Request};

#[wasm_bindgen(inline_js = r#"
let storePromise;
async function holdSession(name) {
  if (!navigator.locks) return false;
  try {
    await new Promise((resolve) => {
      navigator.locks.request(name, () => {
        resolve();
        return new Promise(() => {});
      }).catch(() => resolve());
    });
    return true;
  } catch (_) { return false; }
}
async function collectOpfs(root) {
  if (!navigator.locks) return;
  try {
    await navigator.locks.request('xonata-gc', async () => {
      const held = new Set((await navigator.locks.query()).held.map((lock) => lock.name));
      for await (const [name] of root.entries()) {
        if (name.startsWith('xonata-') && !held.has(name)) await root.removeEntry(name, {recursive: true});
      }
    });
  } catch (_) { /* Current session remains usable without collection. */ }
}
async function collectIdb(db) {
  if (!navigator.locks) return;
  try {
    await navigator.locks.request('xonata-gc', async () => {
      const held = new Set((await navigator.locks.query()).held.map((lock) => lock.name));
      const {table, done} = transaction(db, 'readwrite');
      await new Promise((resolve, reject) => {
        const cursor = table.openKeyCursor();
        cursor.onsuccess = () => {
          const item = cursor.result;
          if (!item) return resolve();
          const session = String(item.key).split('/')[0];
          if (session.startsWith('xonata-') && !held.has(session)) table.delete(item.key);
          item.continue();
        };
        cursor.onerror = () => reject(cursor.error);
      });
      await done;
    });
  } catch (_) { /* Current session remains usable without collection. */ }
}
async function storage() {
  if (!storePromise) storePromise = (async () => {
    const name = 'xonata-' + crypto.randomUUID();
    await holdSession(name);
    try {
      if (navigator.storage?.getDirectory) {
        const root = await navigator.storage.getDirectory();
        const session = await root.getDirectoryHandle(name, {create: true});
        await collectOpfs(root);
        return {type: 'opfs', root, session};
      }
    } catch (_) { /* IndexedDB below */ }
    if (!globalThis.indexedDB) throw new Error('Browser temporary storage is unavailable');
    const db = await new Promise((resolve, reject) => {
      const req = indexedDB.open('xonata-pages', 1);
      req.onupgradeneeded = () => req.result.createObjectStore('pages');
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
    await collectIdb(db);
    return {type: 'idb', db, prefix: name + '/'};
  })();
  return storePromise;
}
function request(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}
function transaction(db, mode) {
  const tx = db.transaction('pages', mode);
  return {tx, table: tx.objectStore('pages'), done: new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve(); tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  })};
}
export async function pageRead(trace, key) {
  const s = await storage();
  if (s.type === 'opfs') {
    try {
      const dir = await s.session.getDirectoryHandle(String(trace));
      const handle = await dir.getFileHandle(key);
      return new Uint8Array(await (await handle.getFile()).arrayBuffer());
    } catch (error) { if (error.name === 'NotFoundError') return null; throw error; }
  }
  const {table, done} = transaction(s.db, 'readonly');
  const value = await request(table.get(s.prefix + trace + '/' + key));
  await done;
  return value ? new Uint8Array(value) : null;
}
export async function pageWrite(trace, key, bytes) {
  const s = await storage();
  if (s.type === 'opfs') {
    const dir = await s.session.getDirectoryHandle(String(trace), {create: true});
    const handle = await dir.getFileHandle(key, {create: true});
    const writer = await handle.createWritable();
    try { await writer.write(bytes); await writer.close(); }
    catch (error) { await writer.abort(); throw error; }
    return;
  }
  const {table, done} = transaction(s.db, 'readwrite');
  table.put(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength), s.prefix + trace + '/' + key);
  await done;
}
export async function pageRemove(trace, prefix) {
  const s = await storage();
  if (s.type === 'opfs') {
    if (!prefix) {
      try { await s.session.removeEntry(String(trace), {recursive: true}); }
      catch (error) { if (error.name !== 'NotFoundError') throw error; }
    } else {
      try {
        const dir = await s.session.getDirectoryHandle(String(trace));
        for await (const [name] of dir.entries()) if (name.startsWith(prefix)) await dir.removeEntry(name);
      } catch (error) { if (error.name !== 'NotFoundError') throw error; }
    }
    return;
  }
  const {table, done} = transaction(s.db, 'readwrite');
  const keyPrefix = s.prefix + trace + '/' + prefix;
  await new Promise((resolve, reject) => {
    const cursor = table.openKeyCursor();
    cursor.onsuccess = () => {
      const item = cursor.result;
      if (!item) return resolve();
      if (String(item.key).startsWith(keyPrefix)) table.delete(item.key);
      item.continue();
    };
    cursor.onerror = () => reject(cursor.error);
  });
  await done;
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = pageRead, catch)]
    async fn page_read(trace: u32, key: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = pageWrite, catch)]
    async fn page_write(trace: u32, key: &str, bytes: &Uint8Array) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = pageRemove, catch)]
    async fn page_remove(trace: u32, prefix: &str) -> Result<JsValue, JsValue>;
}
struct BrowserBacking;
#[async_trait(?Send)]
impl BackingStore for BrowserBacking {
    async fn read(&mut self, trace: u32, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let value = page_read(trace, key).await.map_err(js_storage_error)?;
        Ok((!value.is_null() && !value.is_undefined()).then(|| Uint8Array::new(&value).to_vec()))
    }
    async fn write(&mut self, trace: u32, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        page_write(trace, key, &Uint8Array::from(bytes))
            .await
            .map_err(js_storage_error)?;
        Ok(())
    }
    async fn remove(&mut self, trace: u32) -> Result<(), StoreError> {
        page_remove(trace, "").await.map_err(js_storage_error)?;
        Ok(())
    }
    async fn clear_filter(&mut self, trace: u32, generation: u32) -> Result<(), StoreError> {
        page_remove(trace, &format!("filter-{generation}-"))
            .await
            .map_err(js_storage_error)?;
        Ok(())
    }
    async fn clear_search(&mut self, trace: u32) -> Result<(), StoreError> {
        page_remove(trace, "search-")
            .await
            .map_err(js_storage_error)?;
        Ok(())
    }
}
fn js_storage_error(error: JsValue) -> StoreError {
    StoreError::Storage(format!("{error:?}"))
}

/// Browser worker wrapper called by the small JavaScript loader.
#[wasm_bindgen]
pub struct WorkerEngine {
    engine: Engine,
}
#[wasm_bindgen]
impl WorkerEngine {
    /// Creates one worker-owned engine.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            engine: Engine::new(Box::new(BrowserBacking)),
        }
    }
    /// Opens a trace.
    pub async fn open(&mut self, id: u32, name: String) -> Result<String, JsValue> {
        json(self.engine.open(id, name).await)
    }
    /// Appends bounded input bytes.
    pub async fn feed(&mut self, id: u32, bytes: Uint8Array) -> Result<String, JsValue> {
        json(self.engine.feed(id, &bytes.to_vec()).await)
    }
    /// Completes the input stream.
    pub async fn finish(&mut self, id: u32) -> Result<String, JsValue> {
        json(self.engine.finish(id).await)
    }
    /// Runs a viewport, overview, navigation, search, result, or close request.
    pub async fn request(&mut self, request: String) -> Result<String, JsValue> {
        let request: Request =
            serde_json::from_str(&request).map_err(|e| JsValue::from_str(&e.to_string()))?;
        match self
            .engine
            .request(request)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?
        {
            Some(event) => {
                serde_json::to_string(&event).map_err(|e| JsValue::from_str(&e.to_string()))
            }
            None => Ok(String::new()),
        }
    }
    /// Advances bounded search and overview batches.
    pub async fn tick(&mut self) -> Result<String, JsValue> {
        serde_json::to_string(
            &self
                .engine
                .tick()
                .await
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))
    }
    /// Whether more search or overview work remains.
    pub fn has_work(&self) -> bool {
        self.engine.has_search_work()
    }
}
impl Default for WorkerEngine {
    fn default() -> Self {
        Self::new()
    }
}
fn json(value: Result<CoreEvent, StoreError>) -> Result<String, JsValue> {
    serde_json::to_string(&value.map_err(|e| JsValue::from_str(&e.to_string()))?)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

#[derive(Clone)]
struct BrowserTransport {
    worker: Worker,
    canvas: HtmlCanvasElement,
    input: HtmlInputElement,
    events: Rc<RefCell<Vec<CoreEvent>>>,
    context: Rc<RefCell<Option<egui::Context>>>,
    next_id: Rc<Cell<u32>>,
    _message: Rc<Closure<dyn FnMut(MessageEvent)>>,
    _change: Rc<Closure<dyn FnMut(Event)>>,
    _dragover: Rc<Closure<dyn FnMut(DragEvent)>>,
    _drop: Rc<Closure<dyn FnMut(DragEvent)>>,
}
impl BrowserTransport {
    fn new(canvas: &HtmlCanvasElement) -> Result<Self, JsValue> {
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        let worker = Worker::new_with_options("./worker.js", &options)?;
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("No browser window"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("No browser document"))?;
        let input: HtmlInputElement = document.create_element("input")?.dyn_into()?;
        input.set_type("file");
        input.set_multiple(true);
        input.set_hidden(true);
        if let Some(body) = document.body() {
            body.append_child(&input)?;
        }
        let events = Rc::new(RefCell::new(Vec::new()));
        let context: Rc<RefCell<Option<egui::Context>>> = Rc::new(RefCell::new(None));
        let next_id = Rc::new(Cell::new(1u32));
        let queue = events.clone();
        let repaint = context.clone();
        let event_canvas = canvas.clone();
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |message: MessageEvent| {
            if let Some(text) = message.data().as_string()
                && let Ok(event) = serde_json::from_str::<CoreEvent>(&text)
            {
                let name = match event {
                    CoreEvent::Info { .. } => Some("xonata:trace"),
                    CoreEvent::SearchProgress { .. } => Some("xonata:progress"),
                    CoreEvent::Error { .. } => Some("xonata:error"),
                    CoreEvent::FilterProgress { .. } => Some("xonata:filter_progress"),
                    CoreEvent::FilterSuggestions { .. } => Some("xonata:filter_suggestions"),
                    CoreEvent::FilterResults { .. } => Some("xonata:filter_results"),
                    CoreEvent::FilterSortProgress { .. } => Some("xonata:filter_sort_progress"),
                    CoreEvent::SortedFilterResults { .. } => Some("xonata:filter_sorted_results"),
                    CoreEvent::FilterDrawn { .. } => Some("xonata:filter_drawn"),
                    CoreEvent::FilterDrawing { .. } => Some("xonata:filter_drawing"),
                    CoreEvent::FilterError { .. } => Some("xonata:filter_error"),
                    _ => None,
                };
                if let Some(name) = name {
                    dispatch(&event_canvas, name, &text);
                }
                queue.borrow_mut().push(event);
            }
            if let Some(ctx) = repaint.borrow().as_ref() {
                ctx.request_repaint();
            }
        });
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        let input_for_change = input.clone();
        let worker_change = worker.clone();
        let ids = next_id.clone();
        let change = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
            if let Some(files) = input_for_change.files() {
                for i in 0..files.length() {
                    if let Some(file) = files.get(i) {
                        send_file(&worker_change, &ids, file);
                    }
                }
            }
            input_for_change.set_value("");
        });
        input.set_onchange(Some(change.as_ref().unchecked_ref()));
        let dragover = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
            event.prevent_default();
        });
        canvas.add_event_listener_with_callback("dragover", dragover.as_ref().unchecked_ref())?;
        let worker_drop = worker.clone();
        let ids = next_id.clone();
        let drop = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
            event.prevent_default();
            if let Some(files) = event.data_transfer().and_then(|data| data.files()) {
                for i in 0..files.length() {
                    if let Some(file) = files.get(i) {
                        send_file(&worker_drop, &ids, file);
                    }
                }
            }
        });
        canvas.add_event_listener_with_callback("drop", drop.as_ref().unchecked_ref())?;
        Ok(Self {
            worker,
            canvas: canvas.clone(),
            input,
            events,
            context,
            next_id,
            _message: Rc::new(message),
            _change: Rc::new(change),
            _dragover: Rc::new(dragover),
            _drop: Rc::new(drop),
        })
    }
    fn set_context(&self, context: egui::Context) {
        *self.context.borrow_mut() = Some(context);
    }
    fn send(&self, kind: &str, value: JsValue) {
        let object = Object::new();
        let _ = Reflect::set(
            &object,
            &JsValue::from_str("type"),
            &JsValue::from_str(kind),
        );
        let _ = Reflect::set(&object, &JsValue::from_str("payload"), &value);
        let _ = self.worker.post_message(&object);
    }
}
impl Transport for BrowserTransport {
    fn open_dialog(&mut self) {
        self.input.click();
    }
    fn request(&mut self, request: Request) {
        if let Ok(text) = serde_json::to_string(&request) {
            self.send("request", JsValue::from_str(&text));
        }
    }
    fn poll(&mut self) -> Vec<CoreEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
    fn selection(&mut self, trace: u32, op: u64) {
        dispatch(
            &self.canvas,
            "xonata:selection",
            &format!("{{\"trace\":{trace},\"op_id\":\"{op}\"}}"),
        );
    }
    fn reduced_motion(&self) -> bool {
        web_sys::window()
            .and_then(|window| {
                window
                    .match_media("(prefers-reduced-motion: reduce)")
                    .ok()
                    .flatten()
            })
            .is_some_and(|media| media.matches())
    }
}
fn dispatch(canvas: &HtmlCanvasElement, name: &str, text: &str) {
    let init = CustomEventInit::new();
    init.set_detail(&JsValue::from_str(text));
    if let Ok(event) = CustomEvent::new_with_event_init_dict(name, &init) {
        let _ = canvas.dispatch_event(&event);
    }
}
fn send_file(worker: &Worker, next: &Cell<u32>, file: File) {
    let id = next.get();
    next.set(id.saturating_add(1));
    let object = Object::new();
    let _ = Reflect::set(
        &object,
        &JsValue::from_str("type"),
        &JsValue::from_str("open"),
    );
    let _ = Reflect::set(
        &object,
        &JsValue::from_str("id"),
        &JsValue::from_f64(id as f64),
    );
    let _ = Reflect::set(&object, &JsValue::from_str("file"), &file);
    let _ = worker.post_message(&object);
}

/// Handle for embedding Xonata in a browser page.
#[wasm_bindgen]
pub struct WebHandle {
    runner: eframe::WebRunner,
    transport: RefCell<Option<BrowserTransport>>,
}
#[wasm_bindgen]
impl WebHandle {
    /// Creates a mountable viewer.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            runner: eframe::WebRunner::new(),
            transport: RefCell::new(None),
        }
    }
    /// Mounts the viewer on a host-provided canvas.
    pub async fn start(&self, canvas: HtmlCanvasElement) -> Result<(), JsValue> {
        let transport = BrowserTransport::new(&canvas)?;
        let for_app = transport.clone();
        self.runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |cc| {
                    for_app.set_context(cc.egui_ctx.clone());
                    Ok(Box::new(Viewer::new(cc, Box::new(for_app.clone()))))
                }),
            )
            .await?;
        *self.transport.borrow_mut() = Some(transport);
        Ok(())
    }
    /// Opens a host-provided File object.
    pub fn open_file(&self, file: File) -> Result<(), JsValue> {
        let borrow = self.transport.borrow();
        let transport = borrow
            .as_ref()
            .ok_or_else(|| JsValue::from_str("Viewer is not mounted"))?;
        send_file(&transport.worker, &transport.next_id, file);
        Ok(())
    }
    /// Jumps to a decimal operation ID without losing 64-bit precision.
    pub fn navigate(&self, trace: u32, decimal_id: String) -> Result<(), JsValue> {
        let id = decimal_id
            .parse::<u64>()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let borrow = self.transport.borrow();
        let transport = borrow
            .as_ref()
            .ok_or_else(|| JsValue::from_str("Viewer is not mounted"))?;
        let request = Request::Jump {
            trace,
            id,
            retired: false,
        };
        let text =
            serde_json::to_string(&request).map_err(|e| JsValue::from_str(&e.to_string()))?;
        transport.send("request", JsValue::from_str(&text));
        Ok(())
    }
    /// Tears down the browser application and worker.
    pub fn destroy(&self) {
        self.runner.destroy();
        if let Some(transport) = self.transport.borrow_mut().take() {
            transport.worker.set_onmessage(None);
            transport.input.set_onchange(None);
            let _ = transport.canvas.remove_event_listener_with_callback(
                "dragover",
                transport._dragover.as_ref().as_ref().unchecked_ref(),
            );
            let _ = transport.canvas.remove_event_listener_with_callback(
                "drop",
                transport._drop.as_ref().as_ref().unchecked_ref(),
            );
            transport.worker.terminate();
            transport.input.remove();
        }
    }
}
impl Default for WebHandle {
    fn default() -> Self {
        Self::new()
    }
}
