# Crop Editor and Border Functionality Documentation

## Overview

This document describes how the crop editor, inner borders, outer borders, and the "keep original orientation" option interact in VibePrint Studio. The logic is spread across the layout engine, canvas preview, crop editor modal, and multiple UI handlers. **All paths must agree on the same dimension-selection rule** or visible-area distortions will occur.

## Key Concepts

### Print Size Storage Convention

`PrintSize` is always stored **portrait-normalized** (`w <= h`). This means a 4×6" print is stored as `(4.0, 6.0)` regardless of the image's actual orientation.

Because of this convention, any code that picks cell dimensions must **orient the print size to match the source image** before deciding whether to swap.

### Natural Orientation

The "natural orientation" of a box is the print size oriented to match the source image's aspect ratio:
```rust
let src_landscape = sw > sh;
let (oriented_w, oriented_h) = if src_landscape {
    (h_in, w_in)  // landscape source: swap so box is wider than tall
} else {
    (w_in, h_in)  // portrait source: keep as-is
};
```
- `oriented_w` / `oriented_h` represent the box in its **natural** (source-matching) orientation.
- `will_rotate` decides whether the layout engine rotates the image away from this natural orientation to fit the page.

### Crop Inversion

**Purpose:** Crop inversion flips the layout engine's rotation decision. If the engine would normally rotate an image to fit the page better, inversion keeps it un-rotated (and vice versa).

**How it works:**
- Without inversion: Image rotates if it fits better when rotated
- With inversion: Image does NOT rotate if it would normally rotate, and rotates if it wouldn't normally rotate
- Inversion is stored in `QueuedImage.crop_inverted`

### Force Original Orientation (FOO)

**Purpose:** `force_original_orientation` forces an image to stay in its natural orientation (portrait stays portrait, landscape stays landscape) regardless of page fit.

**How it works:**
- When enabled, `will_rotate` is forced to `false`
- The layout engine orients the print size to match the source image (natural orientation)
- This is a per-image setting in `QueuedImage.force_original_orientation`
- The right panel dims the checkbox when the natural box cannot fit the page

### Interaction: FOO + Crop Inverted

This is the most subtle case. When **both** are true:

1. The image itself is **NOT rotated** (FOO forces `will_rotate = false`)
2. BUT the user has requested an **inverted crop**, meaning they want the aspect ratio that would have resulted from the *other* rotation decision
3. Therefore the **cell dimensions must be swapped** to match the inverted crop's aspect ratio, while the **image content stays un-rotated**

**Every code path** that computes dimensions for crop UVs must apply this same swap:
```rust
let (w, h) = if force_original_orientation && crop_inverted {
    (oriented_h, oriented_w)  // swapped (inverted from natural)
} else if force_original_orientation {
    (oriented_w, oriented_h)  // natural orientation
} else if will_rotate {
    (oriented_h, oriented_w)  // swapped (rotated)
} else {
    (oriented_w, oriented_h)  // natural orientation
};
```

### Visible Area Calculation

The visible area is the portion of the print where the image appears:

**No Border:**
```
┌─────────────────────┐
│                     │
│      IMAGE          │
│                     │
└─────────────────────┘
```

**Inner Border:**
```
┌─────────────────────┐
│  ╔═══════════════╗  │
│  ║               ║  │
│  ║    IMAGE      ║  │  ← Image fits inside the border
│  ║               ║  │
│  ╚═══════════════╝  │
└─────────────────────┘
```

**Outer Border:**
```
┌─────────────────────────────┐
│  ╔═══════════════════════╗  │
│  ║                       ║  │
│  ║        IMAGE          ║  │  ← Image fills center, border adds outside
│  ║                       ║  │
│  ╚═══════════════════════╝  │
└─────────────────────────────┘
```

## Code Paths

### 1. Layout Engine (`src/layout_engine.rs`)

`choose_orientation_for_flow_with_state` selects cell dimensions.

**When `force_original_orientation` is true:**
```rust
let src_landscape = sw > sh;
let (natural_w, natural_h) = if src_landscape {
    (h_in, w_in)   // orient print size to match source
} else {
    (w_in, h_in)
};
```

The function returns `(to_px(natural_w), to_px(natural_h), false)`.

After `choose_orientation` returns, all three layout paths (flow, center-to-page, freehand) apply the `crop_inverted` swap:
```rust
let (box_w_px, box_h_px) = if item.force_original_orientation && item.crop_inverted {
    (box_h_px, box_w_px)
} else {
    (box_w_px, box_h_px)
};
```

### 2. Crop Enable (`right_panel.rs` — Crop Image checkbox handler)

**When triggered:** User clicks "Crop Image" checkbox

**Flow:**
1. Calculate `will_rotate` based on oriented dimensions
2. If `force_original_orientation` is set, override `will_rotate = false`
3. Set `(calc_w, calc_h)` using the unified dimension rule (FOO+inverted → swapped; FOO → natural; will_rotate → swapped; else → natural)
4. **For Inner/Outer Border:** swap dimensions before border adjustment if `crop_inverted && !force_original_orientation`, then expand/shrink, then swap back
5. Calculate `will_rotate_for_uv` — if FOO, false; else if inverted, `!will_rotate`
6. Call `calc_crop_uv(calc_w, calc_h, ...)`

### 3. Border Width/Type Change (`right_panel.rs` — border handler)

**When triggered:** User changes border type or width

**Flow:**
1. Set default border width BEFORE crop calculation (order matters)
2. Calculate `will_rotate`
3. Override with `false` if FOO
4. Calculate `effective_will_rotate = if FOO { false } else if inverted { !will } else { will }`
5. Set `(full_w, full_h)` using the unified dimension rule:
   - `FOO && crop_inverted` → `(oriented_h, oriented_w)`
   - `FOO` → `(oriented_w, oriented_h)`
   - `effective_will_rotate` → `(oriented_h, oriented_w)`
   - Else → `(oriented_w, oriented_h)`
6. Apply border to get visible area
7. Recalculate UVs preserving center and area

### 4. Crop Editor Modal (`modals.rs` — `show_crop_editor`)

**When triggered:** User clicks "Edit" to open crop editor

**Flow:**
1. Load stored UVs or calculate auto-crop UVs
2. Calculate `will_rotate` for the oriented box
3. Override with `false` if FOO
4. Set `(target_w, target_h)` using the unified dimension rule
5. Adjust display rect for borders (same swap logic for border adjustment)
6. Handle interactions and save UVs back

### 5. queued_box_px (`app.rs`)

Calculates pixel dimensions for the canvas preview and processor.

```rust
if placed_w_px > 0 && placed_h_px > 0:
    return (placed_w_px, placed_h_px)  // layout engine already handled FOO+inverted swap

if rotation > 0.0:
    swap(w, h)

if Outer Border:
    if crop_inverted && !force_original_orientation:
        swap(w, h)
    expand by 2×border
    if crop_inverted && !force_original_orientation:
        swap_back(w, h)
elif Inner Border:
    if crop_inverted && !force_original_orientation:
        swap(w, h)
    shrink by 2×border
    if crop_inverted && !force_original_orientation:
        swap_back(w, h)
```

Note: `queued_box_px` does NOT need a separate `crop_inverted && force_original_orientation` path because when FOO is true, the layout engine has already baked the swap into `placed_w_px` / `placed_h_px`, and the early return catches it.

### 6. Size Change Handlers (`app.rs`)

When the user changes print size (preset or custom), crop UVs may need recalculation.

Both `update_selected_queue_size` and `update_selected_queue_size_idx` must use the unified dimension rule:
```rust
let (full_w, full_h) = if item.force_original_orientation && item.crop_inverted {
    (oriented_h, oriented_w)
} else if force_original_orientation {
    (oriented_w, oriented_h)
} else if will_rotate {
    (oriented_h, oriented_w)
} else {
    (oriented_w, oriented_h)
};
```

### 7. Canvas Preview (`canvas.rs` — `draw_canvas`)

1. Get box dimensions from `queued_box_px()` (layout-engine dimensions, already correct)
2. Calculate `display_rect` accounting for border type
3. Calculate `will_rotate_for_display`:
   ```rust
   let should = should_rotate_for_full_page(src_size, w_px, h_px);
   let should = if crop_inverted { !should } else { should };
   if force_original_orientation { false } else { should }
   ```
4. Apply UVs to the mesh, rotating if needed

### 8. Processor (`processor.rs` — `process_composite_page`)

Receives `PagePlacement` structs with dimensions already computed by the layout engine. For inner borders, it shrinks the destination by the border amount and stretches the cropped image to fill exactly.

## Common Issues and Solutions

### Issue: Aspect distortion when changing inner border width

**Cause:** Border change recalculation used `will_rotate` directly instead of `effective_will_rotate`

**Fix:** Use `effective_will_rotate` in border change handler.

### Issue: Canvas preview doesn't match crop editor

**Cause:** `queued_box_px` didn't swap dimensions for inner borders with inversion

**Fix:** Added inner border swap logic to `queued_box_px`.

### Issue: Landscape image with FOO produces wrong cell orientation

**Cause:** `choose_orientation_for_flow_with_state` returned `(w_in, h_in)` (portrait-normalized print size) when FOO was true, instead of orienting the print size to match the landscape source.

**Fix:** In the FOO branch of `choose_orientation`, compute the natural orientation:
```rust
let src_landscape = sw > sh;
let (natural_w, natural_h) = if src_landscape { (h_in, w_in) } else { (w_in, h_in) };
```

### Issue: Distortion when applying border AFTER enabling FOO + inverted crop

**Cause:** When `force_original_orientation && crop_inverted` is true, the layout engine swaps the cell dimensions. But the border change handler (and size change handlers) computed the visible area using the **unswapped** natural print size `(cell_w_in, cell_h_in)` because the FOO branch bypassed the swap. The crop UVs were then reshaped to the wrong aspect ratio.

**Fix:** Updated all dimension-selection points to use the unified dimension rule, with `force_original_orientation && crop_inverted` checked **before** `will_rotate`.

### Issue: Minor distortion when switching from "None" to "Inner" border

**Cause:** Border width was not set before crop UV recalculation.

**Fix:** Set default border width before crop calculation:
```rust
if border_enabled {
    border_width_pt = if use_metric { 2.835 } else { 1.0 };
}
```

## Testing Checklist

When modifying crop/border code, verify:

- [ ] Non-inverted crop + no border: aspect correct
- [ ] Non-inverted crop + inner border: aspect correct
- [ ] Non-inverted crop + outer border: aspect correct
- [ ] Inverted crop + no border: aspect correct
- [ ] Inverted crop + inner border: aspect correct
- [ ] Inverted crop + outer border: aspect correct
- [ ] Change inner border width: aspect stays correct
- [ ] Switch from None → Inner → None → Inner: aspect correct each time
- [ ] Open crop editor after border change: selection matches display
- [ ] Reset crop in editor: aspect correct
- [ ] Canvas preview matches crop editor display
- [ ] Export/print matches preview
- [ ] Force original orientation (portrait source): image stays portrait, cell is portrait
- [ ] Force original orientation (landscape source): image stays landscape, cell is landscape
- [ ] Force original orientation + crop inverted (portrait source): cell swapped to landscape
- [ ] Force original orientation + crop inverted (landscape source): cell swapped to portrait
- [ ] Force original orientation + crop inverted + inner border: aspect correct
- [ ] Force original orientation + crop inverted + outer border: aspect correct
- [ ] **ORDER MATTERS:** inverted crop → FOO → border should match inverted crop → border → FOO

## Key Takeaways

1. **`force_original_orientation` uses the natural orientation:** the print size must be oriented to match the source image (`oriented_w, oriented_h`), not used raw.
2. **`force_original_orientation && crop_inverted` is a special case:** the cell dimensions must be swapped from natural, but the image content stays un-rotated.
3. **All dimension-selection points must agree:** any code that chooses between `(w, h)` and `(h, w)` must handle both FOO and FOO+inverted before checking `will_rotate`.
4. **Inversion affects display rotation, not UV calculation directly** — but when combined with FOO, it affects the cell aspect ratio.
5. **Inner borders shrink the visible area, outer borders expand the cell.**
6. **Crop UVs are calculated once, then adjusted (not recalculated) for border changes.**
7. **Order of operations matters:** border width must be set before calculating the visible area and adjusting crop UVs.

## Code References

- **Layout engine:** `src/layout_engine.rs` — `layout_queue`, `choose_orientation_for_flow_with_state`
- **Crop enable:** `src/bin/studio/ui/right_panel.rs` — Crop Image checkbox handler in `draw_tab_image`
- **Border change:** `src/bin/studio/ui/right_panel.rs` — border type/width change handler in `draw_tab_image`
- **Crop editor:** `src/bin/studio/ui/modals.rs` — `show_crop_editor` method
- **Canvas preview:** `src/bin/studio/ui/canvas.rs` — `draw_canvas` method
- **Box dimensions:** `src/bin/studio/app.rs` — `queued_box_px` method
- **Size changes:** `src/bin/studio/app.rs` — `update_selected_queue_size`, `update_selected_queue_size_idx`
- **Processor:** `src/processor.rs` — `process_composite_page`
- **UV calculation:** `src/bin/studio/utils.rs` — `calc_crop_uv`
