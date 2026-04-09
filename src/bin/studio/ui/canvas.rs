use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::types::RULER_PX;
use crate::utils::{aspect_fit_rect_in_box, draw_dashed_rect, draw_ruler_h, draw_ruler_v};
use crate::App;

impl App {
    pub(crate) fn draw_canvas(&mut self, ui: &mut egui::Ui) {
        // Paper dimensions in PostScript points — driven by selected_page_size_idx
        let selected_ps = self.state.caps.as_ref()
            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx));
        let (paper_w_pt, paper_h_pt) = selected_ps
            .map(|ps| ps.paper_size)
            .unwrap_or((612.0_f32, 792.0_f32));
        // Calculate user-adjusted imageable area in points
        let user_border_pt = self.state.user_border_in * 72.0; // Convert inches to points
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

        if self.state.preview_dirty || self.state.preview_cache_page != Some(self.state.current_page) {
            self.rebuild_canvas_texture(ui.ctx());
        }
        self.state.canvas_hit_rects.clear();
        let (ia_w_px, ia_h_px) = self.imageable_size_px();
        let sx = ia_rect.width() / ia_w_px.max(1) as f32;
        let sy = ia_rect.height() / ia_h_px.max(1) as f32;

        for item in self.state.queue.iter().filter(|q| q.page == self.state.current_page) {
            let (w_px, h_px) = self.queued_box_px(item);
            let r = Rect::from_min_size(
                Pos2::new(
                    ia_rect.min.x + item.position.x as f32 * sx,
                    ia_rect.min.y + item.position.y as f32 * sy,
                ),
                Vec2::new(w_px as f32 * sx, h_px as f32 * sy),
            );
            self.state.canvas_hit_rects.push((item.id, r));

            let src_size = item.src_size_px.or_else(|| {
                self.state.full_images
                    .get(&item.filepath)
                    .map(|img| (img.size[0] as u32, img.size[1] as u32))
            });
            let img_rect = if item.crop_enabled {
                // When cropping, fill the entire cell (no letterboxing)
                r
            } else {
                // When not cropping, aspect-fit with letterboxing
                src_size
                    .map(|(sw, sh)| aspect_fit_rect_in_box(r, sw, sh, item.rotation > 0.0))
                    .unwrap_or(r)
            };

            if let Some(tex) = self.state.preview_textures.get(&item.filepath) {
                // Calculate crop UVs
                let stored_uv = match (item.crop_u0, item.crop_v0, item.crop_u1, item.crop_v1) {
                    (Some(u0), Some(v0), Some(u1), Some(v1)) => Some((u0, v0, u1, v1)),
                    _ => None,
                };
                // When we have stored UVs, don't rotate them in calc_crop_uv - the canvas
                // handles rotation manually by remapping UVs to screen corners below.
                // Only pass rotate=true for auto-calculated crops (when no stored UVs).
                let rotate_for_calc = stored_uv.is_none() && item.rotation > 0.0;
                let (u0, v0, u1, v1) = src_size.map(|(sw, sh)| {
                    crate::utils::calc_crop_uv(
                        r.width(),
                        r.height(),
                        sw,
                        sh,
                        rotate_for_calc,
                        item.crop_enabled,
                        stored_uv,
                    )
                }).unwrap_or((0.0, 0.0, 1.0, 1.0));

                // Adjust UVs for rotation
                // After 90° CW rotation:
                // - Original top-left (u0,v0) appears at screen bottom-left
                // - Original top-right (u1,v0) appears at screen top-left
                // - Original bottom-right (u1,v1) appears at screen top-right
                // - Original bottom-left (u0,v1) appears at screen bottom-right
                //
                // For mesh drawing, we assign UVs to screen corners:
                // - Screen top-left gets original top-right (u1, v0)
                // - Screen top-right gets original bottom-right (u1, v1)
                // - Screen bottom-right gets original bottom-left (u0, v1)
                // - Screen bottom-left gets original top-left (u0, v0)
                let (uv_lt, uv_rt, uv_rb, uv_lb) = if item.rotation > 0.0 {
                    (
                        Pos2::new(u1, v0), // screen top-left <- original top-right
                        Pos2::new(u1, v1), // screen top-right <- original bottom-right
                        Pos2::new(u0, v1), // screen bottom-right <- original bottom-left
                        Pos2::new(u0, v0), // screen bottom-left <- original top-left
                    )
                } else {
                    (
                        Pos2::new(u0, v0), // screen top-left <- original top-left
                        Pos2::new(u1, v0), // screen top-right <- original top-right
                        Pos2::new(u1, v1), // screen bottom-right <- original bottom-right
                        Pos2::new(u0, v1), // screen bottom-left <- original bottom-left
                    )
                };

                if item.rotation > 0.0 {
                    let mut mesh = egui::epaint::Mesh::with_texture(tex.id());
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_top(), uv: uv_lt, color: Color32::WHITE });
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_top(), uv: uv_rt, color: Color32::WHITE });
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.right_bottom(), uv: uv_rb, color: Color32::WHITE });
                    mesh.vertices.push(egui::epaint::Vertex { pos: img_rect.left_bottom(), uv: uv_lb, color: Color32::WHITE });
                    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                    painter.add(egui::Shape::mesh(mesh));
                } else {
                    painter.image(tex.id(), img_rect, Rect::from_min_max(uv_lt, uv_rb), Color32::WHITE);
                }
            } else {
                painter.rect_filled(r, 0.0, Color32::from_gray(220));
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(120)));
            }

            let stroke = if Some(item.id) == self.state.selected_queue_id {
                Stroke::new(2.0, Color32::from_rgb(90, 180, 255))
            } else {
                Stroke::new(1.0, Color32::from_rgba_premultiplied(80, 120, 170, 160))
            };
            painter.rect_stroke(r, 0.0, stroke);
        }

        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some((id, _)) = self
                    .state.canvas_hit_rects
                    .iter()
                    .rev()
                    .find(|(_, r)| r.contains(pos))
                    .copied()
                {
                    self.state.selected_queue_id = Some(id);
                    if let Some(item) = self.state.queue.iter().find(|q| q.id == id) {
                        self.state.current_page = item.page;
                    }
                    self.state.right_tab = crate::types::RightTab::ImageProperties;
                }
            }
        }

        // Rulers — pass imageable-area boundaries as pixel offsets from paper origin
        let m_left = ia_l * scale;
        let m_right = ia_r * scale;
        let m_top = (paper_h_pt - ia_t) * scale;
        let m_bottom = (paper_h_pt - ia_b) * scale;
        draw_ruler_h(&painter, canvas_area, paper_origin.x, paper_px_w, scale, RULER_PX, m_left, m_right);
        draw_ruler_v(&painter, canvas_area, paper_origin.y, paper_px_h, scale, RULER_PX, m_top, m_bottom);

        if self.state.softproof_enabled {
            painter.text(
                Pos2::new(canvas_area.max.x - 12.0, canvas_area.min.y + RULER_PX + 8.0),
                egui::Align2::RIGHT_TOP,
                "Softproof",
                egui::FontId::proportional(16.0),
                Color32::from_rgb(220, 90, 90),
            );
        }

        // Status overlay (page size + DPI)
        let info = if let Some(caps) = &self.state.caps {
            let ps = caps.page_sizes.get(self.state.selected_page_size_idx)
                .map(|p| p.label.as_str())
                .unwrap_or("?");
            let dpi = self.state.target_dpi;
            format!("{ps}  ·  {dpi} dpi  ·  Page {} of {}", self.state.current_page + 1, self.state.page_count)
        } else {
            format!("{:.0}×{:.0} pt  ·  {} dpi", paper_w_pt, paper_h_pt, self.state.target_dpi)
        };
        painter.text(
            canvas_area.max - Vec2::new(8.0, 8.0),
            egui::Align2::RIGHT_BOTTOM,
            &info,
            egui::FontId::proportional(11.0),
            Color32::from_gray(160),
        );
    }

    pub(crate) fn draw_canvas_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal_centered(|ui| {
            let has_image = !self.state.queue.is_empty() || self.state.selected_source_image.is_some();
            let icon = "🔍"; // magnifying glass icon
            let mut btn = egui::Button::new(
                RichText::new(icon).strong().size(21.0)
            ).min_size(Vec2::new(48.0, 36.0));
            if self.state.softproof_enabled {
                btn = btn.fill(Color32::from_rgb(60, 120, 200));
            }
            if ui.add_enabled(has_image, btn).clicked() {
                self.state.softproof_enabled = !self.state.softproof_enabled;
                self.mark_preview_dirty();
            }

            ui.add_space(16.0);
            let prev = ui.add_enabled(self.state.current_page > 0, egui::Button::new("◀ Previous Page"));
            if prev.clicked() {
                self.state.current_page = self.state.current_page.saturating_sub(1);
                self.mark_preview_dirty();
            }
            ui.label(format!("Page {} of {}", self.state.current_page + 1, self.state.page_count.max(1)));
            let next = ui.add_enabled(self.state.current_page + 1 < self.state.page_count, egui::Button::new("Next Page ▶"));
            if next.clicked() {
                self.state.current_page = (self.state.current_page + 1).min(self.state.page_count.saturating_sub(1));
                self.mark_preview_dirty();
            }
        });
        ui.add_space(2.0);
    }
}
