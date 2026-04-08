use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::types::LoadKind;
use crate::types::RULER_PX;

/// Aspect-fit a source image into a box, optionally rotating 90° CW
pub(crate) fn aspect_fit_rect_in_box(box_rect: Rect, src_w: u32, src_h: u32, rotate_cw: bool) -> Rect {
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

/// Calculate UV coordinates for cropping an image to fill a box (display path).
/// Returns (u0, v0, u1, v1) where (0,0) is top-left and (1,1) is bottom-right of source.
/// When crop is false, returns full image (0,0,1,1).
/// When crop is true, returns centered crop that minimally fills the target box.
/// This is for the display path where UVs are used for texture sampling with rotation.
pub(crate) fn calc_crop_uv(
    box_w: f32,
    box_h: f32,
    src_w: u32,
    src_h: u32,
    rotate_cw: bool,
    crop_enabled: bool,
) -> (f32, f32, f32, f32) {
    if !crop_enabled {
        return (0.0, 0.0, 1.0, 1.0);
    }

    let sw = src_w.max(1) as f32;
    let sh = src_h.max(1) as f32;
    
    // Use rotated aspect if the image will be rotated
    let src_aspect = if rotate_cw { sh / sw } else { sw / sh };
    let box_aspect = box_w / box_h;

    if box_aspect > src_aspect {
        // Box is wider than source (after rotation) - need to crop top/bottom of rotated image
        if rotate_cw {
            // After rotation, "top/bottom" of rotated image correspond to original left/right
            // Effective source dimensions after rotation: width=sh, height=sw
            // Need to crop effective height to: sh / box_aspect
            let crop_sw = sh / box_aspect; // Width of original to keep (becomes rotated height)
            let crop_ratio = crop_sw / sw; // < 1.0
            let u_margin = (1.0 - crop_ratio) / 2.0;
            (u_margin, 0.0, 1.0 - u_margin, 1.0)
        } else {
            // Normal case - crop top/bottom of original
            // Target height = sw / box_aspect (so that sw/height = box_aspect)
            let crop_sh = sw / box_aspect; // Height of cropped region in source pixels
            let crop_ratio = crop_sh / sh; // < 1.0
            let v_margin = (1.0 - crop_ratio) / 2.0;
            (0.0, v_margin, 1.0, 1.0 - v_margin)
        }
    } else {
        // Box is taller than source (after rotation) - need to crop left/right of rotated image
        if rotate_cw {
            // After rotation, "left/right" of rotated image correspond to original top/bottom
            // Effective source dimensions after rotation: width=sh, height=sw
            // Need to crop effective width to: sw * box_aspect
            let crop_sh = sw * box_aspect; // Height of original to keep (becomes rotated width)
            let crop_ratio = crop_sh / sh; // < 1.0
            let v_margin = (1.0 - crop_ratio) / 2.0;
            (0.0, v_margin, 1.0, 1.0 - v_margin)
        } else {
            // Normal case - crop left/right of original
            // Target width = sh * box_aspect (so that width/sh = box_aspect)
            let crop_sw = sh * box_aspect; // Width of cropped region in source pixels
            let crop_ratio = crop_sw / sw; // < 1.0
            let u_margin = (1.0 - crop_ratio) / 2.0;
            (u_margin, 0.0, 1.0 - u_margin, 1.0)
        }
    }
}

/// Calculate UV coordinates for cropping an image before rotation (processor path).
/// Returns (u0, v0, u1, v1) where (0,0) is top-left and (1,1) is bottom-right of source.
/// This crops the original image so that after rotation it will fill the target box.
/// This is for the processor path where the image is cropped first, then rotated.
pub(crate) fn calc_crop_uv_for_processor(
    box_w: f32,
    box_h: f32,
    src_w: u32,
    src_h: u32,
    rotate_cw: bool,
    crop_enabled: bool,
) -> (f32, f32, f32, f32) {
    if !crop_enabled {
        return (0.0, 0.0, 1.0, 1.0);
    }

    if !rotate_cw {
        // No rotation - use the same calculation as display path
        return calc_crop_uv(box_w, box_h, src_w, src_h, false, true);
    }

    // For processor path with rotation:
    // 1. Calculate what crop is needed on the ROTATED image to fill the box
    // 2. Transform those UVs back to the original image
    //
    // When the image is rotated 90° CW, its dimensions become (src_h, src_w)
    // We need calc_crop_uv to calculate crop on this rotated image, so we
    // pass swapped source dimensions with rotate_cw=false
    let (rot_u0, rot_v0, rot_u1, rot_v1) = calc_crop_uv(
        box_w, box_h, src_h, src_w, false, true,
    );

    // Transform rotated UVs back to original image UVs
    // For 90° CW rotation: new(nx, ny) = old(w-1-ny, nx)
    // Inverse: old(ox, oy) = new(ny, w-1-nx)
    // In UV space: orig_u = 1.0 - rotated_v, orig_v = rotated_u
    let orig_u0 = 1.0 - rot_v1;
    let orig_v0 = rot_u0;
    let orig_u1 = 1.0 - rot_v0;
    let orig_v1 = rot_u1;

    (orig_u0, orig_v0, orig_u1, orig_v1)
}

/// Check if a file is an image
pub(crate) fn is_image(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp"
    )
}

/// Load a thumbnail in background thread
pub(crate) fn load_thumb(path: PathBuf, size: u32, tx: Sender<(PathBuf, ColorImage, Option<Vec<u8>>, LoadKind)>) {
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

/// Draw a dashed rectangle outline via short line segments
pub(crate) fn draw_dashed_rect(painter: &egui::Painter, rect: Rect, color: Color32, width: f32, dash: f32) {
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

/// Draw horizontal ruler with inch markings
pub(crate) fn draw_ruler_h(
    painter: &egui::Painter,
    area: Rect,
    paper_x: f32,
    paper_px_w: f32,
    scale: f32,
    ruler_h: f32,
    margin_l_px: f32,
    margin_r_px: f32,
) {
    let bg = Color32::from_gray(44);
    let fg = Color32::from_gray(215);
    let half_fg = Color32::from_gray(160);
    let qtr_fg = Color32::from_gray(110);
    let margin_col = Color32::from_rgb(255, 150, 50);

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
                Pos2::new(x, bot),
            ],
            margin_col,
            Stroke::NONE,
        ));
    }
}

/// Draw vertical ruler with inch markings
pub(crate) fn draw_ruler_v(
    painter: &egui::Painter,
    area: Rect,
    paper_y: f32,
    paper_px_h: f32,
    scale: f32,
    ruler_w: f32,
    margin_t_px: f32,
    margin_b_px: f32,
) {
    let bg = Color32::from_gray(44);
    let fg = Color32::from_gray(215);
    let half_fg = Color32::from_gray(160);
    let qtr_fg = Color32::from_gray(110);
    let margin_col = Color32::from_rgb(255, 150, 50);

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
                Pos2::new(right, y),
            ],
            margin_col,
            Stroke::NONE,
        ));
    }
}

/// Recursively render one directory node in the folder tree
/// Reads children from disk only when expanded; depth-limited to 8
pub(crate) fn draw_tree_node(
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

    // Check for any non-hidden subdirectory
    let has_children = std::fs::read_dir(path)
        .ok()
        .map(|rd| rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            !n.starts_with('.') && e.path().is_dir()
        }))
        .unwrap_or(false);

    // Default: home (depth==0) starts expanded; everything else collapsed
    let is_expanded = *expanded.get(path).unwrap_or(&(depth == 0));
    let is_current = path == current;
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

/// Returns (fits, rect_landscape)
/// rect_landscape is true ONLY when w×h portrait doesn't fit but h×w landscape does
pub(crate) fn check_size_fit(w_in: f32, h_in: f32, ia_w_in: f32, ia_h_in: f32) -> (bool, bool) {
    let fits_portrait = w_in <= ia_w_in && h_in <= ia_h_in;
    let fits_landscape = h_in <= ia_w_in && w_in <= ia_h_in;
    if fits_portrait { (true, false) }
    else if fits_landscape { (true, true) }
    else { (false, false) }
}

/// Extract embedded ICC profile from image file (JPEG APP2, PNG iCCP, TIFF tag)
pub(crate) fn extract_embedded_icc(path: &std::path::Path) -> Option<Vec<u8>> {
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
