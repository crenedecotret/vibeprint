use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::path::PathBuf;

use crate::types::{ThumbState, THUMB_PX};
use crate::utils::draw_tree_node;
use crate::App;

impl App {
    pub(crate) fn draw_left(&mut self, ui: &mut egui::Ui) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

        // Init addr_bar on first draw
        if self.state.addr_bar.is_empty() {
            self.state.addr_bar = self.state.current_dir.to_string_lossy().into_owned();
        }

        // ── Toolbar ───────────────────────────────────────────────────────
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let can_back = !self.state.nav_history.is_empty();
            let can_fwd = !self.state.nav_forward.is_empty();
            let btn_size = Vec2::new(24.0, 22.0);
            if ui.add_enabled(can_back, egui::Button::new("◀").min_size(btn_size))
                .on_hover_text("Back").clicked() { self.nav_back(); }
            if ui.add_enabled(can_fwd, egui::Button::new("▶").min_size(btn_size))
                .on_hover_text("Forward").clicked() { self.nav_fwd(); }
            if ui.add(egui::Button::new("🏠").min_size(btn_size))
                .on_hover_text("Home").clicked() { self.navigate(home.clone()); }
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
                self.navigate(p);
            } else {
                self.state.addr_bar = self.state.current_dir.to_string_lossy().into_owned();
            }
        }

        // ── Places ────────────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.label(RichText::new("  PLACES").size(9.5).color(Color32::from_gray(130)));
        let places: &[(&str, fn() -> Option<PathBuf>)] = &[
            ("🏠  Home", || dirs::home_dir()),
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

        ui.add_space(4.0);
        ui.label(RichText::new("  FOLDERS").size(9.5).color(Color32::from_gray(130)));

        // ── Folder tree ───────────────────────────────────────────────────
        let avail = ui.available_height();
        let tree_h = (avail * 0.42).max(80.0);
        let mut tree_nav: Option<PathBuf> = None;
        let mut tree_toggle: Option<(PathBuf, bool)> = None;
        egui::ScrollArea::vertical()
            .id_salt("tree_scroll")
            .max_height(tree_h)
            .show(ui, |ui| {
                draw_tree_node(
                    ui, &home, 0,
                    &self.state.current_dir,
                    &self.state.tree_expanded,
                    &mut tree_nav,
                    &mut tree_toggle,
                );
            });
        if let Some((p, exp)) = tree_toggle { self.state.tree_expanded.insert(p, exp); }
        if let Some(p) = tree_nav { self.navigate(p); }

        ui.separator();

        // ── Image count + zoom control ─────────────────────────────────
        let n = self.state.image_files.len();
        let cur_name = self.state.current_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.state.current_dir.to_string_lossy().into_owned());
        ui.horizontal(|ui| {
            ui.label(RichText::new(
                if n == 0 { format!("📂 {cur_name}  (no images)") }
                else { format!("📂 {cur_name}  · {n} image{}", if n == 1 { "" } else { "s" }) }
            ).size(10.5).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn_size = Vec2::new(24.0, 22.0);
                if ui.add(egui::Button::new("+").min_size(btn_size)).on_hover_text("Larger thumbnails").clicked() {
                    self.state.thumb_zoom = (self.state.thumb_zoom + 0.25).min(3.0);
                    self.reload_thumbs();
                }
                if ui.add(egui::Button::new("−").min_size(btn_size)).on_hover_text("Smaller thumbnails").clicked() {
                    self.state.thumb_zoom = (self.state.thumb_zoom - 0.25).max(0.5);
                    self.reload_thumbs();
                }
            });
        });
        ui.add_space(2.0);

        // ── Thumbnail grid ────────────────────────────────────────────────
        egui::ScrollArea::vertical().id_salt("thumbs").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let files = self.state.image_files.clone();
                for path in &files {
                    let is_staged = self.state.staged.as_ref() == Some(path);
                    let is_hi = self.state.highlighted.as_ref() == Some(path);
                    let thumb_f = (THUMB_PX as f32 * self.state.thumb_zoom).round();

                    // Aspect-ratio-preserving display size; square placeholder while loading
                    let (disp_w, disp_h) = match self.state.thumbs.get(path) {
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
                    let painter = ui.painter_at(resp.rect);

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
                    match self.state.thumbs.get(path) {
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
                        egui::FontId::proportional(14.0),
                        Color32::LIGHT_GRAY,
                    );

                    if resp.clicked() {
                        self.state.highlighted = Some(path.clone());
                        self.stage_image(path.clone());
                    }
                }
            });
        });
    }
}
