# Plan: Inverse Crop Selection Direction

## Overview

Add the ability to invert the crop selection box direction in the crop editor. For example, a 4×6 print size could be changed to 6×4 orientation, allowing the user to select a section that will be rotated to fit the opposite orientation.

## Current Behavior

The crop editor (`show_crop_editor` in `src/bin/studio/ui/modals.rs:718-1293`) currently:

1. Determines the target aspect ratio based on the print size and image orientation
2. Calculates whether rotation will occur via `will_rotate` logic (lines 806-812)
3. The crop box maintains the same aspect ratio as the final output

The key orientation logic is in `modals.rs:786-812`:
```rust
let src_landscape = src_w > src_h;
let (oriented_w, oriented_h) = if src_landscape {
    (h_in, w_in) // Swap for landscape images
} else {
    (w_in, h_in) // Keep as-is for portrait images
};
```

## Proposed Change

When the user right-clicks on the crop editor window, the target dimensions should be swapped, effectively inverting the selection direction.

### User Interaction

| Action | Behavior |
|--------|----------|
| Left-drag on crop overlay | Move crop box |
| Drag on resize handle | Resize crop box |
| Scroll wheel | Zoom |
| Right-click on image area | **Toggle inverse orientation** |

### Implementation Details

#### 1. State Changes (`src/bin/studio/types.rs`)

Add a new state field to `AppState`:
```rust
pub crop_editor_inverted: bool,
```

Initialize to `false` in `AppState::new()`:
```rust
crop_editor_inverted: false,
```

#### 2. Crop Editor Logic (`src/bin/studio/ui/modals.rs`)

In `show_crop_editor`:

**a. Right-click detection (after line 1158):**
```rust
// Right-click to toggle inverted orientation
let right_click_sense = Sense::click();
let right_click_response = ui.allocate_rect(image_rect, right_click_sense);
if right_click_response.clicked_by(egui::PointerButton::Secondary) {
    self.state.crop_editor_inverted = !self.state.crop_editor_inverted;
    // Recalculate and reset to auto-crop with new orientation
    let (new_w, new_h) = self.calculate_target_dimensions_for_inverted();
    // ... trigger recalculation similar to Reset button
}
```

**b. Modified target dimension calculation:**

Currently (lines 765-828):
```rust
let (w_in, h_in) = if q_fit_to_page {
    (ia_w_in, ia_h_in)
} else {
    q_size.as_inches()
};
```

With inversion support:
```rust
let (w_in, h_in) = if q_fit_to_page {
    (ia_w_in, ia_h_in)
} else {
    let (w, h) = q_size.as_inches();
    if self.state.crop_editor_inverted {
        (h, w)  // Swap dimensions
    } else {
        (w, h)
    }
};
```

**c. Visual indicator:**
- Add text overlay showing "Inverted" when `crop_editor_inverted` is true
- Could be shown in the instruction area alongside the existing text

#### 3. Reset Button Behavior

When the "Reset" button is clicked, the auto-calculated crop should respect the current `crop_editor_inverted` state:

Lines 1247-1251 currently:
```rust
let (calc_w, calc_h) = if will_rotate {
    (target_h, target_w)
} else {
    (target_w, target_h)
};
```

This should already work correctly because `target_w`/`target_h` are calculated from the potentially inverted dimensions.

## Files to Modify

1. **`src/bin/studio/types.rs`**
   - Add `crop_editor_inverted: bool` field to `AppState`
   - Initialize in `AppState::new()`

2. **`src/bin/studio/ui/modals.rs`**
   - Add right-click handler in `show_crop_editor`
   - Modify target dimension calculation to respect `crop_editor_inverted`
   - Add visual indicator for inverted state
   - Update instruction text dynamically

## Alternative Approaches Considered

### Option A: Toggle Button (Not Recommended)
A dedicated "Swap Aspect" button could be added to the button row. However, this takes up UI space and is less discoverable.

### Option B: Keyboard Shortcut
Ctrl+R or similar could toggle inversion. This is less intuitive than right-click.

### Option C: Double-click Crop Box
Double-clicking the crop box to invert. This conflicts with potential future features and is less discoverable.

**Recommendation: Right-click** as proposed above is the most intuitive and discoverable option.

## Testing Considerations

1. Verify that right-click toggles the inverted state
2. Verify that the crop box aspect ratio changes correctly after toggle
3. Verify that applying the crop works with inverted orientation
4. Verify that Reset button works correctly with inverted state
5. Verify that the existing auto-rotation logic (`will_rotate`) still functions correctly

## Edge Cases

1. **Fit to Page mode**: Inverting dimensions may not have a visible effect when fitting to page, since the page dimensions are fixed. Consider disabling or ignoring inversion in this mode.

2. **Custom sizes**: Inversion should work the same way as predefined sizes.

3. **Images already rotated**: When `will_rotate` is true (layout engine will rotate the image), the inversion should still work but may result in counterintuitive behavior. Document this limitation or prevent inversion in this case.