#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, ColorImage, Context, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use vibeprint::{
    monitor_icc,
    printer_discovery::{self, CupsOption, DiscoveryEvent, PrinterCaps, PrinterInfo},
    processor::{self, ProcessOptions, ResampleEngine},
};

// ── Constants ─────────────────────────────────────────────────────────────────

const LEFT_W:       f32  = 260.0;
const RIGHT_W:      f32  = 295.0;
const THUMB_PX:     u32  = 96;
const RULER_PX:     f32  = 22.0;
const FIT_PAGE_IDX: usize = 15; // sentinel index = "Fit to Page"

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

#[derive(Clone, Copy, PartialEq)]
enum Engine {
    Mks,
    RobidouxEwa,
    Iterative,
    Lanczos3,
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
enum RightTab { PrinterSettings, ImageProperties }

enum ProcState {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone)]
enum ProcessTarget {
    Export(PathBuf),
    Print { temp_path: PathBuf, print_after: bool },
}

#[derive(Clone)]
struct PrintJobStatus {
    job_id: u32,
    printer_name: String,
    state: PrintJobState,
}

#[derive(Clone, PartialEq)]
enum PrintJobState {
    Pending,
    Processing,
    Completed,
    Failed(String),
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
    thumb_tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>)>,
    thumb_rx: Receiver<(PathBuf, ColorImage, Option<Vec<u8>>)>,
    selected: Option<PathBuf>,
    highlighted: Option<PathBuf>,
    canvas_tex: Option<egui::TextureHandle>,
    print_size_idx: Option<usize>,
    canvas_img_size: Option<[usize; 2]>,
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

    // ── Engine settings ──
    engine: Engine,
    sharpen: u8,
    depth16: bool,
    target_dpi: u32,

    // ── Color management ──
    output_icc: Option<PathBuf>,
    intent: Intent,
    bpc: bool,

    // ── Output ──
    output_dir: PathBuf,
    print_to_file: bool,

    // ── Processing ──
    proc_state: ProcState,
    proc_rx: Option<Receiver<Result<(PathBuf, ProcessTarget), String>>>,

    // ── Right-panel tab ──
    right_tab: RightTab,

    // ── Status ──
    log: Vec<String>,

    // ── Saved-settings restoration ──
    pending_printer_name: Option<String>,
    pending_page_size_name: Option<String>,

    // ── Splash screen ──
    discovery_complete: bool,

    // ── Monitor ICC profile ──
    monitor_icc_profile: Option<Vec<u8>>,

    // ── Printing ──
    show_print_confirm: bool,
    pending_print_path: Option<PathBuf>,
    print_job_status: Option<PrintJobStatus>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let out_dir = dirs::desktop_dir().unwrap_or_else(|| home.clone());
        let (thumb_tx, thumb_rx) = mpsc::channel::<(PathBuf, ColorImage, Option<Vec<u8>>)>();
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
        let saved_icc: Option<PathBuf> = s.output_icc.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_file());

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

        let mut app = Self {
            current_dir: start_dir,
            subdirs: Vec::new(),
            image_files: Vec::new(),
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            selected: None,
            highlighted: None,
            canvas_tex: None,
            print_size_idx: None,
            canvas_img_size: None,
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

            engine: saved_engine,
            sharpen: s.sharpen.unwrap_or(5),
            depth16: s.depth16.unwrap_or(true),
            target_dpi: s.target_dpi.unwrap_or(720),

            output_icc: saved_icc,
            intent: saved_intent,
            bpc: s.bpc.unwrap_or(true),

            output_dir: saved_out_dir,
            print_to_file: false,

            proc_state: ProcState::Idle,
            proc_rx: None,

            right_tab: RightTab::PrinterSettings,

            log: Vec::new(),

            pending_printer_name: s.printer_name,
            pending_page_size_name: s.page_size_name,

            discovery_complete: false,

            monitor_icc_profile: monitor_icc::get_monitor_profile(),

            show_print_confirm: false,
            pending_print_path: None,
            print_job_status: None,
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

    fn select_image(&mut self, path: PathBuf) {
        self.selected = Some(path.clone());
        self.canvas_tex = None;
        self.canvas_img_size = None;
        self.print_size_idx = None;
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
                let _ = tx.send((path, ColorImage { size, pixels }, embedded_icc));
            }
        });
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
        } else {
            self.caps = None;
            self.extra_option_indices.clear();
        }
    }

    // ── Background task pump ──────────────────────────────────────────────────

    fn pump(&mut self, ctx: &Context) {
        // Thumbnails / canvas image
        while let Ok((path, mut ci, embedded_icc)) = self.thumb_rx.try_recv() {
            let name = path.to_string_lossy().to_string();
            if self.selected.as_ref() == Some(&path) {
                let size = ci.size;
                
                // Apply monitor ICC profile for color-accurate display (silent)
                if let Some(ref monitor_profile) = self.monitor_icc_profile {
                    // Convert ColorImage pixels to bytes for lcms2
                    let mut pixel_bytes: Vec<u8> = ci.pixels
                        .iter()
                        .flat_map(|c| vec![c.r(), c.g(), c.b()])
                        .collect();
                    
                    let pixel_count = pixel_bytes.len() / 3;
                    
                    // Apply color transform with embedded ICC as source and app's intent/BPC
                    match monitor_icc::apply_monitor_profile(
                        monitor_profile, 
                        embedded_icc.as_deref(),
                        &mut pixel_bytes,
                        self.intent.to_lcms(),
                        self.bpc
                    ) {
                        Some(_) => {
                            // Convert back to ColorImage
                            ci.pixels = pixel_bytes
                                .chunks_exact(3)
                                .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                                .collect();
                        }
                        None => {
                            self.log.push("✗ Color transform failed".into());
                        }
                    }
                } else {
                    self.log.push("⚠ No monitor profile available for canvas".into());
                }
                
                let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                self.canvas_tex = Some(tex.clone());
                self.canvas_img_size = Some(size);
                self.thumbs.insert(path, ThumbState::Ready(tex));
            } else {
                let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                self.thumbs.insert(path, ThumbState::Ready(tex));
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
                    Ok((path, target)) => {
                        match target {
                            ProcessTarget::Export(_) => {
                                self.log.push(format!("✓ Saved: {}", path.display()));
                                self.proc_state = ProcState::Done(path);
                            }
                            ProcessTarget::Print { temp_path, .. } => {
                                self.log.push(format!("✓ Processed for print: {}", path.display()));
                                self.pending_print_path = Some(temp_path);
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
    }

    // ── Start processing ──────────────────────────────────────────────────────

    fn start_process_export(&mut self) {
        let Some(input) = self.selected.clone() else {
            self.log.push("⚠ No image selected.".into());
            return;
        };
        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        let output = self.output_dir.join(format!("{}_vp.tif", stem));
        self.start_process_with_target(ProcessTarget::Export(output));
    }

    fn start_process_print(&mut self) {
        let Some(input) = self.selected.clone() else {
            self.log.push("⚠ No image selected.".into());
            return;
        };
        // Generate temp file path for printing
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let temp_path = std::env::temp_dir().join(format!("vibeprint_{}_{}.tif", timestamp, std::process::id()));
        self.log.push(format!("📝 Temp file will be: {}", temp_path.display()));
        self.start_process_with_target(ProcessTarget::Print { temp_path, print_after: true });
    }

    fn start_process_with_target(&mut self, target: ProcessTarget) {
        let input = match &target {
            ProcessTarget::Export(_) | ProcessTarget::Print { .. } => {
                match self.selected.clone() {
                    Some(path) => path,
                    None => {
                        self.log.push("⚠ No image selected.".into());
                        return;
                    }
                }
            }
        };

        let output_path = match &target {
            ProcessTarget::Export(path) => path.clone(),
            ProcessTarget::Print { temp_path, .. } => temp_path.clone(),
        };

        // Store pending print path if printing
        if let ProcessTarget::Print { temp_path, .. } = &target {
            self.pending_print_path = Some(temp_path.clone());
        }

        // Build page layout from current canvas state (mirrors draw_canvas logic at print resolution)
        let page_layout: Option<processor::PageLayout> = (|| {
            let idx = self.print_size_idx?;
            let ps  = self.caps.as_ref()?.page_sizes.get(self.selected_page_size_idx)?;
            let (ia_l, ia_b, ia_r, ia_t) = ps.imageable_area;
            let dpi = self.target_dpi as f64;

            let ia_w_in = (ia_r - ia_l) as f64 / 72.0;
            let ia_h_in = (ia_t - ia_b) as f64 / 72.0;
            let ia_w_px = (ia_w_in * dpi).round() as u32;
            let ia_h_px = (ia_h_in * dpi).round() as u32;
            // TIFF is sized to the imageable area (GIMP-style): CUPS positions it
            // within the printable area automatically when printing with scaling=100.
            let page_w_px = ia_w_px;
            let page_h_px = ia_h_px;

            let img_aspect = self.canvas_img_size
                .map(|s| s[0] as f64 / s[1] as f64)
                .unwrap_or(1.0);

            let (print_w_px, print_h_px, rotate_cw) = if idx < PRINT_SIZES.len() {
                let (w_in, h_in, _) = PRINT_SIZES[idx];
                let (w_in, h_in) = (w_in as f64, h_in as f64);
                let fits_portrait  = w_in <= ia_w_in && h_in <= ia_h_in;
                let fits_landscape = h_in <= ia_w_in && w_in <= ia_h_in;
                let (rect_landscape, rotate_cw) = if img_aspect > 1.0 {
                    if fits_landscape { (true, false) } else { (false, true) }
                } else {
                    if fits_portrait  { (false, false) } else { (true, true) }
                };
                let (pw, ph) = if rect_landscape { (h_in, w_in) } else { (w_in, h_in) };
                // Calculate exact integer dimensions: inches * DPI (no rounding to avoid 0.5mm errors)
                let target_dpi = self.target_dpi as u32;
                let w_px = ((pw as f64 * target_dpi as f64) + 0.0001).round() as u32;
                let h_px = ((ph as f64 * target_dpi as f64) + 0.0001).round() as u32;
                ((w_px, h_px, rotate_cw))
            } else if idx == FIT_PAGE_IDX {
                let (nw, nh) = if ia_w_in / ia_h_in > img_aspect {
                    (ia_h_in * img_aspect, ia_h_in)
                } else {
                    (ia_w_in, ia_w_in / img_aspect)
                };
                let rot_a = 1.0 / img_aspect;
                let (rw, rh) = if ia_w_in / ia_h_in > rot_a {
                    (ia_h_in * rot_a, ia_h_in)
                } else {
                    (ia_w_in, ia_w_in / img_aspect)
                };
                let (pw, ph, rotate_cw) = if rw * rh > nw * nh { (rw, rh, true) } else { (nw, nh, false) };
                // Calculate exact integer dimensions: inches * DPI (no rounding to avoid 0.5mm errors)
                let target_dpi = self.target_dpi as u32;
                let w_px = ((pw as f64 * target_dpi as f64) + 0.0001).round() as u32;
                let h_px = ((ph as f64 * target_dpi as f64) + 0.0001).round() as u32;
                ((w_px, h_px, rotate_cw))
            } else {
                return None;
            };
            let print_x = ia_w_px.saturating_sub(print_w_px) / 2;
            let print_y = ia_h_px.saturating_sub(print_h_px) / 2;

            Some(processor::PageLayout { page_w_px, page_h_px, print_x, print_y, print_w_px, print_h_px, rotate_cw })
        })();

        let opts = ProcessOptions {
            input,
            output: output_path.clone(),
            input_icc: None,
            output_icc: self.output_icc.clone(),
            target_dpi: self.target_dpi as f64,
            intent: self.intent.to_lcms(),
            bpc: self.bpc,
            engine: self.engine.to_proc(),
            // Force 8-bit for printing (CUPS/img2pdf compatibility), respect UI for export
            depth: if matches!(target, ProcessTarget::Print { .. }) { 8 } else { if self.depth16 { 16 } else { 8 } },
            sharpen: self.sharpen,
            page_layout,
        };
        
        let target_clone = match &target {
            ProcessTarget::Export(_) => target.clone(),
            ProcessTarget::Print { temp_path, print_after } => ProcessTarget::Print { 
                temp_path: temp_path.clone(), 
                print_after: *print_after 
            },
        };
        
        let (tx, rx) = mpsc::channel::<Result<(PathBuf, ProcessTarget), String>>();
        self.proc_rx = Some(rx);
        self.proc_state = ProcState::Running;
        thread::spawn(move || {
            let result: Result<(PathBuf, ProcessTarget), String> = processor::process(opts)
                .map(|_| (output_path, target_clone))
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    // ── Print to CUPS ─────────────────────────────────────────────────────────

    fn submit_print_job(&mut self, temp_path: &PathBuf) {
        let Some(caps) = &self.caps else {
            self.log.push("✗ No printer selected".into());
            return;
        };
        let Some(printer) = self.printers.get(self.printer_idx) else {
            self.log.push("✗ No printer selected".into());
            return;
        };

        // Build lpr command with all CUPS options
        let mut cmd = std::process::Command::new("lpr");
        cmd.arg("-P").arg(&printer.name);

        // Paper size
        let media = caps.page_sizes.get(self.selected_page_size_idx)
            .map(|ps| ps.name.clone())
            .unwrap_or_else(|| "Letter".to_string());
        cmd.arg("-o").arg(format!("media={}", media));

        // Media type
        if let Some(media_type) = caps.media_types.get(self.props_media_idx) {
            cmd.arg("-o").arg(format!("MediaType={}", media_type));
        }

        // Input slot
        if let Some(input_slot) = caps.input_slots.get(self.props_slot_idx) {
            cmd.arg("-o").arg(format!("InputSlot={}", input_slot));
        }

        // Prevent CUPS scaling
        cmd.arg("-o").arg("scaling=100");
        cmd.arg("-o").arg("fit-to-page=false");

        // All extra options from the modal
        for opt in &caps.extra_options {
            if let Some(&idx) = self.extra_option_indices.get(&opt.key) {
                if let Some((key, _)) = opt.choices.get(idx) {
                    cmd.arg("-o").arg(format!("{}={}", opt.key, key));
                }
            }
        }

        // Convert TIFF to PDF for CUPS compatibility (CUPS 2.x removed image filters)
        let pdf_path = temp_path.with_extension("pdf");
        let img2pdf_result = std::process::Command::new("img2pdf")
            .arg(temp_path)
            .arg("--imgsize")
            .arg(format!("{}dpi", self.target_dpi))
            .arg("--fit")
            .arg("exact")
            .arg("-o")
            .arg(&pdf_path)
            .output();
        
        let print_path = match img2pdf_result {
            Ok(output) if output.status.success() => {
                self.log.push("📄 Converted TIFF to PDF for printing".into());
                pdf_path
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                self.log.push(format!("⚠ img2pdf failed ({}), trying TIFF directly", stderr));
                temp_path.clone()
            }
            Err(e) => {
                self.log.push(format!("⚠ img2pdf not available ({}), trying TIFF directly", e));
                temp_path.clone()
            }
        };

        cmd.arg(print_path);

        // Execute lpr and capture job ID
        match cmd.output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                
                if !output.status.success() {
                    self.log.push(format!("✗ Print failed: {}", stderr));
                    self.print_job_status = Some(PrintJobStatus {
                        job_id: 0,
                        printer_name: printer.name.clone(),
                        state: PrintJobState::Failed(stderr.to_string()),
                    });
                    return;
                }

                // Parse job ID from output: "request id is Printer-123 (1 file(s))"
                let job_id = stdout.lines()
                    .chain(stderr.lines())
                    .find_map(|line| {
                        let re = regex::Regex::new(r"request id is \w+-(\d+)").ok()?;
                        re.captures(line).and_then(|cap| cap.get(1)).and_then(|m| m.as_str().parse::<u32>().ok())
                    })
                    .unwrap_or(0);

                self.log.push(format!("📤 Print job #{} submitted to {}", job_id, printer.name));
                self.print_job_status = Some(PrintJobStatus {
                    job_id,
                    printer_name: printer.name.clone(),
                    state: PrintJobState::Pending,
                });

                // Temp files left in /tmp for CUPS to process (cleaned up on reboot)
            }
            Err(e) => {
                self.log.push(format!("✗ Failed to submit print job: {}", e));
                self.print_job_status = Some(PrintJobStatus {
                    job_id: 0,
                    printer_name: printer.name.clone(),
                    state: PrintJobState::Failed(e.to_string()),
                });
            }
        }
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
                    let is_sel  = self.selected.as_ref()     == Some(path);
                    let is_hi   = self.highlighted.as_ref()  == Some(path);
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

                    let fill = if is_sel { Color32::from_rgb(30, 90, 170) }
                               else if is_hi { Color32::from_rgb(45, 55, 70) }
                               else { Color32::from_gray(40) };
                    painter.rect_filled(resp.rect, 4.0, fill);
                    if is_sel {
                        painter.rect_stroke(resp.rect, 4.0,
                            Stroke::new(1.5, Color32::from_rgb(80, 140, 255)));
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

                    if resp.clicked()        { self.highlighted = Some(path.clone()); }
                    if resp.double_clicked() { self.select_image(path.clone()); }
                }
            });
        });
    }

    // ── Center pane / canvas ──────────────────────────────────────────────────

    fn draw_canvas(&self, ui: &mut egui::Ui) {
        // Paper dimensions in PostScript points — driven by selected_page_size_idx
        let selected_ps = self.caps.as_ref()
            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx));
        let (paper_w_pt, paper_h_pt) = selected_ps
            .map(|ps| ps.paper_size)
            .unwrap_or((612.0_f32, 792.0_f32));
        let (ia_l, ia_b, ia_r, ia_t) = selected_ps
            .map(|ps| ps.imageable_area)
            .unwrap_or((0.0, 0.0, 612.0, 792.0));

        let (resp, _) = ui.allocate_painter(ui.available_size(), Sense::hover());
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

        // Selected image preview centered in imageable area
        if let (Some(tex), Some(img_sz)) = (&self.canvas_tex, self.canvas_img_size) {
            let img_aspect = img_sz[0] as f32 / img_sz[1] as f32;

            match self.print_size_idx {
                Some(idx) if idx < PRINT_SIZES.len() => {
                    let (w_in, h_in, _) = PRINT_SIZES[idx];
                    let ia_w_in = (ia_r - ia_l) / 72.0;
                    let ia_h_in = (ia_t - ia_b) / 72.0;
                    let fits_portrait  = w_in <= ia_w_in && h_in <= ia_h_in;
                    let fits_landscape = h_in <= ia_w_in && w_in <= ia_h_in;
                    // Choose rect orientation to MATCH the image; only force the other if needed
                    let (rect_landscape, print_rotated) = if img_aspect > 1.0 {
                        // Landscape image: prefer landscape rect (h×w), rotate only if forced portrait
                        if fits_landscape  { (true,  false) } else { (false, true) }
                    } else {
                        // Portrait image: prefer portrait rect (w×h), rotate only if forced landscape
                        if fits_portrait   { (false, false) } else { (true,  true) }
                    };
                    let (pw_in, ph_in) = if rect_landscape { (h_in, w_in) } else { (w_in, h_in) };
                    let rw = pw_in * 72.0 * scale;
                    let rh = ph_in * 72.0 * scale;
                    let print_rect = Rect::from_center_size(ia_rect.center(), Vec2::new(rw, rh));
                    // When image is rotated, use its flipped aspect for letterbox sizing
                    let eff_aspect = if print_rotated { 1.0 / img_aspect } else { img_aspect };
                    let (iw, ih) = if rw / rh > eff_aspect {
                        (rh * eff_aspect, rh)
                    } else {
                        (rw, rw / eff_aspect)
                    };
                    let img_rect = Rect::from_center_size(ia_rect.center(), Vec2::new(iw, ih));
                    // Faint yellow outline showing the exact print area
                    painter.rect_stroke(print_rect, 0.0,
                        Stroke::new(1.0, Color32::from_rgba_premultiplied(220, 200, 80, 160)));
                    if print_rotated {
                        // Draw image rotated 90° CW via custom UV mesh
                        // 90° CW: TL←BL, TR←TL, BR←TR, BL←BR (original UV)
                        let mut mesh = egui::epaint::Mesh::with_texture(tex.id());
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_top(),     uv: Pos2::new(0.0, 1.0), color: Color32::WHITE });
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_top(),    uv: Pos2::new(0.0, 0.0), color: Color32::WHITE });
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_bottom(), uv: Pos2::new(1.0, 0.0), color: Color32::WHITE });
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_bottom(),  uv: Pos2::new(1.0, 1.0), color: Color32::WHITE });
                        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                        painter.add(egui::Shape::mesh(mesh));
                    } else {
                        painter.image(tex.id(), img_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                }
                _ => {
                    // Fit to Page — maximize coverage of imageable area; try both orientations
                    let ia_w = ia_rect.width();
                    let ia_h = ia_rect.height();
                    // Normal orientation
                    let (nw, nh) = if ia_w / ia_h > img_aspect {
                        (ia_h * img_aspect, ia_h)
                    } else {
                        (ia_w, ia_w / img_aspect)
                    };
                    // Rotated 90° (effective aspect = 1/img_aspect)
                    let rot_aspect = 1.0 / img_aspect;
                    let (rw, rh) = if ia_w / ia_h > rot_aspect {
                        (ia_h * rot_aspect, ia_h)
                    } else {
                        (ia_w, ia_w * img_aspect)
                    };
                    if rw * rh > nw * nh {
                        // Rotated fills more — draw 90° CW
                        let img_rect = Rect::from_center_size(ia_rect.center(), Vec2::new(rw, rh));
                        let mut mesh = egui::epaint::Mesh::with_texture(tex.id());
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_top(),     uv: Pos2::new(0.0, 1.0), color: Color32::WHITE });
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_top(),    uv: Pos2::new(0.0, 0.0), color: Color32::WHITE });
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_bottom(), uv: Pos2::new(1.0, 0.0), color: Color32::WHITE });
                        mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_bottom(),  uv: Pos2::new(1.0, 1.0), color: Color32::WHITE });
                        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                        painter.add(egui::Shape::mesh(mesh));
                    } else {
                        let img_rect = Rect::from_center_size(ia_rect.center(), Vec2::new(nw, nh));
                        painter.image(tex.id(), img_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
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

        // Status overlay (page size + DPI)
        let info = if let Some(caps) = &self.caps {
            let ps = caps.page_sizes.get(self.selected_page_size_idx)
                .map(|p| p.label.as_str())
                .unwrap_or("?");
            let dpi = self.target_dpi;
            format!("{ps}  ·  {dpi} dpi")
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
        }
    }

    fn draw_tab_printer(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("tab_printer_scroll").show(ui, |ui| {
            ui.add_space(6.0);

            // ── Block A: Hardware ─────────────────────────────────────────────
            ui.label(RichText::new("Hardware & Properties").strong().size(12.0));
            ui.separator();

            let prev_idx = self.printer_idx;
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

            // ── Print to file ───────────────────────────────────────────
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

            ui.add_space(10.0);

            // ── Block C: Color Management ─────────────────────────────────────
            ui.label(RichText::new("Color Management").strong().size(12.0));

            // Output ICC
            ui.horizontal(|ui| {
                ui.label("Output ICC:");
                let icc_label = self.output_icc.as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "embedded / sRGB".into());
                ui.add(egui::Label::new(
                    RichText::new(&icc_label).small().monospace()
                ).truncate());
                if ui.small_button("…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("ICC Profile", &["icc", "icm"])
                        .pick_file()
                    {
                        self.output_icc = Some(p);
                    }
                }
                if self.output_icc.is_some() && ui.small_button("✕").clicked() {
                    self.output_icc = None;
                }
            });

            // Intent
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

            ui.add_space(10.0);

            
        let is_running = matches!(self.proc_state, ProcState::Running);
        let has_image = self.selected.is_some();

        // Primary: Print button (dynamic text based on print_to_file)
        let btn_text = if self.print_to_file { "💾  Print to File" } else { "[P] Print" };
        let print_btn = egui::Button::new(
            RichText::new(btn_text).size(14.0).strong(),
        )
        .min_size(Vec2::new(ui.available_width(), 36.0))
        .fill(Color32::from_rgb(60, 120, 200));

        if ui.add_enabled(has_image && !is_running, print_btn).clicked() {
            if self.print_to_file {
                self.start_process_export();
            } else {
                self.start_process_print();
            }
        }

        ui.add_space(4.0);

        if is_running {
            ui.horizontal(|ui| { ui.spinner(); ui.label("Processing…"); });
        } else if !has_image {
            ui.label(RichText::new("Select an image first").small().weak());
        } else if let ProcState::Done(ref p) = self.proc_state {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            ui.label(RichText::new(format!("✓ {name}")).small().color(Color32::GREEN));
        } else if let ProcState::Failed(ref e) = self.proc_state {
            ui.label(RichText::new(format!("✗ {e}")).small().color(Color32::RED));
        }

        // Print job status
        if let Some(ref job) = self.print_job_status {
            ui.add_space(4.0);
            let status_text = match job.state {
                PrintJobState::Pending => format!("📤 Print Job #{} - Pending", job.job_id),
                PrintJobState::Processing => format!("⚙️ Print Job #{} - Processing", job.job_id),
                PrintJobState::Completed => format!("✅ Print Job #{} - Complete", job.job_id),
                PrintJobState::Failed(ref e) => format!("❌ Print Job #{} - Failed: {}", job.job_id, e),
            };
            let color = match job.state {
                PrintJobState::Pending => Color32::from_rgb(200, 200, 100),
                PrintJobState::Processing => Color32::from_rgb(100, 180, 255),
                PrintJobState::Completed => Color32::from_rgb(100, 220, 100),
                PrintJobState::Failed(_) => Color32::from_rgb(255, 100, 100),
            };
            ui.label(RichText::new(status_text).small().color(color));
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

        if let Some(ref job) = self.print_job_status {
            ui.add_space(4.0);
            let status_text = match job.state {
                PrintJobState::Pending => format!("📤 Print Job #{} - Pending", job.job_id),
                PrintJobState::Processing => format!("⚙️ Print Job #{} - Processing", job.job_id),
                PrintJobState::Completed => format!("✅ Print Job #{} - Complete", job.job_id),
                PrintJobState::Failed(ref e) => format!("❌ Print Job #{} - Failed: {}", job.job_id, e),
            };
            let color = match job.state {
                PrintJobState::Pending => Color32::from_rgb(200, 200, 100),
                PrintJobState::Processing => Color32::from_rgb(100, 180, 255),
                PrintJobState::Completed => Color32::GREEN,
                PrintJobState::Failed(_) => Color32::RED,
            };
            ui.label(RichText::new(status_text).small().color(color));
        }

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
        // Imageable area in inches (margins already excluded)
        let (ia_w_in, ia_h_in) = self.caps.as_ref()
            .and_then(|c| c.page_sizes.get(self.selected_page_size_idx))
            .map(|ps| {
                let (l, b, r, t) = ps.imageable_area;
                ((r - l) / 72.0, (t - b) / 72.0)
            })
            .unwrap_or((7.5, 10.0));

        let has_image = self.selected.is_some();

        ui.add_space(4.0);
        ui.label(RichText::new("Print Size").strong().size(12.0));
        ui.separator();

        if !has_image {
            ui.add_space(8.0);
            ui.label(RichText::new("Load an image first").weak().italics().size(11.0));
            return;
        }

        // Show usable area for reference
        ui.label(RichText::new(
            format!("Printable area: {:.2}\" × {:.2}\"", ia_w_in, ia_h_in)
        ).size(10.0).weak());
        ui.add_space(4.0);

        egui::ScrollArea::vertical().id_salt("print_sizes").show(ui, |ui| {
            for (idx, &(w, h, label)) in PRINT_SIZES.iter().enumerate() {
                let (fits, _) = check_size_fit(w, h, ia_w_in, ia_h_in);
                let is_sel = self.print_size_idx == Some(idx);

                let (text_color, hover_text) = if !fits {
                    (Color32::from_rgb(200, 60, 60), Some("Too large for the printable area"))
                } else if is_sel {
                    (Color32::from_rgb(110, 210, 110), None)
                } else {
                    (Color32::from_gray(210), None)
                };

                let row_text = RichText::new(label).size(13.0).color(text_color);

                let resp = ui.add_enabled(fits, egui::SelectableLabel::new(is_sel, row_text));
                if let Some(tip) = hover_text { resp.clone().on_disabled_hover_text(tip); }
                if resp.clicked() {
                    self.print_size_idx = Some(idx);
                }
            }

            ui.separator();

            // Fit to Page
            let is_fit = self.print_size_idx == Some(FIT_PAGE_IDX);
            let fit_text = RichText::new("Fit to Page").size(13.0)
                .color(if is_fit { Color32::from_rgb(110, 210, 110) } else { Color32::from_gray(210) });
            if ui.selectable_label(is_fit, fit_text).clicked() {
                self.print_size_idx = Some(FIT_PAGE_IDX);
            }
        });

    }

    // ── Printer Properties modal ──────────────────────────────────────────────

    fn show_printer_props(&mut self, ctx: &Context) {
        let Some(caps) = self.caps.clone() else { self.show_props = false; return };

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
    }

    // ── Print Confirmation Modal ──────────────────────────────────────────────

    fn show_print_confirm(&mut self, ctx: &Context) {
        let Some(caps) = self.caps.clone() else { self.show_print_confirm = false; return };
        let printer_name = self.printers.get(self.printer_idx).map(|p| p.name.clone());
        let Some(printer_name) = printer_name else { self.show_print_confirm = false; return };
        let Some(temp_path) = self.pending_print_path.clone() else { self.show_print_confirm = false; return };

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.30).clamp(320.0, 480.0);

        egui::Window::new("Confirm Print")
            .collapsible(false)
            .resizable(false)
            .fixed_size([width, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Ready to Print").size(18.0).strong());
                    ui.add_space(8.0);
                });

                ui.separator();
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
                ui.separator();
                ui.add_space(4.0);

                // Buttons
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_print_confirm = false;
                            self.pending_print_path = None;
                            // Clean up temp file
                            let _ = std::fs::remove_file(&temp_path);
                        }
                        
                        let print_btn = ui.add(egui::Button::new(
                            RichText::new("Print").strong().color(Color32::WHITE)
                        ));
                        if print_btn.clicked() {
                            self.show_print_confirm = false;
                            self.submit_print_job(&temp_path);
                            self.pending_print_path = None;
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
            output_icc: self.output_icc.as_ref().map(|p| p.to_string_lossy().into_owned()),
            intent:    Some(intent_str.into()),
            bpc:       Some(self.bpc),
            output_dir: Some(self.output_dir.to_string_lossy().into_owned()),
        });
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);

        // INSERT key loads the highlighted image onto the canvas
        if ctx.input(|i| i.key_pressed(egui::Key::Insert)) {
            if let Some(path) = self.highlighted.clone() {
                self.select_image(path);
            }
        }

        if self.show_props { self.show_printer_props(ctx); }
        if self.show_print_confirm { self.show_print_confirm(ctx); }

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
            .show(ctx, |ui| self.draw_canvas(ui));
    }
}

// ── Standalone helpers ────────────────────────────────────────────────────────

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

fn load_thumb(path: PathBuf, size: u32, tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>)>) {
    if let Ok(img) = image::open(&path) {
        let thumb = img.thumbnail(size, size).into_rgb8();
        let w = thumb.width() as usize;
        let h = thumb.height() as usize;
        let pixels = thumb.into_raw()
            .chunks_exact(3)
            .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
            .collect();
        let _ = tx.send((path, ColorImage { size: [w, h], pixels }, None));
    } else {
        // Signal failure by sending an empty 1×1 magenta image
        let _ = tx.send((path, ColorImage {
            size: [1, 1],
            pixels: vec![Color32::from_rgb(200, 0, 80)],
        }, None));
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
