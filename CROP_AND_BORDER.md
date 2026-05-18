# Crop Editor and Border Functionality Documentation

## Overview

This document describes how the crop editor, inner borders, and outer borders work together in VibePrint Studio, with special attention to the interaction with crop inversion.

## Key Concepts

### Crop Inversion

**Purpose:** Crop inversion allows the user to flip the rotation decision. If the layout engine would normally rotate an image to fit the page better, inversion prevents that rotation (and vice versa).

**How it works:**
- Without inversion: Image rotates if it fits the page better when rotated
- With inversion: Image does NOT rotate if it would normally rotate, and rotates if it wouldn't normally rotate
- Inversion is stored in `QueuedImage.crop_inverted` boolean field

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

## Code Locations and Logic Flow

### 1. Crop Enable (right_panel.rs:660-755)

**When triggered:** User clicks "Crop Image" checkbox to enable crop

**Flow:**
1. Calculate `will_rotate` based on oriented dimensions (line 680)
2. Set `(calc_w, calc_h)` based on `will_rotate` (lines 683-687)
3. **For Inner Border with Inversion:** 
   - Swap `calc_w, calc_h` before shrinking (line 717-721)
   - Shrink by border amount (lines 722-725)
   - Swap back (lines 726-730)
4. Calculate `will_rotate_for_uv` accounting for inversion (lines 737-741)
5. Call `calc_crop_uv(calc_w, calc_h, ...)` to get UVs (lines 742-750)

**Key Point:** The crop enable path correctly accounts for inversion in both:
- The dimension calculations (swap before/after border adjustment)
- The UV calculation (`will_rotate_for_uv`)

### 2. Border Width/Type Change (right_panel.rs:1120-1240)

**When triggered:** User changes border type (None/Inner/Outer) or border width

**Flow:**
1. Calculate `will_rotate` based on oriented dimensions (line 1161)
2. **CRITICAL:** Calculate `effective_will_rotate` accounting for inversion (line 1162)
3. Set `(full_w, full_h)` based on `effective_will_rotate` (lines 1163-1166)
4. Apply border to get `new_vis_w, new_vis_h` (lines 1175-1187)
5. Calculate `target_aspect = (new_vis_w / new_vis_h) / src_aspect` (lines 1194-1196)
6. Recalculate UVs preserving center point and area (lines 1209-1225)

**Key Point:** The border change path now uses `effective_will_rotate` to match the crop enable logic. Previously it used `will_rotate` directly, causing distortion for inverted crops.

### 3. Crop Editor Modal (modals.rs:719-1399)

**When triggered:** User clicks "Edit" button to open crop editor

**Flow:**
1. Load stored UVs or calculate auto-crop UVs
2. Calculate `will_rotate` for the oriented box (line 808)
3. Adjust display rect for borders
4. Handle user interactions (drag, resize)
5. On "Apply": Save UVs back to queue item

**Key Point:** The crop editor displays the crop selection based on the visible area (after border adjustment), so it correctly shows the aspect ratio that will be rendered.

### 4. Canvas Preview (canvas.rs:97-233)

**When triggered:** Main window renders the print preview

**Flow:**
1. Get box dimensions from `queued_box_px()` (line 97)
2. Calculate `display_rect` accounting for border type (lines 116-137)
3. Calculate `will_rotate_for_display` accounting for inversion (lines 162-171)
4. Apply UVs to the mesh, rotating if needed (lines 185-232)

**Key Point:** The canvas uses `queued_box_px()` which returns different dimensions based on border type:
- Inner border: Returns original cell size (border eats inside)
- Outer border: Returns expanded cell size (border adds outside)

### 5. queued_box_px (app.rs:488-526)

**Purpose:** Calculate pixel dimensions of a queue item's placement box

**Logic:**
```rust
if Outer Border:
    if crop_inverted:
        swap(w, h)
    expand by 2×border
    if crop_inverted:
        swap_back(w, h)
elif Inner Border:
    if crop_inverted:
        swap(w, h)
    shrink by 2×border
    if crop_inverted:
        swap_back(w, h)
else:
    return (w, h)
```

**Key Point:** This ensures the processor and canvas preview use dimensions that match the actual visible area after accounting for borders and inversion.

### 6. Processor (processor.rs:214-248)

**When triggered:** Export/print generates the final output

**Flow:**
1. Get dimensions from `queued_box_px()` via `PagePlacement`
2. Calculate `scale_dest_w, scale_dest_h` by subtracting 2×border for inner borders
3. If crop enabled: Stretch cropped image to fill inner area
4. If no crop: Aspect-fit image within inner area

**Key Point:** For inner borders, the processor shrinks the destination by the border amount, then stretches the cropped image to fill exactly.

## Aspect Ratio Calculations

### For Non-Inverted Crops

**Page:** 6×4 inches (landscape)
**Image:** 3000×2000 (landscape, 3:2)
**Inner Border:** 0.5 inches

1. Oriented dims: 4×6 (portrait, image is landscape)
2. `will_rotate = true` (fits better rotated)
3. `calc_w, calc_h = 6, 4` (swapped for rotation)
4. Inner border: shrink 6×4 by 1 inch → 5×3
5. Aspect ratio: 5/3 ≈ 1.67
6. UVs calculated for aspect 1.67

### For Inverted Crops

Same setup but inverted:

1. Oriented dims: 4×6
2. `will_rotate = true`
3. `effective_will_rotate = false` (inversion flip)
4. `calc_w, calc_h = 4, 6` (NOT swapped)
5. Inner border with inversion: swap 4×6 → 6×4, shrink → 5×3, swap back → 3×5
6. Aspect ratio: 3/5 = 0.6
7. UVs calculated for aspect 0.6

## Common Issues and Solutions

### Issue: Aspect distortion when changing inner border width

**Cause:** Border change recalculation used `will_rotate` directly instead of `effective_will_rotate`

**Fix:** Line 1162 in right_panel.rs:
```rust
let effective_will_rotate = if crop_inverted { !will_rotate } else { will_rotate };
```

### Issue: Canvas preview doesn't match crop editor

**Cause:** `queued_box_px` didn't swap dimensions for inner borders with inversion

**Fix:** Added inner border swap logic to `queued_box_px` (app.rs:518-520)

### Issue: Crop editor shows wrong selection after border change

**Cause:** UVs were recalculated with wrong aspect ratio due to `will_rotate` mismatch

**Fix:** Use `effective_will_rotate` in border change handler (right_panel.rs:1162)

### Issue: Minor distortion when switching from "None" to "Inner" border

**Cause:** When switching border types, the UI showed 0mm. The UV recalculation used 0 for the border width, but then the code set the actual border to a default value. The UV adjustment happened with the wrong border size.

**Fix:** 
1. Changed default border width on first enable to a small fixed value (1mm or 1pt) instead of calculated value
2. **Critical:** Moved the border width assignment to BEFORE the crop UV recalculation

**Code:** `src/bin/studio/ui/right_panel.rs:1127-1134`
```rust
// IMPORTANT: Set default border width BEFORE crop calculation
// so UV recalculation uses the correct border size
if border_enabled {
    border_width_pt = if use_metric { 2.835 } else { 1.0 };
}
```

**Key insight:** The order of operations matters. Border width must be set before calculating the visible area and adjusting crop UVs.

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

## Key Takeaways

1. **Inversion affects display rotation, not UV calculation directly**
2. **Always use `effective_will_rotate` when dimensions affect the visible area**
3. **Inner borders shrink the visible area, outer borders expand the cell**
4. **Crop UVs are calculated once, then adjusted (not recalculated) for border changes**
5. **The three paths (crop enable, border change, canvas preview) must use consistent logic**

## Code References

- **Crop enable:** `src/bin/studio/ui/right_panel.rs:660-755`
- **Border change:** `src/bin/studio/ui/right_panel.rs:1120-1240`
- **Crop editor:** `src/bin/studio/ui/modals.rs:719-1399`
- **Canvas preview:** `src/bin/studio/ui/canvas.rs:97-233`
- **Box dimensions:** `src/bin/studio/app.rs:488-526`
- **Processor:** `src/processor.rs:214-248`
- **UV calculation:** `src/bin/studio/utils.rs:37-113`
