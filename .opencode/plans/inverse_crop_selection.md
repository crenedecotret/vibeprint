# Plan: Inverse Crop Selection Direction

## Status: COMPLETE ✓

## Overview

Add the ability to invert the crop selection box direction in the crop editor. This allows selecting a section with swapped dimensions (e.g., 6×4 instead of 4×6) that will then be rotated to fit the original canvas cell.

**Key Point**: The canvas cell size remains unchanged. Only the crop selection box aspect ratio is inverted. For a 4×6 cell, the user can select a 6×4 area that will be rotated to fit the 4×6 output.

## Implementation Summary

### Files Modified

1. **`src/layout_engine.rs`**:
   - Added `crop_inverted: bool` field to `QueuedImage` struct (line 42-64)
   - Updated test helpers to include `crop_inverted: false`

2. **`src/bin/studio/ui/modals.rs`**:
   - Modified target dimension calculation to swap `(w, h)` when `crop_editor_inverted` is true
   - Added right-click handler to toggle inversion and reset crop
   - Updated instruction text to show "(Inverted)" status
   - Save `crop_editor_inverted` state when Apply is clicked

3. **`src/bin/studio/ui/right_panel.rs`**:
   - Initialize `crop_editor_inverted` from queue item when opening crop editor
   - Reset `crop_inverted` when enabling/disabling crop
   - Fixed auto-UV calculation for borders: calculate `will_rotate` AFTER border adjustment so UVs match the adjusted dimensions

4. **`src/bin/studio/ui/canvas.rs`**:
   - Calculate `will_rotate_for_display` based on `crop_inverted`

5. **`src/bin/studio/app.rs`**:
   - Pass `crop_inverted` from queue item to processor
   - Flip the `will_rotate` determination when `crop_inverted` is true
   - Initialize `crop_inverted = false` when adding to queue

6. **`src/bin/studio/utils.rs`**:
   - No changes needed - `calc_crop_uv_for_processor` returns stored UVs as-is

## Bug Fixes for Border Compatibility

The main issue was that `will_rotate` was calculated from cell dimensions BEFORE border adjustment, but UVs were calculated with border-adjusted dimensions AFTER. This caused distortion when using inverted crop with borders.

**Fix**: Calculate `will_rotate` AFTER border adjustment so the UV calculation uses consistent dimensions with the rotation decision.

### Changed Logic

**Before** (buggy):
```
will_rotate = calculate from cell dimensions
calc_w, calc_h = swap based on will_rotate
calc_w, calc_h = adjust for inner border
calc_crop_uv(calc_w, calc_h, will_rotate)  // MISMATCH!
```

**After** (fixed):
```
calc_w, calc_h = swap based on initial will_rotate
calc_w, calc_h = adjust for inner border  
will_rotate = calculate from adjusted dimensions  // RECALCULATE!
calc_crop_uv(crop_w, crop_h, will_rotate)  // CONSISTENT
```

## Key Design Decisions

1. **`crop_inverted` on `QueuedImage`**: Persists the inversion state per queue item
2. **UV calculation with `rotate_cw=false`**: UVs are calculated for the original image; rotation is handled separately
3. **`will_rotate` calculation AFTER border adjustment**: Ensures UV calculation uses consistent dimensions
4. **Both processor and canvas use same logic**: Ensures consistency between output and display