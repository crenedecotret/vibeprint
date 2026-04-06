use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

#[derive(Clone, Copy, PartialEq)]
pub enum Unit {
    Inches,
    Millimeters,
}

#[derive(Clone, Copy)]
pub struct PrintSize {
    pub width: f32,
    pub height: f32,
    pub unit: Unit,
}

impl PrintSize {
    pub fn as_inches(self) -> (f32, f32) {
        match self.unit {
            Unit::Inches => (self.width, self.height),
            Unit::Millimeters => (self.width / 25.4, self.height / 25.4),
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
    pub source_icc: Option<PathBuf>,
    pub position: Point,
    pub page: usize,
    pub rotation: f32,
    pub placed_w_px: u32,
    pub placed_h_px: u32,
    pub src_size_px: Option<(u32, u32)>,
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

            let rotate = should_rotate_for_full_page(item.src_size_px, page_w_px, page_h_px);
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

        let (mut w_in, mut h_in) = item.size.as_inches();
        w_in = w_in.max(0.01);
        h_in = h_in.max(0.01);

        let (box_w_px, box_h_px, rotate) = choose_orientation_for_flow_with_state(
            item.src_size_px,
            w_in,
            h_in,
            dpi,
            cursor_x,
            cursor_y,
            row_h,
            page_w_px,
            page_h_px,
            spacing_px,
        );

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

    let preferred = if src_landscape { (h_in, w_in) } else { (w_in, h_in) };
    let alternate = if src_landscape { (w_in, h_in) } else { (h_in, w_in) };
    let pref_w_px = to_px(preferred.0);
    let pref_h_px = to_px(preferred.1);
    let pref_rotate = best_rotate_for_box(sw, sh, preferred.0, preferred.1);
    let pref_sim = simulate_insertion(
        cursor_x,
        cursor_y,
        row_h,
        pref_w_px,
        pref_h_px,
        page_w_px,
        page_h_px,
        spacing_px,
    );

    let alt_w_px = to_px(alternate.0);
    let alt_h_px = to_px(alternate.1);
    let alt_rotate = best_rotate_for_box(sw, sh, alternate.0, alternate.1);
    let alt_sim = simulate_insertion(
        cursor_x,
        cursor_y,
        row_h,
        alt_w_px,
        alt_h_px,
        page_w_px,
        page_h_px,
        spacing_px,
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
        return (pref_w_px, pref_h_px, pref_rotate);
    }

    let fallback_w = pref_w_px.min(page_w_px.max(1));
    let fallback_h = pref_h_px.min(page_h_px.max(1));
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

fn should_rotate_for_full_page(
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
            source_icc: None,
            position: Point::default(),
            page: 0,
            rotation: 0.0,
            placed_w_px: 0,
            placed_h_px: 0,
            src_size_px: Some(src),
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
            source_icc: None,
            position: Point::default(),
            page: 0,
            rotation: 0.0,
            placed_w_px: 0,
            placed_h_px: 0,
            src_size_px: Some(src),
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
            assert!(p.x_px.saturating_add(p.w_px) <= page_w_px, "placement overflows x");
            assert!(p.y_px.saturating_add(p.h_px) <= page_h_px, "placement overflows y");
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
        assert!(p.x_px.saturating_add(p.w_px) <= page_w_px, "placement overflows x");
        assert!(p.y_px.saturating_add(p.h_px) <= page_h_px, "placement overflows y");
    }

    #[test]
    fn final_fit_to_page_item_does_not_create_blank_trailing_page() {
        let a = queued(Uuid::new_v4(), 5.0, 7.0, (3000, 2000));
        let b = queued_fit(Uuid::new_v4(), (2000, 3000));
        let items = vec![a.clone(), b.clone()];

        let result = layout_queue(&items, 1000, 1400, 100, 0.25);

        let page_a = result.placements.get(&a.id).expect("placement missing").page;
        let page_b = result.placements.get(&b.id).expect("placement missing").page;
        let highest_page = page_a.max(page_b);

        assert_eq!(page_b, highest_page, "fit-to-page item should be on the last used page");
        assert_eq!(result.page_count, highest_page + 1, "page count should match highest placed page");
    }
}
