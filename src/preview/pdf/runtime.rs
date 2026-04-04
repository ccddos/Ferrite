use eframe::egui;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::object::dict::keys::{A, DEST, FIRST, NEXT, OUTLINES, TITLE};
use hayro::hayro_syntax::object::{Dict, ObjRef, Object, ObjectIdentifier, String as PdfString};
use hayro::hayro_syntax::Pdf;
use hayro::{render, RenderSettings};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use super::model::PdfOutlineItem;

const WORKER_COUNT: usize = 2;
const THUMBNAIL_ZOOM_BUCKET: u16 = 20;
const IDLE_WORKER_SLEEP: Duration = Duration::from_millis(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PdfRenderKind {
    VisiblePage,
    PrefetchPage,
    Thumbnail,
}

#[derive(Debug, Clone)]
pub struct PdfRenderRequest {
    pub kind: PdfRenderKind,
    pub path: PathBuf,
    pub page_index: usize,
    pub zoom_bucket: u16,
}

impl PdfRenderRequest {
    pub fn visible(path: PathBuf, page_index: usize, zoom_bucket: u16) -> Self {
        Self {
            kind: PdfRenderKind::VisiblePage,
            path,
            page_index,
            zoom_bucket,
        }
    }

    pub fn prefetch(path: PathBuf, page_index: usize, zoom_bucket: u16) -> Self {
        Self {
            kind: PdfRenderKind::PrefetchPage,
            path,
            page_index,
            zoom_bucket,
        }
    }

    pub fn thumbnail(path: PathBuf, page_index: usize, zoom_bucket: u16) -> Self {
        Self {
            kind: PdfRenderKind::Thumbnail,
            path,
            page_index,
            zoom_bucket,
        }
    }
}

pub fn zoom_bucket(zoom: f32) -> u16 {
    ((zoom * 100.0).round() as i32).clamp(50, 400) as u16
}

pub(crate) fn thumbnail_zoom_bucket() -> u16 {
    THUMBNAIL_ZOOM_BUCKET
}

fn kind_rank(kind: PdfRenderKind) -> u8 {
    match kind {
        PdfRenderKind::VisiblePage => 0,
        PdfRenderKind::PrefetchPage => 1,
        PdfRenderKind::Thumbnail => 2,
    }
}

#[derive(Debug, Clone)]
struct QueuedRequest {
    rank: u8,
    sequence: u64,
    request: PdfRenderRequest,
}

impl QueuedRequest {
    fn new(rank: u8, sequence: u64, request: PdfRenderRequest) -> Self {
        Self {
            rank,
            sequence,
            request,
        }
    }
}

impl PartialEq for QueuedRequest {
    fn eq(&self, other: &Self) -> bool {
        self.rank == other.rank && self.sequence == other.sequence
    }
}

impl Eq for QueuedRequest {}

impl PartialOrd for QueuedRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .rank
            .cmp(&self.rank)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

#[derive(Default)]
pub struct PdfRequestQueue {
    heap: BinaryHeap<QueuedRequest>,
    next_sequence: u64,
}

impl PdfRequestQueue {
    pub fn push(&mut self, req: PdfRenderRequest) {
        let queued = QueuedRequest::new(kind_rank(req.kind), self.next_sequence, req);
        self.heap.push(queued);
        self.next_sequence += 1;
    }

    pub fn pop(&mut self) -> Option<PdfRenderRequest> {
        self.heap.pop().map(|queued| queued.request)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageRenderKey {
    pub path: PathBuf,
    pub page_index: usize,
    pub zoom_bucket: u16,
}

impl PageRenderKey {
    pub fn new(path: &Path, page_index: usize, zoom_bucket: u16) -> Self {
        Self {
            path: path.to_path_buf(),
            page_index,
            zoom_bucket,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderedPdfBitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

pub struct PdfPageCache {
    page_limit: usize,
    thumbnail_limit: usize,
    pages: HashMap<PageRenderKey, RenderedPdfBitmap>,
    page_order: VecDeque<PageRenderKey>,
    thumbnails: HashMap<PageRenderKey, RenderedPdfBitmap>,
    thumbnail_order: VecDeque<PageRenderKey>,
}

impl PdfPageCache {
    pub fn new(page_limit: usize, thumbnail_limit: usize) -> Self {
        Self {
            page_limit,
            thumbnail_limit,
            pages: HashMap::new(),
            page_order: VecDeque::new(),
            thumbnails: HashMap::new(),
            thumbnail_order: VecDeque::new(),
        }
    }

    pub fn insert_page(&mut self, key: PageRenderKey, bitmap: RenderedPdfBitmap) {
        self.pages.insert(key.clone(), bitmap);
        touch_lru(&mut self.page_order, &key);
        self.evict_after_page_insert();
    }

    pub fn insert_thumbnail(&mut self, key: PageRenderKey, bitmap: RenderedPdfBitmap) {
        self.thumbnails.insert(key.clone(), bitmap);
        touch_lru(&mut self.thumbnail_order, &key);
        while self.thumbnails.len() > self.thumbnail_limit {
            if let Some(oldest) = self.thumbnail_order.pop_front() {
                self.thumbnails.remove(&oldest);
            }
        }
        self.evict_to_total_budget();
    }

    pub fn get_page(&mut self, key: &PageRenderKey) -> Option<RenderedPdfBitmap> {
        let bitmap = self.pages.get(key)?.clone();
        touch_lru(&mut self.page_order, key);
        Some(bitmap)
    }

    pub fn get_thumbnail(&mut self, key: &PageRenderKey) -> Option<RenderedPdfBitmap> {
        let bitmap = self.thumbnails.get(key)?.clone();
        touch_lru(&mut self.thumbnail_order, key);
        Some(bitmap)
    }

    pub fn page_dimensions(&self, key: &PageRenderKey) -> Option<(f32, f32)> {
        self.pages
            .get(key)
            .or_else(|| self.thumbnails.get(key))
            .map(|bitmap| (bitmap.width as f32, bitmap.height as f32))
    }

    fn evict_after_page_insert(&mut self) {
        if self.total_entries() <= self.page_limit + self.thumbnail_limit {
            return;
        }

        if !self.thumbnails.is_empty() {
            if let Some(oldest) = self.thumbnail_order.pop_front() {
                self.thumbnails.remove(&oldest);
            }
        } else {
            while self.pages.len() > self.page_limit {
                if let Some(oldest) = self.page_order.pop_front() {
                    self.pages.remove(&oldest);
                } else {
                    break;
                }
            }
        }

        self.evict_to_total_budget();
    }

    fn evict_to_total_budget(&mut self) {
        while self.total_entries() > self.page_limit + self.thumbnail_limit {
            if let Some(oldest) = self.thumbnail_order.pop_front() {
                self.thumbnails.remove(&oldest);
                continue;
            }

            if let Some(oldest) = self.page_order.pop_front() {
                self.pages.remove(&oldest);
                continue;
            }

            break;
        }
    }

    fn total_entries(&self) -> usize {
        self.pages.len() + self.thumbnails.len()
    }
}

fn touch_lru(order: &mut VecDeque<PageRenderKey>, key: &PageRenderKey) {
    if let Some(pos) = order.iter().position(|existing| existing == key) {
        order.remove(pos);
    }
    order.push_back(key.clone());
}

#[derive(Debug, Clone)]
pub(crate) struct PdfDocumentMetadata {
    pub total_pages: usize,
    pub outline: Vec<PdfOutlineItem>,
}

#[derive(Debug, Clone)]
pub(crate) enum PdfDocumentLoadState {
    Loading,
    Loaded(PdfDocumentMetadata),
    Failed(String),
}

#[derive(Debug, Clone)]
enum PdfWorkerResponse {
    DocumentLoaded {
        path: PathBuf,
        result: Result<PdfDocumentMetadata, String>,
    },
    PageRendered {
        kind: PdfRenderKind,
        key: PageRenderKey,
        result: Result<RenderedPdfBitmap, String>,
    },
}

struct WorkerPdfDocument {
    pdf: Pdf,
    metadata: PdfDocumentMetadata,
}

static PDF_RUNTIME: OnceLock<Mutex<PdfRuntimeCore>> = OnceLock::new();

pub(crate) fn with_pdf_runtime<R>(
    ctx: &egui::Context,
    f: impl FnOnce(&mut PdfRuntimeCore) -> R,
) -> R {
    let runtime = PDF_RUNTIME.get_or_init(|| Mutex::new(PdfRuntimeCore::new(ctx)));
    let mut guard = runtime.lock().expect("pdf runtime mutex poisoned");
    f(&mut guard)
}

pub(crate) struct PdfRuntimeCore {
    document_tx: Sender<PathBuf>,
    visible_tx: Sender<PdfRenderRequest>,
    prefetch_tx: Sender<PdfRenderRequest>,
    thumbnail_tx: Sender<PdfRenderRequest>,
    response_rx: Receiver<PdfWorkerResponse>,
    documents: HashMap<PathBuf, PdfDocumentLoadState>,
    page_cache: PdfPageCache,
    in_flight_docs: HashSet<PathBuf>,
    in_flight_pages: HashSet<PageRenderKey>,
    page_errors: HashMap<PageRenderKey, String>,
}

impl PdfRuntimeCore {
    fn new(ctx: &egui::Context) -> Self {
        let (document_tx, document_rx) = mpsc::channel();
        let (visible_tx, visible_rx) = mpsc::channel();
        let (prefetch_tx, prefetch_rx) = mpsc::channel();
        let (thumbnail_tx, thumbnail_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();

        spawn_workers(
            ctx.clone(),
            Arc::new(Mutex::new(document_rx)),
            Arc::new(Mutex::new(visible_rx)),
            Arc::new(Mutex::new(prefetch_rx)),
            Arc::new(Mutex::new(thumbnail_rx)),
            response_tx,
        );

        Self {
            document_tx,
            visible_tx,
            prefetch_tx,
            thumbnail_tx,
            response_rx,
            documents: HashMap::new(),
            page_cache: PdfPageCache::new(8, 12),
            in_flight_docs: HashSet::new(),
            in_flight_pages: HashSet::new(),
            page_errors: HashMap::new(),
        }
    }

    pub(crate) fn document_state(&mut self, path: &Path) -> PdfDocumentLoadState {
        self.drain_responses();
        let key = path.to_path_buf();
        if !self.documents.contains_key(&key) && !self.in_flight_docs.contains(&key) {
            let _ = self.document_tx.send(key.clone());
            self.in_flight_docs.insert(key.clone());
            self.documents.insert(key.clone(), PdfDocumentLoadState::Loading);
        }

        self.documents
            .get(&key)
            .cloned()
            .unwrap_or(PdfDocumentLoadState::Loading)
    }

    pub(crate) fn request_visible_range(
        &mut self,
        path: &Path,
        zoom: f32,
        visible_pages: impl IntoIterator<Item = usize>,
        prefetch_pages: impl IntoIterator<Item = usize>,
    ) {
        self.drain_responses();
        self.document_state(path);

        let zoom_bucket = zoom_bucket(zoom);
        for page_index in visible_pages {
            self.enqueue_page(PdfRenderRequest::visible(
                path.to_path_buf(),
                page_index,
                zoom_bucket,
            ));
        }
        for page_index in prefetch_pages {
            self.enqueue_page(PdfRenderRequest::prefetch(
                path.to_path_buf(),
                page_index,
                zoom_bucket,
            ));
        }
    }

    pub(crate) fn request_thumbnail_range(
        &mut self,
        path: &Path,
        pages: impl IntoIterator<Item = usize>,
    ) {
        self.drain_responses();
        self.document_state(path);

        for page_index in pages {
            self.enqueue_page(PdfRenderRequest::thumbnail(
                path.to_path_buf(),
                page_index,
                thumbnail_zoom_bucket(),
            ));
        }
    }

    pub(crate) fn page_bitmap(&mut self, key: &PageRenderKey) -> Option<RenderedPdfBitmap> {
        self.drain_responses();
        if key.zoom_bucket == thumbnail_zoom_bucket() {
            self.page_cache.get_thumbnail(key)
        } else {
            self.page_cache.get_page(key)
        }
    }

    pub(crate) fn page_error(&mut self, key: &PageRenderKey) -> Option<String> {
        self.drain_responses();
        self.page_errors.get(key).cloned()
    }

    pub(crate) fn page_dimensions(&self, key: &PageRenderKey) -> Option<(f32, f32)> {
        self.page_cache.page_dimensions(key)
    }

    fn enqueue_page(&mut self, request: PdfRenderRequest) {
        let key = PageRenderKey::new(&request.path, request.page_index, request.zoom_bucket);
        if self.in_flight_pages.contains(&key)
            || self.page_errors.contains_key(&key)
            || self.page_cache.page_dimensions(&key).is_some()
        {
            return;
        }

        let sender = match request.kind {
            PdfRenderKind::VisiblePage => &self.visible_tx,
            PdfRenderKind::PrefetchPage => &self.prefetch_tx,
            PdfRenderKind::Thumbnail => &self.thumbnail_tx,
        };

        if sender.send(request).is_ok() {
            self.in_flight_pages.insert(key);
        }
    }

    fn drain_responses(&mut self) {
        loop {
            match self.response_rx.try_recv() {
                Ok(PdfWorkerResponse::DocumentLoaded { path, result }) => {
                    self.in_flight_docs.remove(&path);
                    let next = match result {
                        Ok(metadata) => PdfDocumentLoadState::Loaded(metadata),
                        Err(msg) => PdfDocumentLoadState::Failed(msg),
                    };
                    self.documents.insert(path, next);
                }
                Ok(PdfWorkerResponse::PageRendered { kind, key, result }) => {
                    self.in_flight_pages.remove(&key);
                    match result {
                        Ok(bitmap) => {
                            self.page_errors.remove(&key);
                            match kind {
                                PdfRenderKind::Thumbnail => {
                                    self.page_cache.insert_thumbnail(key, bitmap);
                                }
                                PdfRenderKind::VisiblePage | PdfRenderKind::PrefetchPage => {
                                    self.page_cache.insert_page(key, bitmap);
                                }
                            }
                        }
                        Err(msg) => {
                            self.page_errors.insert(key, msg);
                        }
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }
}

fn spawn_workers(
    repaint_ctx: egui::Context,
    document_rx: Arc<Mutex<Receiver<PathBuf>>>,
    visible_rx: Arc<Mutex<Receiver<PdfRenderRequest>>>,
    prefetch_rx: Arc<Mutex<Receiver<PdfRenderRequest>>>,
    thumbnail_rx: Arc<Mutex<Receiver<PdfRenderRequest>>>,
    response_tx: Sender<PdfWorkerResponse>,
) {
    for worker_index in 0..WORKER_COUNT {
        let document_rx = Arc::clone(&document_rx);
        let visible_rx = Arc::clone(&visible_rx);
        let prefetch_rx = Arc::clone(&prefetch_rx);
        let thumbnail_rx = Arc::clone(&thumbnail_rx);
        let response_tx = response_tx.clone();
        let repaint_ctx = repaint_ctx.clone();

        thread::Builder::new()
            .name(format!("ferrite-pdf-worker-{worker_index}"))
            .spawn(move || {
                let mut cache = HashMap::<PathBuf, Arc<WorkerPdfDocument>>::new();
                loop {
                    if let Some(path) = try_recv_locked(&document_rx) {
                        let result = load_worker_document(&path, &mut cache).map(|doc| doc.metadata.clone());
                        let _ = response_tx.send(PdfWorkerResponse::DocumentLoaded { path, result });
                        repaint_ctx.request_repaint();
                        continue;
                    }

                    if let Some(request) = try_recv_locked(&visible_rx)
                        .or_else(|| try_recv_locked(&prefetch_rx))
                        .or_else(|| try_recv_locked(&thumbnail_rx))
                    {
                        let key = PageRenderKey::new(&request.path, request.page_index, request.zoom_bucket);
                        let result = load_worker_document(&request.path, &mut cache)
                            .and_then(|doc| render_pdf_page_bitmap(&key, &doc));
                        let _ = response_tx.send(PdfWorkerResponse::PageRendered {
                            kind: request.kind,
                            key,
                            result,
                        });
                        repaint_ctx.request_repaint();
                        continue;
                    }

                    thread::sleep(IDLE_WORKER_SLEEP);
                }
            })
            .expect("Failed to spawn PDF worker");
    }
}

fn try_recv_locked<T>(rx: &Arc<Mutex<Receiver<T>>>) -> Option<T> {
    let guard = rx.lock().ok()?;
    guard.try_recv().ok()
}

fn load_worker_document(
    path: &Path,
    cache: &mut HashMap<PathBuf, Arc<WorkerPdfDocument>>,
) -> Result<Arc<WorkerPdfDocument>, String> {
    if let Some(doc) = cache.get(path) {
        return Ok(Arc::clone(doc));
    }

    let file = std::fs::read(path).map_err(|e| format!("Failed to read: {}", e))?;
    let pdf = Pdf::new(Arc::new(file)).map_err(|e| format!("Failed to parse PDF: {e:?}"))?;
    let total_pages = pdf.pages().len();
    if total_pages == 0 {
        return Err("PDF has no pages".to_string());
    }

    let doc = Arc::new(WorkerPdfDocument {
        metadata: PdfDocumentMetadata {
            total_pages,
            outline: parse_pdf_outline(&pdf),
        },
        pdf,
    });
    cache.insert(path.to_path_buf(), Arc::clone(&doc));
    Ok(doc)
}

fn render_pdf_page_bitmap(
    key: &PageRenderKey,
    doc: &WorkerPdfDocument,
) -> Result<RenderedPdfBitmap, String> {
    let page = doc
        .pdf
        .pages()
        .get(key.page_index)
        .ok_or_else(|| format!("Page {} out of range", key.page_index + 1))?;

    let zoom = key.zoom_bucket as f32 / 100.0;
    let render_settings = RenderSettings {
        x_scale: zoom,
        y_scale: zoom,
        ..Default::default()
    };

    let interpreter_settings = InterpreterSettings::default();
    let pixmap = render(page, &interpreter_settings, &render_settings);
    let png_bytes = pixmap
        .into_png()
        .map_err(|e| format!("Failed to encode rendered page: {e:?}"))?;

    let img = image::load_from_memory(&png_bytes)
        .map_err(|e| format!("Failed to decode rendered page: {}", e))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    Ok(RenderedPdfBitmap {
        width,
        height,
        rgba: Arc::from(rgba.into_raw()),
    })
}

fn decode_pdf_string(value: PdfString<'_>) -> String {
    String::from_utf8_lossy(value.get().as_ref())
        .trim()
        .to_string()
}

fn page_index_from_dest_array(
    array: hayro::hayro_syntax::object::Array<'_>,
    page_map: &HashMap<ObjectIdentifier, usize>,
) -> Option<usize> {
    let mut iter = array.raw_iter();
    let first = iter.next()?;
    match first {
        hayro::hayro_syntax::object::MaybeRef::Ref(r) => page_map.get(&r.into()).copied(),
        hayro::hayro_syntax::object::MaybeRef::NotRef(Object::Dict(d)) => {
            d.obj_id().and_then(|id| page_map.get(&id).copied())
        }
        _ => None,
    }
}

fn outline_dest_page(
    item: &Dict<'_>,
    page_map: &HashMap<ObjectIdentifier, usize>,
) -> Option<usize> {
    if let Some(dest) = item.get::<hayro::hayro_syntax::object::Array<'_>>(DEST) {
        return page_index_from_dest_array(dest, page_map);
    }

    if let Some(action) = item.get::<Dict<'_>>(A) {
        if let Some(dest) = action.get::<hayro::hayro_syntax::object::Array<'_>>(b"D".as_ref()) {
            return page_index_from_dest_array(dest, page_map);
        }
    }

    None
}

fn collect_outline_items(
    xref: &hayro::hayro_syntax::xref::XRef,
    current: Option<ObjRef>,
    depth: usize,
    page_map: &HashMap<ObjectIdentifier, usize>,
    items: &mut Vec<PdfOutlineItem>,
    visited: &mut HashSet<ObjectIdentifier>,
) {
    let mut next_ref = current;
    let mut steps = 0usize;

    while let Some(obj_ref) = next_ref {
        let id: ObjectIdentifier = obj_ref.into();
        if !visited.insert(id) {
            break;
        }
        steps += 1;
        if steps > 2048 {
            break;
        }

        let Some(dict) = xref.get::<Dict<'_>>(id) else {
            break;
        };

        if let Some(title) = dict.get::<PdfString<'_>>(TITLE) {
            if let Some(page_index) = outline_dest_page(&dict, page_map) {
                let title_text = decode_pdf_string(title);
                if !title_text.is_empty() {
                    items.push(PdfOutlineItem {
                        title: title_text,
                        page_index,
                        depth,
                    });
                }
            }
        }

        collect_outline_items(
            xref,
            dict.get_ref(FIRST),
            depth + 1,
            page_map,
            items,
            visited,
        );
        next_ref = dict.get_ref(NEXT);
    }
}

fn parse_pdf_outline(pdf: &Pdf) -> Vec<PdfOutlineItem> {
    let mut page_map = HashMap::new();
    for (index, page) in pdf.pages().iter().enumerate() {
        if let Some(id) = page.raw().obj_id() {
            page_map.insert(id, index);
        }
    }

    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return Vec::new();
    };
    let Some(outlines) = catalog.get::<Dict<'_>>(OUTLINES) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    let mut visited = HashSet::new();
    collect_outline_items(
        xref,
        outlines.get_ref(FIRST),
        0,
        &page_map,
        &mut items,
        &mut visited,
    );
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key(page_index: usize) -> PageRenderKey {
        PageRenderKey::new(Path::new("/tmp/doc.pdf"), page_index, 125)
    }

    fn sample_thumb_key(page_index: usize) -> PageRenderKey {
        PageRenderKey::new(Path::new("/tmp/doc.pdf"), page_index, thumbnail_zoom_bucket())
    }

    fn sample_bitmap() -> RenderedPdfBitmap {
        RenderedPdfBitmap {
            width: 1,
            height: 1,
            rgba: Arc::from(vec![255, 255, 255, 255]),
        }
    }

    #[test]
    fn test_zoom_bucket_rounds_to_stable_cache_keys() {
        assert_eq!(zoom_bucket(1.0), 100);
        assert_eq!(zoom_bucket(1.249), 125);
        assert_eq!(zoom_bucket(1.251), 125);
    }

    #[test]
    fn test_request_queue_orders_visible_pages_before_thumbnails() {
        let mut queue = PdfRequestQueue::default();
        queue.push(PdfRenderRequest::thumbnail(
            PathBuf::from("/tmp/doc.pdf"),
            8,
            thumbnail_zoom_bucket(),
        ));
        queue.push(PdfRenderRequest::visible(
            PathBuf::from("/tmp/doc.pdf"),
            2,
            125,
        ));
        queue.push(PdfRenderRequest::prefetch(
            PathBuf::from("/tmp/doc.pdf"),
            3,
            125,
        ));

        assert_eq!(queue.pop().unwrap().kind, PdfRenderKind::VisiblePage);
        assert_eq!(queue.pop().unwrap().kind, PdfRenderKind::PrefetchPage);
        assert_eq!(queue.pop().unwrap().kind, PdfRenderKind::Thumbnail);
    }

    #[test]
    fn test_cache_budget_evicts_thumbnail_entries_first() {
        let mut cache = PdfPageCache::new(2, 1);
        cache.insert_page(sample_key(1), sample_bitmap());
        cache.insert_page(sample_key(2), sample_bitmap());
        cache.insert_thumbnail(sample_thumb_key(1), sample_bitmap());
        cache.insert_page(sample_key(3), sample_bitmap());

        assert!(cache.get_thumbnail(&sample_thumb_key(1)).is_none());
        assert!(cache.get_page(&sample_key(3)).is_some());
    }
}
