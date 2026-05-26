use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

#[derive(Clone, Copy, PartialEq)]
pub enum Unit {
    Inches,
    Millimeters,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum BorderType {
    #[default]
    None,
    Inner,
    Outer,
}

#[derive(Clone, Copy)]
pub struct PrintSize {
    pub width: f32,
    pub height: f32,
    pub unit: Unit,
}

/// Convert millimeters to inches using f64 intermediate arithmetic for precision.
/// Returns the closest f32 to the exact rational result (mm / 25.4).
pub fn mm_to_inches(mm: f32) -> f32 {
    (mm as f64 / 25.4) as f32
}

/// Convert inches to millimeters using f64 intermediate arithmetic for precision.
/// Returns the closest f32 to the exact rational result (inches * 25.4).
pub fn inches_to_mm(inches: f32) -> f32 {
    (inches as f64 * 25.4) as f32
}

impl PrintSize {
    pub fn as_inches(self) -> (f32, f32) {
        match self.unit {
            Unit::Inches => (self.width, self.height),
            Unit::Millimeters => (mm_to_inches(self.width), mm_to_inches(self.height)),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone)]
pub struct QueuedImage {
    pub id: Uuid,
    pub filepath: PathBuf,
    pub size: PrintSize,
    pub fit_to_page: bool,
    pub center_to_page: bool,
    pub freehand_placement: bool,
    pub freehand_x_pt: f32,
    pub freehand_y_pt: f32,
    pub source_icc: Option<PathBuf>,
    pub position: Point,
    pub page: usize,
    pub rotation: f32,
    pub placed_w_px: u32,
    pub placed_h_px: u32,
    pub src_size_px: Option<(u32, u32)>,
    pub crop_enabled: bool,
    pub crop_u0: Option<f32>,
    pub crop_v0: Option<f32>,
    pub crop_u1: Option<f32>,
    pub crop_v1: Option<f32>,
    pub crop_inverted: bool,
    pub border_type: BorderType,
    pub border_width_pt: f32,
    pub force_original_orientation: bool,
}

#[derive(Clone, Copy)]
pub struct Placement {
    pub page: usize,
    pub x_px: u32,
    pub y_px: u32,
    pub w_px: u32,
    pub h_px: u32,
    pub rotation_deg: f32,
}

pub struct LayoutResult {
    pub placements: HashMap<Uuid, Placement>,
    pub page_count: usize,
}

pub fn layout_queue(
    items: &[QueuedImage],
    page_w_px: u32,
    page_h_px: u32,
    dpi: u32,
    spacing_in: f32,
) -> LayoutResult {
    let spacing_px = (spacing_in.max(0.0) * dpi as f32).round() as u32;

    let mut placements = HashMap::new();
    let mut cursor_x = 0u32;
    let mut cursor_y = 0u32;
    let mut row_h = 0u32;
    let mut page = 0usize;

    for item in items {
        if item.fit_to_page {
            if cursor_x > 0 || cursor_y > 0 || row_h > 0 {
                page = page.saturating_add(1);
            }

            let rotate = should_rotate_for_full_page(item.src_size_px, page_w_px, page_h_px)
                && !item.force_original_orientation;
            placements.insert(
                item.id,
                Placement {
                    page,
                    x_px: 0,
                    y_px: 0,
                    w_px: page_w_px.max(1),
                    h_px: page_h_px.max(1),
                    rotation_deg: if rotate { 90.0 } else { 0.0 },
                },
            );

            page = page.saturating_add(1);
            cursor_x = 0;
            cursor_y = 0;
            row_h = 0;
            continue;
        }

        // Center to page: place image alone and centered on its own page
        if item.center_to_page {
            if cursor_x > 0 || cursor_y > 0 || row_h > 0 {
                page = page.saturating_add(1);
            }

            let (mut w_in, mut h_in) = item.size.as_inches();
            w_in = w_in.max(0.01);
            h_in = h_in.max(0.01);

            // Expand dimensions by outer border BEFORE orientation selection so
            // simulate_insertion sees the true final box size and can rotate to fit.
            let border_expansion_in = if item.border_type == BorderType::Outer {
                item.border_width_pt / 72.0
            } else {
                0.0
            };

            // Use same orientation logic as normal flow (cursor at 0,0 on fresh page)
            let (box_w_px, box_h_px, rotate) = choose_orientation_for_flow_with_state(
                item.src_size_px,
                w_in + border_expansion_in * 2.0,
                h_in + border_expansion_in * 2.0,
                dpi,
                0, // cursor_x - fresh page start
                0, // cursor_y - fresh page start
                0, // row_h - fresh page start
                page_w_px,
                page_h_px,
                spacing_px,
            );
            let rotate = rotate && !item.force_original_orientation;
            // box_w_px / box_h_px already include the border expansion

            // Calculate center position based on actual box size (including outer border expansion)
            let x_px = (page_w_px.saturating_sub(box_w_px)) / 2;
            let y_px = (page_h_px.saturating_sub(box_h_px)) / 2;

            placements.insert(
                item.id,
                Placement {
                    page,
                    x_px,
                    y_px,
                    w_px: box_w_px,
                    h_px: box_h_px,
                    rotation_deg: if rotate { 90.0 } else { 0.0 },
                },
            );

            page = page.saturating_add(1);
            cursor_x = 0;
            cursor_y = 0;
            row_h = 0;
            continue;
        }

        // Freehand placement: place alone on its own page at user-saved position
        if item.freehand_placement {
            if cursor_x > 0 || cursor_y > 0 || row_h > 0 {
                page = page.saturating_add(1);
            }

            let (mut w_in, mut h_in) = item.size.as_inches();
            w_in = w_in.max(0.01);
            h_in = h_in.max(0.01);

            let border_expansion_in = if item.border_type == BorderType::Outer {
                item.border_width_pt / 72.0
            } else {
                0.0
            };

            let (box_w_px, box_h_px, rotate) = choose_orientation_for_flow_with_state(
                item.src_size_px,
                w_in + border_expansion_in * 2.0,
                h_in + border_expansion_in * 2.0,
                dpi,
                0,
                0,
                0,
                page_w_px,
                page_h_px,
                spacing_px,
            );
            let rotate = rotate && !item.force_original_orientation;

            // Convert stored point position to pixels, clamp within page
            let x_px = ((item.freehand_x_pt * dpi as f32 / 72.0).round().max(0.0) as u32)
                .min(page_w_px.saturating_sub(box_w_px));
            let y_px = ((item.freehand_y_pt * dpi as f32 / 72.0).round().max(0.0) as u32)
                .min(page_h_px.saturating_sub(box_h_px));

            placements.insert(
                item.id,
                Placement {
                    page,
                    x_px,
                    y_px,
                    w_px: box_w_px,
                    h_px: box_h_px,
                    rotation_deg: if rotate { 90.0 } else { 0.0 },
                },
            );

            page = page.saturating_add(1);
            cursor_x = 0;
            cursor_y = 0;
            row_h = 0;
            continue;
        }

        let (mut w_in, mut h_in) = item.size.as_inches();
        w_in = w_in.max(0.01);
        h_in = h_in.max(0.01);

        // Expand dimensions by outer border BEFORE orientation selection so
        // simulate_insertion sees the true final box size and can rotate to fit.
        let border_expansion_in = if item.border_type == BorderType::Outer {
            item.border_width_pt / 72.0
        } else {
            0.0
        };

        // When force_original_orientation is set, isolate to its own page (no rotation,
        // no flow packing) — same page-break logic as center_to_page but top-left aligned.
        if item.force_original_orientation {
            if cursor_x > 0 || cursor_y > 0 || row_h > 0 {
                page = page.saturating_add(1);
            }
            let (box_w_px, box_h_px, _) = choose_orientation_for_flow_with_state(
                item.src_size_px,
                w_in + border_expansion_in * 2.0,
                h_in + border_expansion_in * 2.0,
                dpi,
                0,
                0,
                0,
                page_w_px,
                page_h_px,
                spacing_px,
            );
            placements.insert(
                item.id,
                Placement {
                    page,
                    x_px: 0,
                    y_px: 0,
                    w_px: box_w_px,
                    h_px: box_h_px,
                    rotation_deg: 0.0,
                },
            );
            page = page.saturating_add(1);
            cursor_x = 0;
            cursor_y = 0;
            row_h = 0;
            continue;
        }

        let (box_w_px, box_h_px, rotate) = choose_orientation_for_flow_with_state(
            item.src_size_px,
            w_in + border_expansion_in * 2.0,
            h_in + border_expansion_in * 2.0,
            dpi,
            cursor_x,
            cursor_y,
            row_h,
            page_w_px,
            page_h_px,
            spacing_px,
        );
        // box_w_px / box_h_px already include the border expansion

        if cursor_x > 0 && cursor_x.saturating_add(box_w_px) > page_w_px {
            cursor_x = 0;
            cursor_y = cursor_y.saturating_add(row_h).saturating_add(spacing_px);
            row_h = 0;
        }

        if cursor_y > 0 && cursor_y.saturating_add(box_h_px) > page_h_px {
            page = page.saturating_add(1);
            cursor_x = 0;
            cursor_y = 0;
            row_h = 0;
        }

        placements.insert(
            item.id,
            Placement {
                page,
                x_px: cursor_x,
                y_px: cursor_y,
                w_px: box_w_px,
                h_px: box_h_px,
                rotation_deg: if rotate { 90.0 } else { 0.0 },
            },
        );

        cursor_x = cursor_x.saturating_add(box_w_px).saturating_add(spacing_px);
        row_h = row_h.max(box_h_px);
    }

    center_rows_horizontally_per_page(&mut placements, page_w_px);

    let page_count = placements
        .values()
        .map(|p| p.page)
        .max()
        .map(|max_page| max_page.saturating_add(1))
        .unwrap_or(1);

    LayoutResult {
        placements,
        page_count,
    }
}

fn center_rows_horizontally_per_page(placements: &mut HashMap<Uuid, Placement>, page_w_px: u32) {
    let mut bounds_by_page_row: HashMap<(usize, u32), (u32, u32)> = HashMap::new();

    for p in placements.values() {
        let key = (p.page, p.y_px);
        let min_x = p.x_px;
        let max_x = p.x_px.saturating_add(p.w_px);
        bounds_by_page_row
            .entry(key)
            .and_modify(|b| {
                b.0 = b.0.min(min_x);
                b.1 = b.1.max(max_x);
            })
            .or_insert((min_x, max_x));
    }

    let mut offsets_by_page_row: HashMap<(usize, u32), i64> = HashMap::new();
    for (key, (min_x, max_x)) in bounds_by_page_row {
        let used_w = max_x.saturating_sub(min_x).min(page_w_px.max(1));
        let dx = ((page_w_px.saturating_sub(used_w)) / 2) as i64 - min_x as i64;
        offsets_by_page_row.insert(key, dx);
    }

    for p in placements.values_mut() {
        if let Some(dx) = offsets_by_page_row.get(&(p.page, p.y_px)).copied() {
            p.x_px = (p.x_px as i64 + dx).max(0) as u32;
        }
    }
}

fn choose_orientation_for_flow_with_state(
    src_size_px: Option<(u32, u32)>,
    w_in: f32,
    h_in: f32,
    dpi: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
    page_w_px: u32,
    page_h_px: u32,
    spacing_px: u32,
) -> (u32, u32, bool) {
    let to_px = |inches: f32| (inches * dpi as f32).round().max(1.0) as u32;

    let Some((sw, sh)) = src_size_px else {
        return (to_px(w_in), to_px(h_in), false);
    };

    let sw = sw.max(1) as f32;
    let sh = sh.max(1) as f32;
    let src_landscape = sw > sh;

    let preferred = if src_landscape {
        (h_in, w_in)
    } else {
        (w_in, h_in)
    };
    let alternate = if src_landscape {
        (w_in, h_in)
    } else {
        (h_in, w_in)
    };
    let pref_w_px = to_px(preferred.0);
    let pref_h_px = to_px(preferred.1);
    let pref_rotate = best_rotate_for_box(sw, sh, preferred.0, preferred.1);
    let pref_sim = simulate_insertion(
        cursor_x, cursor_y, row_h, pref_w_px, pref_h_px, page_w_px, page_h_px, spacing_px,
    );

    let alt_w_px = to_px(alternate.0);
    let alt_h_px = to_px(alternate.1);
    let alt_rotate = best_rotate_for_box(sw, sh, alternate.0, alternate.1);
    let alt_sim = simulate_insertion(
        cursor_x, cursor_y, row_h, alt_w_px, alt_h_px, page_w_px, page_h_px, spacing_px,
    );

    if pref_sim.valid && !alt_sim.valid {
        return (pref_w_px, pref_h_px, pref_rotate);
    }
    if alt_sim.valid && !pref_sim.valid {
        return (alt_w_px, alt_h_px, alt_rotate);
    }
    if pref_sim.valid && alt_sim.valid {
        let pref_cost = (pref_sim.wrapped_page as u8, pref_sim.wrapped_row as u8);
        let alt_cost = (alt_sim.wrapped_page as u8, alt_sim.wrapped_row as u8);
        if alt_cost < pref_cost {
            return (alt_w_px, alt_h_px, alt_rotate);
        }
        // Don't use packing capacity to override source-based preference
        // Source orientation should be respected when both fit equally
        return (pref_w_px, pref_h_px, pref_rotate);
    }

    // Fallback: scale to fit page while preserving aspect ratio
    let page_w = page_w_px.max(1) as f32;
    let page_h = page_h_px.max(1) as f32;
    let scale = (page_w / pref_w_px as f32)
        .min(page_h / pref_h_px as f32)
        .min(1.0);
    let fallback_w = (pref_w_px as f32 * scale).round().max(1.0) as u32;
    let fallback_h = (pref_h_px as f32 * scale).round().max(1.0) as u32;
    (fallback_w, fallback_h, pref_rotate)
}

fn best_rotate_for_box(src_w: f32, src_h: f32, box_w: f32, box_h: f32) -> bool {
    let area_no_rotate = fitted_area(src_w, src_h, box_w, box_h);
    let area_rotate = fitted_area(src_h, src_w, box_w, box_h);
    area_rotate > area_no_rotate
}

struct SimulatedInsertion {
    wrapped_row: bool,
    wrapped_page: bool,
    valid: bool,
}

fn simulate_insertion(
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
    box_w_px: u32,
    box_h_px: u32,
    page_w_px: u32,
    page_h_px: u32,
    spacing_px: u32,
) -> SimulatedInsertion {
    let page_w_px = page_w_px.max(1);
    let page_h_px = page_h_px.max(1);

    let mut x = cursor_x;
    let mut y = cursor_y;
    let mut wrapped_row = false;
    let mut wrapped_page = false;

    if cursor_x > 0 && cursor_x.saturating_add(box_w_px) > page_w_px {
        x = 0;
        y = y.saturating_add(row_h).saturating_add(spacing_px);
        wrapped_row = true;
    }

    if y > 0 && y.saturating_add(box_h_px) > page_h_px {
        x = 0;
        y = 0;
        wrapped_page = true;
    }

    let valid = box_w_px <= page_w_px
        && box_h_px <= page_h_px
        && x.saturating_add(box_w_px) <= page_w_px
        && y.saturating_add(box_h_px) <= page_h_px;

    SimulatedInsertion {
        wrapped_row,
        wrapped_page,
        valid,
    }
}

fn fitted_area(src_w: f32, src_h: f32, box_w: f32, box_h: f32) -> f32 {
    let s = (box_w / src_w).min(box_h / src_h);
    let fw = src_w * s;
    let fh = src_h * s;
    fw * fh
}

pub fn should_rotate_for_full_page(
    src_size_px: Option<(u32, u32)>,
    page_w_px: u32,
    page_h_px: u32,
) -> bool {
    let Some((sw, sh)) = src_size_px else {
        return false;
    };
    let sw = sw.max(1) as f32;
    let sh = sh.max(1) as f32;
    let pw = page_w_px.max(1) as f32;
    let ph = page_h_px.max(1) as f32;

    let n_scale = (pw / sw).min(ph / sh);
    let n_area = (sw * n_scale) * (sh * n_scale);

    let r_scale = (pw / sh).min(ph / sw);
    let r_area = (sh * r_scale) * (sw * r_scale);

    r_area > n_area
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(id: Uuid, w: f32, h: f32, src: (u32, u32)) -> QueuedImage {
        QueuedImage {
            id,
            filepath: PathBuf::from(format!("{id}.jpg")),
            size: PrintSize {
                width: w,
                height: h,
                unit: Unit::Inches,
            },
            fit_to_page: false,
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
            src_size_px: Some(src),
            crop_enabled: false,
            crop_u0: None,
            crop_v0: None,
            crop_u1: None,
            crop_v1: None,
            crop_inverted: false,
            border_type: BorderType::None,
            border_width_pt: 0.0,
            force_original_orientation: false,
        }
    }

    fn queued_metric(id: Uuid, w_mm: f32, h_mm: f32, src: (u32, u32)) -> QueuedImage {
        QueuedImage {
            id,
            filepath: PathBuf::from(format!("{id}.jpg")),
            size: PrintSize {
                width: w_mm,
                height: h_mm,
                unit: Unit::Millimeters,
            },
            fit_to_page: false,
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
            src_size_px: Some(src),
            crop_enabled: false,
            crop_u0: None,
            crop_v0: None,
            crop_u1: None,
            crop_v1: None,
            crop_inverted: false,
            border_type: BorderType::None,
            border_width_pt: 0.0,
            force_original_orientation: false,
        }
    }

    fn queued_fit(id: Uuid, src: (u32, u32)) -> QueuedImage {
        QueuedImage {
            id,
            filepath: PathBuf::from(format!("{id}.jpg")),
            size: PrintSize {
                width: 8.0,
                height: 10.0,
                unit: Unit::Inches,
            },
            fit_to_page: true,
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
            src_size_px: Some(src),
            crop_enabled: false,
            crop_u0: None,
            crop_v0: None,
            crop_u1: None,
            crop_v1: None,
            crop_inverted: false,
            border_type: BorderType::None,
            border_width_pt: 0.0,
            force_original_orientation: false,
        }
    }

    #[test]
    fn repeated_fixed_preset_items_stay_in_bounds() {
        let a = queued(Uuid::new_v4(), 5.0, 7.0, (3000, 2000));
        let b = queued(Uuid::new_v4(), 5.0, 7.0, (3200, 2100));
        let items = vec![a.clone(), b.clone()];

        let page_w_px = 1000;
        let page_h_px = 1100;
        let result = layout_queue(&items, page_w_px, page_h_px, 100, 0.0);

        for item in [&a, &b] {
            let p = result.placements.get(&item.id).expect("placement missing");
            assert!(p.w_px <= page_w_px, "placement width exceeds page");
            assert!(p.h_px <= page_h_px, "placement height exceeds page");
            assert!(
                p.x_px.saturating_add(p.w_px) <= page_w_px,
                "placement overflows x"
            );
            assert!(
                p.y_px.saturating_add(p.h_px) <= page_h_px,
                "placement overflows y"
            );
        }
    }

    #[test]
    fn selects_in_bounds_orientation_when_preferred_overflows() {
        let a = queued(Uuid::new_v4(), 5.0, 7.0, (3000, 2000));
        let items = vec![a.clone()];

        let page_w_px = 650;
        let page_h_px = 900;
        let result = layout_queue(&items, page_w_px, page_h_px, 100, 0.0);
        let p = result.placements.get(&a.id).expect("placement missing");

        assert!(p.w_px <= page_w_px, "placement width exceeds page");
        assert!(p.h_px <= page_h_px, "placement height exceeds page");
        assert!(
            p.x_px.saturating_add(p.w_px) <= page_w_px,
            "placement overflows x"
        );
        assert!(
            p.y_px.saturating_add(p.h_px) <= page_h_px,
            "placement overflows y"
        );
    }

    #[test]
    fn final_fit_to_page_item_does_not_create_blank_trailing_page() {
        let a = queued(Uuid::new_v4(), 5.0, 7.0, (3000, 2000));
        let b = queued_fit(Uuid::new_v4(), (2000, 3000));
        let items = vec![a.clone(), b.clone()];

        let result = layout_queue(&items, 1000, 1400, 100, 0.25);

        let page_a = result
            .placements
            .get(&a.id)
            .expect("placement missing")
            .page;
        let page_b = result
            .placements
            .get(&b.id)
            .expect("placement missing")
            .page;
        let highest_page = page_a.max(page_b);

        assert_eq!(
            page_b, highest_page,
            "fit-to-page item should be on the last used page"
        );
        assert_eq!(
            result.page_count,
            highest_page + 1,
            "page count should match highest placed page"
        );
    }

    #[test]
    fn portrait_duplicates_can_repack_to_single_page() {
        // Two 5×7" items (portrait src) on a 10×15" page @ 100dpi with 0.25" spacing.
        // Both items are 500×700px. Row 1 fits one item; row 2 starts at y=725.
        // 725+700=1425 <= 1500, so both fit on a single page.
        let a = queued(Uuid::new_v4(), 5.0, 7.0, (2000, 3000));
        let b = queued(Uuid::new_v4(), 5.0, 7.0, (2100, 3200));
        let items = vec![a.clone(), b.clone()];

        let result = layout_queue(&items, 1000, 1500, 100, 0.25);
        let pa = result.placements.get(&a.id).expect("placement missing");
        let pb = result.placements.get(&b.id).expect("placement missing");

        assert_eq!(
            result.page_count, 1,
            "both portrait duplicates should fit on one page after repack"
        );
        assert_eq!(pa.page, 0, "first item should remain on page 1");
        assert_eq!(pb.page, 0, "second item should remain on page 1");
        assert!(
            pa.y_px.saturating_add(pa.h_px) <= 1500,
            "first item overflowed page height"
        );
        assert!(
            pb.y_px.saturating_add(pb.h_px) <= 1500,
            "second item overflowed page height"
        );
    }

    #[test]
    fn single_item_is_centered_horizontally_and_top_anchored() {
        let a = queued(Uuid::new_v4(), 2.0, 2.0, (1000, 1000));
        let result = layout_queue(&[a.clone()], 1000, 1000, 100, 0.0);
        let p = result.placements.get(&a.id).expect("placement missing");

        assert_eq!(p.w_px, 200);
        assert_eq!(p.h_px, 200);
        assert_eq!(p.x_px, 400, "item should be centered horizontally");
        assert_eq!(p.y_px, 0, "first row should be top-anchored");
    }

    #[test]
    fn final_row_is_centered_horizontally_with_three_items() {
        let a = queued(Uuid::new_v4(), 4.0, 6.0, (2000, 3000));
        let b = queued(Uuid::new_v4(), 4.0, 6.0, (2100, 3200));
        let c = queued(Uuid::new_v4(), 4.0, 6.0, (1900, 2900));
        let items = vec![a.clone(), b.clone(), c.clone()];

        let result = layout_queue(&items, 1000, 1400, 100, 0.25);
        let pa = result.placements.get(&a.id).expect("placement missing");
        let pb = result.placements.get(&b.id).expect("placement missing");
        let pc = result.placements.get(&c.id).expect("placement missing");

        assert_eq!(
            result.page_count, 1,
            "three 4x6 items should remain on one page"
        );
        assert_eq!(pa.y_px, 0, "first row should start at top margin");
        assert_eq!(pb.y_px, 0, "first row should start at top margin");
        assert!(pc.y_px > 0, "third item should be on a lower row");
        assert_eq!(
            pc.x_px, 300,
            "single item on second row should be horizontally centered"
        );
    }

    #[test]
    fn outer_border_does_not_overflow_page() {
        // 5×7" item + 1" outer border = 7×9" expanded box.
        // Page is 874×1076px @ 100dpi = 8.74×10.76".
        // Portrait 7×9 fits (9 < 10.76). Landscape 9×7 also fits.
        // Either way the placed box must stay within page bounds.
        let mut item = queued(Uuid::new_v4(), 5.0, 7.0, (3000, 2000));
        item.border_type = BorderType::Outer;
        item.border_width_pt = 72.0; // 1 inch = 72 pt

        let page_w_px = 874u32;
        let page_h_px = 1076u32;
        let result = layout_queue(&[item.clone()], page_w_px, page_h_px, 100, 0.0);
        let p = result.placements.get(&item.id).expect("placement missing");

        assert!(
            p.w_px <= page_w_px,
            "placed width {} exceeds page {}",
            p.w_px,
            page_w_px
        );
        assert!(
            p.h_px <= page_h_px,
            "placed height {} exceeds page {}",
            p.h_px,
            page_h_px
        );
        assert!(
            p.x_px.saturating_add(p.w_px) <= page_w_px,
            "placement overflows x: x={} w={} page_w={}",
            p.x_px,
            p.w_px,
            page_w_px
        );
        assert!(
            p.y_px.saturating_add(p.h_px) <= page_h_px,
            "placement overflows y: y={} h={} page_h={}",
            p.y_px,
            p.h_px,
            page_h_px
        );
    }

    #[test]
    fn outer_border_center_to_page_does_not_overflow() {
        // Same scenario with center_to_page flag set.
        let mut item = queued(Uuid::new_v4(), 5.0, 7.0, (3000, 2000));
        item.center_to_page = true;
        item.border_type = BorderType::Outer;
        item.border_width_pt = 72.0; // 1 inch

        let page_w_px = 874u32;
        let page_h_px = 1076u32;
        let result = layout_queue(&[item.clone()], page_w_px, page_h_px, 100, 0.0);
        let p = result.placements.get(&item.id).expect("placement missing");

        assert!(
            p.w_px <= page_w_px,
            "placed width {} exceeds page {}",
            p.w_px,
            page_w_px
        );
        assert!(
            p.h_px <= page_h_px,
            "placed height {} exceeds page {}",
            p.h_px,
            page_h_px
        );
        assert!(
            p.x_px.saturating_add(p.w_px) <= page_w_px,
            "placement overflows x"
        );
        assert!(
            p.y_px.saturating_add(p.h_px) <= page_h_px,
            "placement overflows y"
        );
    }

    #[test]
    fn mm_to_inches_precision() {
        let w = mm_to_inches(210.0);
        let h = mm_to_inches(297.0);
        let expected_w = 210.0_f64 / 25.4_f64;
        let expected_h = 297.0_f64 / 25.4_f64;
        assert!(
            (w as f64 - expected_w).abs() < 1e-6,
            "A4 width: got {}, expected ~{}",
            w,
            expected_w
        );
        assert!(
            (h as f64 - expected_h).abs() < 1e-6,
            "A4 height: got {}, expected ~{}",
            h,
            expected_h
        );

        let a0_w = mm_to_inches(841.0);
        let a0_h = mm_to_inches(1189.0);
        let expected_a0_w = 841.0_f64 / 25.4_f64;
        let expected_a0_h = 1189.0_f64 / 25.4_f64;
        assert!(
            (a0_w as f64 - expected_a0_w).abs() < 1e-5,
            "A0 width: got {}, expected ~{}",
            a0_w,
            expected_a0_w
        );
        assert!(
            (a0_h as f64 - expected_a0_h).abs() < 1e-5,
            "A0 height: got {}, expected ~{}",
            a0_h,
            expected_a0_h
        );

        let ps = PrintSize {
            width: 210.0,
            height: 297.0,
            unit: Unit::Millimeters,
        };
        let (w2, h2) = ps.as_inches();
        assert_eq!(w2, w, "as_inches should match mm_to_inches for width");
        assert_eq!(h2, h, "as_inches should match mm_to_inches for height");
    }

    #[test]
    fn inches_to_mm_precision() {
        let w = inches_to_mm(8.268);
        let h = inches_to_mm(11.693);
        let expected_w = 8.268_f64 * 25.4_f64;
        let expected_h = 11.693_f64 * 25.4_f64;
        assert!(
            (w as f64 - expected_w).abs() < 1e-3,
            "A4 width: got {}, expected ~{}",
            w,
            expected_w
        );
        assert!(
            (h as f64 - expected_h).abs() < 1e-3,
            "A4 height: got {}, expected ~{}",
            h,
            expected_h
        );

        let roundtrip = inches_to_mm(mm_to_inches(210.0));
        assert!(
            (roundtrip - 210.0).abs() < 0.1,
            "roundtrip 210mm->in->mm: got {}, expected ~210",
            roundtrip
        );
    }

    #[test]
    fn metric_a4_layout_at_720dpi() {
        let a4 = queued_metric(Uuid::new_v4(), 210.0, 297.0, (3000, 4000));
        let page_w_px = 6000u32;
        let page_h_px = 8500u32;
        let result = layout_queue(&[a4.clone()], page_w_px, page_h_px, 720, 0.0);
        let p = result.placements.get(&a4.id).expect("placement missing");

        assert_eq!(p.w_px, 5953, "A4 width at 720dpi");
        assert_eq!(p.h_px, 8419, "A4 height at 720dpi");
    }

    #[test]
    fn metric_a4_layout_at_300dpi() {
        let a4 = queued_metric(Uuid::new_v4(), 210.0, 297.0, (3000, 4000));
        let page_w_px = 2600u32;
        let page_h_px = 3600u32;
        let result = layout_queue(&[a4.clone()], page_w_px, page_h_px, 300, 0.0);
        let p = result.placements.get(&a4.id).expect("placement missing");

        assert_eq!(p.w_px, 2480, "A4 width at 300dpi");
        assert_eq!(p.h_px, 3508, "A4 height at 300dpi");
    }
}
