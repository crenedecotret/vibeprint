use eframe::egui::{ColorImage, TextureHandle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

use vibeprint::{
    layout_engine::QueuedImage,
    printer_discovery::{DiscoveryEvent, PrinterCaps, PrinterInfo},
    processor::ResampleEngine,
};

// ── Constants ─────────────────────────────────────────────────────────────────

pub(crate) const LEFT_W: f32 = 260.0;
pub(crate) const RIGHT_W: f32 = 295.0;
pub(crate) const THUMB_PX: u32 = 96;
pub(crate) const RULER_PX: f32 = 22.0;
pub(crate) const FIT_PAGE_IDX: usize = 15; // sentinel index = "Fit to Page"
pub(crate) const QUEUE_SPACING_IN: f32 = 0.25;

/// Standard print sizes (width × height in inches, label). Width is always the shorter side.
pub(crate) const PRINT_SIZES: &[(f32, f32, &str)] = &[
    (24.0, 36.0, "24 × 36"),
    (16.0, 20.0, "16 × 20"),
    (13.0, 19.0, "13 × 19"),
    (12.0, 18.0, "12 × 18"),
    (12.0, 16.0, "12 × 16"),
    (11.0, 17.0, "11 × 17"),
    (11.0, 14.0, "11 × 14"),
    (8.0, 12.0, "8 × 12"),
    (8.0, 10.0, "8 × 10"),
    (5.0, 7.0, "5 × 7"),
    (4.0, 6.0, "4 × 6"),
    (3.5, 5.0, "3.5 × 5"),
    (2.5, 3.5, "2.5 × 3.5"),
    (2.0, 3.0, "2 × 3"),
    (2.0, 2.0, "2 × 2"),
    // FIT_PAGE_IDX (15) = "Fit to Page" — handled as sentinel, not a real entry
];

// ── ICC Profile Types ───────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct IccProfileEntry {
    pub path: PathBuf,
    pub description: String,
    pub date: String,
    pub source: IccProfileSource,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum IccProfileSource {
    System,
    User,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IccProfileFilter {
    All,
    System,
    User,
}

impl IccProfileEntry {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
            .to_string()
    }

    pub fn location(&self) -> String {
        self.path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string()
    }
}

// ── Engine & Color Types ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Engine {
    Mks,
    RobidouxEwa,
    Iterative,
    Lanczos3,
}

impl Engine {
    pub const ALL: &'static [Engine] = &[Engine::Mks, Engine::RobidouxEwa, Engine::Iterative, Engine::Lanczos3];
    
    pub fn label(&self) -> &'static str {
        match self {
            Engine::Mks => "MKS (Magic Kernel Sharp)",
            Engine::RobidouxEwa => "Robidoux-EWA",
            Engine::Iterative => "Iterative Step",
            Engine::Lanczos3 => "Lanczos3",
        }
    }
    
    pub fn to_proc(&self) -> ResampleEngine {
        match self {
            Engine::Mks => ResampleEngine::Mks,
            Engine::RobidouxEwa => ResampleEngine::RobidouxEwa,
            Engine::Iterative => ResampleEngine::IterativeStep,
            Engine::Lanczos3 => ResampleEngine::Lanczos3,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Intent {
    Perceptual,
    Relative,
    Saturation,
}

impl Intent {
    pub fn label(&self) -> &'static str {
        match self {
            Intent::Perceptual => "Perceptual",
            Intent::Relative => "Relative Colorimetric",
            Intent::Saturation => "Saturation",
        }
    }
    
    pub fn to_lcms(&self) -> lcms2::Intent {
        match self {
            Intent::Perceptual => lcms2::Intent::Perceptual,
            Intent::Relative => lcms2::Intent::RelativeColorimetric,
            Intent::Saturation => lcms2::Intent::Saturation,
        }
    }
}

// ── UI State Types ───────────────────────────────────────────────────────────

#[allow(dead_code)]
pub(crate) enum ThumbState {
    Loading,
    Ready(TextureHandle),
    Failed,
}

impl std::fmt::Debug for ThumbState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThumbState::Loading => write!(f, "Loading"),
            ThumbState::Ready(_) => write!(f, "Ready(<texture>)"),
            ThumbState::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum LoadKind {
    Thumb,
    FullResStaged,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RightTab {
    PrinterSettings,
    ImageProperties,
    ImageQueue,
}

#[derive(Debug)]
pub(crate) enum ProcState {
    Idle,
    Running,
    Done(Vec<PathBuf>),
    Failed(String),
}

#[derive(Clone)]
pub(crate) enum ProcessTarget {
    Export,
    Print,
}

// ── Persistent Settings ────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct Settings {
    pub current_dir: Option<String>,
    pub printer_name: Option<String>,
    pub page_size_name: Option<String>,
    pub engine: Option<String>,
    pub sharpen: Option<u8>,
    pub depth16: Option<bool>,
    pub target_dpi: Option<u32>,
    pub output_icc: Option<String>,
    pub intent: Option<String>,
    pub bpc: Option<bool>,
    pub output_dir: Option<String>,
    pub user_border_in: Option<f32>,
    pub icc_filter: Option<String>,
}

// ── App State ───────────────────────────────────────────────────────────────

pub(crate) struct AppState {
    // ── Asset manager ──
    pub current_dir: PathBuf,
    pub subdirs: Vec<(String, PathBuf)>,
    pub image_files: Vec<PathBuf>,
    pub thumbs: HashMap<PathBuf, ThumbState>,
    pub thumb_tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>,
    pub thumb_rx: Receiver<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>,
    pub staged: Option<PathBuf>,
    pub staged_embedded_icc: Option<Vec<u8>>,
    pub staged_source_image: Option<ColorImage>,
    pub staged_img_size: Option<[usize; 2]>,
    pub selected: Option<PathBuf>,
    pub selected_embedded_icc: Option<Vec<u8>>,
    pub selected_source_image: Option<ColorImage>,
    pub highlighted: Option<PathBuf>,
    pub canvas_tex: Option<TextureHandle>,
    pub canvas_img_size: Option<[usize; 2]>,
    pub full_images: HashMap<PathBuf, ColorImage>,
    pub embedded_icc_by_path: HashMap<PathBuf, Option<Vec<u8>>>,
    pub preview_textures: HashMap<PathBuf, TextureHandle>,
    pub preview_dirty: bool,
    pub preview_cache_page: Option<usize>,
    pub queue: Vec<QueuedImage>,
    pub selected_queue_id: Option<Uuid>,
    pub current_page: usize,
    pub page_count: usize,
    pub canvas_hit_rects: Vec<(Uuid, eframe::egui::Rect)>,
    pub nav_history: Vec<PathBuf>,
    pub nav_forward: Vec<PathBuf>,
    pub tree_expanded: HashMap<PathBuf, bool>,
    pub addr_bar: String,
    pub thumb_zoom: f32,

    // ── CUPS ──
    pub printers: Vec<PrinterInfo>,
    pub all_caps: HashMap<String, PrinterCaps>,
    pub caps: Option<PrinterCaps>,
    pub printer_idx: usize,
    pub discovery_rx: Option<Receiver<DiscoveryEvent>>,

    // ── Printer props modal ──
    pub show_props: bool,
    pub props_media_idx: usize,
    pub props_slot_idx: usize,
    pub selected_page_size_idx: usize,
    pub extra_option_indices: HashMap<String, usize>,

    // ── Border override ──
    pub reported_border_in: f32,
    pub user_border_in: f32,
    pub border_edit_string: String,

    // ── Engine settings ──
    pub engine: Engine,
    pub sharpen: u8,
    pub depth16: bool,
    pub target_dpi: u32,

    // ── Color management ──
    pub output_icc: Option<IccProfileEntry>,
    pub intent: Intent,
    pub bpc: bool,
    pub softproof_enabled: bool,

    // ── Output ──
    pub output_dir: PathBuf,
    pub print_to_file: bool,

    // ── Processing ──
    pub proc_state: ProcState,
    pub proc_rx: Option<Receiver<Result<(Vec<PathBuf>, ProcessTarget), String>>>,

    // ── Right-panel tab ──
    pub right_tab: RightTab,

    // ── Status ──
    pub log: Vec<String>,

    // ── Saved-settings restoration ──
    pub pending_printer_name: Option<String>,
    pub pending_page_size_name: Option<String>,
    pub pending_user_border_in: Option<f32>,

    // ── Splash screen ──
    pub discovery_complete: bool,

    // ── Monitor ICC profile ──
    pub monitor_icc_profile: Option<Vec<u8>>,

    // ── Printing ──
    pub show_print_confirm: bool,
    pub pending_print_paths: Vec<PathBuf>,
    pub print_rx: Option<Receiver<Result<(), String>>>,
    pub print_log_rx: Option<Receiver<String>>,

    // ── ICC Picker Modal ──
    pub show_icc_picker: bool,
    pub icc_profiles: Vec<IccProfileEntry>,
    pub icc_filter_text: String,
    pub icc_profile_filter: IccProfileFilter,
    pub icc_scan_pending: bool,
    pub icc_scan_rx: Option<Receiver<Vec<IccProfileEntry>>>,
    pub icc_auto_switch_pending: bool,
    pub saved_icc_filter_for_restore: IccProfileFilter,

    // ── CLI auto-load ──
    pub auto_enqueue_path: Option<PathBuf>,
    pub auto_enqueue_pending: bool,
}

impl AppState {
    pub fn new(
        thumb_tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>,
        thumb_rx: Receiver<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>,
        home: PathBuf,
        saved_out_dir: PathBuf,
        saved_icc: Option<IccProfileEntry>,
        saved_engine: Engine,
        saved_intent: Intent,
        saved_sharpen: u8,
        saved_depth16: bool,
        saved_target_dpi: u32,
        saved_icc_filter: IccProfileFilter,
        pending_printer_name: Option<String>,
        pending_page_size_name: Option<String>,
        pending_user_border_in: Option<f32>,
        monitor_icc_profile: Option<Vec<u8>>,
        discovery_rx: Receiver<DiscoveryEvent>,
    ) -> Self {
        Self {
            current_dir: home.clone(),
            subdirs: Vec::new(),
            image_files: Vec::new(),
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            staged: None,
            staged_embedded_icc: None,
            staged_source_image: None,
            staged_img_size: None,
            selected: None,
            selected_embedded_icc: None,
            selected_source_image: None,
            highlighted: None,
            canvas_tex: None,
            canvas_img_size: None,
            full_images: HashMap::new(),
            embedded_icc_by_path: HashMap::new(),
            preview_textures: HashMap::new(),
            preview_dirty: true,
            preview_cache_page: None,
            queue: Vec::new(),
            selected_queue_id: None,
            current_page: 0,
            page_count: 1,
            canvas_hit_rects: Vec::new(),
            nav_history: Vec::new(),
            nav_forward: Vec::new(),
            tree_expanded: HashMap::new(),
            addr_bar: String::new(),
            thumb_zoom: 1.0,
            printers: Vec::new(),
            all_caps: HashMap::new(),
            caps: None,
            printer_idx: 0,
            discovery_rx: Some(discovery_rx),
            show_props: false,
            props_media_idx: 0,
            props_slot_idx: 0,
            selected_page_size_idx: 0,
            extra_option_indices: HashMap::new(),
            reported_border_in: 0.25,
            user_border_in: 0.25,
            border_edit_string: format!("{:.3}", 0.25),
            engine: saved_engine,
            sharpen: saved_sharpen,
            depth16: saved_depth16,
            target_dpi: saved_target_dpi,
            output_icc: saved_icc,
            intent: saved_intent,
            bpc: true,
            softproof_enabled: false,
            output_dir: saved_out_dir,
            print_to_file: false,
            proc_state: ProcState::Idle,
            proc_rx: None,
            right_tab: RightTab::PrinterSettings,
            log: Vec::new(),
            pending_printer_name,
            pending_page_size_name,
            pending_user_border_in,
            discovery_complete: false,
            monitor_icc_profile,
            show_print_confirm: false,
            pending_print_paths: Vec::new(),
            print_rx: None,
            print_log_rx: None,
            show_icc_picker: false,
            icc_profiles: Vec::new(),
            icc_filter_text: String::new(),
            icc_profile_filter: saved_icc_filter,
            icc_scan_pending: false,
            icc_scan_rx: None,
            icc_auto_switch_pending: false,
            saved_icc_filter_for_restore: saved_icc_filter,
            auto_enqueue_path: None,
            auto_enqueue_pending: false,
        }
    }
}
