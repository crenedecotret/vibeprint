# Crop Editor and Border Functionality Documentation

## Overview

The crop editor, inner/outer borders, crop inversion, and "Keep original orientation" option interact in five places that all need to agree on the same dimension-selection logic:

1. **Layout engine** (`src/layout_engine.rs` — `layout_queue` / `choose_orientation_for_flow_with_state`) — picks the cell the source will be placed into.
2. **Crop UV calculator** (`src/bin/studio/utils.rs` — `calc_crop_uv`) — picks which source pixels to show inside that cell.
3. **Right-panel crop enable** (`src/bin/studio/ui/right_panel.rs`) — initial UVs when the user clicks "Crop Image".
4. **Right-panel border change** (`src/bin/studio/ui/right_panel.rs`) — recalculates UVs when border type/width changes.
5. **Crop editor modal** (`src/bin/studio/ui/modals.rs` — `show_crop_editor` / `crop_editor_dimensions`) — shows the crop rectangle interactively.

Plus two read-only consumers of the layout result:
- **Canvas preview** (`src/bin/studio/ui/canvas.rs` — `draw_canvas`) — uses `queued_box_px()` and re-derives rotation from `should_rotate_for_full_page`.
- **Processor** (`src/processor.rs` — `process_composite_page`) — receives `placed_w_px` / `placed_h_px` from the layout; trusts them.

The bugs that have lived in this area all come from two independent mistakes:
- a **dimension rule** that picks the wrong `(w, h)` for the cell;
- a **rotation decision** that picks the wrong `will_rotate` for the crop UVs.

The two are linked, because a wrong rotation feeds into a wrong `src_aspect`, which distorts the crop rectangle. This file documents what the code actually does, not a simplified version of it.

## Key Concepts

### Print Size Storage Convention

`PrintSize` is always stored **portrait-normalized** (`width <= height`). A 4×6" print is stored as `width=4.0, height=6.0` regardless of the image's actual orientation. (See `src/layout_engine.rs:20-46`.)

Any code that needs the cell's orientation-relative dimensions must derive `(oriented_w, oriented_h)` from `src_size_px` first — it cannot just read `size.width` / `size.height` and decide which way is "natural".

### Natural Orientation

Given `(w_in, h_in)` (portrait-normalized) and a source size `(sw, sh)`:

```rust
let src_landscape = (sw as f32) > (sh as f32);
let (oriented_w, oriented_h) = if src_landscape {
    (h_in, w_in)  // swap to make the box wider than tall
} else {
    (w_in, h_in)  // keep — the box is already portrait
};
```

`(oriented_w, oriented_h)` is the cell the **un-rotated** source fits into. It is the box the layout engine uses for FOO and the box the crop UVs are computed against when rotation is off.

Note: because the print size is always portrait-normalized, the swap above is the *only* way to produce a landscape cell from the stored `PrintSize`. There is no separate "landscape print size" code path.

### `will_rotate` — Two Meanings in the Codebase

There are two distinct `will_rotate` values, and they can differ:

- **Layout's `will_rotate`**: the boolean that the layout engine returns as the third element of `choose_orientation_for_flow_with_state` and stores in `Placement::rotation_deg`. For FOO, this is always `false` (forced).
- **Handler's `will_rotate`**: the boolean handlers recompute inline via `fitted_area_rotate > fitted_area_no_rotate` when they need to decide what `(w, h)` to use for the visible area. This is the same formula as `should_rotate_for_full_page` in `src/layout_engine.rs:533`, but applied to the print size, not the page.

The crop UVs and the canvas preview use a third flavor: `will_rotate_for_uv` / `will_rotate_for_display`, which is the handler's `will_rotate` XORed with `crop_inverted` and ANDed with `!force_original_orientation`. See `src/bin/studio/ui/right_panel.rs:1281-1287`, `src/bin/studio/ui/modals.rs:1044-1049, 1335-1340, 1458-1463`, and `src/bin/studio/ui/canvas.rs:359-373`.

The three values collapse to the same answer for non-FOO, non-inverted images. They diverge in the corner cases below.

### Force Original Orientation (FOO)

`QueuedImage.force_original_orientation` (`src/layout_engine.rs:80`) is a per-image flag with three effects:

1. **Layout engine** forces `will_rotate = false` and uses `(oriented_w, oriented_h)` for the cell, then if `crop_inverted` is also true, swaps the cell to `(oriented_h, oriented_w)` (`src/layout_engine.rs:392-414` and `:171-177, :232-238, :290-296`).
2. **Handlers** (right panel, crop editor, FOO toggle) override their own `will_rotate` to `false` before any dimension/UV calculation, and use the four-branch dimension rule (below).
3. **FOO checkbox** is dimmed (with a tooltip) when any of these are true (`src/bin/studio/ui/right_panel.rs:1499-1572`):
   - no source image,
   - the image is already at its natural orientation (`!effective_will_rotate`),
   - the natural-orientation box cannot fit in the imageable area at the current print size.

### Crop Inversion

`QueuedImage.crop_inverted` flips the rotation decision everywhere it appears:

- Layout engine: after `choose_orientation` returns, if FOO + inverted, the cell is swapped to `(oriented_h, oriented_w)` to give the cell the *other* aspect ratio while leaving the source un-rotated. This is the FOO + inverted case described above.
- Handlers: `effective_will_rotate = if FOO { false } else if inverted { !will_rotate } else { will_rotate }`. This affects which branch of the dimension rule is selected.
- Crop UV calculation: `will_rotate_for_uv = if FOO { false } else if inverted { !will_rotate } else { will_rotate }`. This affects which pixels the crop rectangle picks.
- Canvas display: `should_rotate_for_full_page` result is XORed with `crop_inverted`.

The purpose of inversion is to let the user request the *other* aspect ratio's crop without rotating the image. The most common use is: I have a portrait source and a 4×6 print that the layout engine rotates to landscape; I want the cell to be landscape (rotated aspect) but the image content to stay portrait — enable FOO and crop_inverted.

### Border Types

`BorderType::None | Inner | Outer` (`src/layout_engine.rs:12-18`). Borders are sized in **points** (`border_width_pt`, 1 pt = 1/72 in) on `QueuedImage`. Default width is 1 pt (≈ 0.353 mm) when the user enables a border for the first time; see `src/bin/studio/ui/right_panel.rs:2335-2338`.

**Inner border**: the border *eats into* the cell. Visible area = cell − 2 × border, along the long axis the border is sized against. The image is stretched to fill the smaller visible area.

**Outer border**: the border *adds outside* the cell. Visible area = cell + 2 × border. The image fills the original cell, and the border surrounds it.

`fit_to_page` forces `BorderType::Outer` off (right-panel line 1833-1835) because the page is the boundary.

## The Dimension Rule — Two Variants in the Code

There is **no** single "unified" dimension rule. There are two related ones, and the code uses whichever fits.

### Four-branch rule (most sites)

```rust
let (full_w, full_h) = if force_original_orientation && crop_inverted {
    (oriented_h, oriented_w)         // 1. FOO + inverted: cell swapped from natural
} else if force_original_orientation {
    (oriented_w, oriented_h)         // 2. FOO: cell in natural orientation
} else if effective_will_rotate {   // 3. No FOO: rotated if user would see rotation
    (oriented_h, oriented_w)
} else {
    (oriented_w, oriented_h)         // 4. No FOO, no rotation: natural
};
```

Used in:
- `src/bin/studio/ui/right_panel.rs:1612-1620` (FOO toggle handler)
- `src/bin/studio/ui/right_panel.rs:2403-2411` (border type/width change handler)
- `src/bin/studio/ui/right_panel.rs:1225-1231` (crop enable handler)
- `src/bin/studio/ui/right_panel.rs:1379-1384` (crop editor init handler)
- `src/bin/studio/ui/modals.rs:780-786` (crop_editor_dimensions)

### Three-branch rule (size-change handlers)

```rust
let (full_w, full_h) = if force_original_orientation && crop_inverted {
    (oriented_h, oriented_w)
} else if will_rotate {
    (oriented_h, oriented_w)
} else {
    (oriented_w, oriented_h)
};
```

Used in:
- `src/bin/studio/app.rs:1131-1137` (`update_selected_queue_size`)
- `src/bin/studio/app.rs:1285-1291` (`update_selected_queue_size_idx`)

**Why it works without an explicit FOO branch:** these two handlers recompute `will_rotate` via `fitted_area_rotate > fitted_area_no_rotate` and feed it *natural-orientation* dimensions (`oriented_w, oriented_h`) into that test. When FOO is on, the natural-orientation box matches the source aspect, so `fitted_no_rot` always wins and `will_rotate` comes out `false`. The three-branch rule therefore yields `(oriented_w, oriented_h)` for FOO, which matches the four-branch rule's branch 2. If you change these handlers to also call this rule on a non-natural box, the test breaks — the FOO path needs the natural dimensions as input.

The 4-branch and 3-branch rules agree on every observable input today. The 4-branch is the safer one to copy into new sites; the 3-branch is a micro-optimization for two specific handlers.

## The Border Change Pipeline

When the user changes border type or width, the handler at `src/bin/studio/ui/right_panel.rs:2316-2478` runs in this order:

1. **Capture state and pre-compute the new default width** (line 2330-2338). The default (`1.0` pt or `2.835` mm — same value, different units) must be set *before* the crop-UV recalculation, because the UV step uses `border_width_pt` to size the visible area. If the user is enabling a border for the first time (None → Inner/Outer), this default kicks in.
2. **Compute `effective_will_rotate`** using the four-branch rule's inputs (line 2370-2396). `effective_will_rotate = if FOO { false } else if inverted { !will_rotate } else { will_rotate }`.
3. **Pick `(full_w, full_h)`** with the four-branch dimension rule (line 2403-2411).
4. **Apply border** to get the new visible area (line 2420-2429). Inner shrinks; outer expands.
5. **Compute `target_aspect = box_aspect / src_aspect`** (line 2431-2446). This is the ratio to reshape the existing crop rectangle into the new visible area.
6. **Apply changes** to `border_type`, `border_width_pt`, `border_color` and re-layout (line 2448-2477).

The crop UVs are *reshaped* (not recomputed) — same center, same area, but new width/height ratio. This is why the old crop rectangle's center is preserved.

## `queued_box_px` — Cell Dimensions for the Canvas and Processor

Defined in `src/bin/studio/app.rs:719-773`. The early-return path (line 720-722) is the dominant one: if the layout engine has already produced `placed_w_px` / `placed_h_px` for this image, those values are returned verbatim — including the FOO + crop_inverted swap. Everything downstream (canvas, processor) trusts these.

The fallback path (line 723-772) is exercised only when the layout engine hasn't run yet (e.g. mid-UI-update, or for images where `placed_*` is zero). It applies the rotation flag from `QueuedImage.rotation` (line 724-728) and then the border with a `crop_inverted && !force_original_orientation` swap-around (line 734-764). It does **not** handle `crop_inverted && force_original_orientation` separately, because the early return catches that case for layout-driven images.

The processor does not use `queued_box_px`; it uses `PagePlacement.placed_w_px` / `placed_h_px` directly (`src/processor.rs`). The layout engine bakes the FOO + crop_inverted swap into those values, so the processor also doesn't need to redo the swap.

## Layout Engine — `layout_queue` and `choose_orientation_for_flow_with_state`

`src/layout_engine.rs:373-470`. The FOO branch (line 392-414):

1. Returns `(to_px(natural_w), to_px(natural_h), false)` (after possibly scaling to fit the page — line 402-412).
2. The `false` is critical: even if the rotated box would have fit better, FOO forces the rotation flag off.

After `choose_orientation` returns, the three layout paths in `layout_queue` (flow line 158-177, center-to-page line 219-238, freehand line 277-296) all apply the same post-step:

```rust
let (box_w_px, box_h_px) = if item.force_original_orientation && item.crop_inverted {
    (box_h_px, box_w_px)   // layout engine also bakes the FOO+inverted swap
} else {
    (box_w_px, box_h_px)
};
```

The outer-border expansion is folded in **before** `choose_orientation` is called (line 152-156, 213-217, 271-275) so the engine sees the true final box size and can rotate to fit if it would.

## Crop UV Calculation

`src/bin/studio/utils.rs:37-113` (`calc_crop_uv`) takes `(box_w, box_h, src_w, src_h, rotate_cw, crop_enabled, stored_uv)`. It picks a centered crop that minimally fills the box at the source's rotated aspect:

- If `box_aspect > src_aspect_after_rotation`: crop top/bottom of rotated image.
- Otherwise: crop left/right of rotated image.

The `rotate_cw` parameter must be the *display* rotation — i.e. `will_rotate_for_uv` from the handlers, not the layout's `will_rotate`. The handlers consistently do:

```rust
let will_rotate_for_uv = if force_original_orientation {
    false
} else if crop_inverted {
    !will_rotate
} else {
    will_rotate
};
```

`calc_crop_uv` returns stored UVs transformed for rotation, so a stored portrait crop on a rotated display looks the same as a stored landscape crop on a non-rotated display (line 57-65).

## Canvas Preview Rotation

`src/bin/studio/ui/canvas.rs:359-373` re-derives rotation independently of the layout engine:

```rust
let should = should_rotate_for_full_page(Some((src_w, src_h)), w_px, h_px);
let should = if item.crop_inverted { !should } else { should };
if item.force_original_orientation { false } else { should }
```

This uses the **placed** pixel dimensions from `queued_box_px`, so it always matches what the layout engine decided (and what the processor will render). UV→screen-corner mapping for the 90° CW rotation is at `canvas.rs:387-401`.

## Common Failure Modes

The bugs that have been fixed in this area are all symptoms of one or both of:

1. **Forgetting the FOO branch in the dimension rule.** A handler uses the three-branch rule and gets the cell aspect wrong for FOO with non-natural print-size orientation. Mitigations: use the four-branch rule, or feed natural-orientation dimensions into the three-branch (the way the size-change handlers do).
2. **Using `will_rotate` directly when `effective_will_rotate` is required.** A handler computes the visible area or the box aspect using `will_rotate` and gets the wrong aspect for crop_inverted. The fix is `effective_will_rotate` (defined above).
3. **Setting `border_width_pt` after computing the visible area.** The new border width is used to size the visible area, so it must be assigned before the UV reshape step. (Right-panel border handler line 2333-2338 does this correctly.)

Historical bugs the code has fixed are listed in the inline `#[cfg(test)]` blocks in `src/layout_engine.rs:980-1058` and in the prior-art plan files in `.opencode/plans/`.

## Testing Checklist

When modifying crop/border code, verify all of these. The `force_original_orientation_*` tests in `src/layout_engine.rs:980-1058` cover most of the layout-engine cases; the others are best checked by hand.

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
- [ ] FOO (portrait source): image stays portrait, cell is portrait
- [ ] FOO (landscape source): image stays landscape, cell is landscape (`layout_engine.rs:1023`)
- [ ] FOO + crop_inverted (portrait source): cell swapped to landscape (`layout_engine.rs:980`)
- [ ] FOO + crop_inverted (landscape source): cell swapped to portrait (`layout_engine.rs:1040`)
- [ ] FOO + crop_inverted + inner border: aspect correct
- [ ] FOO + crop_inverted + outer border: aspect correct
- [ ] **ORDER MATTERS:** changing crop_inverted → FOO → border must give the same result as border → FOO → crop_inverted. This requires all three sites (dimension rule, `effective_will_rotate`, UV reshape) to use the same inputs.

## Code References

- **Layout engine**: `src/layout_engine.rs` — `layout_queue`, `choose_orientation_for_flow_with_state`, `should_rotate_for_full_page`
- **Crop enable handler**: `src/bin/studio/ui/right_panel.rs:1181-1311` (crop checkbox change)
- **FOO toggle handler**: `src/bin/studio/ui/right_panel.rs:1583-1660` (keep-original-orientation checkbox)
- **Border type/width change handler**: `src/bin/studio/ui/right_panel.rs:2316-2478`
- **Crop editor modal**: `src/bin/studio/ui/modals.rs:760-830` (dimensions helper), `:832-` (modal)
- **Canvas preview**: `src/bin/studio/ui/canvas.rs` — `draw_canvas` (rotation at `:359-373`, UV mapping at `:387-401`)
- **Box dimensions**: `src/bin/studio/app.rs` — `queued_box_px` at `:719-773`
- **Size change handlers**: `src/bin/studio/app.rs` — `update_selected_queue_size` at `:1058-1177`, `update_selected_queue_size_idx` at `:1191-`
- **Processor**: `src/processor.rs` — `process_composite_page` (consumes layout's `placed_w_px` / `placed_h_px`)
- **UV calculation**: `src/bin/studio/utils.rs:37-113` (`calc_crop_uv`)
