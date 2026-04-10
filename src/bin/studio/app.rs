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

use crate::types::{
    AppState, Engine, IccProfileEntry, IccProfileFilter, IccProfileSource, 
    Intent, LoadKind, ProcState, ProcessTarget, RightTab, Settings,
    FIT_PAGE_IDX, PRINT_SIZES, QUEUE_SPACING_IN, THUMB_PX,
};
use crate::icc::{apply_preview_transform, extract_file_date};
use crate::utils::{extract_embedded_icc, is_image, load_thumb};

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
        let s = load_settings();

        let start_dir = s.current_dir.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| home.clone());

        let saved_out_dir = s.output_dir.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or(out_dir);

        let saved_icc: Option<IccProfileEntry> = s.output_icc.as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .map(|path| {
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
            Some("lanczos3") => Engine::Lanczos3,
            Some("iterative") => Engine::Iterative,
            Some("robidoux") => Engine::RobidouxEwa,
            _ => Engine::Mks,
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

        let mut state = AppState::new(
            thumb_tx, thumb_rx, start_dir, saved_out_dir, saved_icc,
            saved_engine, saved_intent, s.sharpen.unwrap_or(5), s.depth16.unwrap_or(true),
            s.target_dpi.unwrap_or(720), saved_icc_filter, s.printer_name, s.page_size_name,
            s.user_border_in, monitor_icc::get_monitor_profile(), printer_discovery::spawn_discovery(),
        );

        if state.monitor_icc_profile.is_none() {
            state.log.push("⚠ No monitor ICC profile found".into());
        }

        let mut app = Self { state };

        if let Some(path) = auto_image_path {
            if path.exists() && is_image(&path) {
                app.state.log.push(format!("Auto-loading image: {}", path.display()));
                app.state.auto_enqueue_path = Some(path.clone());
                app.state.auto_enqueue_pending = true;
                app.stage_image(path);
            } else {
                app.state.log.push(format!("⚠ CLI image path not found or not an image: {}", path.display()));
            }
        }

        app.scan_dir();
        app
    }

    pub(crate) fn scan_dir(&mut self) {
        self.state.subdirs.clear();
        self.state.image_files.clear();

        let Ok(read) = std::fs::read_dir(&self.state.current_dir) else { return };

        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| (e.path().is_file(), e.file_name()));

        let selected = self.state.selected.clone();
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') { continue; }
            if path.is_dir() {
                self.state.subdirs.push((name, path));
            } else if is_image(&path) {
                if selected.as_ref() != Some(&path) {
                    let tx = self.state.thumb_tx.clone();
                    let p = path.clone();
                    let px = self.thumb_load_px();
                    thread::spawn(move || load_thumb(p, px, tx));
                }
                self.state.image_files.push(path);
            }
        }
    }

    pub(crate) fn thumb_load_px(&self) -> u32 {
        ((THUMB_PX as f32) * self.state.thumb_zoom).round().max(32.0) as u32
    }

    pub(crate) fn reload_thumbs(&mut self) {
        let selected = self.state.selected.clone();
        self.state.thumbs.retain(|p, _| selected.as_ref() == Some(p));
        let px = self.thumb_load_px();
        for path in &self.state.image_files {
            if selected.as_ref() == Some(path) { continue; }
            let tx = self.state.thumb_tx.clone();
            let p = path.clone();
            thread::spawn(move || load_thumb(p, px, tx));
        }
    }

    pub(crate) fn navigate(&mut self, path: PathBuf) {
        if path == self.state.current_dir { return; }
        let prev = self.state.current_dir.clone();
        self.state.nav_history.push(prev);
        self.state.nav_forward.clear();
        self.state.current_dir = path.clone();
        self.state.addr_bar = path.to_string_lossy().into_owned();
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

        let tx = self.state.thumb_tx.clone();
        thread::spawn(move || {
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

    pub(crate) fn mark_preview_dirty(&mut self) {
        self.state.preview_dirty = true;
        self.state.preview_cache_page = None;
    }

    pub(crate) fn rebuild_canvas_texture(&mut self, ctx: &Context) {
        self.state.preview_textures.clear();

        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = self
            .state.queue
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
            let Some(base) = self.ensure_full_image_loaded(&path) else { continue };
            let mut ci = base.clone();

            if let Some(ref monitor_profile) = self.state.monitor_icc_profile {
                let mut pixel_bytes: Vec<u8> = ci
                    .pixels
                    .iter()
                    .flat_map(|c| [c.r(), c.g(), c.b()])
                    .collect();

                let src_icc = self
                    .state.embedded_icc_by_path
                    .get(&path)
                    .and_then(|v| v.as_deref());

                if apply_preview_transform(
                    monitor_profile,
                    src_icc,
                    self.state.output_icc.as_ref().map(|e| &e.path),
                    &mut pixel_bytes,
                    self.state.intent.to_lcms(),
                    self.state.bpc,
                    self.state.softproof_enabled,
                ).is_some() {
                    ci.pixels = pixel_bytes
                        .chunks_exact(3)
                        .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                        .collect();
                }
            }

            let tex_name = format!("page_preview::{}", path.to_string_lossy());
            let tex = ctx.load_texture(&tex_name, ci, egui::TextureOptions::LINEAR);
            self.state.preview_textures.insert(path.clone(), tex.clone());

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

    pub(crate) fn ensure_full_image_loaded(&mut self, path: &PathBuf) -> Option<&ColorImage> {
        if !self.state.full_images.contains_key(path) {
            let img = image::open(path).ok()?.into_rgb8();
            let size = [img.width() as usize, img.height() as usize];
            let pixels = img
                .into_raw()
                .chunks_exact(3)
                .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                .collect();
            self.state.full_images.insert(path.clone(), ColorImage { size, pixels });
            self.state.embedded_icc_by_path
                .entry(path.clone())
                .or_insert_with(|| extract_embedded_icc(path));
        }
        self.state.full_images.get(path)
    }

    pub(crate) fn calc_reported_border(&self) -> f32 {
        self.state.caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
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

    pub(crate) fn imageable_size_in(&self) -> (f32, f32) {
        let (pw, ph) = self.state.caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
            .unwrap_or((8.5, 11.0));
        
        let w = (pw - 2.0 * self.state.user_border_in).max(0.1);
        let h = (ph - 2.0 * self.state.user_border_in).max(0.1);
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
        let (pw, ph) = self.state.caps
            .as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
            .unwrap_or((8.5, 11.0));
        let w = (pw - 2.0 * self.state.reported_border_in).max(0.1);
        let h = (ph - 2.0 * self.state.reported_border_in).max(0.1);
        let dpi = self.state.target_dpi as f32;
        (
            (w * dpi).round().max(1.0) as u32,
            (h * dpi).round().max(1.0) as u32,
        )
    }

    pub(crate) fn border_offset_px(&self) -> (u32, u32) {
        let dpi = self.state.target_dpi as f32;
        let diff_in = self.state.user_border_in - self.state.reported_border_in;
        let offset = (diff_in * dpi).round().max(0.0) as u32;
        (offset, offset)
    }

    pub(crate) fn queued_box_px(&self, qi: &vibeprint::layout_engine::QueuedImage) -> (u32, u32) {
        if qi.placed_w_px > 0 && qi.placed_h_px > 0 {
            return (qi.placed_w_px, qi.placed_h_px);
        }
        let (w_in, h_in) = qi.size.as_inches();
        let (w_in, h_in) = if qi.rotation > 0.0 { (h_in, w_in) } else { (w_in, h_in) };
        let dpi = self.state.target_dpi as f32;

        // For outer borders, expand the cell size
        let (w_in, h_in) = if qi.border_type == vibeprint::layout_engine::BorderType::Outer {
            let border_in = qi.border_width_pt / 72.0; // Convert pt to inches
            (w_in + border_in * 2.0, h_in + border_in * 2.0)
        } else {
            (w_in, h_in)
        };

        (
            (w_in * dpi).round().max(1.0) as u32,
            (h_in * dpi).round().max(1.0) as u32,
        )
    }

    pub(crate) fn size_from_idx(&self, idx: usize, src_size_px: Option<(u32, u32)>) -> Option<vibeprint::layout_engine::PrintSize> {
        use vibeprint::layout_engine::{PrintSize, Unit};
        
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

    pub(crate) fn relayout_queue(&mut self) {
        let (page_w_px, page_h_px) = self.imageable_size_px();
        let result = layout_engine::layout_queue(
            &self.state.queue,
            page_w_px,
            page_h_px,
            self.state.target_dpi,
            QUEUE_SPACING_IN,
        );

        for qi in &mut self.state.queue {
            if let Some(p) = result.placements.get(&qi.id) {
                qi.position = Point { x: p.x_px, y: p.y_px };
                qi.page = p.page;
                qi.rotation = p.rotation_deg;
                qi.placed_w_px = p.w_px;
                qi.placed_h_px = p.h_px;
            }
        }

        self.state.page_count = result.page_count.max(1);
        if self.state.current_page >= self.state.page_count {
            self.state.current_page = self.state.page_count.saturating_sub(1);
        }
        self.mark_preview_dirty();
    }

    pub(crate) fn enqueue_staged_with_idx(&mut self, idx: usize) -> bool {
        let Some(path) = self.state.staged.clone() else { return false };
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

        self.state.queue.push(vibeprint::layout_engine::QueuedImage {
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
            crop_enabled: false,
            crop_u0: None,
            crop_v0: None,
            crop_u1: None,
            crop_v1: None,
            border_type: vibeprint::layout_engine::BorderType::None,
            border_width_pt: 4.0,
        });
        self.state.selected_queue_id = self.state.queue.last().map(|q| q.id);
        self.state.selected = Some(path.clone());
        self.state.selected_source_image = Some(src.clone());
        self.state.selected_embedded_icc = self.state.staged_embedded_icc.clone();
        self.state.canvas_img_size = Some(size);
        self.state.full_images.insert(path.clone(), src.clone());
        self.state.embedded_icc_by_path
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

    pub(crate) fn selected_queue_mut(&mut self) -> Option<&mut vibeprint::layout_engine::QueuedImage> {
        let id = self.state.selected_queue_id?;
        self.state.queue.iter_mut().find(|q| q.id == id)
    }

    pub(crate) fn selected_queue(&self) -> Option<&vibeprint::layout_engine::QueuedImage> {
        let id = self.state.selected_queue_id?;
        self.state.queue.iter().find(|q| q.id == id)
    }

    pub(crate) fn update_selected_queue_size_idx(&mut self, idx: usize) {
        let src_size = self.selected_queue().and_then(|q| q.src_size_px);
        let Some(ps) = self.size_from_idx(idx, src_size) else { return };
        let sel = self.state.selected_queue_id;
        // Get imageable size before mutable borrow
        let (ia_w_in, ia_h_in) = self.imageable_size_in();

        if let Some(item) = self.selected_queue_mut() {
            // Store old aspect ratio for crop preservation check
            let old_size = item.size.as_inches();
            let old_aspect = old_size.0 / old_size.1;
            let new_size = ps.as_inches();
            let new_aspect = new_size.0 / new_size.1;

            item.size = ps;
            item.fit_to_page = idx == FIT_PAGE_IDX;

            // Recalculate crop for new aspect ratio while preserving center/zoom
            if let (Some(u0), Some(v0), Some(u1), Some(v1)) = (item.crop_u0, item.crop_v0, item.crop_u1, item.crop_v1) {
                let aspect_diff = (old_aspect - new_aspect).abs() / old_aspect.max(new_aspect);
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

                    let (full_w, full_h) = if will_rotate {
                        (oriented_h, oriented_w)
                    } else {
                        (oriented_w, oriented_h)
                    };

                    // Adjust for inner border
                    let border_in = item.border_width_pt / 72.0;
                    let is_inner = item.border_type == vibeprint::layout_engine::BorderType::Inner;
                    let (new_visible_w, new_visible_h) = if is_inner && border_in > 0.0 {
                        ((full_w - border_in * 2.0).max(0.1), (full_h - border_in * 2.0).max(0.1))
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
                    let src_aspect = if will_rotate { sh_f / sw_f } else { sw_f / sh_f };
                    let box_aspect = new_visible_w / new_visible_h;
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
            self.state.props_media_idx = 0;
            self.state.props_slot_idx = 0;
            
            self.state.selected_page_size_idx = if let Some(ref sz_name) = self.state.pending_page_size_name {
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
                self.state.extra_option_indices.insert(opt.key.clone(), opt.default_idx);
            }
            self.state.caps = Some(caps.clone());
            
            self.state.reported_border_in = self.calc_reported_border();
            self.state.user_border_in = if let Some(saved) = self.state.pending_user_border_in {
                if saved >= self.state.reported_border_in {
                    saved
                } else {
                    self.state.reported_border_in
                }
            } else {
                self.state.reported_border_in
            };
            self.state.pending_user_border_in = None;
            self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
            
            self.relayout_queue();
        } else {
            self.state.caps = None;
            self.state.extra_option_indices.clear();
            self.state.reported_border_in = 0.25;
            self.state.user_border_in = 0.25;
            self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
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
                    self.state.thumbs.insert(path, crate::types::ThumbState::Ready(tex));
                }
                LoadKind::FullResStaged => {
                    if self.state.staged.as_ref() == Some(&path) {
                        let size = ci.size;
                        self.state.full_images.insert(path.clone(), ci.clone());
                        self.state.embedded_icc_by_path.insert(path.clone(), embedded_icc.clone());
                        self.state.staged_embedded_icc = embedded_icc;
                        self.state.staged_source_image = Some(ci);
                        self.state.staged_img_size = Some(size);

                        if self.state.auto_enqueue_pending && self.state.auto_enqueue_path.as_ref() == Some(&path) {
                            if self.enqueue_staged_with_idx(FIT_PAGE_IDX) {
                                self.state.log.push(format!("Auto-enqueued with 'Fit to Page': {}", path.display()));
                            }
                            self.state.auto_enqueue_path = None;
                            self.state.auto_enqueue_pending = false;
                        }
                    } else {
                        self.state.full_images.insert(path.clone(), ci.clone());
                        self.state.embedded_icc_by_path.insert(path.clone(), embedded_icc);
                        let tex = ctx.load_texture(&name, ci, egui::TextureOptions::LINEAR);
                        self.state.thumbs.insert(path, crate::types::ThumbState::Ready(tex));
                    }
                    self.mark_preview_dirty();
                }
            }
        }

        // Printer discovery
        let disc_events: Vec<DiscoveryEvent> = {
            let mut v = Vec::new();
            if let Some(rx) = &self.state.discovery_rx {
                while let Ok(ev) = rx.try_recv() { v.push(ev); }
            }
            v
        };
        let mut need_sync = false;
        for ev in disc_events {
            match ev {
                DiscoveryEvent::PrintersListed(p) => {
                    self.state.printers = p;
                    if let Some(ref name) = self.state.pending_printer_name.clone() {
                        if let Some(idx) = self.state.printers.iter().position(|p| &p.name == name) {
                            self.state.printer_idx = idx;
                        }
                        self.state.pending_printer_name = None;
                    }
                    need_sync = true;
                }
                DiscoveryEvent::CapsReady(c) => {
                    self.state.all_caps.insert(c.name.clone(), c);
                    need_sync = true;
                }
                DiscoveryEvent::Warning(w) => self.state.log.push(format!("⚠ {w}")),
                DiscoveryEvent::Error(e) => self.state.log.push(format!("✗ CUPS: {e}")),
            }
        }
        if need_sync { 
            self.sync_caps_to_selection();
            if !self.state.discovery_complete && !self.state.printers.is_empty() {
                let all_have_caps = self.state.printers.iter().all(|p| self.state.all_caps.contains_key(&p.name));
                if all_have_caps {
                    self.state.discovery_complete = true;
                    self.state.log.push("Ready to print!".to_string());
                }
            }
        }

        // Process result
        if let Some(rx) = &self.state.proc_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((paths, target)) => {
                        match target {
                            ProcessTarget::Export => {
                                if let Some(first) = paths.first() {
                                    self.state.log.push(format!("✓ Saved {} page(s). First: {}", paths.len(), first.display()));
                                } else {
                                    self.state.log.push("✓ Export complete".into());
                                }
                                self.state.proc_state = ProcState::Done(paths);
                            }
                            ProcessTarget::Print => {
                                self.state.log.push(format!("✓ Processed {} page(s) for print", paths.len()));
                                self.state.pending_print_paths = paths;
                                self.state.show_print_confirm = true;
                                self.state.proc_state = ProcState::Idle;
                            }
                        }
                    }
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
                        self.state.log.push("✓ Print jobs submitted successfully".into());
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
        let mut per_page: Vec<Vec<processor::PagePlacement>> = vec![Vec::new(); max_page.saturating_add(1)];
        for q in &self.state.queue {
            let (w, h) = self.queued_box_px(q);
            // Calculate crop UVs if cropping is enabled - use processor-specific function
            let (crop_u0, crop_v0, crop_u1, crop_v1) = if let Some((src_w, src_h)) = q.src_size_px {
                // Use the same rotation logic as the layout engine
                let will_rotate = vibeprint::layout_engine::should_rotate_for_full_page(
                    q.src_size_px, w, h
                );
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
                // Debug: log the crop UVs and check if they would be detected as crop
                if q.crop_enabled {
                    let has_crop = (uvs.2 - uvs.0) < 0.999 || (uvs.3 - uvs.1) < 0.999;
                    self.state.log.push(format!("Debug: crop UVs: {:.3},{:.3},{:.3},{:.3} rot={} src={}x{} box={}x{} has_crop={}",
                        uvs.0, uvs.1, uvs.2, uvs.3, will_rotate, src_w, src_h, w, h, has_crop));
                }
                uvs
            } else {
                (0.0, 0.0, 1.0, 1.0)
            };

            // Use the same rotation logic as the layout engine for consistency
            let will_rotate = vibeprint::layout_engine::should_rotate_for_full_page(
                q.src_size_px, w, h
            );
            // Always log rotation and crop state for debugging
            self.state.log.push(format!("Debug: queue item - rotation={:.1} crop={} will_rotate={}", 
                q.rotation, q.crop_enabled, will_rotate));
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
            });
        }

        let stem = self
            .state.queue
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
                    .state.output_dir
                    .join(format!("{}_page_{:03}_vp.tif", stem, idx + 1)),
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
            ProcessTarget::Export => if self.state.depth16 { 16 } else { 8 },
            ProcessTarget::Print => 16,
        };
        let sharpen = self.state.sharpen;

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

/// Load settings from disk
pub(crate) fn load_settings() -> Settings {
    let path = match config_path() { Some(p) => p, None => return Settings::default() };
    let text = match std::fs::read_to_string(&path) { Ok(t) => t, Err(_) => return Settings::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Save settings to disk
pub(crate) fn save_settings(s: &Settings) {
    let Some(path) = config_path() else { return };
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    if let Ok(text) = serde_json::to_string_pretty(s) { let _ = std::fs::write(path, text); }
}

fn config_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("vibeprint");
    p.push("settings.json");
    Some(p)
}
