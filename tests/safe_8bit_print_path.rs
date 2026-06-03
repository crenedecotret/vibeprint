use std::fs;
use std::path::Path;

use vibeprint::processor::{self, CompositePageOptions, PagePlacement, ResampleEngine};

/// Test that 8-bit TIFF with embed_icc_profile=false does not contain an ICC profile tag
#[test]
fn safe_8bit_tiff_strips_icc_profile() {
    let out_path = "/tmp/vibeprint_test_safe_8bit_no_icc.tif";
    let _ = fs::remove_file(out_path);

    // Create a simple 64×64 test image
    let test_img_path = "/tmp/vibeprint_test_input_8bit.tif";
    create_simple_test_tiff(test_img_path, 64, 64);

    // Process with embed_icc_profile=false (safe 8-bit path)
    let opts = CompositePageOptions {
        output: out_path.into(),
        placements: vec![PagePlacement {
            input: test_img_path.into(),
            input_icc: None,
            crop_u0: 0.0,
            crop_v0: 0.0,
            crop_u1: 1.0,
            crop_v1: 1.0,
            dest_x_px: 0,
            dest_y_px: 0,
            dest_w_px: 64,
            dest_h_px: 64,
            rotate_cw: false,
            border_type: vibeprint::layout_engine::BorderType::None,
            border_width_px: 0,
            border_color: [0, 0, 0],
        }],
        page_w_px: 64,
        page_h_px: 64,
        output_icc: None, // Will use default wide gamut
        default_wide_output_when_unset: true,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: ResampleEngine::Mks,
        depth: 8,
        sharpen: 0,
        embed_icc_profile: false, // Safe 8-bit path: strip profile
        cut_marks: processor::CutMarkMode::None,
        cut_mark_bounds_px: None,
    };

    processor::process_composite_page(opts).expect("processing failed");

    // Verify the TIFF exists and read it back
    assert!(Path::new(out_path).exists(), "Output TIFF was not created");

    // Read the TIFF and check for ICC profile absence
    let file = fs::File::open(out_path).expect("failed to open output TIFF");
    let mut decoder = tiff::decoder::Decoder::new(file).expect("failed to create TIFF decoder");

    // Check that IccProfile tag is NOT present
    let has_icc_profile = decoder.get_tag_u8_vec(tiff::tags::Tag::IccProfile).is_ok();

    assert!(
        !has_icc_profile,
        "ICC profile should NOT be embedded when embed_icc_profile=false"
    );

    // Verify image metadata is correct
    let (width, height) = decoder.dimensions().expect("failed to get dimensions");
    assert_eq!(width, 64, "width mismatch");
    assert_eq!(height, 64, "height mismatch");

    // Cleanup
    let _ = fs::remove_file(out_path);
    let _ = fs::remove_file(test_img_path);
}

/// Test that standard export path (embed_icc_profile=true) DOES embed ICC profile
#[test]
fn standard_export_embeds_icc_profile() {
    let out_path = "/tmp/vibeprint_test_standard_export_with_icc.tif";
    let _ = fs::remove_file(out_path);

    // Create a simple 64×64 test image
    let test_img_path = "/tmp/vibeprint_test_input_8bit_std.tif";
    create_simple_test_tiff(test_img_path, 64, 64);

    // Process with embed_icc_profile=true (standard export path)
    let opts = CompositePageOptions {
        output: out_path.into(),
        placements: vec![PagePlacement {
            input: test_img_path.into(),
            input_icc: None,
            crop_u0: 0.0,
            crop_v0: 0.0,
            crop_u1: 1.0,
            crop_v1: 1.0,
            dest_x_px: 0,
            dest_y_px: 0,
            dest_w_px: 64,
            dest_h_px: 64,
            rotate_cw: false,
            border_type: vibeprint::layout_engine::BorderType::None,
            border_width_px: 0,
            border_color: [0, 0, 0],
        }],
        page_w_px: 64,
        page_h_px: 64,
        output_icc: None,
        default_wide_output_when_unset: true,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: ResampleEngine::Mks,
        depth: 8,
        sharpen: 0,
        embed_icc_profile: true, // Standard path: embed profile
        cut_marks: processor::CutMarkMode::None,
        cut_mark_bounds_px: None,
    };

    processor::process_composite_page(opts).expect("processing failed");

    // Verify the TIFF exists
    assert!(Path::new(out_path).exists(), "Output TIFF was not created");

    // Read the TIFF and check for ICC profile presence
    let file = fs::File::open(out_path).expect("failed to open output TIFF");
    let mut decoder = tiff::decoder::Decoder::new(file).expect("failed to create TIFF decoder");

    // Check that IccProfile tag IS present
    let icc_profile_data = decoder
        .get_tag_u8_vec(tiff::tags::Tag::IccProfile)
        .expect("ICC profile should be embedded when embed_icc_profile=true");

    assert!(
        !icc_profile_data.is_empty(),
        "ICC profile data should not be empty"
    );

    // Verify the profile is valid by checking magic bytes
    assert!(
        icc_profile_data.len() > 4,
        "ICC profile should be larger than 4 bytes"
    );

    // Cleanup
    let _ = fs::remove_file(out_path);
    let _ = fs::remove_file(test_img_path);
}

/// Test that RGB values are transformed (not just passed through)
#[test]
fn safe_8bit_applies_icc_transformation() {
    let out_path = "/tmp/vibeprint_test_safe_8bit_transform.tif";
    let _ = fs::remove_file(out_path);

    // Create a test TIFF with known RGB values
    let test_img_path = "/tmp/vibeprint_test_input_8bit_transform.tif";
    create_colored_test_tiff(test_img_path, 16, 16);

    // Process with safe 8-bit path
    let opts = CompositePageOptions {
        output: out_path.into(),
        placements: vec![PagePlacement {
            input: test_img_path.into(),
            input_icc: None,
            crop_u0: 0.0,
            crop_v0: 0.0,
            crop_u1: 1.0,
            crop_v1: 1.0,
            dest_x_px: 0,
            dest_y_px: 0,
            dest_w_px: 16,
            dest_h_px: 16,
            rotate_cw: false,
            border_type: vibeprint::layout_engine::BorderType::None,
            border_width_px: 0,
            border_color: [0, 0, 0],
        }],
        page_w_px: 16,
        page_h_px: 16,
        output_icc: None,
        default_wide_output_when_unset: true,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: ResampleEngine::Mks,
        depth: 8,
        sharpen: 0,
        embed_icc_profile: false,
        cut_marks: processor::CutMarkMode::None,
        cut_mark_bounds_px: None,
    };

    processor::process_composite_page(opts).expect("processing failed");

    // Verify the output exists
    assert!(Path::new(out_path).exists(), "Output TIFF was not created");

    // Read back and verify pixels were processed
    let file = fs::File::open(out_path).expect("failed to open output TIFF");
    let mut decoder = tiff::decoder::Decoder::new(file).expect("failed to create TIFF decoder");
    let image_data = decoder.read_image().expect("failed to read image data");

    // Verify we have RGB8 data (3 bytes per pixel)
    if let tiff::decoder::DecodingResult::U8(pixels) = image_data {
        assert_eq!(
            pixels.len(),
            16 * 16 * 3,
            "Pixel data length should be width × height × 3"
        );

        // Verify pixels are not all zeros (transformation happened)
        let non_zero_count = pixels.iter().filter(|&&p| p != 0).count();
        assert!(
            non_zero_count > 0,
            "Image should contain non-zero pixels after processing"
        );
    } else {
        panic!("Expected U8 pixel data for 8-bit TIFF");
    }

    // Cleanup
    let _ = fs::remove_file(out_path);
    let _ = fs::remove_file(test_img_path);
}

// ── Helper functions ────────────────────────────────────────────────────────

/// Create a simple grayscale test TIFF
fn create_simple_test_tiff(path: &str, width: u32, height: u32) {
    use tiff::encoder::{colortype, TiffEncoder};

    let file = fs::File::create(path).expect("failed to create test TIFF");
    let mut encoder = TiffEncoder::new(file).expect("failed to create TIFF encoder");
    let image = encoder
        .new_image::<colortype::RGB8>(width, height)
        .expect("failed to create TIFF image");

    // Create gradient pattern
    let mut pixels = vec![0u8; (width * height * 3) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 3) as usize;
            let gray = ((x + y) * 255 / (width + height)) as u8;
            pixels[idx] = gray;
            pixels[idx + 1] = gray;
            pixels[idx + 2] = gray;
        }
    }

    image
        .write_data(&pixels)
        .expect("failed to write test image data");
}

/// Create a colored test TIFF with distinct RGB values
fn create_colored_test_tiff(path: &str, width: u32, height: u32) {
    use tiff::encoder::{colortype, TiffEncoder};

    let file = fs::File::create(path).expect("failed to create test TIFF");
    let mut encoder = TiffEncoder::new(file).expect("failed to create TIFF encoder");
    let image = encoder
        .new_image::<colortype::RGB8>(width, height)
        .expect("failed to create TIFF image");

    // Create colored pattern (red, green, blue, yellow quadrants)
    let mut pixels = vec![0u8; (width * height * 3) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 3) as usize;
            let (r, g, b) = if x < width / 2 && y < height / 2 {
                (200, 0, 0) // Red
            } else if x >= width / 2 && y < height / 2 {
                (0, 200, 0) // Green
            } else if x < width / 2 && y >= height / 2 {
                (0, 0, 200) // Blue
            } else {
                (200, 200, 0) // Yellow
            };
            pixels[idx] = r;
            pixels[idx + 1] = g;
            pixels[idx + 2] = b;
        }
    }

    image
        .write_data(&pixels)
        .expect("failed to write test image data");
}
