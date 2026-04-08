#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, ColorImage, Context, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use uuid::Uuid;

use vibeprint::{
    layout_engine::{self, Point, PrintSize, QueuedImage, Unit},
    monitor_icc,
    printer_discovery::{self, DiscoveryEvent, PrinterCaps, PrinterInfo},
    processor::{self, ResampleEngine},
};

// ── Constants ─────────────────────────────────────────────────────────────────

const LEFT_W:       f32  = 260.0;
const RIGHT_W:      f32  = 295.0;
const THUMB_PX:     u32  = 96;
const RULER_PX:     f32  = 22.0;
const FIT_PAGE_IDX: usize = 15; // sentinel index = "Fit to Page"
const QUEUE_SPACING_IN: f32 = 0.25;

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Standard print sizes (width × height in inches, label).  Width is always the shorter side.
const PRINT_SIZES: &[(f32, f32, &str)] = &[
    (24.0, 36.0, "24 × 36"),
    (16.0, 20.0, "16 × 20"),
    (13.0, 19.0, "13 × 19"),
    (12.0, 18.0, "12 × 18"),
    (12.0, 16.0, "12 × 16"),
    (11.0, 17.0, "11 × 17"),
    (11.0, 14.0, "11 × 14"),
    ( 8.0, 12.0, "8 × 12"),
    ( 8.0, 10.0, "8 × 10"),
    ( 5.0,  7.0, "5 × 7"),
    ( 4.0,  6.0, "4 × 6"),
    ( 3.5,  5.0, "3.5 × 5"),
    ( 2.5,  3.5, "2.5 × 3.5"),
    ( 2.0,  3.0, "2 × 3"),
    ( 2.0,  2.0, "2 × 2"),
    // FIT_PAGE_IDX (15) = "Fit to Page" — handled as sentinel, not a real entry
];


// ── Small types ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct IccProfileEntry {
    path: PathBuf,
    description: String,
    date: String,
    source: IccProfileSource,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum IccProfileSource {
    System,
    User,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IccProfileFilter {
    All,
    System,
    User,
}

impl IccProfileEntry {
    fn file_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
            .to_string()
    }

    fn location(&self) -> String {
        self.path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Engine {
    Mks,
    RobidouxEwa,
    Iterative,
    Lanczos3,
}

fn aspect_fit_rect_in_box(box_rect: Rect, src_w: u32, src_h: u32, rotate_cw: bool) -> Rect {
    let sw = src_w.max(1) as f32;
    let sh = src_h.max(1) as f32;
    let aspect = if rotate_cw { sh / sw } else { sw / sh };
    let bw = box_rect.width().max(1.0);
    let bh = box_rect.height().max(1.0);

    let (w, h) = if bw / bh > aspect {
        (bh * aspect, bh)
    } else {
        (bw, bw / aspect)
    };

    Rect::from_center_size(box_rect.center(), Vec2::new(w, h))
}

fn extract_file_date(path: &PathBuf) -> String {
    use chrono::{DateTime, Local, Utc};
    
    if let Ok(metadata) = std::fs::metadata(path) {
        if let Ok(modified) = metadata.modified() {
            let datetime: DateTime<Utc> = modified.into();
            let local_datetime = datetime.with_timezone(&Local);
            return local_datetime.format("%d %b %Y").to_string();
        }
    }
    "Unknown".to_string()
}

fn scan_icc_directories(tx: Sender<Vec<IccProfileEntry>>) {
    let mut profiles = Vec::new();

    // Standard Linux ICC profile directories (system)
    let system_dirs = vec![
        PathBuf::from("/usr/share/color/icc"),
        PathBuf::from("/usr/local/share/color/icc"),
    ];

    // User-local directories
    let mut user_dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        user_dirs.push(home.join(".local/share/color/icc"));
        user_dirs.push(home.join(".local/share/icc"));
        user_dirs.push(home.join(".color/icc"));
    }

    // Scan system directories
    for dir in system_dirs {
        scan_directory(&dir, IccProfileSource::System, &mut profiles);
    }

    // Scan user directories
    for dir in user_dirs {
        scan_directory(&dir, IccProfileSource::User, &mut profiles);
    }

    // Sort by description for consistent ordering
    profiles.sort_by(|a, b| a.description.to_lowercase().cmp(&b.description.to_lowercase()));

    let _ = tx.send(profiles);
}

fn scan_directory(dir: &PathBuf, source: IccProfileSource, profiles: &mut Vec<IccProfileEntry>) {
    use lcms2::Profile;

    if !dir.exists() {
        return;
    }

    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            let extension = path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());

            if extension.as_deref() != Some("icc") && extension.as_deref() != Some("icm") {
                continue;
            }

            // Try to extract the internal profile description and date
            let (description, date) = if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(profile) = Profile::new_icc(&bytes) {
                    // Try to get the profile description tag
                    let desc = profile
                        .info(lcms2::InfoType::Description, lcms2::Locale::none())
                        .unwrap_or_else(|| {
                            // Fallback to filename if description extraction fails
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Unknown")
                                .to_string()
                        });

                    // Try to get creation date from ICC profile metadata
                    // Note: lcms2 doesn't provide direct access to creation date, so we fall back to file date
                    let file_date = extract_file_date(&path);
                    (desc, file_date)
                } else {
                    // Fallback to filename if profile loading fails
                    let desc = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let file_date = extract_file_date(&path);
                    (desc, file_date)
                }
            } else {
                // Fallback to filename if file read fails
                let desc = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let file_date = extract_file_date(&path);
                (desc, file_date)
            };

            profiles.push(IccProfileEntry {
                path,
                description,
                date,
                source,
            });
        }
    }
}

fn apply_preview_transform(
    monitor_profile_data: &[u8],
    source_profile_data: Option<&[u8]>,
    output_icc_path: Option<&PathBuf>,
    pixels: &mut [u8],
    intent: lcms2::Intent,
    bpc: bool,
    softproof_enabled: bool,
) -> Option<()> {
    use lcms2::{Flags, PixelFormat, Profile, Transform};

    let source_profile = if let Some(src_data) = source_profile_data {
        Profile::new_icc(src_data).unwrap_or_else(|_| Profile::new_srgb())
    } else {
        Profile::new_srgb()
    };

    let monitor_profile = Profile::new_icc(monitor_profile_data).ok()?;
    let flags = if bpc {
        Flags::BLACKPOINT_COMPENSATION | Flags::NO_CACHE
    } else {
        Flags::NO_CACHE
    };

    if softproof_enabled {
        let output_profile = if let Some(path) = output_icc_path {
            let bytes = std::fs::read(path).ok()?;
            Profile::new_icc(&bytes).ok()?
        } else if let Some(src_data) = source_profile_data {
            Profile::new_icc(src_data).unwrap_or_else(|_| Profile::new_srgb())
        } else {
            Profile::new_srgb()
        };

        let to_output = Transform::new_flags(
            &source_profile,
            PixelFormat::RGB_8,
            &output_profile,
            PixelFormat::RGB_8,
            intent,
            flags,
        )
        .ok()?;

        let mut output_space = vec![0u8; pixels.len()];
        let src = pixels.to_vec();
        to_output.transform_pixels(&src, &mut output_space);

        let to_monitor = Transform::new_flags(
            &output_profile,
            PixelFormat::RGB_8,
            &monitor_profile,
            PixelFormat::RGB_8,
            intent,
            flags,
        )
        .ok()?;
        to_monitor.transform_pixels(&output_space, pixels);
        return Some(());
    }

    let to_monitor = Transform::new_flags(
        &source_profile,
        PixelFormat::RGB_8,
        &monitor_profile,
        PixelFormat::RGB_8,
        intent,
        flags,
    )
    .ok()?;
    let src = pixels.to_vec();
    to_monitor.transform_pixels(&src, pixels);
    Some(())
}
impl Engine {
    const ALL: &'static [Engine] = &[Engine::Mks, Engine::RobidouxEwa, Engine::Iterative, Engine::Lanczos3];
    fn label(&self) -> &'static str {
        match self {
            Engine::Mks        => "MKS (Magic Kernel Sharp)",
            Engine::RobidouxEwa => "Robidoux-EWA",
            Engine::Iterative  => "Iterative Step",
            Engine::Lanczos3   => "Lanczos3",
        }
    }
    fn to_proc(&self) -> ResampleEngine {
        match self {
            Engine::Mks        => ResampleEngine::Mks,
            Engine::RobidouxEwa => ResampleEngine::RobidouxEwa,
            Engine::Iterative  => ResampleEngine::IterativeStep,
            Engine::Lanczos3   => ResampleEngine::Lanczos3,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Intent { Perceptual, Relative, Saturation }
impl Intent {
    fn label(&self) -> &'static str {
        match self {
            Intent::Perceptual  => "Perceptual",
            Intent::Relative    => "Relative Colorimetric",
            Intent::Saturation  => "Saturation",
        }
    }
    fn to_lcms(&self) -> lcms2::Intent {
        match self {
            Intent::Perceptual  => lcms2::Intent::Perceptual,
            Intent::Relative    => lcms2::Intent::RelativeColorimetric,
            Intent::Saturation  => lcms2::Intent::Saturation,
        }
    }
}

#[allow(dead_code)]
enum ThumbState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

#[derive(Clone, Copy, PartialEq)]
enum LoadKind {
    Thumb,
    FullResStaged,
}

#[derive(Clone, Copy, PartialEq)]
enum RightTab { PrinterSettings, ImageProperties, ImageQueue }

enum ProcState {
    Idle,
    Running,
    Done(Vec<PathBuf>),
    Failed(String),
}

#[derive(Clone)]
enum ProcessTarget {
    Export,
    Print,
}

// ── Persistent settings ───────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Settings {
    current_dir:   Option<String>,
    printer_name:  Option<String>,
    page_size_name: Option<String>,
    engine:        Option<String>,
    sharpen:       Option<u8>,
    depth16:       Option<bool>,
    target_dpi:    Option<u32>,
    output_icc:    Option<String>,
    intent:        Option<String>,
    bpc:           Option<bool>,
    output_dir:    Option<String>,
    user_border_in: Option<f32>,
    icc_filter:    Option<String>,
}

fn config_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("vibeprint");
    p.push("settings.json");
    Some(p)
}

fn load_settings() -> Settings {
    let path = match config_path() { Some(p) => p, None => return Settings::default() };
    let text = match std::fs::read_to_string(&path) { Ok(t) => t, Err(_) => return Settings::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_settings(s: &Settings) {
    let Some(path) = config_path() else { return };
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    if let Ok(text) = serde_json::to_string_pretty(s) { let _ = std::fs::write(path, text); }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct App {
    // ── Asset manager ──
    current_dir: PathBuf,
    subdirs: Vec<(String, PathBuf)>,
    image_files: Vec<PathBuf>,
    thumbs: HashMap<PathBuf, ThumbState>,
    thumb_tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>,
    thumb_rx: Receiver<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>,
    staged: Option<PathBuf>,
    staged_embedded_icc: Option<Vec<u8>>,
    staged_source_image: Option<ColorImage>,
    staged_img_size: Option<[usize; 2]>,
    selected: Option<PathBuf>,
    selected_embedded_icc: Option<Vec<u8>>,
    selected_source_image: Option<ColorImage>,
    highlighted: Option<PathBuf>,
    canvas_tex: Option<egui::TextureHandle>,
    canvas_img_size: Option<[usize; 2]>,
    full_images: HashMap<PathBuf, ColorImage>,
    embedded_icc_by_path: HashMap<PathBuf, Option<Vec<u8>>>,
    preview_textures: HashMap<PathBuf, egui::TextureHandle>,
    preview_dirty: bool,
    preview_cache_page: Option<usize>,
    queue: Vec<QueuedImage>,
    selected_queue_id: Option<Uuid>,
    current_page: usize,
    page_count: usize,
    canvas_hit_rects: Vec<(Uuid, Rect)>,
    nav_history: Vec<PathBuf>,
    nav_forward: Vec<PathBuf>,
    tree_expanded: HashMap<PathBuf, bool>,
    addr_bar: String,
    thumb_zoom: f32,

    // ── CUPS ──
    printers: Vec<PrinterInfo>,
    all_caps: HashMap<String, PrinterCaps>,
    caps: Option<PrinterCaps>,
    printer_idx: usize,
    discovery_rx: Option<Receiver<DiscoveryEvent>>,

    // ── Printer props modal ──
    show_props: bool,
    props_media_idx: usize,
    props_slot_idx: usize,
    selected_page_size_idx: usize,
    extra_option_indices: HashMap<String, usize>,

    // ── Border override ──
    reported_border_in: f32,   // Original printer-reported border (max of L/R/T/B margins)
    user_border_in: f32,       // User-defined border (>= reported_border_in)
    border_edit_string: String, // Persistent edit string for border input field

    // ── Engine settings ──
    engine: Engine,
    sharpen: u8,
    depth16: bool,
    target_dpi: u32,

    // ── Color management ──
    output_icc: Option<IccProfileEntry>,
    intent: Intent,
    bpc: bool,
    softproof_enabled: bool,

    // ── Output ──
    output_dir: PathBuf,
    print_to_file: bool,

    // ── Processing ──
    proc_state: ProcState,
    proc_rx: Option<Receiver<Result<(Vec<PathBuf>, ProcessTarget), String>>>,

    // ── Right-panel tab ──
    right_tab: RightTab,

    // ── Status ──
    log: Vec<String>,

    // ── Saved-settings restoration ──
    pending_printer_name: Option<String>,
    pending_page_size_name: Option<String>,
    pending_user_border_in: Option<f32>,

    // ── Splash screen ──
    discovery_complete: bool,

    // ── Monitor ICC profile ──
    monitor_icc_profile: Option<Vec<u8>>,

    // ── Printing ──
    show_print_confirm: bool,
    pending_print_paths: Vec<PathBuf>,
    print_rx: Option<Receiver<Result<(), String>>>,
    print_log_rx: Option<Receiver<String>>,

    // ── ICC Picker Modal ──
    show_icc_picker: bool,
    icc_profiles: Vec<IccProfileEntry>,
    icc_filter_text: String,
    icc_profile_filter: IccProfileFilter,
    icc_scan_pending: bool,
    icc_scan_rx: Option<Receiver<Vec<IccProfileEntry>>>,
    icc_auto_switch_pending: bool,
    saved_icc_filter_for_restore: IccProfileFilter,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let out_dir = dirs::desktop_dir().unwrap_or_else(|| home.clone());
        let (thumb_tx, thumb_rx) = mpsc::channel::<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>();
        let s = load_settings();

        // Restore last folder (fall back to home if missing)
        let start_dir = s.current_dir.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| home.clone());

        // Restore output folder (fall back to desktop)
        let saved_out_dir = s.output_dir.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or(out_dir);

        // Restore output ICC (fall back to None if file missing)
        let saved_icc: Option<IccProfileEntry> = s.output_icc.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .map(|path| {
                // Try to extract description from the saved ICC file
                let (description, date) = if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(profile) = lcms2::Profile::new_icc(&bytes) {
                        let desc = profile.info(lcms2::InfoType::Description, lcms2::Locale::none())
                            .unwrap_or_else(|| path.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string());
                        let d = extract_file_date(&path);
                        (desc, d)
                    } else {
                        (path.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string(),
                         extract_file_date(&path))
                    }
                } else {
                    (path.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string(),
                     extract_file_date(&path))
                };
                IccProfileEntry { path, description, date, source: IccProfileSource::User }
            });

        let saved_engine = match s.engine.as_deref() {
            Some("lanczos3")  => Engine::Lanczos3,
            Some("iterative") => Engine::Iterative,
            Some("robidoux")  => Engine::RobidouxEwa,
            _                 => Engine::Mks,
        };
        let saved_intent = match s.intent.as_deref() {
            Some("perceptual") => Intent::Perceptual,
            Some("saturation") => Intent::Saturation,
            _                  => Intent::Relative,
        };
        let saved_icc_filter = match s.icc_filter.as_deref() {
            Some("all")    => IccProfileFilter::All,
            Some("system") => IccProfileFilter::System,
            Some("user")   => IccProfileFilter::User,
            _              => IccProfileFilter::System, // Default to System
        };

        let mut app = Self {
            current_dir: start_dir,
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
            discovery_rx: Some(printer_discovery::spawn_discovery()),

            show_props: false,
            props_media_idx: 0,
            props_slot_idx: 0,
            selected_page_size_idx: 0,
            extra_option_indices: HashMap::new(),

            // ── Border override ──
            reported_border_in: 0.25,
            user_border_in: 0.25,
            border_edit_string: format!("{:.3}", 0.25),

            engine: saved_engine,
            sharpen: s.sharpen.unwrap_or(5),
            depth16: s.depth16.unwrap_or(true),
            target_dpi: s.target_dpi.unwrap_or(720),

            output_icc: saved_icc,
            intent: saved_intent,
            bpc: s.bpc.unwrap_or(true),
            softproof_enabled: false,

            output_dir: saved_out_dir,
            print_to_file: false,

            proc_state: ProcState::Idle,
            proc_rx: None,

            right_tab: RightTab::PrinterSettings,

            log: Vec::new(),

            pending_printer_name: s.printer_name,
            pending_page_size_name: s.page_size_name,
            pending_user_border_in: s.user_border_in,

            discovery_complete: false,

            monitor_icc_profile: monitor_icc::get_monitor_profile(),

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
        };
        
        // Log monitor ICC status (silent - only log errors)
        if app.monitor_icc_profile.is_none() {
            app.log.push("⚠ No monitor ICC profile found".into());
        }
        
        app.scan_dir();
        app
    }

    fn scan_dir(&mut self) {
        self.subdirs.clear();
        self.image_files.clear();

        let Ok(read) = std::fs::read_dir(&self.current_dir) else { return };

        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| (e.path().is_file(), e.file_name()));

        let selected = self.selected.clone();
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') { continue; }
            if path.is_dir() {
                self.subdirs.push((name, path));
            } else if is_image(&path) {
                // Never fire a thumbnail load for the selected image — its full-res
                // canvas texture must not be overwritten by a small thumbnail.
                if selected.as_ref() != Some(&path) {
                    let tx = self.thumb_tx.clone();
                    let p  = path.clone();
                    let px = self.thumb_load_px();
                    thread::spawn(move || load_thumb(p, px, tx));
                }
                self.image_files.push(path);
            }
        }
    }

    fn thumb_load_px(&self) -> u32 {
        ((THUMB_PX as f32) * self.thumb_zoom).round().max(32.0) as u32
    }

    fn reload_thumbs(&mut self) {
        // Keep the selected image's thumb entry so we don't clobber canvas_tex
        let selected = self.selected.clone();
        self.thumbs.retain(|p, _| selected.as_ref() == Some(p));
        let px = self.thumb_load_px();
        for path in &self.image_files {
            if selected.as_ref() == Some(path) { continue; } // canvas load is independent
            let tx = self.thumb_tx.clone();
            let p  = path.clone();
            thread::spawn(move || load_thumb(p, px, tx));
        }
    }

    fn navigate(&mut self, path: PathBuf) {
        if path == self.current_dir { return; }
        let prev = self.current_dir.clone();
        self.nav_history.push(prev);
        self.nav_forward.clear();
        self.current_dir = path.clone();
        self.addr_bar = path.to_string_lossy().into_owned();
        let sel = self.selected.clone();
        self.thumbs.retain(|p, _| sel.as_ref() == Some(p));
        self.scan_dir();
    }

    fn nav_back(&mut self) {
        if let Some(prev) = self.nav_history.pop() {
            let cur = self.current_dir.clone();
            self.nav_forward.push(cur);
            self.current_dir = prev.clone();
            self.addr_bar = prev.to_string_lossy().into_owned();
            let sel = self.selected.clone();
            self.thumbs.retain(|p, _| sel.as_ref() == Some(p));
            self.scan_dir();
        }
    }

    fn nav_fwd(&mut self) {
        if let Some(next) = self.nav_forward.pop() {
            let cur = self.current_dir.clone();
            self.nav_history.push(cur);
            self.current_dir = next.clone();
            self.addr_bar = next.to_string_lossy().into_owned();
            let sel = self.selected.clone();
            self.thumbs.retain(|p, _| sel.as_ref() == Some(p));
            self.scan_dir();
        }
    }

    fn stage_image(&mut self, path: PathBuf) {
        self.staged = Some(path.clone());
        self.staged_embedded_icc = None;
        self.staged_source_image = None;
        self.staged_img_size = None;
        self.right_tab = RightTab::ImageProperties;

        // Send a full-res load via the thumb channel with embedded ICC extraction
        let tx = self.thumb_tx.clone();
        thread::spawn(move || {
            // Try to extract embedded ICC profile
            let embedded_icc = extract_embedded_icc(&path);
            
            if let Ok(img) = image::open(&path) {
                let rgb = img.into_rgb8();
                let size = [rgb.width() as usize, rgb.height() as usize];
                let pixels = rgb.into_raw()
                    .chunks_exact(3)
                    .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                    .collect();
                let _ = tx.send((path, ColorImage { size, pixels }, embedded_icc, LoadKind::FullResStaged));
            }
        });
    }

    fn mark_preview_dirty(&mut self) {
        self.preview_dirty = true;
        self.preview_cache_page = None;
    }

    fn rebuild_canvas_texture(&mut self, ctx: &Context) {
        self.preview_textures.clear();

        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = self
            .queue
            .iter()
            .filter(|q| q.page == self.current_page)
            .filter_map(|q| {
                if seen.insert(q.filepath.clone()) {
                    Some(q.filepath.clone())
                } else {
                    None
                }
            })
            .collect();

        for path in paths {
            let Some(base) = self.ensure_full_image_loaded(&path) else { continue; };
            let mut ci = base.clone();

            if let Some(ref monitor_profile) = self.monitor_icc_profile {
                let mut pixel_bytes: Vec<u8> = ci
                    .pixels
                    .iter()
                    .flat_map(|c| [c.r(), c.g(), c.b()])
                    .collect();

                let src_icc = self
                    .embedded_icc_by_path
                    .get(&path)
                    .and_then(|v| v.as_deref());

                if apply_preview_transform(
                    monitor_profile,
                    src_icc,
                    self.output_icc.as_ref().map(|e| &e.path),
                    &mut pixel_bytes,
                    self.intent.to_lcms(),
                    self.bpc,
                    self.softproof_enabled,
                )
                .is_some()
                {
                    ci.pixels = pixel_bytes
                        .chunks_exact(3)
                        .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                        .collect();
                }
            }

            let tex_name = format!("page_preview::{}", path.to_string_lossy());
            let tex = ctx.load_texture(&tex_name, ci, egui::TextureOptions::LINEAR);
            self.preview_textures.insert(path.clone(), tex.clone());

            if self.selected.as_ref() == Some(&path) {
                self.canvas_tex = Some(tex);
            }
        }

        if let Some(sel) = &self.selected {
            if let Some(ci) = self.full_images.get(sel) {
                self.canvas_img_size = Some(ci.size);
            }
        }

        self.preview_cache_page = Some(self.current_page);
        self.preview_dirty = false;
    }

    fn ensure_full_image_loaded(&mut self, path: &PathBuf) -> Option<&ColorImage> {
        if !self.full_images.contains_key(path) {
            let img = image::open(path).ok()?.into_rgb8();
            let size = [img.width() as usize, img.height() as usize];
            let pixels = img
                .into_raw()
                .chunks_exact(3)
                .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                .collect();
            self.full_images.insert(path.clone(), ColorImage { size, pixels });
            self.embedded_icc_by_path
                .entry(path.clone())
                .or_insert_with(|| extract_embedded_icc(path));
        }
        self.full_images.get(path)
    }

    /// Calculate the maximum border from printer's imageable area for current page size
    fn calc_reported_border(&self) -> f32 {
        self.caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
            .map(|ps| {
                let (l, b, r, t) = ps.imageable_area;
                let (pw, ph) = ps.paper_size;
                let left = l / 72.0;
                let right = (pw - r) / 72.0;
                let bottom = b / 72.0;
                let top = (ph - t) / 72.0;
                left.max(right).max(bottom).max(top)
            })
            .unwrap_or(0.25)
    }

    fn imageable_size_in(&self) -> (f32, f32) {
        let (pw, ph) = self.caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
            .unwrap_or((8.5, 11.0)); // Default Letter size
        
        let w = (pw - 2.0 * self.user_border_in).max(0.1);
        let h = (ph - 2.0 * self.user_border_in).max(0.1);
        (w, h)
    }

    fn imageable_size_px(&self) -> (u32, u32) {
        let (w_in, h_in) = self.imageable_size_in();
        let dpi = self.target_dpi as f32;
        (
            (w_in * dpi).round().max(1.0) as u32,
            (h_in * dpi).round().max(1.0) as u32,
        )
    }

    fn max_imageable_size_px(&self) -> (u32, u32) {
        let (pw, ph) = self.caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
            .unwrap_or((8.5, 11.0));
        let w = (pw - 2.0 * self.reported_border_in).max(0.1);
        let h = (ph - 2.0 * self.reported_border_in).max(0.1);
        let dpi = self.target_dpi as f32;
        (
            (w * dpi).round().max(1.0) as u32,
            (h * dpi).round().max(1.0) as u32,
        )
    }

    fn border_offset_px(&self) -> (u32, u32) {
        let dpi = self.target_dpi as f32;
        let diff_in = self.user_border_in - self.reported_border_in;
        let offset = (diff_in * dpi).round().max(0.0) as u32;
        (offset, offset)
    }

    fn queued_box_px(&self, qi: &QueuedImage) -> (u32, u32) {
        if qi.placed_w_px > 0 && qi.placed_h_px > 0 {
            return (qi.placed_w_px, qi.placed_h_px);
        }
        let (w_in, h_in) = qi.size.as_inches();
        let (w_in, h_in) = if qi.rotation > 0.0 { (h_in, w_in) } else { (w_in, h_in) };
        let dpi = self.target_dpi as f32;
        (
            (w_in * dpi).round().max(1.0) as u32,
            (h_in * dpi).round().max(1.0) as u32,
        )
    }

    fn size_from_idx(&self, idx: usize, src_size_px: Option<(u32, u32)>) -> Option<PrintSize> {
        if idx < PRINT_SIZES.len() {
            let (w, h, _) = PRINT_SIZES[idx];
            return Some(PrintSize { width: w, height: h, unit: Unit::Inches });
        }
        if idx == FIT_PAGE_IDX {
            let (ia_w_in, ia_h_in) = self.imageable_size_in();
            if let Some((sw, sh)) = src_size_px {
                let aspect = (sw.max(1) as f32) / (sh.max(1) as f32);
                let (nw, nh) = if ia_w_in / ia_h_in > aspect {
                    (ia_h_in * aspect, ia_h_in)
                } else {
                    (ia_w_in, ia_w_in / aspect)
                };
                let rot_aspect = 1.0 / aspect;
                let (rw, rh) = if ia_w_in / ia_h_in > rot_aspect {
                    (ia_h_in * rot_aspect, ia_h_in)
                } else {
                    (ia_w_in, ia_w_in * aspect)
                };
                let (w, h) = if rw * rh > nw * nh { (rw, rh) } else { (nw, nh) };
                return Some(PrintSize { width: w, height: h, unit: Unit::Inches });
            }
            return Some(PrintSize { width: ia_w_in, height: ia_h_in, unit: Unit::Inches });
        }
        None
    }

    fn relayout_queue(&mut self) {
        let (page_w_px, page_h_px) = self.imageable_size_px();
        let result = layout_engine::layout_queue(
            &self.queue,
            page_w_px,
            page_h_px,
            self.target_dpi,
            QUEUE_SPACING_IN,
        );

        for qi in &mut self.queue {
            if let Some(p) = result.placements.get(&qi.id) {
                qi.position = Point { x: p.x_px, y: p.y_px };
                qi.page = p.page;
                qi.rotation = p.rotation_deg;
                qi.placed_w_px = p.w_px;
                qi.placed_h_px = p.h_px;
            }
        }

        self.page_count = result.page_count.max(1);
        if self.current_page >= self.page_count {
            self.current_page = self.page_count.saturating_sub(1);
        }
        self.mark_preview_dirty();
    }

    fn enqueue_staged_with_idx(&mut self, idx: usize) -> bool {
        let Some(path) = self.staged.clone() else { return false; };
        let Some(src) = self.staged_source_image.as_ref() else {
            self.log.push("⚠ Image still loading…".into());
            return false;
        };
        let size = src.size;
        let src_size = (size[0] as u32, size[1] as u32);
        let Some(print_size) = self.size_from_idx(idx, Some(src_size)) else {
            return false;
        };
        let fit_to_page = idx == FIT_PAGE_IDX;

        self.queue.push(QueuedImage {
            id: Uuid::new_v4(),
            filepath: path.clone(),
            size: print_size,
            fit_to_page,
            source_icc: None,
            position: Point::default(),
            page: 0,
            rotation: 0.0,
            placed_w_px: 0,
            placed_h_px: 0,
            src_size_px: Some(src_size),
        });
        self.selected_queue_id = self.queue.last().map(|q| q.id);
        self.selected = Some(path.clone());
        self.selected_source_image = Some(src.clone());
        self.selected_embedded_icc = self.staged_embedded_icc.clone();
        self.canvas_img_size = Some(size);
        self.full_images.insert(path.clone(), src.clone());
        self.embedded_icc_by_path
            .insert(path, self.staged_embedded_icc.clone());

        self.staged = None;
        self.staged_embedded_icc = None;
        self.staged_source_image = None;
        self.staged_img_size = None;

        self.relayout_queue();
        if let Some(id) = self.selected_queue_id {
            if let Some(item) = self.queue.iter().find(|q| q.id == id) {
                self.current_page = item.page;
            }
        }
        true
    }

    fn selected_queue_mut(&mut self) -> Option<&mut QueuedImage> {
        let id = self.selected_queue_id?;
        self.queue.iter_mut().find(|q| q.id == id)
    }

    fn selected_queue(&self) -> Option<&QueuedImage> {
        let id = self.selected_queue_id?;
        self.queue.iter().find(|q| q.id == id)
    }

    fn update_selected_queue_size_idx(&mut self, idx: usize) {
        let src_size = self.selected_queue().and_then(|q| q.src_size_px);
        let Some(ps) = self.size_from_idx(idx, src_size) else { return; };
        let sel = self.selected_queue_id;
        if let Some(item) = self.selected_queue_mut() {
            item.size = ps;
            item.fit_to_page = idx == FIT_PAGE_IDX;
        }
        self.relayout_queue();
        if let Some(id) = sel {
            if let Some(item) = self.queue.iter().find(|q| q.id == id) {
                self.current_page = item.page;
            }
        }
    }

    // ── Printer caps sync ──────────────────────────────────────────────────────

    fn sync_caps_to_selection(&mut self) {
        let name = match self.printers.get(self.printer_idx) {
            Some(p) => p.name.clone(),
            None    => return,
        };
        if self.caps.as_ref().map(|c| &c.name) == Some(&name) {
            return; // already correct
        }
        if let Some(caps) = self.all_caps.get(&name) {
            self.props_media_idx        = 0;
            self.props_slot_idx         = 0;
            
            // Try to restore saved paper size, otherwise default to 0
            self.selected_page_size_idx = if let Some(ref sz_name) = self.pending_page_size_name {
                if let Some(idx) = caps.page_sizes.iter().position(|ps| &ps.name == sz_name) {
                    self.pending_page_size_name = None; // clear after successful restore
                    idx
                } else {
                    self.pending_page_size_name = None; // clear even if not found
                    0
                }
            } else {
                0
            };
            
            // Seed extra_option_indices from each option's PPD default
            self.extra_option_indices.clear();
            for opt in &caps.extra_options {
                self.extra_option_indices.insert(opt.key.clone(), opt.default_idx);
            }
            self.caps = Some(caps.clone());
            
            // Recalculate reported border
            self.reported_border_in = self.calc_reported_border();
            // Restore saved user border if valid, otherwise use reported border
            self.user_border_in = if let Some(saved) = self.pending_user_border_in {
                if saved >= self.reported_border_in {
                    saved
                } else {
                    self.reported_border_in
                }
            } else {
                self.reported_border_in
            };
            self.pending_user_border_in = None;
            self.border_edit_string = format!("{:.3}", self.user_border_in);
            
            self.relayout_queue();
        } else {
            self.caps = None;
            self.extra_option_indices.clear();
            self.reported_border_in = 0.25;
            self.user_border_in = 0.25;
            self.border_edit_string = format!("{:.3}", self.user_border_in);
            self.relayout_queue();
        }
    }

    // ── Background task pump ──────────────────────────────────────────────────

    fn pump(&mut self, ctx: &Context) {
        // Thumbnails / canvas image
        while let Ok((path, ci, embedded_icc, kind)) = self.thumb_rx.try_recv() {
            let name = path.to_string_lossy().to_string();
            match kind {
                LoadKind::Thumb => {
                    let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                    self.thumbs.insert(path, ThumbState::Ready(tex));
                }
                LoadKind::FullResStaged => {
                    if self.staged.as_ref() == Some(&path) {
                        let size = ci.size;
                        self.full_images.insert(path.clone(), ci.clone());
                        self.embedded_icc_by_path.insert(path.clone(), embedded_icc.clone());
                        self.staged_embedded_icc = embedded_icc;
                        self.staged_source_image = Some(ci);
                        self.staged_img_size = Some(size);
                    } else {
                        self.full_images.insert(path.clone(), ci.clone());
                        self.embedded_icc_by_path.insert(path.clone(), embedded_icc);
                        let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                        self.thumbs.insert(path, ThumbState::Ready(tex));
                    }
                    self.mark_preview_dirty();
                }
            }
        }

        // Printer discovery — collect events first to avoid holding &self.discovery_rx
        let disc_events: Vec<DiscoveryEvent> = {
            let mut v = Vec::new();
            if let Some(rx) = &self.discovery_rx {
                while let Ok(ev) = rx.try_recv() { v.push(ev); }
            }
            v
        };
        let mut need_sync = false;
        for ev in disc_events {
            match ev {
                DiscoveryEvent::PrintersListed(p) => {
                    self.printers = p;
                    // Restore saved printer selection by name
                    if let Some(ref name) = self.pending_printer_name.clone() {
                        if let Some(idx) = self.printers.iter().position(|p| &p.name == name) {
                            self.printer_idx = idx;
                        }
                        self.pending_printer_name = None;
                    }
                    need_sync = true;
                }
                DiscoveryEvent::CapsReady(c) => {
                    self.all_caps.insert(c.name.clone(), c);
                    need_sync = true;
                }
                DiscoveryEvent::Warning(w) => self.log.push(format!("⚠ {w}")),
                DiscoveryEvent::Error(e)   => self.log.push(format!("✗ CUPS: {e}")),
            }
        }
        if need_sync { 
                self.sync_caps_to_selection();
                // Check if discovery is complete (all printers have caps)
                if !self.discovery_complete && !self.printers.is_empty() {
                    let all_have_caps = self.printers.iter().all(|p| self.all_caps.contains_key(&p.name));
                    if all_have_caps {
                        self.discovery_complete = true;
                        self.log.push("Ready to print!".to_string());
                    }
                }
            }

        // Process result
        if let Some(rx) = &self.proc_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((paths, target)) => {
                        match target {
                            ProcessTarget::Export => {
                                if let Some(first) = paths.first() {
                                    self.log.push(format!("✓ Saved {} page(s). First: {}", paths.len(), first.display()));
                                } else {
                                    self.log.push("✓ Export complete".into());
                                }
                                self.proc_state = ProcState::Done(paths);
                            }
                            ProcessTarget::Print => {
                                self.log.push(format!("✓ Processed {} page(s) for print", paths.len()));
                                self.pending_print_paths = paths;
                                self.show_print_confirm = true;
                                self.proc_state = ProcState::Idle;
                            }
                        }
                    }
                    Err(e) => {
                        self.log.push(format!("✗ {e}"));
                        self.proc_state = ProcState::Failed(e);
                    }
                }
                self.proc_rx = None;
            }
        }

        if matches!(self.proc_state, ProcState::Running) {
            ctx.request_repaint();
        }

        // Keep UI updating while print job is running
        if self.print_rx.is_some() {
            ctx.request_repaint();
        }

        // Check for ICC scan completion
        if self.icc_scan_pending {
            if let Some(ref rx) = self.icc_scan_rx {
                if let Ok(profiles) = rx.try_recv() {
                    self.icc_profiles = profiles;
                    self.icc_scan_pending = false;
                    self.icc_scan_rx = None;
                    // Save the user's preferred filter for restoration
                    self.saved_icc_filter_for_restore = self.icc_profile_filter;
                    // Start with All for window sizing
                    self.icc_profile_filter = IccProfileFilter::All;
                    // Restore saved filter after one frame
                    self.icc_auto_switch_pending = true;
                    self.show_icc_picker = true; // Open modal after scan completes
                }
            }
        }

        // Print job log messages
        if let Some(rx) = &self.print_log_rx {
            while let Ok(msg) = rx.try_recv() {
                self.log.push(msg);
            }
        }

        // Print job result
        if let Some(rx) = &self.print_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(()) => {
                        self.log.push("✓ Print jobs submitted successfully".into());
                    }
                    Err(e) => {
                        self.log.push(format!("✗ Print failed: {}", e));
                    }
                }
                self.print_rx = None;
                self.print_log_rx = None;
            }
        }
    }

    // ── Start processing ──────────────────────────────────────────────────────

    fn start_process_export(&mut self) {
        self.start_process_with_target(ProcessTarget::Export);
    }

    fn start_process_print(&mut self) {
        self.start_process_with_target(ProcessTarget::Print);
    }

    fn start_process_with_target(&mut self, target: ProcessTarget) {
        if self.queue.is_empty() {
            self.log.push("⚠ Queue is empty.".into());
            return;
        }

        let (page_w_px, page_h_px) = self.max_imageable_size_px();
        let (offset_x, offset_y) = self.border_offset_px();
        let max_page = self.queue.iter().map(|q| q.page).max().unwrap_or(0);
        let mut per_page: Vec<Vec<processor::PagePlacement>> = vec![Vec::new(); max_page.saturating_add(1)];
        for q in &self.queue {
            let (w, h) = self.queued_box_px(q);
            per_page[q.page].push(processor::PagePlacement {
                input: q.filepath.clone(),
                input_icc: q.source_icc.clone(),
                dest_x_px: q.position.x + offset_x,
                dest_y_px: q.position.y + offset_y,
                dest_w_px: w,
                dest_h_px: h,
                rotate_cw: q.rotation > 0.0,
            });
        }

        let stem = self
            .queue
            .first()
            .and_then(|q| q.filepath.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "vibeprint".to_string());

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let outputs: Vec<PathBuf> = per_page
            .iter()
            .enumerate()
            .map(|(idx, _)| match target {
                ProcessTarget::Export => self
                    .output_dir
                    .join(format!("{}_page_{:03}_vp.tif", stem, idx + 1)),
                ProcessTarget::Print => std::env::temp_dir().join(format!(
                    "vibeprint_{}_{}_page_{:03}.tif",
                    timestamp,
                    std::process::id(),
                    idx + 1
                )),
            })
            .collect();

        let output_icc = self.output_icc.as_ref().map(|e| e.path.clone());
        let target_dpi = self.target_dpi as f64;
        let intent = self.intent.to_lcms();
        let bpc = self.bpc;
        let engine = self.engine.to_proc();
        let depth = match target {
            ProcessTarget::Export => if self.depth16 { 16 } else { 8 },
            ProcessTarget::Print => 16,
        };
        let sharpen = self.sharpen;

        let target_clone = target.clone();
        let (tx, rx) = mpsc::channel::<Result<(Vec<PathBuf>, ProcessTarget), String>>();
        self.proc_rx = Some(rx);
        self.proc_state = ProcState::Running;
        thread::spawn(move || {
            let mut done = Vec::new();
            for (idx, placements) in per_page.into_iter().enumerate() {
                let out = outputs[idx].clone();
                let opts = processor::CompositePageOptions {
                    output: out.clone(),
                    placements,
                    page_w_px,
                    page_h_px,
                    output_icc: output_icc.clone(),
                    default_wide_output_when_unset: false,
                    target_dpi,
                    intent,
                    bpc,
                    engine: engine.clone(),
                    depth,
                    sharpen,
                };
                if let Err(e) = processor::process_composite_page(opts) {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
                done.push(out);
            }
            let result: Result<(Vec<PathBuf>, ProcessTarget), String> = Ok((done, target_clone));
            let _ = tx.send(result);
        });
    }

    // ── Left pane ─────────────────────────────────────────────────────────────

    fn draw_left(&mut self, ui: &mut egui::Ui) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

        // Init addr_bar on first draw
        if self.addr_bar.is_empty() {
            self.addr_bar = self.current_dir.to_string_lossy().into_owned();
        }

        // ── Toolbar ───────────────────────────────────────────────────────
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let can_back = !self.nav_history.is_empty();
            let can_fwd  = !self.nav_forward.is_empty();
            let btn_size = Vec2::new(24.0, 22.0);
            if ui.add_enabled(can_back, egui::Button::new("◀").min_size(btn_size))
                .on_hover_text("Back").clicked() { self.nav_back(); }
            if ui.add_enabled(can_fwd,  egui::Button::new("▶").min_size(btn_size))
                .on_hover_text("Forward").clicked() { self.nav_fwd(); }
            if ui.add(egui::Button::new("🏠").min_size(btn_size))
                .on_hover_text("Home").clicked() { self.navigate(home.clone()); }
        });

        // ── Address bar ───────────────────────────────────────────────────
        ui.add_space(2.0);
        let addr_resp = ui.add(
            egui::TextEdit::singleline(&mut self.addr_bar)
                .desired_width(ui.available_width())
                .font(egui::FontId::proportional(11.0))
                .hint_text("Path…"),
        );
        if addr_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let p = PathBuf::from(&self.addr_bar);
            if p.is_dir() {
                self.navigate(p);
            } else {
                self.addr_bar = self.current_dir.to_string_lossy().into_owned();
            }
        }

        // ── Places ────────────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.label(RichText::new("  PLACES").size(9.5).color(Color32::from_gray(130)));
        let places: &[(&str, fn() -> Option<PathBuf>)] = &[
            ("🏠  Home",        || dirs::home_dir()),
            ("🖥  Desktop",     || dirs::desktop_dir()),
            ("📁  Documents",   || dirs::document_dir()),
            ("📁  Downloads",   || dirs::download_dir()),
            ("🖼  Pictures",    || dirs::picture_dir()),
        ];
        for (label, get_path) in places {
            if let Some(path) = get_path() {
                if path.is_dir() {
                    let active = self.current_dir == path;
                    let text = RichText::new(*label).size(12.0);
                    if ui.selectable_label(active, text).clicked() && !active {
                        self.navigate(path);
                    }
                }
            }
        }

        ui.add_space(4.0);
        ui.label(RichText::new("  FOLDERS").size(9.5).color(Color32::from_gray(130)));

        // ── Folder tree ───────────────────────────────────────────────────
        let avail = ui.available_height();
        let tree_h = (avail * 0.42).max(80.0);
        let mut tree_nav:    Option<PathBuf>          = None;
        let mut tree_toggle: Option<(PathBuf, bool)>  = None;
        egui::ScrollArea::vertical()
            .id_salt("tree_scroll")
            .max_height(tree_h)
            .show(ui, |ui| {
                draw_tree_node(
                    ui, &home, 0,
                    &self.current_dir,
                    &self.tree_expanded,
                    &mut tree_nav,
                    &mut tree_toggle,
                );
            });
        if let Some((p, exp)) = tree_toggle { self.tree_expanded.insert(p, exp); }
        if let Some(p) = tree_nav           { self.navigate(p); }

        ui.separator();

        // ── Image count + zoom control ─────────────────────────────────
        let n = self.image_files.len();
        let cur_name = self.current_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.current_dir.to_string_lossy().into_owned());
        ui.horizontal(|ui| {
            ui.label(RichText::new(
                if n == 0 { format!("📂 {cur_name}  (no images)") }
                else      { format!("📂 {cur_name}  · {n} image{}", if n == 1 { "" } else { "s" }) }
            ).size(10.5).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+").on_hover_text("Larger thumbnails").clicked() {
                    self.thumb_zoom = (self.thumb_zoom + 0.25).min(3.0);
                    self.reload_thumbs();
                }
                if ui.small_button("−").on_hover_text("Smaller thumbnails").clicked() {
                    self.thumb_zoom = (self.thumb_zoom - 0.25).max(0.5);
                    self.reload_thumbs();
                }
            });
        });
        ui.add_space(2.0);

        // ── Thumbnail grid ────────────────────────────────────────────────
        egui::ScrollArea::vertical().id_salt("thumbs").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let files = self.image_files.clone();
                for path in &files {
                    let is_staged     = self.staged.as_ref()     == Some(path);
                    let is_hi         = self.highlighted.as_ref()  == Some(path);
                    let thumb_f = (THUMB_PX as f32 * self.thumb_zoom).round();

                    // Aspect-ratio-preserving display size; square placeholder while loading
                    let (disp_w, disp_h) = match self.thumbs.get(path) {
                        Some(ThumbState::Ready(tex)) => {
                            let [tw, th] = tex.size();
                            let scale = (thumb_f / tw.max(1) as f32)
                                .min(thumb_f / th.max(1) as f32);
                            ((tw as f32 * scale).round(), (th as f32 * scale).round())
                        }
                        _ => (thumb_f, thumb_f),
                    };

                    let cell_size = Vec2::new(disp_w, disp_h + 14.0);
                    let (resp, _) = ui.allocate_painter(cell_size, Sense::click());
                    let painter   = ui.painter_at(resp.rect);

                    let fill = if is_staged { Color32::from_rgb(255, 215, 0) }
                               else if is_hi { Color32::from_rgb(45, 55, 70) }
                               else { Color32::from_gray(40) };
                    painter.rect_filled(resp.rect, 4.0, fill);
                    if is_staged {
                        painter.rect_stroke(resp.rect, 4.0,
                            Stroke::new(1.5, Color32::from_rgb(160, 100, 0)));
                    } else if is_hi {
                        painter.rect_stroke(resp.rect, 4.0,
                            Stroke::new(1.5, Color32::from_rgb(100, 130, 180)));
                    }

                    let img_rect = Rect::from_min_size(resp.rect.min, Vec2::new(disp_w, disp_h));
                    match self.thumbs.get(path) {
                        Some(ThumbState::Ready(tex)) => {
                            painter.image(tex.id(), img_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE);
                        }
                        Some(ThumbState::Loading) | None => {
                            painter.text(img_rect.center(), egui::Align2::CENTER_CENTER,
                                "⏳", egui::FontId::proportional(18.0), Color32::GRAY);
                        }
                        Some(ThumbState::Failed) => {
                            painter.text(img_rect.center(), egui::Align2::CENTER_CENTER,
                                "✗", egui::FontId::proportional(18.0), Color32::RED);
                        }
                    }

                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    painter.text(
                        Pos2::new(resp.rect.min.x + 2.0, resp.rect.min.y + disp_h + 1.0),
                        egui::Align2::LEFT_TOP,
                        if name.len() > 12 { &name[..12] } else { name.as_ref() },
                        egui::FontId::proportional(9.0),
                        Color32::LIGHT_GRAY,
                    );

                    if resp.clicked() {
                        self.highlighted = Some(path.clone());
                        self.stage_image(path.clone());
                    }
                }
            });
        });
    }

    // ── Center pane / canvas ──────────────────────────────────────────────────

    fn draw_canvas(&mut self, ui: &mut egui::Ui) {
        // Paper dimensions in PostScript points — driven by selected_page_size_idx
        let selected_ps = self.caps.as_ref()
            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx));
        let (paper_w_pt, paper_h_pt) = selected_ps
            .map(|ps| ps.paper_size)
            .unwrap_or((612.0_f32, 792.0_f32));
        // Calculate user-adjusted imageable area in points
        let user_border_pt = self.user_border_in * 72.0; // Convert inches to points
        let (ia_l, ia_b, ia_r, ia_t) = (
            user_border_pt,                           // left
            user_border_pt,                           // bottom  
            paper_w_pt - user_border_pt,              // right
            paper_h_pt - user_border_pt               // top
        );

        let (resp, _) = ui.allocate_painter(ui.available_size(), Sense::click());
        let canvas_area = resp.rect;
        let inner = Rect::from_min_size(
            canvas_area.min + Vec2::new(RULER_PX, RULER_PX),
            canvas_area.size() - Vec2::splat(RULER_PX),
        );

        // Scale paper to fit inner area with padding
        let pad = 32.0_f32;
        let scale = ((inner.width() - pad * 2.0) / paper_w_pt)
            .min((inner.height() - pad * 2.0) / paper_h_pt);

        let paper_px_w = paper_w_pt * scale;
        let paper_px_h = paper_h_pt * scale;
        let paper_origin = inner.min + Vec2::new(
            (inner.width()  - paper_px_w) / 2.0,
            (inner.height() - paper_px_h) / 2.0,
        );
        let paper_rect = Rect::from_min_size(paper_origin, Vec2::new(paper_px_w, paper_px_h));

        let painter = ui.painter_at(canvas_area);

        // Background
        painter.rect_filled(canvas_area, 0.0, Color32::from_gray(58));

        // Paper (white)
        painter.rect_filled(paper_rect, 2.0, Color32::WHITE);
        painter.rect_stroke(paper_rect, 2.0, Stroke::new(1.0, Color32::from_gray(180)));

        // Imageable area — gray dashed outline
        let ia_rect = Rect::from_min_max(
            paper_origin + Vec2::new(ia_l * scale, (paper_h_pt - ia_t) * scale),
            paper_origin + Vec2::new(ia_r * scale, (paper_h_pt - ia_b) * scale),
        );
        draw_dashed_rect(&painter, ia_rect, Color32::from_rgba_premultiplied(80, 140, 220, 200), 1.5, 6.0);

        if self.preview_dirty || self.preview_cache_page != Some(self.current_page) {
            self.rebuild_canvas_texture(ui.ctx());
        }
        self.canvas_hit_rects.clear();
        let (ia_w_px, ia_h_px) = self.imageable_size_px();
        let sx = ia_rect.width() / ia_w_px.max(1) as f32;
        let sy = ia_rect.height() / ia_h_px.max(1) as f32;

        for item in self.queue.iter().filter(|q| q.page == self.current_page) {
            let (w_px, h_px) = self.queued_box_px(item);
            let r = Rect::from_min_size(
                Pos2::new(
                    ia_rect.min.x + item.position.x as f32 * sx,
                    ia_rect.min.y + item.position.y as f32 * sy,
                ),
                Vec2::new(w_px as f32 * sx, h_px as f32 * sy),
            );
            self.canvas_hit_rects.push((item.id, r));

            let src_size = item.src_size_px.or_else(|| {
                self.full_images
                    .get(&item.filepath)
                    .map(|img| (img.size[0] as u32, img.size[1] as u32))
            });
            let img_rect = src_size
                .map(|(sw, sh)| aspect_fit_rect_in_box(r, sw, sh, item.rotation > 0.0))
                .unwrap_or(r);

            if let Some(tex) = self.preview_textures.get(&item.filepath) {
                if item.rotation > 0.0 {
                    let mut mesh = egui::epaint::Mesh::with_texture(tex.id());
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_top(),     uv: Pos2::new(0.0, 1.0), color: Color32::WHITE });
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_top(),    uv: Pos2::new(0.0, 0.0), color: Color32::WHITE });
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_bottom(), uv: Pos2::new(1.0, 0.0), color: Color32::WHITE });
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_bottom(),  uv: Pos2::new(1.0, 1.0), color: Color32::WHITE });
                    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                    painter.add(egui::Shape::mesh(mesh));
                } else {
                    painter.image(tex.id(), img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
            } else {
                painter.rect_filled(r, 0.0, Color32::from_gray(220));
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(120)));
            }

            let stroke = if Some(item.id) == self.selected_queue_id {
                Stroke::new(2.0, Color32::from_rgb(90, 180, 255))
            } else {
                Stroke::new(1.0, Color32::from_rgba_premultiplied(80, 120, 170, 160))
            };
            painter.rect_stroke(r, 0.0, stroke);
        }

        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some((id, _)) = self
                    .canvas_hit_rects
                    .iter()
                    .rev()
                    .find(|(_, r)| r.contains(pos))
                    .copied()
                {
                    self.selected_queue_id = Some(id);
                    if let Some(item) = self.queue.iter().find(|q| q.id == id) {
                        self.current_page = item.page;
                    }
                    self.right_tab = RightTab::ImageProperties;
                }
            }
        }

        // Rulers — pass imageable-area boundaries as pixel offsets from paper origin
        let m_left   = ia_l * scale;
        let m_right  = ia_r * scale;
        let m_top    = (paper_h_pt - ia_t) * scale;
        let m_bottom = (paper_h_pt - ia_b) * scale;
        draw_ruler_h(&painter, canvas_area, paper_origin.x, paper_px_w, scale, RULER_PX, m_left, m_right);
        draw_ruler_v(&painter, canvas_area, paper_origin.y, paper_px_h, scale, RULER_PX, m_top, m_bottom);

        if self.softproof_enabled {
            painter.text(
                Pos2::new(canvas_area.max.x - 12.0, canvas_area.min.y + RULER_PX + 8.0),
                egui::Align2::RIGHT_TOP,
                "Softproof",
                egui::FontId::proportional(16.0),
                Color32::from_rgb(220, 90, 90),
            );
        }

        // Status overlay (page size + DPI)
        let info = if let Some(caps) = &self.caps {
            let ps = caps.page_sizes.get(self.selected_page_size_idx)
                .map(|p| p.label.as_str())
                .unwrap_or("?");
            let dpi = self.target_dpi;
            format!("{ps}  ·  {dpi} dpi  ·  Page {} of {}", self.current_page + 1, self.page_count)
        } else {
            format!("{:.0}×{:.0} pt  ·  {} dpi", paper_w_pt, paper_h_pt, self.target_dpi)
        };
        painter.text(
            canvas_area.max - Vec2::new(8.0, 8.0),
            egui::Align2::RIGHT_BOTTOM,
            &info,
            egui::FontId::proportional(11.0),
            Color32::from_gray(160),
        );
    }

    fn draw_canvas_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal_centered(|ui| {
            let has_image = !self.queue.is_empty() || self.selected_source_image.is_some();
            let icon = "🔍"; // magnifying glass icon
            let mut btn = egui::Button::new(
                RichText::new(icon).strong().size(21.0)
            ).min_size(Vec2::new(48.0, 36.0));
            if self.softproof_enabled {
                btn = btn.fill(Color32::from_rgb(60, 120, 200));
            }
            if ui.add_enabled(has_image, btn).clicked() {
                self.softproof_enabled = !self.softproof_enabled;
                self.mark_preview_dirty();
            }

            ui.add_space(16.0);
            let prev = ui.add_enabled(self.current_page > 0, egui::Button::new("◀ Previous Page"));
            if prev.clicked() {
                self.current_page = self.current_page.saturating_sub(1);
                self.mark_preview_dirty();
            }
            ui.label(format!("Page {} of {}", self.current_page + 1, self.page_count.max(1)));
            let next = ui.add_enabled(self.current_page + 1 < self.page_count, egui::Button::new("Next Page ▶"));
            if next.clicked() {
                self.current_page = (self.current_page + 1).min(self.page_count.saturating_sub(1));
                self.mark_preview_dirty();
            }
        });
        ui.add_space(2.0);
    }

    // ── Right pane / sidebar ──────────────────────────────────────────────────

    fn draw_right(&mut self, ui: &mut egui::Ui) {
        // ── Tab bar ───────────────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.right_tab, RightTab::PrinterSettings, "Printer Settings",
            );
            ui.selectable_value(
                &mut self.right_tab, RightTab::ImageProperties, "Image Properties",
            );
            ui.selectable_value(
                &mut self.right_tab, RightTab::ImageQueue, "Image Queue",
            );
        });
        ui.separator();

        match self.right_tab {
            RightTab::PrinterSettings => {
                // ── Settings Section (Top - Scrollable) ─────────────────────────────
                let available_height = ui.available_height();
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .max_height(available_height * 0.6)
                    .show(ui, |ui| {
                        self.draw_tab_printer(ui);
                    });

                // ── Print Section (Bottom - Fixed) ─────────────────────────────────────
                ui.separator();
                self.draw_print_controls(ui);
            }
            RightTab::ImageProperties => {
                self.draw_tab_image(ui);
            }
            RightTab::ImageQueue => {
                self.draw_tab_queue(ui);
            }
        }
    }

    fn draw_tab_printer(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("tab_printer_scroll").show(ui, |ui| {
            ui.add_space(6.0);
            let mut preview_dirty = false;

            // ── Block A: Hardware ─────────────────────────────────────────────
            ui.label(RichText::new("Hardware & Properties").strong().size(12.0));
            ui.separator();

            let prev_idx = self.printer_idx;
            let prev_page_size_idx = self.selected_page_size_idx;
            let prev_dpi = self.target_dpi;
            ui.horizontal(|ui| {
                let selected_name = self.printers.get(self.printer_idx)
                    .map(|p| p.name.as_str())
                    .unwrap_or("No printer found");
                egui::ComboBox::from_id_salt("printer_cb")
                    .width(ui.available_width() - 36.0)
                    .selected_text(selected_name)
                    .show_ui(ui, |ui| {
                        for (i, p) in self.printers.iter().enumerate() {
                            let label = if p.is_default {
                                format!("★ {}", p.name)
                            } else {
                                p.name.clone()
                            };
                            ui.selectable_value(&mut self.printer_idx, i, label);
                        }
                    });
                if ui.small_button("⚙").on_hover_text("Printer properties").clicked() {
                    self.show_props = true;
                }
            });
            if self.printer_idx != prev_idx {
                self.sync_caps_to_selection();
            }

            // ── Paper Size ────────────────────────────────────────────
            if let Some(caps) = &self.caps {
                // Use real caps when available (discovery is complete)
                let ps_label = caps.page_sizes
                    .get(self.selected_page_size_idx)
                    .map(|p| p.label.as_str())
                    .unwrap_or("—");
                ui.horizontal(|ui| {
                    ui.label("Paper Size:");
                    egui::ComboBox::from_id_salt("paper_size_cb")
                        .selected_text(ps_label)
                        .show_ui(ui, |ui| {
                            for i in 0..caps.page_sizes.len() {
                                let label = caps.page_sizes[i].label.clone();
                                ui.selectable_value(&mut self.selected_page_size_idx, i, label);
                            }
                        });
                });
            }

            // ---- Border ----
            ui.horizontal(|ui| {
                ui.label("Border:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.border_edit_string)
                        .desired_width(60.0)
                        .font(egui::FontId::proportional(12.0)),
                );
                
                // Update edit string when gaining focus to show current value
                if resp.gained_focus() {
                    self.border_edit_string = format!("{:.3}", self.user_border_in);
                }
                
                // Apply changes when losing focus
                if resp.lost_focus() {
                    if let Ok(v) = self.border_edit_string.parse::<f32>() {
                        // Calculate maximum border (25% of smaller paper dimension)
                        let (paper_w_in, paper_h_in) = self.caps
                            .as_ref()
                            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
                            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
                            .unwrap_or((8.5, 11.0));
                        let max_border = (paper_w_in.min(paper_h_in) * 0.25).max(self.reported_border_in);
                        
                        let new_border = v.clamp(self.reported_border_in, max_border);
                        if (new_border - self.user_border_in).abs() > 0.0001 {
                            self.user_border_in = new_border;
                            self.border_edit_string = format!("{:.3}", self.user_border_in);
                            self.relayout_queue();
                        } else if (v - self.user_border_in).abs() > 0.0001 {
                            // Update edit string to show clamped value if user tried to exceed limits
                            self.border_edit_string = format!("{:.3}", self.user_border_in);
                        }
                    } else {
                        // Reset to current value if invalid input
                        self.border_edit_string = format!("{:.3}", self.user_border_in);
                    }
                }
                
                if ui.small_button("✖").on_hover_text("Reset to printer default").clicked() {
                    self.user_border_in = self.reported_border_in;
                    self.border_edit_string = format!("{:.3}", self.user_border_in);
                    self.relayout_queue();
                }
                ui.label("in");
            });

            // ---- Print to file ----───────────────────────────────────────────
            ui.checkbox(&mut self.print_to_file, "Print to file");

            ui.add_space(10.0);

            // ── Block B: Processing Engine ────────────────────────────────────
            ui.label(RichText::new("Processing Engine").strong().size(12.0));
            ui.separator();

            // Interpolate
            ui.horizontal(|ui| {
                ui.label("Interpolate:");
                egui::ComboBox::from_id_salt("engine_cb")
                    .selected_text(self.engine.label())
                    .show_ui(ui, |ui| {
                        for e in Engine::ALL {
                            ui.selectable_value(&mut self.engine, e.clone(), e.label());
                        }
                    });
            });

            if self.selected_page_size_idx != prev_page_size_idx {
                // Recalculate reported border for new page size, but preserve user border
                // if it's still >= the new reported minimum
                let new_reported = self.calc_reported_border();
                self.reported_border_in = new_reported;
                // Ensure user border respects the new minimum
                if self.user_border_in < new_reported {
                    self.user_border_in = new_reported;
                }
                // Update edit string to reflect any changes
                self.border_edit_string = format!("{:.3}", self.user_border_in);
                self.relayout_queue();
            }

            // Sharpen
            ui.horizontal(|ui| {
                ui.label("Sharpen:");
                ui.add(egui::Slider::new(&mut self.sharpen, 0..=20).show_value(true));
            });

            // Output DPI
            ui.horizontal(|ui| {
                ui.label("Output DPI:");
                egui::ComboBox::from_id_salt("dpi_cb")
                    .selected_text(format!("{}", self.target_dpi))
                    .show_ui(ui, |ui| {
                        for &dpi in &[300u32, 360, 600, 720] {
                            ui.selectable_value(&mut self.target_dpi, dpi, format!("{dpi}"));
                        }
                    });
            });

            if self.target_dpi != prev_dpi {
                self.relayout_queue();
            }

            ui.add_space(10.0);

            // ── Block C: Color Management ─────────────────────────────────────
            ui.label(RichText::new("Color Management").strong().size(12.0));

            // Output ICC
            ui.horizontal(|ui| {
                ui.label("Output ICC:");
                let icc_label = self.output_icc.as_ref()
                    .map(|e| e.description.clone())
                    .unwrap_or_else(|| "sRGB".into());
                ui.add(egui::Label::new(
                    RichText::new(&icc_label).small().monospace()
                ).truncate());
                if self.icc_scan_pending {
                    ui.label("Scanning...");
                } else if ui.small_button("…").clicked() {
                    // Start ICC scan before opening modal
                    let (tx, rx) = mpsc::channel::<Vec<IccProfileEntry>>();
                    self.icc_scan_rx = Some(rx);
                    self.icc_scan_pending = true;
                    self.icc_profiles.clear();
                    self.icc_filter_text.clear();
                    thread::spawn(move || scan_icc_directories(tx));
                }
                if self.output_icc.is_some() && ui.small_button("✖").clicked() {
                    self.output_icc = None;
                    preview_dirty = true;
                }
            });

            // Intent
            let prev_intent = self.intent;
            ui.horizontal(|ui| {
                ui.label("Intent:");
                egui::ComboBox::from_id_salt("intent_cb")
                    .selected_text(self.intent.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.intent, Intent::Relative,   Intent::Relative.label());
                        ui.selectable_value(&mut self.intent, Intent::Perceptual, Intent::Perceptual.label());
                        ui.selectable_value(&mut self.intent, Intent::Saturation, Intent::Saturation.label());
                    });
            });
            if self.intent != prev_intent {
                preview_dirty = true;
            }

            if ui.checkbox(&mut self.bpc, "Black Point Compensation").changed() {
                preview_dirty = true;
            }

            if preview_dirty {
                self.mark_preview_dirty();
            }

            ui.add_space(10.0);

            
        let is_running = matches!(self.proc_state, ProcState::Running);
        let is_printing = self.print_rx.is_some();
        let has_image = !self.queue.is_empty();

        // Primary: Print button (dynamic text based on print_to_file)
        let btn_text = if self.print_to_file { "Print to File" } else { "Print" };
        let print_btn = egui::Button::new(
            RichText::new(btn_text).size(14.0).strong(),
        )
        .min_size(Vec2::new(ui.available_width(), 36.0))
        .fill(Color32::from_rgb(60, 120, 200));

        if ui.add_enabled(has_image && !is_running && !is_printing, print_btn).clicked() {
            if self.print_to_file {
                self.start_process_export();
            } else {
                self.start_process_print();
            }
        }

        ui.add_space(4.0);

        if is_running {
            ui.horizontal(|ui| { ui.spinner(); ui.label("Processing…"); });
        } else if is_printing {
            ui.horizontal(|ui| { ui.spinner(); ui.label("Printing…"); });
        } else if !has_image {
            ui.label(RichText::new("Add queued images first").small().weak());
        } else if let ProcState::Done(ref paths) = self.proc_state {
            let msg = if let Some(first) = paths.first() {
                format!("✓ {} page(s): {}", paths.len(), first.file_name().unwrap_or_default().to_string_lossy())
            } else {
                "✓ Done".to_string()
            };
            ui.label(RichText::new(msg).small().color(Color32::GREEN));
        } else if let ProcState::Failed(ref e) = self.proc_state {
            ui.label(RichText::new(format!("✗ {e}")).small().color(Color32::RED));
        }
        });
    }

    fn draw_print_controls(&mut self, ui: &mut egui::Ui) {
        // Output folder section - identical widgets in both cases for perfect height matching
        if self.print_to_file {
            ui.label(RichText::new("Output Folder").strong().size(12.0));
        } else {
            ui.label(RichText::new("Output Folder").strong().size(12.0).color(Color32::TRANSPARENT));
        }
        if self.print_to_file {
            ui.separator();
        } else {
            // Invisible separator - just add space to maintain height
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            let label = self.output_dir.to_string_lossy();
            if self.print_to_file {
                ui.add(egui::Label::new(
                    RichText::new(label.as_ref()).small().monospace()
                ).truncate());
                if ui.small_button("…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.output_dir = p;
                    }
                }
            } else {
                // Same widgets, invisible content - guarantees identical height
                ui.add(egui::Label::new(
                    RichText::new(label.as_ref()).small().monospace().color(Color32::TRANSPARENT)
                ).truncate());
                let _ = ui.label(RichText::new("…").color(Color32::TRANSPARENT));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if self.print_to_file {
                ui.label("Output depth:");
            } else {
                ui.label(RichText::new("Output depth:").color(Color32::TRANSPARENT));
            }
            if self.print_to_file {
                ui.selectable_value(&mut self.depth16, true,  "16-bit");
                ui.selectable_value(&mut self.depth16, false, "8-bit Dithered");
            } else {
                // Same widgets, invisible content - guarantees identical height
                let _ = ui.label(RichText::new("16-bit").color(Color32::TRANSPARENT));
                let _ = ui.label(RichText::new("8-bit Dithered").color(Color32::TRANSPARENT));
            }
        });

        ui.add_space(4.0);

        // ── Log (at the bottom) ───────────────────────────────────────────────
        ui.add_space(12.0);
        ui.label(RichText::new("Log").strong().size(12.0));
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("log_scroll")
            .max_height(80.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &self.log {
                    ui.label(RichText::new(entry).small().monospace());
                }
            });

    }

    fn draw_tab_image(&mut self, ui: &mut egui::Ui) {
        let (ia_w_in, ia_h_in) = self.imageable_size_in();

        ui.add_space(4.0);
        ui.label(RichText::new("Print Size").strong().size(12.0));
        ui.separator();

        let has_target = self.staged.is_some() || self.selected_queue_id.is_some();
        if !has_target {
            ui.add_space(8.0);
            ui.label(RichText::new("Stage an image or select one from queue").weak().italics().size(11.0));
            return;
        }

        ui.label(
            RichText::new(format!("Printable area: {:.2}\" × {:.2}\"", ia_w_in, ia_h_in))
                .size(10.0)
                .weak(),
        );
        ui.add_space(4.0);

        // Determine the currently selected size index for queued images
        let selected_size_idx = if self.staged.is_some() {
            None // No highlighting for staged images
        } else if let Some(qi) = self.selected_queue() {
            if qi.fit_to_page {
                Some(FIT_PAGE_IDX)
            } else {
                let (qw, qh) = qi.size.as_inches();
                // Find matching size index with approximate comparison
                PRINT_SIZES.iter().enumerate().find_map(|(i, &(w, h, _))| {
                    if (qw - w).abs() < 0.001 && (qh - h).abs() < 0.001 {
                        Some(i)
                    } else {
                        None
                    }
                })
            }
        } else {
            None
        };

        egui::ScrollArea::vertical().id_salt("print_sizes").show(ui, |ui| {
            for (idx, &(w, h, label)) in PRINT_SIZES.iter().enumerate() {
                let (fits, _) = check_size_fit(w, h, ia_w_in, ia_h_in);
                let is_selected = selected_size_idx == Some(idx);
                let row_text = RichText::new(label).size(13.0).color(if is_selected {
                    Color32::from_rgb(100, 200, 100) // Green highlight for selected size
                } else if fits {
                    Color32::from_gray(210)
                } else {
                    Color32::from_rgb(200, 60, 60)
                });
                let resp = ui.add_enabled(fits, egui::SelectableLabel::new(false, row_text));
                if !fits {
                    resp.clone().on_disabled_hover_text("Too large for the printable area");
                }
                if resp.clicked() {
                    if self.staged.is_some() {
                        let _ = self.enqueue_staged_with_idx(idx);
                    } else {
                        self.update_selected_queue_size_idx(idx);
                    }
                }
            }

            ui.separator();
            let is_fit_to_page_selected = selected_size_idx == Some(FIT_PAGE_IDX);
            let fit_text = RichText::new("Fit to Page").size(13.0).color(if is_fit_to_page_selected {
                Color32::from_rgb(100, 200, 100) // Green highlight for selected fit-to-page
            } else {
                Color32::from_gray(210)
            });
            if ui.selectable_label(false, fit_text).clicked() {
                if self.staged.is_some() {
                    let _ = self.enqueue_staged_with_idx(FIT_PAGE_IDX);
                } else {
                    self.update_selected_queue_size_idx(FIT_PAGE_IDX);
                }
            }
        });
    }

    fn draw_tab_queue(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("Queued Images").strong().size(12.0));
        ui.separator();

        if self.queue.is_empty() {
            ui.label(RichText::new("Queue is empty").weak().italics().size(11.0));
            return;
        }

        let mut delete_id: Option<Uuid> = None;
        let rows: Vec<(Uuid, PathBuf, usize)> = self
            .queue
            .iter()
            .map(|q| (q.id, q.filepath.clone(), q.page))
            .collect();
        egui::ScrollArea::vertical().id_salt("queue_list").show(ui, |ui| {
            for (id, path, page) in &rows {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());

                ui.horizontal(|ui| {
                    let sel = self.selected_queue_id == Some(*id);
                    let lbl = format!("{}  (P{})", name, *page + 1);
                    if ui.selectable_label(sel, lbl).clicked() {
                        self.selected_queue_id = Some(*id);
                        self.current_page = *page;
                        self.right_tab = RightTab::ImageProperties;
                    }
                    if ui.small_button("✖").clicked() {
                        delete_id = Some(*id);
                    }
                });
            }
        });

        if let Some(id) = delete_id {
            self.queue.retain(|q| q.id != id);
            if self.selected_queue_id == Some(id) {
                self.selected_queue_id = None;
            }
            self.relayout_queue();
        }
    }

    // ── Printer Properties modal ──────────────────────────────────────────────

    fn show_printer_props(&mut self, ctx: &Context) {
        let Some(caps) = self.caps.clone() else { self.show_props = false; return };
        let prev_page_size = self.selected_page_size_idx;

        // Calculate dynamic size based on content and screen
        let num_extra = caps.extra_options.len();
        let base_height = if num_extra > 0 { 280.0 } else { 180.0 };
        let extra_height = (num_extra as f32 * 28.0).min(350.0); // ~28px per row, max 350
        let content_height = base_height + extra_height;
        
        // Use percentage of screen with min/max constraints
        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.35).clamp(340.0, 520.0);
        let height = (screen.height() * 0.7).clamp(220.0, content_height.max(400.0));

        egui::Window::new(format!("Properties — {}", caps.name))
            .collapsible(false)
            .resizable(true)
            .default_size([width, height])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Use percentage-based widths for dropdowns
                let combo_width = ui.available_width() * 0.55;
                
                egui::Grid::new("props_grid")
                    .num_columns(2)
                    .spacing([ui.available_width() * 0.03, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        // ── Media Type ───────────────────────────────────
                        ui.label("Media Type:");
                        if caps.media_types.is_empty() {
                            ui.label(RichText::new("—").weak());
                        } else {
                            egui::ComboBox::from_id_salt("props_media")
                                .width(combo_width)
                                .selected_text(
                                    caps.media_types.get(self.props_media_idx)
                                        .map(|s| s.as_str()).unwrap_or("—")
                                )
                                .show_ui(ui, |ui| {
                                    for (i, m) in caps.media_types.iter().enumerate() {
                                        ui.selectable_value(&mut self.props_media_idx, i, m);
                                    }
                                });
                        }
                        ui.end_row();

                        // ── Paper Size ───────────────────────────────────
                        ui.label("Paper Size:");
                        if caps.page_sizes.is_empty() {
                            ui.label(RichText::new("—").weak());
                        } else {
                            let ps_label = caps.page_sizes
                                .get(self.selected_page_size_idx)
                                .map(|p| p.label.as_str()).unwrap_or("—");
                            egui::ComboBox::from_id_salt("props_paper")
                                .width(combo_width)
                                .selected_text(ps_label)
                                .show_ui(ui, |ui| {
                                    for i in 0..caps.page_sizes.len() {
                                        let label = caps.page_sizes[i].label.clone();
                                        ui.selectable_value(
                                            &mut self.selected_page_size_idx, i, label
                                        );
                                    }
                                });
                        }
                        ui.end_row();

                        // ── Input Slot / Source Tray ─────────────────────
                        if !caps.input_slots.is_empty() {
                            ui.label("Input Slot:");
                            egui::ComboBox::from_id_salt("props_slot")
                                .width(combo_width)
                                .selected_text(
                                    caps.input_slots.get(self.props_slot_idx)
                                        .map(|s| s.as_str()).unwrap_or("—")
                                )
                                .show_ui(ui, |ui| {
                                    for (i, s) in caps.input_slots.iter().enumerate() {
                                        ui.selectable_value(&mut self.props_slot_idx, i, s);
                                    }
                                });
                            ui.end_row();
                        }
                    });

                // ── Additional CUPS options ───────────────────────────────────
                if !caps.extra_options.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new("Advanced / Printer-specific").small().weak());
                    ui.add_space(4.0);

                    // Dynamic scroll area filling remaining space
                    let remaining = ui.available_height() - 40.0; // Reserve for Close button
                    egui::ScrollArea::vertical()
                        .id_salt("props_extra_scroll")
                        .max_height(remaining.max(100.0))
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let extra_combo_width = ui.available_width() * 0.50;
                            egui::Grid::new("props_extra_grid")
                                .num_columns(2)
                                .spacing([ui.available_width() * 0.03, 6.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    for opt in &caps.extra_options {
                                        // Truncate very long labels
                                        let label_text = if opt.label.len() > 35 {
                                            format!("{:.32}...", opt.label)
                                        } else {
                                            opt.label.clone()
                                        };
                                        ui.label(label_text);
                                        
                                        let idx = self.extra_option_indices
                                            .entry(opt.key.clone())
                                            .or_insert(opt.default_idx);
                                        let sel_text = opt.choices.get(*idx)
                                            .map(|(_, l)| {
                                                if l.len() > 30 {
                                                    format!("{:.27}...", l)
                                                } else {
                                                    l.clone()
                                                }
                                            })
                                            .unwrap_or("—".to_string());
                                        
                                        egui::ComboBox::from_id_salt(
                                            format!("props_extra_{}", opt.key)
                                        )
                                        .width(extra_combo_width)
                                        .selected_text(sel_text)
                                        .show_ui(ui, |ui| {
                                            for (i, (_, cl)) in opt.choices.iter().enumerate() {
                                                let display_cl = if cl.len() > 35 {
                                                    format!("{:.32}...", cl)
                                                } else {
                                                    cl.clone()
                                                };
                                                ui.selectable_value(idx, i, display_cl);
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                        });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_props = false;
                        }
                    });
                });
            });

        if self.selected_page_size_idx != prev_page_size {
            self.relayout_queue();
        }
    }

    // ── Print Confirmation Modal ──────────────────────────────────────────────

    fn show_print_confirm(&mut self, ctx: &Context) {
        let Some(caps) = self.caps.clone() else { self.show_print_confirm = false; return };
        let printer_name = self.printers.get(self.printer_idx).map(|p| p.name.clone());
        let Some(printer_name) = printer_name else { self.show_print_confirm = false; return };
        if self.pending_print_paths.is_empty() { self.show_print_confirm = false; return };
        let temp_paths = self.pending_print_paths.clone();

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.30).clamp(320.0, 480.0);

        egui::Window::new("Confirm Print")
            .collapsible(false)
            .resizable(false)
            .fixed_size([width, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);

                // Printer info
                ui.label(RichText::new("Printer:").small().weak());
                ui.label(&printer_name);
                ui.add_space(4.0);

                // Paper size
                let paper = caps.page_sizes.get(self.selected_page_size_idx)
                    .map(|p| p.label.as_str())
                    .unwrap_or("—");
                ui.label(RichText::new("Paper:").small().weak());
                ui.label(paper);
                ui.add_space(4.0);
                ui.label(RichText::new("Pages:").small().weak());
                ui.label(format!("{}", temp_paths.len()));
                ui.add_space(4.0);

                // Media type
                if !caps.media_types.is_empty() {
                    let media = caps.media_types.get(self.props_media_idx)
                        .map(|m| m.as_str())
                        .unwrap_or("—");
                    ui.label(RichText::new("Media Type:").small().weak());
                    ui.label(media);
                    ui.add_space(4.0);
                }

                // Input slot
                if !caps.input_slots.is_empty() {
                    let slot = caps.input_slots.get(self.props_slot_idx)
                        .map(|s| s.as_str())
                        .unwrap_or("—");
                    ui.label(RichText::new("Input Slot:").small().weak());
                    ui.label(slot);
                    ui.add_space(4.0);
                }

                // Extra options summary (if any)
                if !caps.extra_options.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Additional Options:").small().weak());
                    egui::ScrollArea::vertical()
                        .max_height(80.0)
                        .show(ui, |ui| {
                            for opt in &caps.extra_options {
                                if let Some(&idx) = self.extra_option_indices.get(&opt.key) {
                                    if let Some((_, label)) = opt.choices.get(idx) {
                                        ui.label(format!("• {}: {}", opt.label, label));
                                    }
                                }
                            }
                        });
                }

                ui.add_space(12.0);

                // Buttons
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_print_confirm = false;
                            for p in &temp_paths {
                                let _ = std::fs::remove_file(p);
                            }
                            self.pending_print_paths.clear();
                        }
                        
                        let print_btn = ui.add(egui::Button::new(
                            RichText::new("Print").strong().color(Color32::WHITE)
                        ));
                        if print_btn.clicked() {
                            self.show_print_confirm = false;
                            // Spawn print job in background thread
                            let temp_paths_clone = temp_paths.clone();
                            let (tx, rx) = mpsc::channel::<Result<(), String>>();
                            let (log_tx, log_rx) = mpsc::channel::<String>();
                            self.print_rx = Some(rx);
                            self.print_log_rx = Some(log_rx);
                            self.log.push("Submitting print jobs...".into());
                            let caps = self.caps.clone();
                            let printer_idx = self.printer_idx;
                            let printers = self.printers.clone();
                            let selected_page_size_idx = self.selected_page_size_idx;
                            let props_media_idx = self.props_media_idx;
                            let props_slot_idx = self.props_slot_idx;
                            let extra_option_indices = self.extra_option_indices.clone();
                            thread::spawn(move || {
                                let result = submit_print_jobs_sync(
                                    &temp_paths_clone,
                                    caps,
                                    printer_idx,
                                    &printers,
                                    selected_page_size_idx,
                                    props_media_idx,
                                    props_slot_idx,
                                    &extra_option_indices,
                                    &log_tx,
                                );
                                let _ = tx.send(result);
                            });
                            self.pending_print_paths.clear();
                        }
                    });
                });
            });
    }

    // ── ICC Profile Picker Modal ───────────────────────────────────────────────

    fn show_icc_picker(&mut self, ctx: &Context) {
        // Auto-switch to saved filter after first frame (for proper window sizing)
        if self.icc_auto_switch_pending {
            self.icc_profile_filter = self.saved_icc_filter_for_restore;
            self.icc_auto_switch_pending = false;
        }

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.50).clamp(600.0, 900.0);
        let height = (screen.height() * 0.70).clamp(500.0, 700.0);

        egui::Window::new("Select ICC Profile")
            .collapsible(false)
            .resizable(false)
            .min_size([width, height])
            .max_size([width, height])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);

                // Profile filter radio buttons
                ui.horizontal(|ui| {
                    ui.label("Show:");
                    let previous_filter = self.icc_profile_filter;
                    ui.radio_value(&mut self.icc_profile_filter, IccProfileFilter::All, "All profiles");
                    ui.radio_value(&mut self.icc_profile_filter, IccProfileFilter::System, "System level profiles");
                    ui.radio_value(&mut self.icc_profile_filter, IccProfileFilter::User, "User profiles");
                    if previous_filter != self.icc_profile_filter {
                        let printer_name = self.printers.get(self.printer_idx).map(|p| p.name.clone());
                        let engine_str = match self.engine {
                            Engine::Lanczos3     => "lanczos3",
                            Engine::Iterative    => "iterative",
                            Engine::RobidouxEwa  => "robidoux",
                            Engine::Mks          => "mks",
                        };
                        let intent_str = match self.intent {
                            Intent::Perceptual  => "perceptual",
                            Intent::Saturation  => "saturation",
                            Intent::Relative    => "relative",
                        };
                        save_settings(&Settings {
                            current_dir:   Some(self.current_dir.to_string_lossy().into_owned()),
                            printer_name,
                            page_size_name: self.caps.as_ref()
                                .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
                                .map(|ps| ps.name.clone()),
                            engine:        Some(engine_str.into()),
                            sharpen:       Some(self.sharpen),
                            depth16:       Some(self.depth16),
                            target_dpi:    Some(self.target_dpi),
                            output_icc:    self.output_icc.as_ref().map(|e| e.path.to_string_lossy().into_owned()),
                            intent:        Some(intent_str.into()),
                            bpc:           Some(self.bpc),
                            output_dir:    Some(self.output_dir.to_string_lossy().into_owned()),
                            user_border_in: Some(self.user_border_in),
                            icc_filter:    Some(match self.icc_profile_filter {
                                IccProfileFilter::All => "all",
                                IccProfileFilter::System => "system",
                                IccProfileFilter::User => "user",
                            }.to_string()),
                        });
                    }
                });

                ui.add_space(8.0);

                // Search/filter input
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.icc_filter_text)
                            .hint_text("Search profiles...")
                            .desired_width(f32::INFINITY)
                    );
                });

                ui.add_space(8.0);

                // Profile list
                let filter_lower = self.icc_filter_text.to_lowercase();
                let filtered: Vec<&IccProfileEntry> = self.icc_profiles
                    .iter()
                    .filter(|p| {
                        // Filter by location (radio button selection)
                        let location_match = match self.icc_profile_filter {
                            IccProfileFilter::All => true,
                            IccProfileFilter::System => p.source == IccProfileSource::System,
                            IccProfileFilter::User => p.source == IccProfileSource::User,
                        };

                        // Filter by text search
                        let text_match = filter_lower.is_empty()
                            || p.description.to_lowercase().contains(&filter_lower)
                            || p.file_name().to_lowercase().contains(&filter_lower)
                            || p.location().to_lowercase().contains(&filter_lower);

                        location_match && text_match
                    })
                    .collect();

                let mut selected_path: Option<PathBuf> = None;

                // Calculate scroll area height to prevent window resizing
                // Account for header, radio buttons, filter, and footer
                let scroll_height = height - 200.0; // Reserve space for UI elements

                egui::ScrollArea::vertical()
                    .max_height(scroll_height)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if filtered.is_empty() {
                            ui.centered_and_justified(|ui| {
                                if self.icc_profiles.is_empty() {
                                    ui.label("No ICC profiles found in standard directories.");
                                } else {
                                    ui.label("No profiles match your filter.");
                                }
                            });
                        } else {
                            use egui_extras::{TableBuilder, Column};

                            // Calculate fixed column widths based on modal width
                            let table_width = ui.available_width();
                            let desc_width = table_width * 0.50;
                            let loc_width = table_width * 0.35;
                            let file_width = table_width * 0.15;

                            TableBuilder::new(ui)
                                .striped(true)
                                .resizable(true)
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .column(Column::initial(desc_width))
                                .column(Column::initial(loc_width))
                                .column(Column::initial(file_width))
                                .column(Column::remainder())
                                .header(20.0, |mut header| {
                                    header.col(|ui| {
                                        ui.strong("Description");
                                    });
                                    header.col(|ui| {
                                        ui.strong("Location");
                                    });
                                    header.col(|ui| {
                                        ui.strong("Filename");
                                    });
                                    header.col(|ui| {
                                        ui.strong("Date");
                                    });
                                })
                                .body(|mut body| {
                                    for profile in filtered {
                                        body.row(22.0, |mut row| {
                                            row.col(|ui| {
                                                if ui.selectable_label(false, &profile.description).clicked() {
                                                    selected_path = Some(profile.path.clone());
                                                }
                                            });
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(
                                                    RichText::new(&profile.location())
                                                        .weak()
                                                ).truncate());
                                            });
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(
                                                    RichText::new(profile.file_name())
                                                        .monospace()
                                                        .weak()
                                                ).truncate());
                                            });
                                            row.col(|ui| {
                                                ui.label(
                                                    RichText::new(&profile.date)
                                                        .weak()
                                                );
                                            });
                                        });
                                    }
                                });
                        }
                    });

                // Apply selection outside the closure to avoid borrow issues
                if let Some(path) = selected_path {
                    // Find the full entry with description
                    if let Some(entry) = self.icc_profiles.iter().find(|e| e.path == path) {
                        self.output_icc = Some(entry.clone());
                    } else {
                        // Fallback: create entry without description (shouldn't happen)
                        let date = extract_file_date(&path);
                        let description = path.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string();
                        self.output_icc = Some(IccProfileEntry { path, description, date, source: IccProfileSource::User });
                    }
                    self.show_icc_picker = false;
                    self.mark_preview_dirty();
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                // Browse button for non-standard locations
                ui.horizontal(|ui| {
                    if ui.button("Browse for File...").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("ICC Profile", &["icc", "icm"])
                            .pick_file()
                        {
                            // Extract description from the selected file
                            let date = extract_file_date(&p);
                            let description = if let Ok(bytes) = std::fs::read(&p) {
                                if let Ok(profile) = lcms2::Profile::new_icc(&bytes) {
                                    profile.info(lcms2::InfoType::Description, lcms2::Locale::none())
                                        .unwrap_or_else(|| p.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string())
                                } else {
                                    p.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string()
                                }
                            } else {
                                p.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string()
                            };
                            self.output_icc = Some(IccProfileEntry { path: p, description, date, source: IccProfileSource::User });
                            self.show_icc_picker = false;
                            self.mark_preview_dirty();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_icc_picker = false;
                            self.icc_scan_rx = None;
                            self.icc_scan_pending = false;
                        }
                    });
                });
            });
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let engine_str = match self.engine {
            Engine::Lanczos3     => "lanczos3",
            Engine::Iterative    => "iterative",
            Engine::RobidouxEwa  => "robidoux",
            Engine::Mks          => "mks",
        };
        let intent_str = match self.intent {
            Intent::Perceptual  => "perceptual",
            Intent::Saturation  => "saturation",
            Intent::Relative    => "relative",
        };
        let icc_filter_str = match self.icc_profile_filter {
            IccProfileFilter::All => "all",
            IccProfileFilter::System => "system",
            IccProfileFilter::User => "user",
        };
        let printer_name = self.printers.get(self.printer_idx).map(|p| p.name.clone());
        save_settings(&Settings {
            current_dir:   Some(self.current_dir.to_string_lossy().into_owned()),
            printer_name,
            page_size_name: self.caps.as_ref()
                .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
                .map(|ps| ps.name.clone()),
            engine:    Some(engine_str.into()),
            sharpen:   Some(self.sharpen),
            depth16:   Some(self.depth16),
            target_dpi: Some(self.target_dpi),
            output_icc: self.output_icc.as_ref().map(|e| e.path.to_string_lossy().into_owned()),
            intent:    Some(intent_str.into()),
            bpc:       Some(self.bpc),
            output_dir: Some(self.output_dir.to_string_lossy().into_owned()),
            user_border_in: Some(self.user_border_in),
            icc_filter: Some(icc_filter_str.into()),
        });
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);

        // INSERT key stages the highlighted image in Image Properties
        if ctx.input(|i| i.key_pressed(egui::Key::Insert)) {
            if let Some(path) = self.highlighted.clone() {
                self.stage_image(path);
            }
        }

        // DELETE key removes the selected queue item and returns to Printer Settings
        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
            if let Some(id) = self.selected_queue_id {
                self.queue.retain(|q| q.id != id);
                self.selected_queue_id = None;
                self.right_tab = RightTab::PrinterSettings;
                self.relayout_queue();
            }
        }

        if self.show_props { self.show_printer_props(ctx); }
        if self.show_print_confirm { self.show_print_confirm(ctx); }
        if self.show_icc_picker { self.show_icc_picker(ctx); }

        // Show splash screen during printer discovery
        if !self.discovery_complete {
            egui::CentralPanel::default()
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.3);
                        
                        // Title
                        ui.label(RichText::new("VibePrint Studio").size(32.0).strong());
                        ui.add_space(24.0);
                        
                        // Loading indicator
                        ui.add_space(16.0);
                        // Center the container but keep spinner+text together on the left
                        ui.horizontal(|ui| {
                            let space_each_side = (ui.available_width() - 200.0) / 2.0;
                            ui.add_space(space_each_side.max(0.0));
                            ui.spinner();
                            ui.label(RichText::new("Discovering printers...").size(16.0));
                        });
                        
                        ui.add_space(ui.available_height() * 0.3);
                        
                        // Show any discovery warnings/errors
                        if !self.log.is_empty() {
                            ui.separator();
                            ui.add_space(8.0);
                            egui::ScrollArea::vertical()
                                .max_height(120.0)
                                .show(ui, |ui| {
                                    for entry in self.log.iter().take(5) {
                                        ui.label(RichText::new(entry).small().monospace());
                                    }
                                });
                        }
                    });
                });
            return;
        }

        // Normal UI after discovery complete
        egui::SidePanel::left("assets")
            .default_width(LEFT_W)
            .min_width(140.0)
            .max_width(520.0)
            .resizable(true)
            .show(ctx, |ui| self.draw_left(ui));

        egui::SidePanel::right("sidebar")
            .default_width(RIGHT_W)
            .min_width(220.0)
            .max_width(600.0)
            .resizable(true)
            .show(ctx, |ui| self.draw_right(ui));

        egui::CentralPanel::default()
            .show(ctx, |ui| {
                let toolbar_h = 42.0_f32;
                let canvas_h = (ui.available_height() - toolbar_h).max(0.0);
                ui.allocate_ui(Vec2::new(ui.available_width(), canvas_h), |ui| {
                    self.draw_canvas(ui);
                });
                self.draw_canvas_toolbar(ui);
            });
    }
}

// ── Standalone helpers ────────────────────────────────────────────────────────

/// Print job submission (sync version for background thread)
fn submit_print_jobs_sync(
    temp_paths: &[PathBuf],
    caps: Option<PrinterCaps>,
    printer_idx: usize,
    printers: &[PrinterInfo],
    _selected_page_size_idx: usize,
    _props_media_idx: usize,
    _props_slot_idx: usize,
    _extra_option_indices: &HashMap<String, usize>,
    log_tx: &Sender<String>,
) -> Result<(), String> {
    if temp_paths.is_empty() {
        return Err("No pages to print".into());
    }
    let _caps = caps.ok_or("No printer selected")?;
    let printer = printers.get(printer_idx).ok_or("No printer selected")?;

    for (i, temp_path) in temp_paths.iter().enumerate() {
        let _ = log_tx.send(format!("Processing page {} of {}...", i + 1, temp_paths.len()));
        
        // Generate unique temp file paths
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let pid = std::process::id();

        // Use the 16-bit TIFF directly (Ghostscript can handle it)
        let temp_path_q = shell_quote(&temp_path.display().to_string());
        let _ = log_tx.send(format!("Page {}: Converting to PDF...", i + 1));

        // Step 1: Convert TIFF to PDF using Ghostscript with color preservation
        let pdf_path = format!("/tmp/vibeprint_{}_{}.pdf", timestamp, pid);
        let pdf_q = shell_quote(&pdf_path);

        // Use tiff2ps piped to Ghostscript to convert TIFF to PDF with color preservation
        // -sColorConversionStrategy=LeaveColorUnchanged prevents color conversion
        // -dNOTRANSPARENCY flattens any transparency
        // -dVERBOSE provides progress output for high-DPI conversions
        let gs_cmd = format!(
            "tiff2ps {} | gs -dVERBOSE -o {} -sDEVICE=pdfwrite -sColorConversionStrategy=LeaveColorUnchanged -dNOTRANSPARENCY -",
            temp_path_q, pdf_q
        );

        let gs_output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&gs_cmd)
            .output()
            .map_err(|e| format!("Failed to run Ghostscript: {}", e))?;

        if !gs_output.status.success() {
            let stderr = String::from_utf8_lossy(&gs_output.stderr);
            return Err(format!("PDF conversion failed (page {}): {}", i + 1, stderr));
        }
        let _ = log_tx.send(format!("Page {}: Sending to printer...", i + 1));

        // Send PDF via LPR (no media options needed, just the file)
        let printer_q = shell_quote(&printer.name);
        let lpr_cmd = format!("lpr -P {} {}", printer_q, pdf_q);

        let lpr_result = std::process::Command::new("sh")
            .arg("-c")
            .arg(&lpr_cmd)
            .output();

        match lpr_result {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                
                if !output.status.success() {
                    return Err(format!("Print failed (page {}): {}", i + 1, stderr));
                }
            }
            Err(e) => {
                return Err(format!("Failed to submit print job: {}", e));
            }
        }
    }
    
    Ok(())
}

/// Recursively render one directory node in the folder tree.
/// Reads children from disk only when expanded; depth-limited to 8.
fn draw_tree_node(
    ui: &mut egui::Ui,
    path: &PathBuf,
    depth: usize,
    current: &PathBuf,
    expanded: &HashMap<PathBuf, bool>,
    nav: &mut Option<PathBuf>,
    toggle: &mut Option<(PathBuf, bool)>,
) {
    if depth > 8 { return; }

    let name: String = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    if name.starts_with('.') && depth > 0 { return; }

    // Check for any non-hidden subdirectory (determines whether to show toggle)
    let has_children = std::fs::read_dir(path)
        .ok()
        .map(|rd| rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            !n.starts_with('.') && e.path().is_dir()
        }))
        .unwrap_or(false);

    // Default: home (depth==0) starts expanded; everything else collapsed
    let is_expanded = *expanded.get(path).unwrap_or(&(depth == 0));
    let is_current  = path == current;
    // Also expand if current dir is a descendant of this node
    let is_ancestor = !is_expanded && current.starts_with(path) && current != path;
    let is_expanded = is_expanded || is_ancestor;

    let indent = depth as f32 * 14.0 + 4.0;

    ui.horizontal(|ui| {
        ui.add_space(indent);

        // Expand/collapse arrow
        if has_children {
            let arrow = if is_expanded { "▼" } else { "▶" };
            let resp = ui.add(
                egui::Label::new(
                    RichText::new(arrow).size(9.0).color(Color32::from_gray(140))
                ).sense(Sense::click())
            ).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                *toggle = Some((path.clone(), !is_expanded));
            }
        } else {
            ui.add_space(12.0);
        }

        // Folder icon + name
        let icon = if is_expanded && has_children { "📂" } else { "📁" };
        let color = if is_current {
            Color32::from_rgb(100, 180, 255)
        } else if is_ancestor {
            Color32::from_gray(220)
        } else {
            Color32::from_gray(185)
        };
        let label = RichText::new(format!("{icon} {name}")).size(12.0).color(color);
        let resp = ui.add(egui::Label::new(label).sense(Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            *nav = Some(path.clone());
        }
        if is_current {
            // Subtle highlight behind the active row
            ui.painter().rect_filled(
                resp.rect.expand2(Vec2::new(4.0, 1.0)),
                3.0,
                Color32::from_rgba_premultiplied(60, 120, 220, 40),
            );
        }
    });

    if is_expanded {
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut children: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    !n.starts_with('.') && e.path().is_dir()
                })
                .map(|e| e.path())
                .collect();
            children.sort();
            for child in &children {
                draw_tree_node(ui, child, depth + 1, current, expanded, nav, toggle);
            }
        }
    }
}

/// Returns `(fits, rect_landscape)`.
/// `rect_landscape` is true ONLY when w×h portrait doesn't fit but h×w landscape does.
/// Image rotation (to fill the rect) is a separate decision made in the caller.
fn check_size_fit(w_in: f32, h_in: f32, ia_w_in: f32, ia_h_in: f32) -> (bool, bool) {
    let fits_portrait  = w_in <= ia_w_in && h_in <= ia_h_in;
    let fits_landscape = h_in <= ia_w_in && w_in <= ia_h_in;
    if fits_portrait        { (true,  false) }
    else if fits_landscape  { (true,  true)  }
    else                    { (false, false) }
}

/// Extract embedded ICC profile from image file (JPEG APP2, PNG iCCP, TIFF tag)
fn extract_embedded_icc(path: &std::path::Path) -> Option<Vec<u8>> {
    let ext = path.extension().and_then(|s| s.to_str())?.to_ascii_lowercase();
    let data = std::fs::read(path).ok()?;
    
    match ext.as_str() {
        "jpg" | "jpeg" => {
            // Look for APP2 marker (0xFFE2) followed by "ICC_PROFILE"
            let mut i = 0;
            while i < data.len().saturating_sub(16) {
                if data[i] == 0xFF && data[i+1] == 0xE2 {
                    // Found APP2 marker, check for ICC_PROFILE signature
                    let len = ((data[i+2] as usize) << 8) | (data[i+3] as usize);
                    if i + 4 + 11 < data.len() && &data[i+4..i+4+11] == b"ICC_PROFILE" {
                        // ICC profile data starts after "ICC_PROFILE\0" + sequence byte
                        let icc_start = i + 4 + 14; // Skip "ICC_PROFILE\0" + 2 bytes (sequence/chunk)
                        let icc_len = len.saturating_sub(16);
                        if icc_start + icc_len <= data.len() {
                            return Some(data[icc_start..icc_start + icc_len].to_vec());
                        }
                    }
                    i += 2 + len;
                } else if data[i] == 0xFF && (data[i+1] == 0xD8 || data[i+1] == 0xD9 || data[i+1] >= 0xE0) {
                    // Skip other markers
                    let len = if data[i+1] == 0xD8 { 0 } else { ((data[i+2] as usize) << 8) | (data[i+3] as usize) };
                    i += 2 + len;
                } else {
                    i += 1;
                }
            }
            None
        }
        "png" => {
            // Look for iCCP chunk
            let mut i = 8; // Skip PNG signature
            while i < data.len().saturating_sub(12) {
                let len = ((data[i] as usize) << 24) | ((data[i+1] as usize) << 16) |
                          ((data[i+2] as usize) << 8) | (data[i+3] as usize);
                let chunk_type = &data[i+4..i+8];
                if chunk_type == b"iCCP" {
                    // iCCP chunk: profile name + compression method + compressed profile
                    let chunk_data = &data[i+8..i+8+len];
                    // Find null terminator for profile name
                    let null_pos = chunk_data.iter().position(|&b| b == 0)?;
                    let compression = chunk_data.get(null_pos + 1)?;
                    if *compression == 0 { // deflate
                        let compressed = &chunk_data[null_pos + 2..];
                        use std::io::Read;
                        let mut decoder = flate2::read::ZlibDecoder::new(compressed);
                        let mut profile = Vec::new();
                        if decoder.read_to_end(&mut profile).is_ok() {
                            return Some(profile);
                        }
                    }
                } else if chunk_type == b"IEND" {
                    break;
                }
                i += 12 + len; // len + type + data + CRC
            }
            None
        }
        "tif" | "tiff" => {
            // Look for ICC tag (34675 = 0x8773)
            if data.len() < 8 { return None; }
            let little_endian = data[0] == 0x49; // "II" = little endian
            let ifd_offset = if little_endian {
                (data[4] as usize) | ((data[5] as usize) << 8) | ((data[6] as usize) << 16) | ((data[7] as usize) << 24)
            } else {
                ((data[4] as usize) << 24) | ((data[5] as usize) << 16) | ((data[6] as usize) << 8) | (data[7] as usize)
            };
            
            if ifd_offset + 2 > data.len() { return None; }
            let num_entries = if little_endian {
                (data[ifd_offset] as usize) | ((data[ifd_offset + 1] as usize) << 8)
            } else {
                ((data[ifd_offset] as usize) << 8) | (data[ifd_offset + 1] as usize)
            };
            
            let mut offset = ifd_offset + 2;
            for _ in 0..num_entries {
                if offset + 12 > data.len() { break; }
                let tag = if little_endian {
                    (data[offset] as u16) | ((data[offset + 1] as u16) << 8)
                } else {
                    ((data[offset] as u16) << 8) | (data[offset + 1] as u16)
                };
                if tag == 34675 { // ICC profile tag
                    let len = if little_endian {
                        (data[offset + 4] as usize) | ((data[offset + 5] as usize) << 8) |
                        ((data[offset + 6] as usize) << 16) | ((data[offset + 7] as usize) << 24)
                    } else {
                        ((data[offset + 4] as usize) << 24) | ((data[offset + 5] as usize) << 16) |
                        ((data[offset + 6] as usize) << 8) | (data[offset + 7] as usize)
                    };
                    let value_offset = if little_endian {
                        (data[offset + 8] as usize) | ((data[offset + 9] as usize) << 8) |
                        ((data[offset + 10] as usize) << 16) | ((data[offset + 11] as usize) << 24)
                    } else {
                        ((data[offset + 8] as usize) << 24) | ((data[offset + 9] as usize) << 16) |
                        ((data[offset + 10] as usize) << 8) | (data[offset + 11] as usize)
                    };
                    if value_offset + len <= data.len() {
                        return Some(data[value_offset..value_offset + len].to_vec());
                    }
                }
                offset += 12;
            }
            None
        }
        _ => None,
    }
}

fn is_image(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp"
    )
}

fn load_thumb(path: PathBuf, size: u32, tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>) {
    if let Ok(img) = image::open(&path) {
        let thumb = img.thumbnail(size, size).into_rgb8();
        let w = thumb.width() as usize;
        let h = thumb.height() as usize;
        let pixels = thumb.into_raw()
            .chunks_exact(3)
            .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
            .collect();
        let _ = tx.send((path, ColorImage { size: [w, h], pixels }, None, LoadKind::Thumb));
    } else {
        // Signal failure by sending an empty 1×1 magenta image
        let _ = tx.send((path, ColorImage {
            size: [1, 1],
            pixels: vec![Color32::from_rgb(200, 0, 80)],
        }, None, LoadKind::Thumb));
    }
}

/// Draw a dashed rectangle outline via short line segments.
fn draw_dashed_rect(painter: &egui::Painter, rect: Rect, color: Color32, width: f32, dash: f32) {
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let total = (b - a).length();
        let dir = (b - a) / total;
        let mut t = 0.0_f32;
        let mut draw = true;
        while t < total {
            let end = (t + dash).min(total);
            if draw {
                painter.line_segment([a + dir * t, a + dir * end], Stroke::new(width, color));
            }
            t = end + dash * 0.5;
            draw = !draw;
        }
    }
}

fn draw_ruler_h(
    painter: &egui::Painter,
    area: Rect,
    paper_x: f32,
    paper_px_w: f32,
    scale: f32,
    ruler_h: f32,
    margin_l_px: f32,
    margin_r_px: f32,
) {
    let bg          = Color32::from_gray(44);
    let fg          = Color32::from_gray(215);
    let half_fg     = Color32::from_gray(160);
    let qtr_fg      = Color32::from_gray(110);
    let margin_col  = Color32::from_rgb(255, 150, 50);

    let ruler_rect = Rect::from_min_size(area.min, Vec2::new(area.width(), ruler_h));
    painter.rect_filled(ruler_rect, 0.0, bg);
    // Bottom separator
    painter.line_segment(
        [Pos2::new(area.min.x, area.min.y + ruler_h),
         Pos2::new(area.max.x, area.min.y + ruler_h)],
        Stroke::new(1.0, Color32::from_gray(72)),
    );

    let ppi = 72.0_f32 * scale; // pixels per inch
    let total = paper_px_w / ppi;

    for i in 0..=(total as u32 + 1) {
        let x = paper_x + i as f32 * ppi;
        if x < area.min.x || x > area.max.x { continue; }

        // Inch tick
        painter.line_segment(
            [Pos2::new(x, area.min.y + ruler_h - 13.0),
             Pos2::new(x, area.min.y + ruler_h)],
            Stroke::new(1.0, fg),
        );
        if i > 0 {
            painter.text(
                Pos2::new(x + 2.5, area.min.y + 2.5),
                egui::Align2::LEFT_TOP,
                format!("{i}\""),
                egui::FontId::proportional(10.0),
                fg,
            );
        }
        // Half-inch
        let xh = x + ppi * 0.5;
        if xh > area.min.x && xh < area.max.x {
            painter.line_segment(
                [Pos2::new(xh, area.min.y + ruler_h - 8.0),
                 Pos2::new(xh, area.min.y + ruler_h)],
                Stroke::new(1.0, half_fg),
            );
        }
        // Quarter-inch ticks
        for &frac in &[0.25_f32, 0.75] {
            let xq = x + ppi * frac;
            if xq > area.min.x && xq < area.max.x {
                painter.line_segment(
                    [Pos2::new(xq, area.min.y + ruler_h - 5.0),
                     Pos2::new(xq, area.min.y + ruler_h)],
                    Stroke::new(0.75, qtr_fg),
                );
            }
        }
    }

    // Margin markers — orange triangle pointing down toward canvas
    for &offset_px in &[margin_l_px, margin_r_px] {
        let x = paper_x + offset_px;
        if x < area.min.x || x > area.max.x { continue; }
        painter.line_segment(
            [Pos2::new(x, area.min.y),
             Pos2::new(x, area.min.y + ruler_h)],
            Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 150, 50, 120)),
        );
        let bot = area.min.y + ruler_h;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(x - 4.0, bot - 8.0),
                Pos2::new(x + 4.0, bot - 8.0),
                Pos2::new(x,       bot),
            ],
            margin_col,
            Stroke::NONE,
        ));
    }
}

fn draw_ruler_v(
    painter: &egui::Painter,
    area: Rect,
    paper_y: f32,
    paper_px_h: f32,
    scale: f32,
    ruler_w: f32,
    margin_t_px: f32,
    margin_b_px: f32,
) {
    let bg          = Color32::from_gray(44);
    let fg          = Color32::from_gray(215);
    let half_fg     = Color32::from_gray(160);
    let qtr_fg      = Color32::from_gray(110);
    let margin_col  = Color32::from_rgb(255, 150, 50);

    let ruler_rect = Rect::from_min_size(
        area.min + Vec2::new(0.0, RULER_PX),
        Vec2::new(ruler_w, area.height() - RULER_PX),
    );
    painter.rect_filled(ruler_rect, 0.0, bg);
    // Right separator
    painter.line_segment(
        [Pos2::new(area.min.x + ruler_w, area.min.y + RULER_PX),
         Pos2::new(area.min.x + ruler_w, area.max.y)],
        Stroke::new(1.0, Color32::from_gray(72)),
    );

    let ppi = 72.0_f32 * scale;
    let total = paper_px_h / ppi;

    for i in 0..=(total as u32 + 1) {
        let y = paper_y + i as f32 * ppi;
        if y < area.min.y || y > area.max.y { continue; }

        // Inch tick
        painter.line_segment(
            [Pos2::new(area.min.x + ruler_w - 13.0, y),
             Pos2::new(area.min.x + ruler_w, y)],
            Stroke::new(1.0, fg),
        );
        if i > 0 {
            painter.text(
                Pos2::new(area.min.x + 2.0, y + 2.5),
                egui::Align2::LEFT_TOP,
                format!("{i}\""),
                egui::FontId::proportional(10.0),
                fg,
            );
        }
        // Half-inch
        let yh = y + ppi * 0.5;
        if yh > area.min.y && yh < area.max.y {
            painter.line_segment(
                [Pos2::new(area.min.x + ruler_w - 8.0, yh),
                 Pos2::new(area.min.x + ruler_w, yh)],
                Stroke::new(1.0, half_fg),
            );
        }
        // Quarter-inch ticks
        for &frac in &[0.25_f32, 0.75] {
            let yq = y + ppi * frac;
            if yq > area.min.y && yq < area.max.y {
                painter.line_segment(
                    [Pos2::new(area.min.x + ruler_w - 5.0, yq),
                     Pos2::new(area.min.x + ruler_w, yq)],
                    Stroke::new(0.75, qtr_fg),
                );
            }
        }
    }

    // Margin markers — orange triangle pointing right toward canvas
    for &offset_px in &[margin_t_px, margin_b_px] {
        let y = paper_y + offset_px;
        if y < area.min.y || y > area.max.y { continue; }
        painter.line_segment(
            [Pos2::new(area.min.x, y),
             Pos2::new(area.min.x + ruler_w, y)],
            Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 150, 50, 120)),
        );
        let right = area.min.x + ruler_w;
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(right - 8.0, y - 4.0),
                Pos2::new(right - 8.0, y + 4.0),
                Pos2::new(right,       y),
            ],
            margin_col,
            Stroke::NONE,
        ));
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VibePrint Studio")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "VibePrint Studio",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc)) as Box<dyn eframe::App>)),
    )
}
