use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::path::PathBuf;

use crate::types::{ThumbState, THUMB_PX};
use crate::utils::draw_tree_node;
use crate::App;

const CAPTION_LINES: usize = 3;
const CAPTION_H: f32 = 46.0;

impl App {
    pub(crate) fn draw_left(&mut self, ui: &mut egui::Ui) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let root = PathBuf::from("/");

        // Init addr_bar on first draw
        if self.state.addr_bar.is_empty() {
            self.state.addr_bar = self.state.current_dir.to_string_lossy().into_owned();
        }

        // ── Toolbar ───────────────────────────────────────────────────────
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let btn_size = Vec2::new(24.0, 22.0);

            // Navigation buttons (left side)
            let can_back = !self.state.nav_history.is_empty();
            let can_fwd = !self.state.nav_forward.is_empty();
            if ui
                .add_enabled(can_back, egui::Button::new("◀").min_size(btn_size))
                .on_hover_text("Back")
                .clicked()
            {
                self.nav_back();
            }
            if ui
                .add_enabled(can_fwd, egui::Button::new("▶").min_size(btn_size))
                .on_hover_text("Forward")
                .clicked()
            {
                self.nav_fwd();
            }
            if ui
                .add(egui::Button::new("🏠").min_size(btn_size))
                .on_hover_text("Home")
                .clicked()
            {
                self.navigate(home.clone());
            }
            if ui
                .add(egui::Button::new("⟳").min_size(btn_size))
                .on_hover_text("Refresh (F5)")
                .clicked()
            {
                self.refresh_full();
            }

            // Push hamburger menu to the right
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("☰", |ui| {
                    ui.set_min_width(140.0);
                    if ui.button("About VibePrint Studio…").clicked() {
                        self.state.show_about = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Preferences…").clicked() {
                        self.state.show_preferences = true;
                        ui.close_menu();
                    };
                });
            });
        });

        // ── Address bar ───────────────────────────────────────────────────
        ui.add_space(2.0);
        let addr_resp = ui.add(
            egui::TextEdit::singleline(&mut self.state.addr_bar)
                .desired_width(ui.available_width())
                .font(egui::FontId::proportional(14.0))
                .hint_text("Path…"),
        );
        if addr_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let p = PathBuf::from(&self.state.addr_bar);
            if p.is_dir() {
                if p == self.state.current_dir {
                    self.refresh_full();
                } else {
                    self.navigate(p);
                }
            } else {
                self.state.addr_bar = self.state.current_dir.to_string_lossy().into_owned();
            }
        }

        // ── Places ────────────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.label(
            RichText::new("  PLACES")
                .size(9.5)
                .color(Color32::from_gray(130)),
        );
        let places: &[(&str, fn() -> Option<PathBuf>)] = &[
            ("🏠  Home", || dirs::home_dir()),
            ("💾  Root", || Some(PathBuf::from("/"))),
            ("🖥  Desktop", || dirs::desktop_dir()),
            ("📁  Documents", || dirs::document_dir()),
            ("📁  Downloads", || dirs::download_dir()),
            ("🖼  Pictures", || dirs::picture_dir()),
        ];
        for (label, get_path) in places {
            if let Some(path) = get_path() {
                if path.is_dir() {
                    let active = self.state.current_dir == path;
                    let text = RichText::new(*label).size(12.0);
                    if ui.selectable_label(active, text).clicked() && !active {
                        self.navigate(path);
                    }
                }
            }
        }

        // ── Devices ───────────────────────────────────────────────────────
        if !self.state.devices.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new("  DEVICES")
                    .size(9.5)
                    .color(Color32::from_gray(130)),
            );
            // Clone to avoid borrow conflict with self.navigate
            let devices = self.state.devices.clone();
            let current = self.state.current_dir.clone();
            let mut nav: Option<PathBuf> = None;
            let mut mount_req: Option<String> = None;
            let mut eject_req: Option<String> = None;
            for dev in &devices {
                match &dev.mount_point {
                    Some(mp) => {
                        ui.horizontal(|ui| {
                            let active = current == *mp || current.starts_with(mp);
                            let icon = if dev.is_optical { "💿  " } else { "💾  " };
                            let text = RichText::new(format!("{icon}{}", dev.label)).size(12.0);
                            let hover = format!("{} ({})", mp.display(), dev.devnode.as_deref().unwrap_or("?"));
                            if ui
                                .selectable_label(active, text)
                                .on_hover_text(hover)
                                .clicked()
                                && !active
                            {
                                nav = Some(mp.clone());
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_enabled(
                                        dev.object_path.is_some(),
                                        egui::Button::new("⏏").small(),
                                    )
                                    .on_hover_text("Eject")
                                    .clicked()
                                {
                                    eject_req = dev.object_path.clone();
                                }
                            });
                        });
                    }
                    None => {
                        let icon = if dev.is_optical { "💿  " } else { "💾  " };
                        let text = RichText::new(format!("{icon}{}  (not mounted)", dev.label))
                            .size(12.0)
                            .weak();
                        let hover = format!(
                            "{} (not mounted) — click to mount",
                            dev.devnode.as_deref().unwrap_or("?")
                        );
                        if ui.selectable_label(false, text).on_hover_text(hover).clicked() {
                            if let Some(op) = &dev.object_path {
                                mount_req = Some(op.clone());
                                self.state.pending_mount_nav = Some(op.clone());
                            }
                        }
                    }
                }
            }
            if let Some(p) = nav {
                self.navigate(p);
            }
            if let Some(op) = mount_req {
                let _ = self
                    .state
                    .device_action_tx
                    .send(crate::devices::DeviceAction::Mount { object_path: op });
            }
            if let Some(op) = eject_req {
                let _ = self
                    .state
                    .device_action_tx
                    .send(crate::devices::DeviceAction::Unmount { object_path: op });
            }
        }

        ui.add_space(4.0);
        ui.label(
            RichText::new("  FOLDERS")
                .size(9.5)
                .color(Color32::from_gray(130)),
        );

        // ── Folder tree ───────────────────────────────────────────────────
        let avail = ui.available_height();
        let tree_h = (avail * 0.42).max(80.0);
        let mut tree_nav: Option<PathBuf> = None;
        let mut tree_toggle: Option<(PathBuf, bool)> = None;
        let mut current_rect: Option<Rect> = None;
        let focus_tree = self.state.tree_focus_pending;
        egui::ScrollArea::vertical()
            .id_salt("tree_scroll")
            .max_height(tree_h)
            .auto_shrink(false)
            .show(ui, |ui| {
                draw_tree_node(
                    ui,
                    &root,
                    0,
                    &self.state.current_dir,
                    &self.state.tree_expanded,
                    &mut self.state.tree_children_cache,
                    &mut tree_nav,
                    &mut tree_toggle,
                    &mut current_rect,
                );
                // After a navigation (e.g. Places click), bring the current
                // folder into view. align = None scrolls the minimum amount
                // needed, so it never fights the user's manual scrolling.
                if focus_tree {
                    if let Some(rect) = current_rect {
                        ui.scroll_to_rect(rect, None);
                    }
                    self.state.tree_focus_pending = false;
                }
            });
        if let Some((p, exp)) = tree_toggle {
            self.state.tree_expanded.insert(p, exp);
        }
        if let Some(p) = tree_nav {
            self.navigate(p);
        }

        ui.separator();

        // ── Image count + zoom control ─────────────────────────────────
        let n = self.state.image_files.len();
        let cur_name = self
            .state
            .current_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.state.current_dir.to_string_lossy().into_owned());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if n == 0 {
                    format!("📂 {cur_name}  (no images)")
                } else {
                    format!(
                        "📂 {cur_name}  · {n} image{}",
                        if n == 1 { "" } else { "s" }
                    )
                })
                .size(10.5)
                .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn_size = Vec2::new(24.0, 22.0);
                if ui
                    .add(egui::Button::new("+").min_size(btn_size))
                    .on_hover_text("Larger thumbnails")
                    .clicked()
                {
                    self.state.thumb_zoom = (self.state.thumb_zoom + 0.25).min(3.0);
                    self.reload_thumbs();
                }
                if ui
                    .add(egui::Button::new("−").min_size(btn_size))
                    .on_hover_text("Smaller thumbnails")
                    .clicked()
                {
                    self.state.thumb_zoom = (self.state.thumb_zoom - 0.25).max(0.5);
                    self.reload_thumbs();
                }
            });
        });
        ui.add_space(2.0);

        // ── Thumbnail grid ────────────────────────────────────────────────
        egui::ScrollArea::vertical()
            .id_salt("thumbs")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let files = self.state.image_files.clone();
                    for path in &files {
                        let is_staged = self.state.staged.as_ref() == Some(path);
                        let is_hi = self.state.highlighted.as_ref() == Some(path);
                        let is_selected = self.state.selected_paths.contains(path);
                        let thumb_f = (THUMB_PX as f32 * self.state.thumb_zoom).round();

                        // Aspect-ratio-preserving display size; square placeholder while loading
                        let (disp_w, disp_h) = match self.state.thumbs.get(path) {
                            Some(ThumbState::Ready(tex)) => {
                                let [tw, th] = tex.size();
                                let scale =
                                    (thumb_f / tw.max(1) as f32).min(thumb_f / th.max(1) as f32);
                                ((tw as f32 * scale).round(), (th as f32 * scale).round())
                            }
                            _ => (thumb_f, thumb_f),
                        };

                        let cell_size = Vec2::new(disp_w, disp_h + CAPTION_H);
                        let (resp, _) = ui.allocate_painter(cell_size, Sense::click_and_drag());
                        let name_owned = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        let resp = resp.on_hover_ui(|ui| {
                            ui.label(RichText::new(name_owned.clone()).color(Color32::WHITE));
                        });
                        let painter = ui.painter_at(resp.rect);
                        let img_rect =
                            Rect::from_min_size(resp.rect.min, Vec2::new(disp_w, disp_h));

                        // Highlight background for focused item
                        if is_hi && !is_selected {
                            painter.rect_filled(
                                img_rect,
                                4.0,
                                Color32::from_rgb(45, 55, 70),
                            );
                            painter.rect_stroke(
                                img_rect,
                                4.0,
                                Stroke::new(1.5, Color32::from_rgb(100, 130, 180)),
                            );
                        }
                        match self.state.thumbs.get(path) {
                            Some(ThumbState::Ready(tex)) => {
                                painter.image(
                                    tex.id(),
                                    img_rect,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                            Some(ThumbState::Loading) | None => {
                                painter.text(
                                    img_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "⏳",
                                    egui::FontId::proportional(18.0),
                                    Color32::GRAY,
                                );
                            }
                            Some(ThumbState::Failed) => {
                                painter.text(
                                    img_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "✗",
                                    egui::FontId::proportional(18.0),
                                    Color32::RED,
                                );
                            }
                        }

                        // Checkbox overlay for multi-selection
                        if is_selected {
                            let cb_size = 18.0;
                            let cb_pad = 3.0;
                            let cb_rect = Rect::from_min_size(
                                img_rect.min + Vec2::new(cb_pad, cb_pad),
                                Vec2::splat(cb_size),
                            );
                            // Dark rounded background
                            painter.rect_filled(cb_rect, 3.0, Color32::from_rgba_premultiplied(0, 0, 0, 180));
                            painter.rect_stroke(
                                cb_rect,
                                3.0,
                                Stroke::new(1.5, Color32::from_rgb(80, 170, 255)),
                            );
                            // Checkmark
                            let cx = cb_rect.center().x;
                            let cy = cb_rect.center().y;
                            let s = cb_size * 0.22;
                            painter.line_segment(
                                [
                                    Pos2::new(cx - s * 1.2, cy),
                                    Pos2::new(cx - s * 0.3, cy + s * 1.0),
                                ],
                                Stroke::new(2.0, Color32::WHITE),
                            );
                            painter.line_segment(
                                [
                                    Pos2::new(cx - s * 0.3, cy + s * 1.0),
                                    Pos2::new(cx + s * 1.4, cy - s * 1.0),
                                ],
                                Stroke::new(2.0, Color32::WHITE),
                            );
                        }

                        let text_color = if is_staged {
                            Color32::from_rgb(100, 180, 255)
                        } else {
                            Color32::LIGHT_GRAY
                        };
                        let mut job = egui::text::LayoutJob::simple(
                            name_owned.clone(),
                            egui::FontId::proportional(11.0),
                            text_color,
                            disp_w.max(1.0),
                        );
                        job.wrap.max_rows = CAPTION_LINES;
                        job.wrap.break_anywhere = true;
                        let galley = ui.fonts(|f| f.layout_job(job));
                        painter.galley(
                            Pos2::new(resp.rect.min.x + 2.0, resp.rect.min.y + disp_h + 2.0),
                            galley,
                            text_color,
                        );
                        if is_staged {
                            let inset = img_rect.shrink(1.5);
                            painter.rect_stroke(
                                inset,
                                3.0,
                                Stroke::new(3.0, Color32::from_rgb(100, 180, 255)),
                            );
                        }

                        // Handle click with modifiers
                        if resp.clicked() {
                            let modifiers = ui.input(|i| i.modifiers);
                            if modifiers.ctrl {
                                // CTRL+click: toggle selection, enter batch mode
                                if is_selected {
                                    self.state.selected_paths.remove(path);
                                } else {
                                    self.state.selected_paths.insert(path.clone());
                                    self.state.highlighted = Some(path.clone());
                                }
                                self.state.batch_add_mode = true;
                                self.state.right_tab = crate::types::RightTab::ImageProperties;
                            } else if modifiers.shift {
                                // SHIFT+click: range select, enter batch mode
                                if let Some(hi_path) = &self.state.highlighted {
                                    let files = self.state.image_files.clone();
                                    if let Some(hi_idx) = files.iter().position(|p| p == hi_path) {
                                        if let Some(click_idx) = files.iter().position(|p| p == path) {
                                            let start = hi_idx.min(click_idx);
                                            let end = hi_idx.max(click_idx);
                                            for i in start..=end {
                                                self.state.selected_paths.insert(files[i].clone());
                                            }
                                        }
                                    } else {
                                        self.state.selected_paths.insert(path.clone());
                                        self.state.highlighted = Some(path.clone());
                                    }
                                } else {
                                    self.state.selected_paths.insert(path.clone());
                                    self.state.highlighted = Some(path.clone());
                                }
                                self.state.batch_add_mode = true;
                                self.state.right_tab = crate::types::RightTab::ImageProperties;
                            } else {
                                // Plain click: clear selection, stage image, enter batch mode
                                self.state.selected_paths.clear();
                                self.state.selected_paths.insert(path.clone());
                                self.state.highlighted = Some(path.clone());
                                self.state.batch_add_mode = true;
                                self.stage_image(path.clone());
                            }
                        }

                        // Drag source setup
                        if resp.drag_started() {
                            // Determine payload: if dragged image is selected, use all selected; else just this one
                            let payload: Vec<PathBuf> = if is_selected && !self.state.selected_paths.is_empty() {
                                self.state.selected_paths.iter().cloned().collect()
                            } else {
                                vec![path.clone()]
                            };
                            resp.dnd_set_drag_payload(payload);
                            self.state.drag_active = true;
                        }
                        if resp.drag_stopped() {
                            self.state.drag_active = false;
                        }
                    }
                });
            });
    }
}
