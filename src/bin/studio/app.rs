use eframe::egui::{self, Color32, ColorImage, Context};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::thread;
use uuid::Uuid;

use vibeprint::{
    layout_engine::{self, Point},
    monitor_icc,
    printer_discovery::{self, DiscoveryEvent},
    processor::{self},
};

use crate::icc::{
    apply_preview_transform, extract_file_date, extract_file_size, transform_preview_border_color,
};
use crate::types::{
    print_sizes, AppState, Borders, CutMarks, Engine, IccProfileEntry, IccProfileFilter,
    IccProfileSource, Intent, LoadKind, ProcState, ProcessTarget, RightTab, Settings, FIT_PAGE_IDX,
    MAX_PREVIEW_PX, QUEUE_SPACING_IN, THUMB_PX,
};
use crate::utils::{extract_embedded_icc_from_bytes, is_image, load_full_image_on_demand, load_thumb};

/// Main application wrapper
pub struct App {
    pub state: AppState,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, auto_image_path: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let out_dir = dirs::desktop_dir().unwrap_or_else(|| home.clone());
        let (thumb_tx, thumb_rx) = channel::<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>();

        // Dedicated staging thread: processes one image at a time (no HDD thrashing)
        let (stager_tx, stager_rx) = channel::<PathBuf>();
        let stager_thumb_tx = thumb_tx.clone();
        thread::spawn(move || {
            while let Ok(mut path) = stager_rx.recv() {
                while let Ok(newer) = stager_rx.try_recv() {
                    path = newer;
                }
                let data = match std::fs::read(&path) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let embedded_icc = extract_embedded_icc_from_bytes(&data);
                if let Ok(img) = image::load_from_memory(&data) {
                    let rgb = img.into_rgb8();
                    let size = [rgb.width() as usize, rgb.height() as usize];
                    let pixels = rgb
                        .into_raw()
                        .chunks_exact(3)
                        .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                        .collect();
                    let _ = stager_thumb_tx.send((
                        path,
                        ColorImage { size, pixels },
                        embedded_icc,
                        LoadKind::FullResStaged,
                    ));
                }
            }
        });

        let s = load_settings();

        let start_dir = s
            .current_dir
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| home.clone());

        let saved_out_dir = s
            .output_dir
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or(out_dir);

        let saved_icc: Option<IccProfileEntry> = s
            .output_icc
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .map(|path| {
                let (description, date, file_size) = if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(profile) = lcms2::Profile::new_icc(&bytes) {
                        let desc = profile
                            .info(lcms2::InfoType::Description, lcms2::Locale::none())
                            .unwrap_or_else(|| {
                                path.file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Unknown")
                                    .to_string()
                            });
                        let d = extract_file_date(&path);
                        let s = extract_file_size(&path);
                        (desc, d, s)
                    } else {
                        (
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Unknown")
                                .to_string(),
                            extract_file_date(&path),
                            extract_file_size(&path),
                        )
                    }
                } else {
                    (
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Unknown")
                            .to_string(),
                        extract_file_date(&path),
                        extract_file_size(&path),
                    )
                };
                IccProfileEntry {
                    path,
                    description,
                    date,
                    file_size,
                    source: IccProfileSource::User,
                }
            });

        let saved_engine = match s.engine.as_deref() {
            Some("lanczos3") => Engine::Lanczos3,
            Some("iterative") => Engine::Iterative,
            Some("mitchell") => Engine::MitchellEwa,
            Some("mitchell-sharp") => Engine::MitchellEwaSharp,
            Some("catmullrom") | Some("mks") => Engine::Mks,
            _ => Engine::MitchellEwa,
        };
        let saved_intent = match s.intent.as_deref() {
            Some("perceptual") => Intent::Perceptual,
            Some("saturation") => Intent::Saturation,
            _ => Intent::Relative,
        };
        let saved_icc_filter = match s.icc_filter.as_deref() {
            Some("all") => IccProfileFilter::All,
            Some("system") => IccProfileFilter::System,
            Some("user") => IccProfileFilter::User,
            _ => IccProfileFilter::System,
        };

        let saved_show_log = s.show_log.unwrap_or(false);

        // Load monitor ICC profile from settings or auto-detect
        let mut deferred_logs: Vec<String> = Vec::new();
        let monitor_icc_profile = match s.monitor_icc_override {
            Some(ref path) => {
                let path = PathBuf::from(path);
                if path.is_file() {
                    if let Ok(bytes) = std::fs::read(&path) {
                        Some(bytes)
                    } else {
                        deferred_logs
                            .push(format!("⚠ Failed to read monitor ICC: {}", path.display()));
                        monitor_icc::get_monitor_profile()
                    }
                } else {
                    deferred_logs.push(format!("⚠ Monitor ICC not found: {}", path.display()));
                    monitor_icc::get_monitor_profile()
                }
            }
            None => monitor_icc::get_monitor_profile(),
        };

        let pending_user_border = {
            if let (Some(l), Some(r), Some(t), Some(b)) = (
                s.user_border_l,
                s.user_border_r,
                s.user_border_t,
                s.user_border_b,
            ) {
                Some(Borders {
                    left: l,
                    right: r,
                    top: t,
                    bottom: b,
                })
            } else if let Some(v) = s.user_border_in {
                Some(Borders {
                    left: v,
                    right: v,
                    top: v,
                    bottom: v,
                })
            } else {
                None
            }
        };

        let mut state = AppState::new(
            thumb_tx,
            thumb_rx,
            start_dir,
            saved_out_dir,
            saved_icc,
            saved_engine,
            saved_intent,
            s.sharpen.unwrap_or(5),
            s.depth16.unwrap_or(true),
            s.target_dpi.unwrap_or(720),
            saved_icc_filter,
            s.printer_name,
            s.page_size_name,
            pending_user_border,
            monitor_icc_profile,
            printer_discovery::spawn_discovery(),
            saved_show_log,
            s.bpc.unwrap_or(true),
            s.use_metric.unwrap_or(false),
            s.safe_8bit_tiff_print_path.unwrap_or(false),
        );
        state.stager_tx = Some(stager_tx);
        state.pending_extra_option_indices = s.extra_option_indices;
        state.pending_media_type_key = s.media_type_key;
        state.pending_input_slot_key = s.input_slot_key;
        state.monitor_icc_override = s.monitor_icc_override.clone();
        state.pref_override_checked = s.monitor_icc_override.is_some();
        state.cut_marks = s
            .cut_marks
            .as_deref()
            .map(CutMarks::from_label)
            .unwrap_or(CutMarks::None);
        state.log.extend(deferred_logs);

        if state.monitor_icc_profile.is_none() {
            state.log.push("⚠ No monitor ICC profile found".into());
        }

        let mut app = Self { state };

        if let Some(path) = auto_image_path {
            if path.exists() && is_image(&path) {
                app.state
                    .log
                    .push(format!("Auto-loading image: {}", path.display()));
                app.state.auto_enqueue_path = Some(path.clone());
                app.state.auto_enqueue_pending = true;
                app.stage_image(path);
            } else {
                app.state.log.push(format!(
                    "⚠ CLI image path not found or not an image: {}",
                    path.display()
                ));
            }
        }

        app.scan_dir();
        app
    }

    pub(crate) fn scan_dir(&mut self) {
        self.state.tree_children_cache.clear();
        self.state.subdirs.clear();
        self.state.image_files.clear();

        let Ok(read) = std::fs::read_dir(&self.state.current_dir) else {
            return;
        };

        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| (e.path().is_file(), e.file_name()));

        let selected = self.state.selected.clone();
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                self.state.subdirs.push((name, path));
            } else if is_image(&path) {
                if selected.as_ref() != Some(&path) {
                    let tx = self.state.thumb_tx.clone();
                    let p = path.clone();
                    let px = self.thumb_load_px();
                    self.state.thumb_pool.spawn(move || load_thumb(p, px, tx));
                }
                self.state.image_files.push(path);
            }
        }
    }

    pub(crate) fn thumb_load_px(&self) -> u32 {
        ((THUMB_PX as f32) * self.state.thumb_zoom)
            .round()
            .max(32.0) as u32
    }

    pub(crate) fn reload_thumbs(&mut self) {
        let selected = self.state.selected.clone();
        self.state
            .thumbs
            .retain(|p, _| selected.as_ref() == Some(p));
        let px = self.thumb_load_px();
        for path in &self.state.image_files {
            if selected.as_ref() == Some(path) {
                continue;
            }
            let tx = self.state.thumb_tx.clone();
            let p = path.clone();
            self.state.thumb_pool.spawn(move || load_thumb(p, px, tx));
        }
    }

    pub(crate) fn navigate(&mut self, path: PathBuf) {
        if path == self.state.current_dir {
            return;
        }
        let prev = self.state.current_dir.clone();
        self.state.nav_history.push(prev);
        self.state.nav_forward.clear();
        self.state.current_dir = path.clone();
        self.state.addr_bar = path.to_string_lossy().into_owned();
        self.state.selected_paths.clear();
        let sel = self.state.selected.clone();
        self.state.thumbs.retain(|p, _| sel.as_ref() == Some(p));
        self.scan_dir();
    }

    pub(crate) fn nav_back(&mut self) {
        if let Some(prev) = self.state.nav_history.pop() {
            let cur = self.state.current_dir.clone();
            self.state.nav_forward.push(cur);
            self.state.current_dir = prev.clone();
            self.state.addr_bar = prev.to_string_lossy().into_owned();
            self.state.selected_paths.clear();
            let sel = self.state.selected.clone();
            self.state.thumbs.retain(|p, _| sel.as_ref() == Some(p));
            self.scan_dir();
        }
    }

    pub(crate) fn nav_fwd(&mut self) {
        if let Some(next) = self.state.nav_forward.pop() {
            let cur = self.state.current_dir.clone();
            self.state.nav_history.push(cur);
            self.state.current_dir = next.clone();
            self.state.addr_bar = next.to_string_lossy().into_owned();
            self.state.selected_paths.clear();
            let sel = self.state.selected.clone();
            self.state.thumbs.retain(|p, _| sel.as_ref() == Some(p));
            self.scan_dir();
        }
    }

    pub(crate) fn stage_image(&mut self, path: PathBuf) {
        self.state.staged = Some(path.clone());
        self.state.staged_embedded_icc = None;
        self.state.staged_source_image = None;
        self.state.staged_img_size = None;
        if !self.state.auto_enqueue_pending {
            self.state.right_tab = RightTab::ImageProperties;
        }

        let _ = self.state.stager_tx.as_ref().unwrap().send(path);
    }

    pub(crate) fn start_batch_enqueue(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let valid: Vec<PathBuf> = paths.into_iter().filter(|p| is_image(p)).collect();
        if valid.is_empty() {
            self.state.log.push("⚠ No valid images to enqueue".into());
            return;
        }
        self.state
            .log
            .push(format!("Enqueuing {} image(s) with Fit to Page...", valid.len()));

        // Clear file-browser selection after drop
        self.state.selected_paths.clear();
        self.state.highlighted = None;
        self.state.batch_add_mode = false;
        self.state.batch_target_size_idx = None;

        let first = valid[0].clone();
        self.state.auto_enqueue_queue = valid.into_iter().skip(1).collect();
        self.state.auto_enqueue_pending = true;
        self.state.auto_enqueue_path = Some(first.clone());

        self.stage_image(first);
    }

    pub(crate) fn start_batch_enqueue_with_size(&mut self, size_idx: usize) {
        if self.state.selected_paths.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.state.selected_paths.iter().cloned().collect();
        let valid: Vec<PathBuf> = paths.into_iter().filter(|p| is_image(p)).collect();
        if valid.is_empty() {
            self.state.log.push("⚠ No valid images to enqueue".into());
            return;
        }

        let size_label = if size_idx == FIT_PAGE_IDX {
            "Fit to Page".to_string()
        } else {
            print_sizes(self.state.use_metric)
                .get(size_idx)
                .map(|(_, _, label)| label.to_string())
                .unwrap_or_else(|| "custom".to_string())
        };
        self.state.log.push(format!(
            "Enqueuing {} image(s) with '{}'...",
            valid.len(),
            size_label
        ));

        // Clear file-browser selection after enqueue
        self.state.selected_paths.clear();
        self.state.highlighted = None;
        self.state.batch_add_mode = false;

        let first = valid[0].clone();
        self.state.auto_enqueue_queue = valid.into_iter().skip(1).collect();
        self.state.auto_enqueue_pending = true;
        self.state.auto_enqueue_path = Some(first.clone());
        self.state.batch_target_size_idx = Some(size_idx);

        self.stage_image(first);
    }

    pub(crate) fn mark_preview_dirty(&mut self) {
        self.state.preview_dirty = true;
        self.state.preview_cache_page = None;
    }

    pub(crate) fn rebuild_canvas_texture(&mut self, ctx: &Context) {
        // Compute ICC settings hash — only rebuilds transform when settings actually change
        let icc_hash = self.icc_settings_hash();
        let icc_changed = icc_hash != self.state.preview_icc_settings_hash;
        if icc_changed {
            self.state.preview_icc_images.clear();
            self.state.preview_icc_settings_hash = icc_hash;
            self.state.preview_textures.clear();
            self.state.preview_border_colors.clear();
        }

        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = self
            .state
            .queue
            .iter()
            .filter(|q| q.page == self.state.current_page)
            .filter_map(|q| {
                if seen.insert(q.filepath.clone()) {
                    Some(q.filepath.clone())
                } else {
                    None
                }
            })
            .collect();

        for path in paths {
            // Skip items that already have a valid texture (no ICC change)
            if !icc_changed && self.state.preview_textures.contains_key(&path) {
                if self.state.selected.as_ref() == Some(&path) {
                    if let Some(tex) = self.state.preview_textures.get(&path) {
                        self.state.canvas_tex = Some(tex.clone());
                    }
                }
                continue;
            }

            let ci = if let Some(cached) = self.state.preview_icc_images.get(&path) {
                // Cache hit — ICC settings unchanged, skip expensive transform
                cached.clone()
            } else {
                // Clone base image to release the &mut self borrow from ensure_full_image_loaded
                let Some(base) = self.ensure_full_image_loaded(&path, ctx).cloned() else {
                    continue;
                };
                let ci = self.build_preview_image(&path, &base);
                self.state
                    .preview_icc_images
                    .insert(path.clone(), ci.clone());
                ci
            };

            let tex_name = format!("page_preview::{}", path.to_string_lossy());
            let tex = ctx.load_texture(&tex_name, ci, egui::TextureOptions::LINEAR);
            self.state
                .preview_textures
                .insert(path.clone(), tex.clone());

            if self.state.selected.as_ref() == Some(&path) {
                self.state.canvas_tex = Some(tex);
            }
        }

        if let Some(sel) = &self.state.selected {
            if let Some(ci) = self.state.full_images.get(sel) {
                self.state.canvas_img_size = Some(ci.size);
            }
        }

        self.state.preview_cache_page = Some(self.state.current_page);
        self.state.preview_dirty = false;
    }

    /// Build a preview ColorImage from the cached full image, applying ICC transform if needed.
    fn build_preview_image(&self, path: &PathBuf, base: &ColorImage) -> ColorImage {
        if let Some(ref monitor_profile) = self.state.monitor_icc_profile {
            let (src_w, src_h) = (base.size[0], base.size[1]);
            let max_dim = src_w.max(src_h);

            // For large images, downscale before ICC transform to reduce CPU work
            let pixel_bytes_base: Vec<u8> = base
                .pixels
                .iter()
                .flat_map(|c| [c.r(), c.g(), c.b()])
                .collect();

            let (scale_w, scale_h, mut pixel_bytes) = if max_dim > MAX_PREVIEW_PX as usize {
                let scale = MAX_PREVIEW_PX as f64 / max_dim as f64;
                let new_w = (src_w as f64 * scale).round() as u32;
                let new_h = (src_h as f64 * scale).round() as u32;
                let rgb = image::RgbImage::from_raw(src_w as u32, src_h as u32, pixel_bytes_base)
                    .expect("RgbImage from_raw with valid dimensions");
                let scaled = image::imageops::resize(
                    &rgb,
                    new_w,
                    new_h,
                    image::imageops::FilterType::CatmullRom,
                );
                (new_w as usize, new_h as usize, scaled.into_raw())
            } else {
                (src_w, src_h, pixel_bytes_base)
            };

            let src_icc = self
                .state
                .embedded_icc_by_path
                .get(path)
                .and_then(|v| v.as_deref());

            if apply_preview_transform(
                monitor_profile,
                src_icc,
                self.state.output_icc.as_ref().map(|e| &e.path),
                &mut pixel_bytes,
                self.state.intent.to_lcms(),
                self.state.bpc,
                self.state.softproof_enabled,
            )
            .is_some()
            {
                ColorImage {
                    size: [scale_w, scale_h],
                    pixels: pixel_bytes
                        .chunks_exact(3)
                        .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                        .collect(),
                }
            } else {
                base.clone()
            }
        } else {
            base.clone()
        }
    }

    /// Return the border color as it should appear in the canvas preview.
    ///
    /// When softproof is disabled, returns the raw sRGB color directly.
    /// When softproof is enabled, transforms the sRGB color through the
    /// output→monitor display leg so it reflects how the border will print.
    /// Results are cached per unique color per ICC-settings epoch.
    pub(crate) fn preview_border_color(&mut self, rgb: [u8; 3]) -> Color32 {
        if !self.state.softproof_enabled {
            return Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        }
        if let Some(&cached) = self.state.preview_border_colors.get(&rgb) {
            return cached;
        }
        let Some(ref monitor_profile) = self.state.monitor_icc_profile else {
            return Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        };
        let transformed = transform_preview_border_color(
            monitor_profile,
            self.state.output_icc.as_ref().map(|e| &e.path),
            self.state.intent.to_lcms(),
            self.state.bpc,
            rgb,
        );
        let color = Color32::from_rgb(transformed[0], transformed[1], transformed[2]);
        self.state.preview_border_colors.insert(rgb, color);
        color
    }

    /// Hash of ICC settings that determine the preview transform.
    fn icc_settings_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        if let Some(ref profile) = self.state.monitor_icc_profile {
            profile.hash(&mut h);
        }
        if let Some(ref entry) = self.state.output_icc {
            entry.path.hash(&mut h);
        }
        let intent_byte: u8 = match self.state.intent {
            crate::types::Intent::Perceptual => 0,
            crate::types::Intent::Relative => 1,
            crate::types::Intent::Saturation => 2,
        };
        intent_byte.hash(&mut h);
        self.state.bpc.hash(&mut h);
        self.state.softproof_enabled.hash(&mut h);
        h.finish()
    }

    /// Return the cached full-resolution image for `path`, or trigger a
    /// background load if not yet cached. Returns `None` while the load is in
    /// flight; the caller can fall back to a placeholder and rely on the
    /// background thread to mark the preview dirty (via the `thumb_rx` channel
    /// handled in `pump`) when the image becomes available.
    pub(crate) fn ensure_full_image_loaded(
        &mut self,
        path: &PathBuf,
        ctx: &Context,
    ) -> Option<&ColorImage> {
        if self.state.full_images.contains_key(path) {
            return self.state.full_images.get(path);
        }
        if !self.state.loading_images.contains(path) {
            self.state.loading_images.insert(path.clone());
            let tx = self.state.thumb_tx.clone();
            load_full_image_on_demand(path.clone(), tx, ctx.clone());
        }
        None
    }

    pub(crate) fn calc_reported_border(&self) -> Borders {
        self.state
            .caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
            .map(|ps| {
                let (l, b, r, t) = ps.imageable_area;
                let (pw, ph) = ps.paper_size;
                Borders {
                    left: l / 72.0,
                    right: (pw - r) / 72.0,
                    top: (ph - t) / 72.0,
                    bottom: b / 72.0,
                }
            })
            .unwrap_or_default()
    }

    pub(crate) fn imageable_size_in(&self) -> (f32, f32) {
        let (pw, ph) = self
            .state
            .caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
            .unwrap_or((8.5, 11.0));

        let b = &self.state.user_border;
        let w = (pw - b.left - b.right).max(0.1);
        let h = (ph - b.top - b.bottom).max(0.1);
        (w, h)
    }

    pub(crate) fn imageable_size_px(&self) -> (u32, u32) {
        let (w_in, h_in) = self.imageable_size_in();
        let dpi = self.state.target_dpi as f32;
        (
            (w_in * dpi).round().max(1.0) as u32,
            (h_in * dpi).round().max(1.0) as u32,
        )
    }

    pub(crate) fn max_imageable_size_px(&self) -> (u32, u32) {
        let (pw, ph) = self
            .state
            .caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
            .unwrap_or((8.5, 11.0));
        let b = &self.state.reported_border;
        let w = (pw - b.left - b.right).max(0.1);
        let h = (ph - b.top - b.bottom).max(0.1);
        let dpi = self.state.target_dpi as f32;
        (
            (w * dpi).round().max(1.0) as u32,
            (h * dpi).round().max(1.0) as u32,
        )
    }

    pub(crate) fn border_offset_px(&self) -> (u32, u32) {
        let dpi = self.state.target_dpi as f32;
        let dx = ((self.state.user_border.left - self.state.reported_border.left) * dpi)
            .round()
            .max(0.0) as u32;
        let dy = ((self.state.user_border.top - self.state.reported_border.top) * dpi)
            .round()
            .max(0.0) as u32;
        (dx, dy)
    }

    pub(crate) fn queued_box_px(&self, qi: &vibeprint::layout_engine::QueuedImage) -> (u32, u32) {
        if qi.placed_w_px > 0 && qi.placed_h_px > 0 {
            return (qi.placed_w_px, qi.placed_h_px);
        }
        let (w_in, h_in) = qi.size.as_inches();
        let (w_in, h_in) = if qi.rotation > 0.0 {
            (h_in, w_in)
        } else {
            (w_in, h_in)
        };
        let dpi = self.state.target_dpi as f32;

        // For outer borders: expand the cell size (border adds outside)
        // For inner borders: shrink the cell size (border eats inside)
        // IMPORTANT: When crop_inverted, swap dimensions BEFORE adjustment
        let (w_in, h_in) = if qi.border_type == vibeprint::layout_engine::BorderType::Outer {
            let border_in = qi.border_width_pt / 72.0; // Convert pt to inches
            let (expand_w, expand_h) = if qi.crop_inverted && !qi.force_original_orientation {
                (h_in, w_in) // Swap before expansion
            } else {
                (w_in, h_in)
            };
            let (expanded_w, expanded_h) = (expand_w + border_in * 2.0, expand_h + border_in * 2.0);
            if qi.crop_inverted && !qi.force_original_orientation {
                (expanded_h, expanded_w) // Swap back
            } else {
                (expanded_w, expanded_h)
            }
        } else if qi.border_type == vibeprint::layout_engine::BorderType::Inner
            && qi.border_width_pt > 0.0
        {
            let border_in = qi.border_width_pt / 72.0; // Convert pt to inches
            let (shrink_w, shrink_h) = if qi.crop_inverted && !qi.force_original_orientation {
                (h_in, w_in) // Swap before shrinking
            } else {
                (w_in, h_in)
            };
            let (shrunk_w, shrunk_h) = (
                (shrink_w - border_in * 2.0).max(0.1),
                (shrink_h - border_in * 2.0).max(0.1),
            );
            if qi.crop_inverted && !qi.force_original_orientation {
                (shrunk_h, shrunk_w) // Swap back
            } else {
                (shrunk_w, shrunk_h)
            }
        } else {
            (w_in, h_in)
        };

        (
            (w_in * dpi).round().max(1.0) as u32,
            (h_in * dpi).round().max(1.0) as u32,
        )
    }

    pub(crate) fn size_from_idx(
        &self,
        idx: usize,
        src_size_px: Option<(u32, u32)>,
    ) -> Option<vibeprint::layout_engine::PrintSize> {
        use vibeprint::layout_engine::{PrintSize, Unit};

        let sizes = print_sizes(self.state.use_metric);
        if idx < sizes.len() {
            let (w, h, _) = sizes[idx];
            let unit = if self.state.use_metric {
                Unit::Millimeters
            } else {
                Unit::Inches
            };
            return Some(PrintSize {
                width: w,
                height: h,
                unit,
            });
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
                let (w, h) = if rw * rh > nw * nh {
                    (rw, rh)
                } else {
                    (nw, nh)
                };
                return Some(PrintSize {
                    width: w,
                    height: h,
                    unit: Unit::Inches,
                });
            }
            return Some(PrintSize {
                width: ia_w_in,
                height: ia_h_in,
                unit: Unit::Inches,
            });
        }
        None
    }

    pub(crate) fn relayout_queue(&mut self) {
        let (page_w_px, page_h_px) = self.imageable_size_px();
        let dpi_f64 = self.state.target_dpi as f64;

        // When cut marks are enabled, reserve space for the mark legs so no
        // mark leg crosses into the user-defined border.  Both the layout area
        // and every resulting position are adjusted by this inset.
        let (inset_l, inset_r, inset_t, inset_b) = match self.state.cut_marks {
            CutMarks::None => (0u32, 0u32, 0u32, 0u32),
            CutMarks::Crop => {
                let v = ((9.0_f64 / 72.0) * dpi_f64).round().max(1.0) as u32;
                (v, v, v, v)
            }
            CutMarks::GuideLines => {
                // Guide lines draw a half-point stroke just outside each
                // placement edge.  Reserve the stroke width on all sides so
                // the line always stays fully inside the user-imageable area
                // and remains visible in both the canvas preview and the
                // processor output.
                let width_px =
                    ((0.5_f64 / 72.0) * dpi_f64).round().max(1.0) as u32;
                (width_px, width_px, width_px, width_px)
            }
        };

        // Reduce the layout canvas by the insets.
        let layout_w =
            page_w_px.saturating_sub(inset_l + inset_r).max(1);
        let layout_h =
            page_h_px.saturating_sub(inset_t + inset_b).max(1);

        let result = layout_engine::layout_queue(
            &self.state.queue,
            layout_w,
            layout_h,
            self.state.target_dpi,
            QUEUE_SPACING_IN,
        );

        for qi in &mut self.state.queue {
            if let Some(p) = result.placements.get(&qi.id) {
                qi.position = Point {
                    x: p.x_px.saturating_add(inset_l),
                    y: p.y_px.saturating_add(inset_t),
                };
                qi.page = p.page;
                qi.rotation = p.rotation_deg;
                qi.placed_w_px = p.w_px;
                qi.placed_h_px = p.h_px;
            }
        }

        // Override position for freehand items with their saved positions.
        // Clamp within the reduced layout canvas (asymmetric insets).
        let dpi = self.state.target_dpi as f32;
        for qi in &mut self.state.queue {
            if qi.freehand_placement {
                let x_px =
                    (qi.freehand_x_pt * dpi / 72.0).round().max(0.0) as u32;
                let y_px =
                    (qi.freehand_y_pt * dpi / 72.0).round().max(0.0) as u32;
                let box_w = qi.placed_w_px.max(1);
                let box_h = qi.placed_h_px.max(1);
                let max_x = page_w_px
                    .saturating_sub(box_w)
                    .saturating_sub(inset_r);
                let max_y = page_h_px
                    .saturating_sub(box_h)
                    .saturating_sub(inset_b);
                qi.position.x =
                    x_px.clamp(inset_l, max_x.max(inset_l));
                qi.position.y =
                    y_px.clamp(inset_t, max_y.max(inset_t));
            }
        }

        self.state.page_count = result.page_count.max(1);
        if self.state.current_page >= self.state.page_count {
            self.state.current_page = self.state.page_count.saturating_sub(1);
        }
        self.mark_preview_dirty();
    }

    pub(crate) fn enqueue_staged_with_idx(&mut self, idx: usize) -> bool {
        let Some(path) = self.state.staged.clone() else {
            return false;
        };
        let Some(src) = self.state.staged_source_image.as_ref() else {
            self.state.log.push("⚠ Image still loading…".into());
            return false;
        };
        let size = src.size;
        let src_size = (size[0] as u32, size[1] as u32);
        let Some(print_size) = self.size_from_idx(idx, Some(src_size)) else {
            return false;
        };
        let fit_to_page = idx == FIT_PAGE_IDX;

        self.state
            .queue
            .push(vibeprint::layout_engine::QueuedImage {
                id: Uuid::new_v4(),
                filepath: path.clone(),
                size: print_size,
                fit_to_page,
                center_to_page: false,
                freehand_placement: false,
                freehand_x_pt: 0.0,
                freehand_y_pt: 0.0,
                source_icc: None,
                position: Point::default(),
                page: 0,
                rotation: 0.0,
                placed_w_px: 0,
                placed_h_px: 0,
                src_size_px: Some(src_size),
                crop_enabled: false,
                crop_u0: None,
                crop_v0: None,
                crop_u1: None,
                crop_v1: None,
                crop_inverted: false,
                border_type: vibeprint::layout_engine::BorderType::None,
                border_width_pt: 4.0,
                border_color: [0, 0, 0],
                force_original_orientation: false,
            });
        self.state.selected_queue_id = self.state.queue.last().map(|q| q.id);
        self.state.selected = Some(path.clone());
        self.state.selected_source_image = Some(src.clone());
        self.state.selected_embedded_icc = self.state.staged_embedded_icc.clone();
        self.state.canvas_img_size = Some(size);
        self.state.full_images.insert(path.clone(), src.clone());
        self.state
            .embedded_icc_by_path
            .insert(path, self.state.staged_embedded_icc.clone());

        self.state.staged = None;
        self.state.staged_embedded_icc = None;
        self.state.staged_source_image = None;
        self.state.staged_img_size = None;

        self.relayout_queue();
        if let Some(id) = self.state.selected_queue_id {
            if let Some(item) = self.state.queue.iter().find(|q| q.id == id) {
                self.state.current_page = item.page;
            }
        }
        true
    }

    pub(crate) fn enqueue_staged_with_size(&mut self, w_in: f32, h_in: f32) -> bool {
        use vibeprint::layout_engine::{PrintSize, Unit};
        let Some(path) = self.state.staged.clone() else {
            return false;
        };
        let Some(src) = self.state.staged_source_image.as_ref() else {
            self.state.log.push("⚠ Image still loading…".into());
            return false;
        };
        let size = src.size;
        let src_size = (size[0] as u32, size[1] as u32);
        // Normalize to portrait notation (w <= h) matching PRINT_SIZES convention
        let (nw, nh) = if w_in <= h_in {
            (w_in, h_in)
        } else {
            (h_in, w_in)
        };
        let print_size = PrintSize {
            width: nw,
            height: nh,
            unit: Unit::Inches,
        };

        self.state
            .queue
            .push(vibeprint::layout_engine::QueuedImage {
                id: uuid::Uuid::new_v4(),
                filepath: path.clone(),
                size: print_size,
                fit_to_page: false,
                center_to_page: false,
                freehand_placement: false,
                freehand_x_pt: 0.0,
                freehand_y_pt: 0.0,
                source_icc: None,
                position: vibeprint::layout_engine::Point::default(),
                page: 0,
                rotation: 0.0,
                placed_w_px: 0,
                placed_h_px: 0,
                src_size_px: Some(src_size),
                crop_enabled: false,
                crop_u0: None,
                crop_v0: None,
                crop_u1: None,
                crop_v1: None,
                crop_inverted: false,
                border_type: vibeprint::layout_engine::BorderType::None,
                border_width_pt: 4.0,
                border_color: [0, 0, 0],
                force_original_orientation: false,
            });
        self.state.selected_queue_id = self.state.queue.last().map(|q| q.id);
        self.state.selected = Some(path.clone());
        self.state.selected_source_image = Some(src.clone());
        self.state.selected_embedded_icc = self.state.staged_embedded_icc.clone();
        self.state.canvas_img_size = Some(size);
        self.state.full_images.insert(path.clone(), src.clone());
        self.state
            .embedded_icc_by_path
            .insert(path, self.state.staged_embedded_icc.clone());

        self.state.staged = None;
        self.state.staged_embedded_icc = None;
        self.state.staged_source_image = None;
        self.state.staged_img_size = None;

        self.relayout_queue();
        if let Some(id) = self.state.selected_queue_id {
            if let Some(item) = self.state.queue.iter().find(|q| q.id == id) {
                self.state.current_page = item.page;
            }
        }
        true
    }

    pub(crate) fn update_selected_queue_size(&mut self, w_in: f32, h_in: f32) {
        use vibeprint::layout_engine::{PrintSize, Unit};
        let sel = self.state.selected_queue_id;
        // Normalize to portrait notation (w <= h) matching PRINT_SIZES convention
        let (w_in, h_in) = if w_in <= h_in {
            (w_in, h_in)
        } else {
            (h_in, w_in)
        };

        if let Some(item) = self.selected_queue_mut() {
            let old_size = item.size.as_inches();
            let new_size = (w_in, h_in);

            item.size = PrintSize {
                width: w_in,
                height: h_in,
                unit: Unit::Inches,
            };
            item.fit_to_page = false;

            // Clamp border_width_pt to 20% of new longest side
            let max_border_pt = w_in.max(h_in) * 0.2 * 72.0;
            let old_border_pt = item.border_width_pt;
            item.border_width_pt = item.border_width_pt.min(max_border_pt);
            let new_border_pt = item.border_width_pt;

            // Recalculate crop UVs when aspect ratio changes significantly
            let old_border_in = old_border_pt / 72.0;
            let new_border_in = new_border_pt / 72.0;
            let is_inner = item.border_type == vibeprint::layout_engine::BorderType::Inner;
            let (old_vis_w, old_vis_h) = if is_inner && old_border_in > 0.0 {
                (
                    (old_size.0 - old_border_in * 2.0).max(0.1),
                    (old_size.1 - old_border_in * 2.0).max(0.1),
                )
            } else {
                old_size
            };
            let (new_vis_w, new_vis_h) = if is_inner && new_border_in > 0.0 {
                (
                    (new_size.0 - new_border_in * 2.0).max(0.1),
                    (new_size.1 - new_border_in * 2.0).max(0.1),
                )
            } else {
                new_size
            };
            let old_vis_aspect = old_vis_w / old_vis_h;
            let new_vis_aspect = new_vis_w / new_vis_h;

            if let (Some(u0), Some(v0), Some(u1), Some(v1)) =
                (item.crop_u0, item.crop_v0, item.crop_u1, item.crop_v1)
            {
                let aspect_diff =
                    (old_vis_aspect - new_vis_aspect).abs() / old_vis_aspect.max(new_vis_aspect);
                if aspect_diff > 0.05 {
                    let (sw, sh) = item.src_size_px.unwrap_or((1, 1));
                    let src_landscape = (sw as f32) > (sh as f32);
                    let (oriented_w, oriented_h) = if src_landscape {
                        (h_in, w_in)
                    } else {
                        (w_in, h_in)
                    };

                    let fitted_no_rot = {
                        let s = (oriented_w / sw as f32).min(oriented_h / sh as f32);
                        (sw as f32 * s) * (sh as f32 * s)
                    };
                    let fitted_rot = {
                        let s = (oriented_w / sh as f32).min(oriented_h / sw as f32);
                        (sh as f32 * s) * (sw as f32 * s)
                    };
                    let will_rotate = fitted_rot > fitted_no_rot;
                    let (full_w, full_h) = if item.force_original_orientation && item.crop_inverted {
                        (oriented_h, oriented_w)
                    } else if will_rotate {
                        (oriented_h, oriented_w)
                    } else {
                        (oriented_w, oriented_h)
                    };

                    let border_in = item.border_width_pt / 72.0;
                    let (vis_w, vis_h) = if is_inner && border_in > 0.0 {
                        (
                            (full_w - border_in * 2.0).max(0.1),
                            (full_h - border_in * 2.0).max(0.1),
                        )
                    } else {
                        (full_w, full_h)
                    };

                    let old_center_u = (u0 + u1) / 2.0;
                    let old_center_v = (v0 + v1) / 2.0;
                    let old_crop_area = (u1 - u0) * (v1 - v0);

                    let sw_f = sw as f32;
                    let sh_f = sh as f32;
                    let src_aspect = if will_rotate {
                        sh_f / sw_f
                    } else {
                        sw_f / sh_f
                    };
                    let target_aspect = (vis_w / vis_h) / src_aspect;

                    let new_crop_w = (old_crop_area * target_aspect).sqrt();
                    let new_crop_h = (old_crop_area / target_aspect).sqrt();
                    item.crop_u0 = Some((old_center_u - new_crop_w / 2.0).max(0.0));
                    item.crop_v0 = Some((old_center_v - new_crop_h / 2.0).max(0.0));
                    item.crop_u1 = Some((old_center_u + new_crop_w / 2.0).min(1.0));
                    item.crop_v1 = Some((old_center_v + new_crop_h / 2.0).min(1.0));
                }
            }
        }
        self.relayout_queue();
        if let Some(id) = sel {
            if let Some(item) = self.state.queue.iter().find(|q| q.id == id) {
                self.state.current_page = item.page;
            }
        }
    }

    pub(crate) fn selected_queue_mut(
        &mut self,
    ) -> Option<&mut vibeprint::layout_engine::QueuedImage> {
        let id = self.state.selected_queue_id?;
        self.state.queue.iter_mut().find(|q| q.id == id)
    }

    pub(crate) fn selected_queue(&self) -> Option<&vibeprint::layout_engine::QueuedImage> {
        let id = self.state.selected_queue_id?;
        self.state.queue.iter().find(|q| q.id == id)
    }

    pub(crate) fn update_selected_queue_size_idx(&mut self, idx: usize) {
        let src_size = self.selected_queue().and_then(|q| q.src_size_px);
        let Some(ps) = self.size_from_idx(idx, src_size) else {
            return;
        };
        let sel = self.state.selected_queue_id;
        // Get imageable size before mutable borrow
        let (ia_w_in, ia_h_in) = self.imageable_size_in();

        if let Some(item) = self.selected_queue_mut() {
            let old_size = item.size.as_inches();
            let new_size = ps.as_inches();

            item.size = ps;
            item.fit_to_page = idx == FIT_PAGE_IDX;

            // Clamp border to new max (20% of longest side in points)
            let (cell_w_in, cell_h_in) = if item.fit_to_page {
                (ia_w_in, ia_h_in)
            } else {
                item.size.as_inches()
            };
            let longest_side_in = cell_w_in.max(cell_h_in);
            let max_border_pt = longest_side_in * 0.2 * 72.0;
            let old_border_pt = item.border_width_pt;
            item.border_width_pt = item.border_width_pt.min(max_border_pt);
            let new_border_pt = item.border_width_pt;

            // Calculate visible area aspects (cell minus border) for proper comparison
            let old_border_in = old_border_pt / 72.0;
            let new_border_in = new_border_pt / 72.0;
            let old_is_inner = item.border_type == vibeprint::layout_engine::BorderType::Inner;
            let new_is_inner = old_is_inner; // Type hasn't changed yet
            let (old_visible_w, old_visible_h) = if old_is_inner && old_border_in > 0.0 {
                (
                    (old_size.0 - old_border_in * 2.0).max(0.1),
                    (old_size.1 - old_border_in * 2.0).max(0.1),
                )
            } else {
                (old_size.0, old_size.1)
            };
            let (new_visible_w, new_visible_h) = if new_is_inner && new_border_in > 0.0 {
                (
                    (new_size.0 - new_border_in * 2.0).max(0.1),
                    (new_size.1 - new_border_in * 2.0).max(0.1),
                )
            } else {
                (new_size.0, new_size.1)
            };
            // For inverted crops, swap the aspect ratios for comparison
            let old_visible_aspect = if item.crop_inverted && !item.force_original_orientation {
                old_visible_h / old_visible_w
            } else {
                old_visible_w / old_visible_h
            };
            let new_visible_aspect = if item.crop_inverted && !item.force_original_orientation {
                new_visible_h / new_visible_w
            } else {
                new_visible_w / new_visible_h
            };

            // Recalculate crop for new aspect ratio while preserving center/zoom
            if let (Some(u0), Some(v0), Some(u1), Some(v1)) =
                (item.crop_u0, item.crop_v0, item.crop_u1, item.crop_v1)
            {
                let aspect_diff = (old_visible_aspect - new_visible_aspect).abs()
                    / old_visible_aspect.max(new_visible_aspect);
                if aspect_diff > 0.05 {
                    // Aspect changed significantly - recalculate crop like border change
                    let (w_in, h_in) = if item.fit_to_page {
                        (ia_w_in, ia_h_in)
                    } else {
                        item.size.as_inches()
                    };

                    let (sw, sh) = item.src_size_px.unwrap_or((1, 1));
                    let src_landscape = (sw as f32) > (sh as f32);
                    let (oriented_w, oriented_h) = if src_landscape {
                        (h_in, w_in)
                    } else {
                        (w_in, h_in)
                    };

                    // Calculate if rotation is needed
                    let fitted_area_no_rotate = {
                        let s = (oriented_w / sw as f32).min(oriented_h / sh as f32);
                        (sw as f32 * s) * (sh as f32 * s)
                    };
                    let fitted_area_rotate = {
                        let s = (oriented_w / sh as f32).min(oriented_h / sw as f32);
                        (sh as f32 * s) * (sw as f32 * s)
                    };
                    let will_rotate = fitted_area_rotate > fitted_area_no_rotate;

                    let (full_w, full_h) = if item.force_original_orientation && item.crop_inverted {
                        (oriented_h, oriented_w)
                    } else if will_rotate {
                        (oriented_h, oriented_w)
                    } else {
                        (oriented_w, oriented_h)
                    };

                    // Adjust for inner border
                    let border_in = item.border_width_pt / 72.0;
                    let is_inner = item.border_type == vibeprint::layout_engine::BorderType::Inner;
                    let (new_visible_w, new_visible_h) = if is_inner && border_in > 0.0 {
                        (
                            (full_w - border_in * 2.0).max(0.1),
                            (full_h - border_in * 2.0).max(0.1),
                        )
                    } else {
                        (full_w, full_h)
                    };

                    // Preserve center and zoom (same logic as right_panel.rs border change)
                    let old_center_u = (u0 + u1) / 2.0;
                    let old_center_v = (v0 + v1) / 2.0;
                    let old_crop_w = u1 - u0;
                    let old_crop_h = v1 - v0;
                    let old_crop_area = old_crop_w * old_crop_h;

                    let sw_f = sw as f32;
                    let sh_f = sh as f32;
                    let src_aspect = if will_rotate {
                        sh_f / sw_f
                    } else {
                        sw_f / sh_f
                    };
                    // For inverted crops, swap the box aspect to match
                    let box_aspect = if item.crop_inverted && !item.force_original_orientation {
                        new_visible_h / new_visible_w
                    } else {
                        new_visible_w / new_visible_h
                    };
                    let target_aspect = box_aspect / src_aspect;

                    let new_crop_w = (old_crop_area * target_aspect).sqrt();
                    let new_crop_h = (old_crop_area / target_aspect).sqrt();

                    let half_w = new_crop_w / 2.0;
                    let half_h = new_crop_h / 2.0;
                    item.crop_u0 = Some((old_center_u - half_w).max(0.0));
                    item.crop_v0 = Some((old_center_v - half_h).max(0.0));
                    item.crop_u1 = Some((old_center_u + half_w).min(1.0));
                    item.crop_v1 = Some((old_center_v + half_h).min(1.0));
                }
            }
        }
        self.relayout_queue();
        if let Some(id) = sel {
            if let Some(item) = self.state.queue.iter().find(|q| q.id == id) {
                self.state.current_page = item.page;
            }
        }
    }

    pub(crate) fn sync_caps_to_selection(&mut self) {
        let name = match self.state.printers.get(self.state.printer_idx) {
            Some(p) => p.name.clone(),
            None => return,
        };
        if self.state.caps.as_ref().map(|c| &c.name) == Some(&name) {
            return;
        }
        if let Some(caps) = self.state.all_caps.get(&name) {
            self.state.props_media_idx = self
                .state
                .pending_media_type_key
                .as_deref()
                .and_then(|k| caps.media_types.iter().position(|(key, _)| key == k))
                .unwrap_or(0);
            self.state.props_slot_idx = self
                .state
                .pending_input_slot_key
                .as_deref()
                .and_then(|k| caps.input_slots.iter().position(|(key, _)| key == k))
                .unwrap_or(0);

            self.state.selected_page_size_idx =
                if let Some(ref sz_name) = self.state.pending_page_size_name {
                    if let Some(idx) = caps.page_sizes.iter().position(|ps| &ps.name == sz_name) {
                        self.state.pending_page_size_name = None;
                        idx
                    } else {
                        self.state.pending_page_size_name = None;
                        0
                    }
                } else {
                    0
                };

            self.state.extra_option_indices.clear();
            for opt in &caps.extra_options {
                self.state
                    .extra_option_indices
                    .insert(opt.key.clone(), opt.default_idx);
            }
            if let Some(saved) = self.state.pending_extra_option_indices.take() {
                for (key, idx) in saved {
                    if self.state.extra_option_indices.contains_key(&key) {
                        let max = caps
                            .extra_options
                            .iter()
                            .find(|o| o.key == key)
                            .map(|o| o.choices.len().saturating_sub(1))
                            .unwrap_or(0);
                        self.state.extra_option_indices.insert(key, idx.min(max));
                    }
                }
            }
            self.state.caps = Some(caps.clone());

            self.state.reported_border = self.calc_reported_border();
            self.state.user_border = if let Some(saved) = self.state.pending_user_border {
                let rb = &self.state.reported_border;
                Borders {
                    left: saved.left.max(rb.left),
                    right: saved.right.max(rb.right),
                    top: saved.top.max(rb.top),
                    bottom: saved.bottom.max(rb.bottom),
                }
            } else {
                self.state.reported_border
            };
            self.state.pending_user_border = None;
            self.state.border_edit_l =
                format_border_edit(self.state.user_border.left, self.state.use_metric);
            self.state.border_edit_r =
                format_border_edit(self.state.user_border.right, self.state.use_metric);
            self.state.border_edit_t =
                format_border_edit(self.state.user_border.top, self.state.use_metric);
            self.state.border_edit_b =
                format_border_edit(self.state.user_border.bottom, self.state.use_metric);

            self.relayout_queue();
        } else {
            self.state.caps = None;
            self.state.extra_option_indices.clear();
            self.state.reported_border = Borders::default();
            self.state.user_border = Borders::default();
            self.state.border_edit_l = format_border_edit(0.25, self.state.use_metric);
            self.state.border_edit_r = format_border_edit(0.25, self.state.use_metric);
            self.state.border_edit_t = format_border_edit(0.25, self.state.use_metric);
            self.state.border_edit_b = format_border_edit(0.25, self.state.use_metric);
            self.relayout_queue();
        }
    }

    pub(crate) fn pump(&mut self, ctx: &Context) {
        // Thumbnails / canvas image
        while let Ok((path, ci, embedded_icc, kind)) = self.state.thumb_rx.try_recv() {
            let name = path.to_string_lossy().to_string();
            match kind {
                LoadKind::Thumb => {
                    let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                    self.state
                        .thumbs
                        .insert(path, crate::types::ThumbState::Ready(tex));
                }
                LoadKind::FullResOnDemand => {
                    // Result of ensure_full_image_loaded()'s background load.
                    self.state.loading_images.remove(&path);
                    self.state.full_images.insert(path.clone(), ci);
                    self.state
                        .embedded_icc_by_path
                        .insert(path, embedded_icc);
                    self.mark_preview_dirty();
                }
                LoadKind::FullResStaged => {
                    if self.state.staged.as_ref() == Some(&path) {
                        let size = ci.size;
                        self.state.full_images.insert(path.clone(), ci.clone());
                        self.state
                            .embedded_icc_by_path
                            .insert(path.clone(), embedded_icc.clone());
                        self.state.staged_embedded_icc = embedded_icc;
                        self.state.staged_source_image = Some(ci);
                        self.state.staged_img_size = Some(size);

                        if self.state.auto_enqueue_pending
                            && self.state.auto_enqueue_path.as_ref() == Some(&path)
                        {
                            let size_idx = self.state.batch_target_size_idx.unwrap_or(FIT_PAGE_IDX);
                            if self.enqueue_staged_with_idx(size_idx) {
                                let size_label = if size_idx == FIT_PAGE_IDX {
                                    "Fit to Page".to_string()
                                } else {
                                    print_sizes(self.state.use_metric)
                                        .get(size_idx)
                                        .map(|(_, _, label)| label.to_string())
                                        .unwrap_or_else(|| "custom".to_string())
                                };
                                self.state.log.push(format!(
                                    "Auto-enqueued with '{}': {}",
                                    size_label,
                                    path.display()
                                ));
                            }
                            self.state.auto_enqueue_path = None;
                            // Check for more images in the batch queue
                            if let Some(next) = self.state.auto_enqueue_queue.pop_front() {
                                self.state.auto_enqueue_path = Some(next.clone());
                                self.stage_image(next);
                            } else {
                                self.state.auto_enqueue_pending = false;
                                self.state.batch_target_size_idx = None;
                                self.state.log.push("✓ Batch enqueue complete".into());
                            }
                        }
                    } else {
                        self.state.full_images.insert(path.clone(), ci.clone());
                        self.state
                            .embedded_icc_by_path
                            .insert(path.clone(), embedded_icc);
                        let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                        self.state
                            .thumbs
                            .insert(path, crate::types::ThumbState::Ready(tex));
                    }
                    self.mark_preview_dirty();
                }
            }
        }

        // Printer discovery
        let disc_events: Vec<DiscoveryEvent> = {
            let mut v = Vec::new();
            if let Some(rx) = &self.state.discovery_rx {
                while let Ok(ev) = rx.try_recv() {
                    v.push(ev);
                }
            }
            v
        };
        let mut need_sync = false;
        let mut first_caps_received = false;
        for ev in disc_events {
            match ev {
                DiscoveryEvent::PrintersListed(p) => {
                    self.state.printers = p;
                    if let Some(ref name) = self.state.pending_printer_name.clone() {
                        if let Some(idx) = self.state.printers.iter().position(|p| &p.name == name)
                        {
                            self.state.printer_idx = idx;
                        }
                        self.state.pending_printer_name = None;
                    }
                    need_sync = true;
                }
                DiscoveryEvent::CapsReady(c) => {
                    self.state.all_caps.insert(c.name.clone(), c);
                    need_sync = true;
                    if !first_caps_received {
                        first_caps_received = true;
                    }
                }
                DiscoveryEvent::Warning(w) => self.state.log.push(format!("⚠ {w}")),
                DiscoveryEvent::Error(e) => self.state.log.push(format!("✗ CUPS: {e}")),
            }
        }
        if need_sync {
            self.sync_caps_to_selection();
            // Complete discovery when: first printer ready OR timeout after printers listed
            if !self.state.discovery_complete && !self.state.printers.is_empty() {
                let has_any_caps = !self.state.all_caps.is_empty();
                if has_any_caps {
                    self.state.discovery_complete = true;
                    let ready_count = self.state.all_caps.len();
                    let total_count = self.state.printers.len();
                    if ready_count == total_count {
                        self.state
                            .log
                            .push(format!("✓ {} printer(s) ready", ready_count));
                    } else {
                        self.state.log.push(format!(
                            "✓ {}/{} printer(s) ready",
                            ready_count, total_count
                        ));
                    }
                }
            }
        }

        // Process result
        if let Some(rx) = &self.state.proc_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((paths, target)) => match target {
                        ProcessTarget::Export => {
                            if let Some(first) = paths.first() {
                                self.state.log.push(format!(
                                    "✓ Saved {} page(s). First: {}",
                                    paths.len(),
                                    first.display()
                                ));
                            } else {
                                self.state.log.push("✓ Export complete".into());
                            }
                            self.state.proc_state = ProcState::Done(paths);
                        }
                        ProcessTarget::Print => {
                            self.state
                                .log
                                .push(format!("✓ Processed {} page(s) for print", paths.len()));
                            self.state.pending_print_paths = paths;
                            self.state.show_print_confirm = true;
                            self.state.proc_state = ProcState::Idle;
                        }
                    },
                    Err(e) => {
                        self.state.log.push(format!("✗ {e}"));
                        self.state.proc_state = ProcState::Failed(e);
                    }
                }
                self.state.proc_rx = None;
            }
        }

        if matches!(self.state.proc_state, ProcState::Running) {
            ctx.request_repaint();
        }

        if self.state.print_rx.is_some() {
            ctx.request_repaint();
        }

        // Check for ICC scan completion
        if self.state.icc_scan_pending {
            if let Some(ref rx) = self.state.icc_scan_rx {
                if let Ok(profiles) = rx.try_recv() {
                    self.state.icc_profiles = profiles;
                    self.state.icc_scan_pending = false;
                    self.state.icc_scan_rx = None;
                    self.state.saved_icc_filter_for_restore = self.state.icc_profile_filter;
                    self.state.icc_profile_filter = IccProfileFilter::All;
                    self.state.icc_auto_switch_pending = true;
                    self.state.show_icc_picker = true;
                }
            }
        }

        // Print job log messages
        if let Some(rx) = &self.state.print_log_rx {
            while let Ok(msg) = rx.try_recv() {
                self.state.log.push(msg);
            }
        }

        // Print job result
        if let Some(rx) = &self.state.print_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(()) => {
                        self.state
                            .log
                            .push("✓ Print jobs submitted successfully".into());
                    }
                    Err(e) => {
                        self.state.log.push(format!("✗ Print failed: {}", e));
                    }
                }
                self.state.print_rx = None;
                self.state.print_log_rx = None;
            }
        }
    }

    pub(crate) fn start_process_export(&mut self) {
        self.start_process_with_target(ProcessTarget::Export);
    }

    pub(crate) fn start_process_print(&mut self) {
        self.start_process_with_target(ProcessTarget::Print);
    }

    fn start_process_with_target(&mut self, target: ProcessTarget) {
        if self.state.queue.is_empty() {
            self.state.log.push("⚠ Queue is empty.".into());
            return;
        }

        let (page_w_px, page_h_px) = self.max_imageable_size_px();
        let (offset_x, offset_y) = self.border_offset_px();
        let max_page = self.state.queue.iter().map(|q| q.page).max().unwrap_or(0);
        let mut per_page: Vec<Vec<processor::PagePlacement>> =
            vec![Vec::new(); max_page.saturating_add(1)];
        for q in &self.state.queue {
            let (w, h) = self.queued_box_px(q);
            // Calculate crop UVs if cropping is enabled - use processor-specific function
            let (crop_u0, crop_v0, crop_u1, crop_v1) = if let Some((src_w, src_h)) = q.src_size_px {
                // Calculate will_rotate for UV calculation - flip if crop was inverted
                // because UVs were calculated for swapped dimensions
                let will_rotate =
                    vibeprint::layout_engine::should_rotate_for_full_page(q.src_size_px, w, h);
                let will_rotate = if q.force_original_orientation {
                    false
                } else if q.crop_inverted {
                    !will_rotate
                } else {
                    will_rotate
                };
                let stored_uv = match (q.crop_u0, q.crop_v0, q.crop_u1, q.crop_v1) {
                    (Some(u0), Some(v0), Some(u1), Some(v1)) => Some((u0, v0, u1, v1)),
                    _ => None,
                };
                let uvs = crate::utils::calc_crop_uv_for_processor(
                    w as f32,
                    h as f32,
                    src_w,
                    src_h,
                    will_rotate,
                    q.crop_enabled,
                    stored_uv,
                );
                uvs
            } else {
                (0.0, 0.0, 1.0, 1.0)
            };

            // Use the same rotation logic as the layout engine for consistency
            // When crop is inverted, the UVs were calculated for swapped dimensions,
            // so flip the rotation decision to compensate
            let will_rotate =
                vibeprint::layout_engine::should_rotate_for_full_page(q.src_size_px, w, h);
            let will_rotate = if q.force_original_orientation {
                false
            } else if q.crop_inverted {
                !will_rotate
            } else {
                will_rotate
            };
            // Calculate border width in pixels for the processor
            let border_width_px = if q.border_type != vibeprint::layout_engine::BorderType::None {
                ((q.border_width_pt / 72.0) * self.state.target_dpi as f32).round() as u32
            } else {
                0
            };

            per_page[q.page].push(processor::PagePlacement {
                input: q.filepath.clone(),
                input_icc: q.source_icc.clone(),
                dest_x_px: q.position.x + offset_x,
                dest_y_px: q.position.y + offset_y,
                dest_w_px: w,
                dest_h_px: h,
                rotate_cw: will_rotate,
                crop_u0,
                crop_v0,
                crop_u1,
                crop_v1,
                border_type: q.border_type,
                border_width_px,
                border_color: q.border_color,
            });
        }

        let stem = self
            .state
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
                ProcessTarget::Export => {
                    self.state
                        .output_dir
                        .join(format!("{}_page_{:03}_vp.tif", stem, idx + 1))
                }
                ProcessTarget::Print => std::env::temp_dir().join(format!(
                    "vibeprint_{}_{}_page_{:03}.tif",
                    timestamp,
                    std::process::id(),
                    idx + 1
                )),
            })
            .collect();

        let output_icc = self.state.output_icc.as_ref().map(|e| e.path.clone());
        let target_dpi = self.state.target_dpi as f64;
        let intent = self.state.intent.to_lcms();
        let bpc = self.state.bpc;
        let engine = self.state.engine.to_proc();
        let depth = match target {
            ProcessTarget::Export => {
                if self.state.depth16 {
                    16
                } else {
                    8
                }
            }
            ProcessTarget::Print => {
                if self.state.safe_8bit_tiff_print_path {
                    8
                } else {
                    16
                }
            }
        };
        let sharpen = self.state.sharpen;

        let embed_icc = match target {
            ProcessTarget::Export => true,
            ProcessTarget::Print => !self.state.safe_8bit_tiff_print_path,
        };

        let cut_marks = match self.state.cut_marks {
            CutMarks::None => processor::CutMarkMode::None,
            CutMarks::Crop => processor::CutMarkMode::Crop,
            CutMarks::GuideLines => processor::CutMarkMode::GuideLines,
        };
        // Guide lines extend across the full max-imageable area (the processor's
        // page extent). This matches the canvas preview which clips guide lines to
        // the max-imageable region (see canvas.rs).
        let cut_mark_bounds_px = Some(processor::CutMarkBoundsPx {
            left: 0,
            top: 0,
            right: page_w_px,
            bottom: page_h_px,
        });

        if let ProcessTarget::Print = target {
            if self.state.safe_8bit_tiff_print_path {
                self.state
                    .log
                    .push("Using safe 8-bit TIFF print path (no ICC profile embedded)".into());
            } else {
                self.state
                    .log
                    .push("Using standard 16-bit → PDF print path".into());
            }
        }

        let target_clone = target.clone();
        let (tx, rx) = channel::<Result<(Vec<PathBuf>, ProcessTarget), String>>();
        self.state.proc_rx = Some(rx);
        self.state.proc_state = ProcState::Running;
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
                    embed_icc_profile: embed_icc,
                    cut_marks,
                    cut_mark_bounds_px,
                };
                if let Err(e) = processor::process_composite_page(opts) {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
                done.push(out);
            }
            let _ = tx.send(Ok((done, target_clone)));
        });
    }
}

pub(crate) fn format_border_edit(value_in: f32, use_metric: bool) -> String {
    if use_metric {
        format!("{:.0}", vibeprint::layout_engine::inches_to_mm(value_in))
    } else {
        format!("{:.3}", value_in)
    }
}

/// Load settings from disk
pub(crate) fn load_settings() -> Settings {
    let path = match config_path() {
        Some(p) => p,
        None => return Settings::default(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Settings::default(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Save settings to disk
pub(crate) fn save_settings(s: &Settings) {
    let Some(path) = config_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(path, text);
    }
}

fn config_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("vibeprint");
    p.push("settings.json");
    Some(p)
}

#[cfg(test)]
mod tests {
    use crate::types::Borders;

    fn calc_imageable_size_in(paper_w_in: f32, paper_h_in: f32, border: &Borders) -> (f32, f32) {
        let w = (paper_w_in - border.left - border.right).max(0.1);
        let h = (paper_h_in - border.top - border.bottom).max(0.1);
        (w, h)
    }

    fn calc_border_offset_px(user: &Borders, reported: &Borders, dpi: f32) -> (u32, u32) {
        let dx = ((user.left - reported.left) * dpi).round().max(0.0) as u32;
        let dy = ((user.top - reported.top) * dpi).round().max(0.0) as u32;
        (dx, dy)
    }

    #[test]
    fn asymmetric_reported_symmetric_user() {
        let reported = Borders {
            left: 0.2,
            right: 0.4,
            top: 0.3,
            bottom: 0.25,
        };
        let user = Borders {
            left: 0.5,
            right: 0.5,
            top: 0.5,
            bottom: 0.5,
        };
        let dpi = 720.0;

        let (w, h) = calc_imageable_size_in(8.5, 11.0, &user);
        assert!((w - 7.5).abs() < 0.01, "w={}", w); // 8.5 - 0.5 - 0.5
        assert!((h - 10.0).abs() < 0.01, "h={}", h); // 11.0 - 0.5 - 0.5

        let (dx, dy) = calc_border_offset_px(&user, &reported, dpi);
        assert_eq!(dx, 216); // (0.5 - 0.2) * 720
        assert_eq!(dy, 144); // (0.5 - 0.3) * 720
    }

    #[test]
    fn fully_asymmetric_borders() {
        let reported = Borders {
            left: 0.2,
            right: 0.4,
            top: 0.3,
            bottom: 0.25,
        };
        let user = Borders {
            left: 0.3,
            right: 0.6,
            top: 0.4,
            bottom: 0.35,
        };
        let dpi = 720.0;

        let (w, h) = calc_imageable_size_in(8.5, 11.0, &user);
        assert!((w - 7.6).abs() < 0.01, "w={}", w); // 8.5 - 0.3 - 0.6
        assert!((h - 10.25).abs() < 0.01, "h={}", h); // 11.0 - 0.4 - 0.35

        let (dx, dy) = calc_border_offset_px(&user, &reported, dpi);
        assert_eq!(dx, 72); // (0.3 - 0.2) * 720
        assert_eq!(dy, 72); // (0.4 - 0.3) * 720
    }

    #[test]
    fn border_dimensions_consistency() {
        let reported = Borders {
            left: 0.2,
            right: 0.4,
            top: 0.3,
            bottom: 0.25,
        };
        let user = Borders {
            left: 0.3,
            right: 0.6,
            top: 0.4,
            bottom: 0.35,
        };
        let (pw, ph) = (8.5f32, 11.0f32);
        let (ia_w, ia_h) = calc_imageable_size_in(pw, ph, &user);
        let (max_w, max_h) = calc_imageable_size_in(pw, ph, &reported);
        let expected_w = ia_w + (user.left - reported.left) + (user.right - reported.right);
        let expected_h = ia_h + (user.top - reported.top) + (user.bottom - reported.bottom);
        assert!(
            (expected_w - max_w).abs() < 0.001,
            "expected_w={} max_w={}",
            expected_w,
            max_w
        );
        assert!(
            (expected_h - max_h).abs() < 0.001,
            "expected_h={} max_h={}",
            expected_h,
            max_h
        );
    }

    #[test]
    fn user_at_reported_minimum_zero_offset() {
        let reported = Borders {
            left: 0.2,
            right: 0.4,
            top: 0.3,
            bottom: 0.25,
        };
        let user = reported;
        let _dpi = 720.0;

        let (dx, dy) = calc_border_offset_px(&user, &reported, _dpi);
        assert_eq!(dx, 0);
        assert_eq!(dy, 0);
    }

    #[test]
    fn sum_cap_left_right() {
        // Mirror the production formula in `right_panel.rs`: user borders must
        // leave at least MIN_IMAGEABLE_IN of imageable width.
        const MIN_IMAGEABLE_IN: f32 = 0.5;
        let reported = Borders {
            left: 0.1,
            right: 0.1,
            top: 0.1,
            bottom: 0.1,
        };
        let paper_w = 8.5f32;

        // Sequentially apply two requested values, each clamped against the
        // current value of the opposite border.
        let left = 3.0f32.clamp(
            reported.left,
            (paper_w - reported.right - MIN_IMAGEABLE_IN).max(reported.left),
        );
        let right = 3.0f32.clamp(
            reported.right,
            (paper_w - left - MIN_IMAGEABLE_IN).max(reported.right),
        );
        assert_eq!(left, 3.0);
        // With the wider cap the user gets the requested 3.0 right border too.
        assert_eq!(right, 3.0);
        assert!(left + right + MIN_IMAGEABLE_IN <= paper_w);
    }

    #[test]
    fn sum_cap_top_bottom() {
        const MIN_IMAGEABLE_IN: f32 = 0.5;
        let reported = Borders {
            left: 0.1,
            right: 0.1,
            top: 0.1,
            bottom: 0.1,
        };
        let paper_h = 11.0f32;

        let top = 4.0f32.clamp(
            reported.top,
            (paper_h - reported.bottom - MIN_IMAGEABLE_IN).max(reported.top),
        );
        let bottom = 2.0f32.clamp(
            reported.bottom,
            (paper_h - top - MIN_IMAGEABLE_IN).max(reported.bottom),
        );
        assert_eq!(top, 4.0);
        assert_eq!(bottom, 2.0);
        assert!(top + bottom + MIN_IMAGEABLE_IN <= paper_h);
    }
}
