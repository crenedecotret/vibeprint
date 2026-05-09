# Plan: Fix Inverted Crop Toggle in Crop Editor

> **Status**: Plan Complete ✅  
> **Selected Approach**: Option 2 - Toggle Button + Preserve Crop Position  
> **Estimated Effort**: ~50 lines of code  
> **Files to Modify**: 1 (`src/bin/studio/ui/modals.rs`)

---

## Problem Analysis

### Current Behavior (Broken)
When the user right-clicks to toggle "inverted crop mode", the crop selector box (the white rectangle with resize handle in the crop editor) does not change its aspect ratio as expected. For example:
- A 6x4 image in normal mode should show a 6x4 selection box
- When inverted, it should show a 4x6 selection box (swapped aspect ratio)
- Currently, the box does not flip its dimensions consistently

### Root Cause Analysis

Looking at `src/bin/studio/ui/modals.rs` lines 829-838 and 1162-1186:

**Flow of execution:**
1. Lines 829-835: `final_w` and `final_h` are calculated based on `crop_editor_inverted` state at the START of each frame
2. Line 838: `crop_aspect = final_w / final_h` is calculated for the visual box
3. Lines 966-973: The crop box rectangle is rendered using `crop_editor_uv` coordinates
4. Lines 1164-1186: Right-click handler toggles `crop_editor_inverted` and recalculates UVs

**The Problem**: There's a TIMING issue with dimension swapping on toggle:

```rust
// Lines 1165-1175 - ON RIGHT CLICK
self.state.crop_editor_inverted = !self.state.crop_editor_inverted;  // Flag toggled
// ...
let (swapped_final_w, swapped_final_h) = (final_h, final_w);  // final_w/final_h were calculated 
                                                               // BEFORE the toggle!
```

When `crop_editor_inverted` was `false`:
- `final_w = target_w` (e.g., 4)
- `final_h = target_h` (e.g., 6)

After toggle to `true`:
- `swapped_final_w = final_h = 6`
- `swapped_final_h = final_w = 4`

But wait - after the toggle, on the NEXT frame, the code at line 831-835 will compute:
- `final_w = target_h = 6` (because inverted is now true)
- `final_h = target_w = 4`

So `swapped_final_w/final_h` is actually CORRECT for the new inverted state. However...

**The Real Issue**: The interaction detection for right-click may not be firing consistently due to interaction zone conflicts:

Lines 1046-1190 allocate multiple overlapping interaction zones:
1. Line 1047: Resize handle (`handle_interact_rect`) - `Sense::click_and_drag()`
2. Line 1124: Crop window drag (`crop_rect`) - `Sense::click_and_drag()`
3. Line 1164: Invert toggle (`image_rect`) - `Sense::click()`

The `image_rect` covers the ENTIRE image including the crop box. Since it's allocated LAST, and `clicked_by()` fires on mouse RELEASE, any drag motion that occurs before release can interfere with the click detection.

Additionally, there's NO guard to prevent the toggle when the user is actively dragging or resizing.

## Solution Design

### Root Cause Fix: Add Drag Guards

The immediate fix is to prevent the toggle when drag operations are active:

```rust
// Only process right-click if we're not actively dragging or resizing
if !self.state.crop_editor_dragging && 
   !self.state.crop_editor_resizing && 
   invert_response.clicked_by(egui::PointerButton::Secondary) {
    self.state.crop_editor_inverted = !self.state.crop_editor_inverted;
    // ... recalculate UVs
}
```

This ensures the toggle only fires on a "clean" right-click without any drag motion.

### Alternative: Add UI Toggle Button

A more reliable approach is to add a visible checkbox/button:

```rust
// Add before the image display area
ui.horizontal(|ui| {
    ui.checkbox(&mut self.state.crop_editor_inverted, "Inverted Crop");
    if self.state.crop_editor_inverted {
        ui.label("(4×6 selection on 6×4 output)");
    }
});
```

This eliminates the interaction conflict entirely and makes the feature discoverable.

### Secondary Fix: Preserve Crop Position on Toggle

Currently, toggling inversion calls `calc_crop_uv()` which calculates a new AUTO crop. Instead, we should transform the existing crop to match the new aspect ratio while keeping the same center position:

```rust
// When toggling, transform current crop to new aspect ratio
let (u0, v0, u1, v1) = self.state.crop_editor_uv;
let center_u = (u0 + u1) / 2.0;
let center_v = (v0 + v1) / 2.0;

// Calculate new dimensions based on swapped aspect
let new_aspect = if self.state.crop_editor_inverted {
    target_h / target_w  // Swapped
} else {
    target_w / target_h  // Normal
};

// Scale to match new aspect while preserving area or maintaining constraints
// Then recenter...
```

## Implementation Plan

### Recommended Approach: Combine C with A OR B

Since Option C (preserve crop position) improves the user experience regardless of the interaction method, it should be combined with either A or B:

---

### **Option 1: Fix Right-Click + Preserve Crop (A + C)** ⭐

Keep the right-click interaction but make it reliable and preserve the user's crop position.

**Changes to `src/bin/studio/ui/modals.rs`:**

**Step 1: Add drag guards (lines 1165-1186)**
```rust
// Only process right-click if we're not actively dragging or resizing
if !self.state.crop_editor_dragging && 
   !self.state.crop_editor_resizing && 
   invert_response.clicked_by(egui::PointerButton::Secondary) {
    
    // Toggle the inversion state
    self.state.crop_editor_inverted = !self.state.crop_editor_inverted;
    
    // Transform current crop to new aspect ratio instead of resetting
    let (u0, v0, u1, v1) = self.state.crop_editor_uv;
    let center_u = (u0 + u1) / 2.0;
    let center_v = (v0 + v1) / 2.0;
    let current_w = u1 - u0;
    let current_h = v1 - v0;
    
    // Calculate target aspect based on NEW inversion state
    // target_w and target_h are the original oriented dimensions
    let target_aspect = if self.state.crop_editor_inverted {
        target_h / target_w  // Swapped: 6/4 = 1.5
    } else {
        target_w / target_h  // Normal: 4/6 = 0.67
    };
    
    // Scale to match new aspect while keeping area roughly constant
    // or match one dimension and adjust the other
    let (new_w, new_h) = if current_w / current_h > target_aspect {
        // Current is wider relative to new target - constrain height
        let nh = current_h;
        let nw = (nh * target_aspect).min(1.0);
        (nw, nh)
    } else {
        // Current is taller - constrain width  
        let nw = current_w;
        let nh = (nw / target_aspect).min(1.0);
        (nw, nh)
    };
    
    // Recenter and clamp
    let new_u0 = (center_u - new_w / 2.0).max(0.0).min(1.0 - new_w);
    let new_v0 = (center_v - new_h / 2.0).max(0.0).min(1.0 - new_h);
    let new_u1 = new_u0 + new_w;
    let new_v1 = new_v0 + new_h;
    
    self.state.crop_editor_uv = (new_u0, new_v0, new_u1, new_v1);
    
    // Update defaults for zoom consistency
    self.state.crop_editor_default_w = new_w;
    self.state.crop_editor_default_h = new_h;
    self.state.crop_editor_center = (center_u, center_v);
}
```

---

### **Option 2: Add Toggle Button + Preserve Crop (B + C)** ⭐

Add a visible checkbox for inversion and preserve crop position when toggled.

**Changes to `src/bin/studio/ui/modals.rs`:**

**Step 1: Add checkbox UI (around line 936, before image display)**
```rust
// Add toggle control above image
ui.horizontal(|ui| {
    let prev_inverted = self.state.crop_editor_inverted;
    let response = ui.checkbox(&mut self.state.crop_editor_inverted, "Inverted Crop");
    
    if self.state.crop_editor_inverted {
        ui.label(format!("({:.1}×{:.1} selection)", target_h, target_w));
    } else {
        ui.label(format!("({:.1}×{:.1} selection)", target_w, target_h));
    }
    
    // Handle toggle - use same transform logic as Option 1
    if response.changed() && prev_inverted != self.state.crop_editor_inverted {
        // [Insert same transform logic from Option 1]
    }
});
ui.add_space(8.0);
```

**Step 2: Keep right-click with guards as secondary (optional)**
```rust
// Optional: keep right-click as shortcut, with same guards
if !self.state.crop_editor_dragging && 
   !self.state.crop_editor_resizing && 
   invert_response.clicked_by(egui::PointerButton::Secondary) {
    self.state.crop_editor_inverted = !self.state.crop_editor_inverted;
    // [Insert same transform logic]
}
```

---

### Comparison: Option 1 vs Option 2

| Factor | Option 1 (A+C) | Option 2 (B+C) ✅ **SELECTED** |
|--------|----------------|-------------------------------|
| **Discoverability** | Low - right-click is hidden | High - visible checkbox |
| **Interaction Conflicts** | Fixed by guards | Eliminated entirely |
| **Implementation Complexity** | Lower | Slightly higher (adds UI) |
| **User Experience** | Good - preserves crop | **Better** - visible + preserves crop |
| **Code Changes** | ~30 lines | ~50 lines |

**Selected**: Option 2 (B+C) provides the best user experience - the feature becomes discoverable, eliminates all interaction conflicts, and preserves the user's crop work when toggling.

---

## Detailed Transform Logic

The key to Option C is transforming the crop instead of resetting. Here's the detailed logic:

### Input:
- Current UVs: `(u0, v0, u1, v1)`
- Current center: `(center_u, center_v)`
- Current dimensions: `current_w = u1 - u0`, `current_h = v1 - v0`
- Target dimensions from layout: `target_w`, `target_h` (e.g., 4×6)
- New inversion state: `crop_editor_inverted`

### Calculation:
1. Determine target aspect based on NEW state:
   - Normal: `target_aspect = target_w / target_h` (4/6 = 0.667)
   - Inverted: `target_aspect = target_h / target_w` (6/4 = 1.5)

2. Scale current crop to match new aspect:
   - If `current_w/current_h > target_aspect`: constrain by height
   - Else: constrain by width

3. Recenter at original center, clamp to [0, 1] bounds

4. Update defaults so zoom works correctly

### Why This Works:
- User's crop center stays in the same visual location
- The crop flips aspect ratio immediately
- Zoom/scaling continues to work correctly
- No jarring reset to auto-crop position

## Testing Checklist

### Basic Functionality
- [ ] Right-click while NOT dragging toggles inversion correctly
- [ ] Right-click while dragging does NOT toggle (prevents accidental toggles)
- [ ] Crop selector box immediately flips aspect ratio when toggled (e.g., 6×4 becomes 4×6)
- [ ] Visual overlay updates correctly (dimmed area outside crop)

### Crop Behavior
- [ ] Crop center position is preserved when possible after toggle
- [ ] Crop stays within image bounds after flip
- [ ] Zoom level remains consistent after toggle
- [ ] Resize handle works correctly in both orientations

### Data Persistence
- [ ] Apply button saves the correct inverted state
- [ ] Apply button saves correct UV coordinates
- [ ] Cancel button discards changes including inversion toggle
- [ ] Reset button works correctly in both normal and inverted modes

### Edge Cases
- [ ] Toggle works correctly at various zoom levels
- [ ] Toggle works correctly when crop is at image edges
- [ ] Toggle works correctly with different image orientations (portrait/landscape)
- [ ] Toggle works correctly with inner borders enabled

## Files to Modify

1. `src/bin/studio/ui/modals.rs` - Lines 1046-1186 (crop editor interaction logic)

## Notes

- The current plan document at `.opencode/plans/inverse_crop_selection.md` describes the original feature implementation
- This new plan focuses on fixing the interaction bugs that prevent reliable toggling
- The fix should be minimal and not change the overall architecture
- Consider user feedback: right-click may not be discoverable; a visible toggle button might be better long-term
