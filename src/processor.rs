use std::{fs::File, io::BufReader, path::Path};

use anyhow::{bail, Context, Result};
use image::{
    codecs::png::PngDecoder, imageops::FilterType, DynamicImage, ImageBuffer, ImageDecoder, Rgb,
};

type Rgb16Image = ImageBuffer<Rgb<u16>, Vec<u16>>;
type Rgb8Image = ImageBuffer<Rgb<u8>, Vec<u8>>;

enum LoadedImage {
    Rgb8(Rgb8Image),
    Rgb16(Rgb16Image),
}

fn load_output_profile(
    output_icc: Option<&std::path::PathBuf>,
    passthrough_icc: Option<&[u8]>,
    default_wide_when_unset: bool,
) -> Result<(lcms2::Profile, Vec<u8>, String)> {
    match output_icc {
        Some(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read output ICC: {}", path.display()))?;
            let profile = lcms2::Profile::new_icc(&bytes)
                .with_context(|| format!("failed to load output ICC: {}", path.display()))?;
            let fname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            Ok((profile, bytes, fname))
        }
        None => {
            if default_wide_when_unset {
                let profile = load_prophoto_working_profile()?;
                let bytes = profile
                    .icc()
                    .context("failed to serialize ProPhoto working profile")?;
                return Ok((profile, bytes, "ProPhoto RGB D50 (Linear)".to_string()));
            }
            match passthrough_icc {
                Some(icc_bytes) => {
                    let profile = lcms2::Profile::new_icc(icc_bytes)
                        .context("failed to load passthrough ICC profile")?;
                    Ok((
                        profile,
                        icc_bytes.to_vec(),
                        "embedded (passthrough)".to_string(),
                    ))
                }
                None => {
                    let profile = lcms2::Profile::new_srgb();
                    let bytes = profile.icc().context("failed to serialize sRGB profile")?;
                    Ok((profile, bytes, "sRGB (passthrough)".to_string()))
                }
            }
        }
    }
}

fn load_input_profile(
    input_icc: Option<&std::path::PathBuf>,
    embedded_icc: Option<&[u8]>,
) -> Result<lcms2::Profile> {
    match (input_icc, embedded_icc) {
        (Some(path), _) => lcms2::Profile::new_file(path)
            .with_context(|| format!("failed to load input ICC: {}", path.display())),
        (None, Some(icc)) => {
            lcms2::Profile::new_icc(icc).context("failed to load embedded ICC profile")
        }
        (None, None) => Ok(lcms2::Profile::new_srgb()),
    }
}

fn load_prophoto_working_profile() -> Result<lcms2::Profile> {
    let wp = lcms2::CIExyY {
        x: 0.3457,
        y: 0.3585,
        Y: 1.0,
    };
    let primaries = lcms2::CIExyYTRIPLE {
        Red: lcms2::CIExyY {
            x: 0.7347,
            y: 0.2653,
            Y: 1.0,
        },
        Green: lcms2::CIExyY {
            x: 0.1596,
            y: 0.8404,
            Y: 1.0,
        },
        Blue: lcms2::CIExyY {
            x: 0.0366,
            y: 0.0001,
            Y: 1.0,
        },
    };

    let curve = lcms2::ToneCurve::new(1.0);
    let mut profile = lcms2::Profile::new_rgb(&wp, &primaries, &[&curve, &curve, &curve])
        .context("failed to create ProPhoto RGB working profile")?;

    let mut desc = lcms2::MLU::new(1);
    desc.set_text_ascii("ProPhoto RGB D50 (Linear)", lcms2::Locale::new("en_US"));
    if !profile.write_tag(
        lcms2::TagSignature::ProfileDescriptionTag,
        lcms2::Tag::MLU(&desc),
    ) {
        bail!("failed to set ProPhoto profile description tag");
    }

    let mut copyright = lcms2::MLU::new(1);
    copyright.set_text_ascii("No copyright, use freely", lcms2::Locale::new("en_US"));
    if !profile.write_tag(
        lcms2::TagSignature::CopyrightTag,
        lcms2::Tag::MLU(&copyright),
    ) {
        bail!("failed to set ProPhoto profile copyright tag");
    }

    Ok(profile)
}

pub fn process_composite_page(opts: CompositePageOptions) -> Result<()> {
    if opts.target_dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let (output_profile, output_icc_bytes, icc_filename) = load_output_profile(
        opts.output_icc.as_ref(),
        None,
        opts.default_wide_output_when_unset,
    )?;
    let working_profile = load_prophoto_working_profile()?;

    let intent_name = match opts.intent {
        lcms2::Intent::Perceptual => "Perceptual",
        lcms2::Intent::RelativeColorimetric => "Relative Colorimetric",
        lcms2::Intent::Saturation => "Saturation",
        lcms2::Intent::AbsoluteColorimetric => "Absolute Colorimetric",
        _ => "Unknown",
    };
    let depth_label = if opts.depth == 8 {
        "8-bit (dithered)"
    } else {
        "16-bit"
    };
    let sharpen_label = if opts.sharpen == 0 {
        "Off".to_string()
    } else {
        opts.sharpen.to_string()
    };
    let description = format!(
        "vibeprint | Engine: {} | Intent: {} | BPC: {} | DPI: {} | Sharpen: {} | Depth: {} | Output ICC: {}",
        opts.engine.display_name(),
        intent_name,
        if opts.bpc { "Enabled" } else { "Disabled" },
        opts.target_dpi,
        sharpen_label,
        depth_label,
        icc_filename,
    );

    let page_data = vec![65535u16; (opts.page_w_px * opts.page_h_px * 3) as usize];
    let mut page: Rgb16Image = ImageBuffer::from_raw(opts.page_w_px, opts.page_h_px, page_data)
        .context("failed to allocate page buffer")?;

    for p in &opts.placements {
        let (img, source_dpi, embedded_icc) = load_image_with_dpi_and_embedded_icc(&p.input)?;
        let img16: Rgb16Image = match img {
            LoadedImage::Rgb8(im) => rgb8_to_rgb16(&im),
            LoadedImage::Rgb16(im) => im,
        };

        let input_profile = load_input_profile(p.input_icc.as_ref(), embedded_icc.as_deref())?;

        let (ow, oh) = (img16.width(), img16.height());

        // Apply crop if specified (crop_u0/v0/u1/v1 are in 0-1 range)
        let has_crop = (p.crop_u1 - p.crop_u0) < 0.999 || (p.crop_v1 - p.crop_v0) < 0.999;
        let cropped_owned;
        let cropped_img: &Rgb16Image = if has_crop {
            // Calculate crop region in pixels
            let crop_x = ((ow as f64 * p.crop_u0 as f64).round() as u32).min(ow - 1);
            let crop_y = ((oh as f64 * p.crop_v0 as f64).round() as u32).min(oh - 1);
            let crop_w = ((ow as f64 * (p.crop_u1 - p.crop_u0) as f64).round() as u32)
                .max(1)
                .min(ow - crop_x);
            let crop_h = ((oh as f64 * (p.crop_v1 - p.crop_v0) as f64).round() as u32)
                .max(1)
                .min(oh - crop_y);

            let cropped = image::imageops::crop_imm(&img16, crop_x, crop_y, crop_w, crop_h);
            cropped_owned = cropped.to_image();
            &cropped_owned
        } else {
            &img16
        };

        let (cw, ch) = (cropped_img.width(), cropped_img.height());

        // For inner border: scale to fit inside the border area (dest - 2*border)
        // For outer border: scale to original size (dest - 2*border), border adds to total size
        let (scale_dest_w, scale_dest_h) = if p.border_width_px > 0 {
            let border = p.border_width_px;
            (
                p.dest_w_px.saturating_sub(border * 2).max(1),
                p.dest_h_px.saturating_sub(border * 2).max(1),
            )
        } else {
            (p.dest_w_px, p.dest_h_px)
        };

        // When crop is enabled with border, stretch to fill inner area (crop UVs preserve aspect)
        // Otherwise use aspect-fit scaling
        let has_crop = (p.crop_u1 - p.crop_u0) < 0.999 || (p.crop_v1 - p.crop_v0) < 0.999;
        let (new_w, new_h) = if p.border_width_px > 0 && has_crop {
            // Stretch to exactly fill inner area - crop UVs already selected correct portion
            // Account for rotation: destination is in final orientation, image is in original orientation
            if p.rotate_cw {
                (scale_dest_h, scale_dest_w) // Swap: image will be rotated to match destination
            } else {
                (scale_dest_w, scale_dest_h)
            }
        } else {
            // Aspect-fit scaling
            let s = if p.rotate_cw {
                (scale_dest_w as f64 / ch as f64).min(scale_dest_h as f64 / cw as f64)
            } else {
                (scale_dest_w as f64 / cw as f64).min(scale_dest_h as f64 / ch as f64)
            };
            (
                ((cw as f64 * s).round().max(1.0)) as u32,
                ((ch as f64 * s).round().max(1.0)) as u32,
            )
        };

        let resized = resize_rgb16(&cropped_img, new_w, new_h, &opts.engine);
        let radius_px = ((opts.target_dpi as u64 * 100) / 720) as f64 / 100.0;
        let sharpened = if opts.sharpen > 0 {
            let sigma = ((radius_px as u64 * 100) / 2) as f64 / 100.0;
            let normalized = map_sharpening_slider(opts.sharpen as f64);
            let amount = normalized * SHARPEN_MAX_AMOUNT;
            unsharp_mask_rgb16(&resized, sigma, amount, 0.03)
        } else {
            resized
        };

        let transformed = transform_rgb16_icc(
            &sharpened,
            &input_profile,
            &working_profile,
            opts.intent,
            opts.bpc,
        )?;
        let placed = if p.rotate_cw {
            rotate_90_cw_rgb16(&transformed)
        } else {
            transformed
        };

        // Composite position: center image in destination box
        // For inner border with crop: image fills inner area, position at top-left of inner area
        // For inner border without crop: center within inner area (aspect-fit letterboxing)
        // For outer border: center within full dest box (border adds to total size)
        // For no border: center within full dest box
        let (pw, ph) = (placed.width(), placed.height());
        let has_crop = (p.crop_u1 - p.crop_u0) < 0.999 || (p.crop_v1 - p.crop_v0) < 0.999;
        let (cx, cy) =
            if p.border_type == crate::layout_engine::BorderType::Inner && p.border_width_px > 0 {
                let border = p.border_width_px;
                if has_crop {
                    // Image fills inner area completely - position at top-left
                    let cx = p.dest_x_px + border;
                    let cy = p.dest_y_px + border;
                    (cx, cy)
                } else {
                    // No crop - center within inner area
                    let inner_w = p.dest_w_px.saturating_sub(border * 2);
                    let inner_h = p.dest_h_px.saturating_sub(border * 2);
                    let cx = p.dest_x_px + border + (inner_w.saturating_sub(pw) / 2);
                    let cy = p.dest_y_px + border + (inner_h.saturating_sub(ph) / 2);
                    (cx, cy)
                }
            } else {
                // Outer border or no border: center within full dest box
                let cx = p.dest_x_px + p.dest_w_px.saturating_sub(pw) / 2;
                let cy = p.dest_y_px + p.dest_h_px.saturating_sub(ph) / 2;
                (cx, cy)
            };

        let img_to_composite = placed;

        composite_rgb16(&mut page, &img_to_composite, cx, cy);

        // Draw border if enabled - only in the gap around the image
        if p.border_type != crate::layout_engine::BorderType::None && p.border_width_px > 0 {
            // For Inner: draw border in gap between smaller image and outer edge
            // For Outer: draw border in gap between image and expanded outer edge
            draw_border_in_gap_rgb16(
                &mut page,
                cx,
                cy, // image position
                img_to_composite.width(),
                img_to_composite.height(), // image size
                p.dest_x_px,
                p.dest_y_px, // box position
                p.dest_w_px,
                p.dest_h_px, // box size
                p.border_width_px,
            );
        }

        let _ = source_dpi;
    }

    let page = transform_rgb16_icc(
        &page,
        &working_profile,
        &output_profile,
        opts.intent,
        opts.bpc,
    )?;

    if opts.depth == 8 {
        let dithered = dither_rgb16_to_rgb8(&page);
        save_rgb8_tiff_with_dpi(
            &opts.output,
            &dithered,
            opts.target_dpi,
            &output_icc_bytes,
            &description,
        )?;
    } else {
        save_rgb16_tiff_with_dpi(
            &opts.output,
            &page,
            opts.target_dpi,
            &output_icc_bytes,
            &description,
        )?;
    }
    Ok(())
}

#[repr(C)]
#[derive(Copy, Clone)]
struct Rgb16Pixel {
    r: u16,
    g: u16,
    b: u16,
}

unsafe impl lcms2::Zeroable for Rgb16Pixel {}
unsafe impl lcms2::Pod for Rgb16Pixel {}

#[derive(Clone)]
pub enum ResampleEngine {
    Mks,
    Lanczos3,
    IterativeStep,
    MitchellEwa,
    MitchellEwaSharp,
}

impl ResampleEngine {
    pub fn display_name(&self) -> &'static str {
        match self {
            ResampleEngine::Mks => "Catmull-Rom",
            ResampleEngine::Lanczos3 => "Lanczos3",
            ResampleEngine::IterativeStep => "Iterative-Step",
            ResampleEngine::MitchellEwa => "Mitchell-EWA",
            ResampleEngine::MitchellEwaSharp => "Mitchell-EWA (Sharp)",
        }
    }
}

/// Full-page layout parameters for print output.
/// The image will be letterboxed into the print rect, optionally rotated 90° CW,
/// then composited onto a blank white full-paper page.
pub struct PageLayout {
    pub page_w_px: u32,  // paper width in pixels  (paper_w_pt / 72 * dpi)
    pub page_h_px: u32,  // paper height in pixels
    pub print_x: u32,    // print rect left on page (centred in imageable area)
    pub print_y: u32,    // print rect top on page
    pub print_w_px: u32, // print rect width  (target box for letterboxing)
    pub print_h_px: u32, // print rect height
    pub rotate_cw: bool, // rotate image 90° CW before compositing
}

pub struct ProcessOptions {
    pub input: std::path::PathBuf,
    pub output: std::path::PathBuf,
    pub input_icc: Option<std::path::PathBuf>,
    pub output_icc: Option<std::path::PathBuf>,
    pub default_wide_output_when_unset: bool,
    pub target_dpi: f64,
    pub intent: lcms2::Intent,
    pub bpc: bool,
    pub engine: ResampleEngine,
    pub depth: u8,
    pub sharpen: u8,
    pub page_layout: Option<PageLayout>,
}

#[derive(Clone)]
pub struct PagePlacement {
    pub input: std::path::PathBuf,
    pub input_icc: Option<std::path::PathBuf>,
    pub dest_x_px: u32,
    pub dest_y_px: u32,
    pub dest_w_px: u32,
    pub dest_h_px: u32,
    pub rotate_cw: bool,
    // Crop UVs: which portion of source image to use (0-1 range)
    pub crop_u0: f32,
    pub crop_v0: f32,
    pub crop_u1: f32,
    pub crop_v1: f32,
    // Border settings (border_width_px is the total border width in pixels)
    pub border_type: crate::layout_engine::BorderType,
    pub border_width_px: u32,
}

pub struct CompositePageOptions {
    pub output: std::path::PathBuf,
    pub placements: Vec<PagePlacement>,
    pub page_w_px: u32,
    pub page_h_px: u32,
    pub output_icc: Option<std::path::PathBuf>,
    pub default_wide_output_when_unset: bool,
    pub target_dpi: f64,
    pub intent: lcms2::Intent,
    pub bpc: bool,
    pub engine: ResampleEngine,
    pub depth: u8,
    pub sharpen: u8,
}

pub fn process(opts: ProcessOptions) -> Result<()> {
    if opts.target_dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let (img, source_dpi, embedded_icc) = load_image_with_dpi_and_embedded_icc(&opts.input)?;

    // Convert to 16-bit before resize so all engines operate at full depth
    let img16: Rgb16Image = match img {
        LoadedImage::Rgb8(im) => rgb8_to_rgb16(&im),
        LoadedImage::Rgb16(im) => im,
    };

    // Determine resize target: explicit print-rect dims or DPI-scaled dims
    let (new_w, new_h) = if let Some(ref layout) = opts.page_layout {
        let (ow, oh) = (img16.width(), img16.height());
        // After 90° CW rotation new_w=oh, new_h=ow; scale so it fits in print rect
        let s = if layout.rotate_cw {
            (layout.print_w_px as f64 / oh as f64).min(layout.print_h_px as f64 / ow as f64)
        } else {
            (layout.print_w_px as f64 / ow as f64).min(layout.print_h_px as f64 / oh as f64)
        };
        // Use integer-based calculation to avoid floating-point precision loss
        let new_w = ((ow as u64 * (s * 10000.0) as u64) / 10000) as u32;
        let new_h = ((oh as u64 * (s * 10000.0) as u64) / 10000) as u32;
        (new_w, new_h)
    } else {
        scaled_dimensions(
            img16.width(),
            img16.height(),
            source_dpi.unwrap_or(opts.target_dpi),
            opts.target_dpi,
        )
    };

    println!(
        "VibePrint Engine: {} initialized.",
        opts.engine.display_name()
    );
    let resized = resize_rgb16(&img16, new_w, new_h, &opts.engine);

    let radius_px = ((opts.target_dpi as u64 * 100) / 720) as f64 / 100.0;
    let sharpened = if opts.sharpen > 0 {
        let sigma = ((radius_px as u64 * 100) / 2) as f64 / 100.0;
        let normalized = map_sharpening_slider(opts.sharpen as f64);
        let amount = normalized * SHARPEN_MAX_AMOUNT;
        println!(
            "VibePrint: Applying Universal Sharpening (Level {}, Radius {:.2}px).",
            opts.sharpen, radius_px
        );
        unsharp_mask_rgb16(&resized, sigma, amount, 0.03)
    } else {
        resized
    };

    let input_profile = match (opts.input_icc.as_ref(), embedded_icc.as_ref()) {
        (Some(path), _) => {
            println!("Using input ICC: {}", path.display());
            load_input_profile(opts.input_icc.as_ref(), embedded_icc.as_deref())?
        }
        (None, Some(_)) => {
            println!("Using embedded ICC profile");
            load_input_profile(opts.input_icc.as_ref(), embedded_icc.as_deref())?
        }
        (None, None) => {
            println!("No profile found, defaulting to sRGB");
            load_input_profile(opts.input_icc.as_ref(), embedded_icc.as_deref())?
        }
    };

    let (output_profile, output_icc_bytes, icc_filename) = match opts.output_icc.as_ref() {
        Some(path) => {
            println!("Using destination ICC: {}", path.display());
            load_output_profile(
                opts.output_icc.as_ref(),
                embedded_icc.as_deref(),
                opts.default_wide_output_when_unset,
            )?
        }
        None => {
            if opts.default_wide_output_when_unset {
                println!("No output ICC specified, using ProPhoto RGB D50 (Linear) (default).");
            } else if embedded_icc.is_some() {
                println!("No output ICC specified, using embedded profile (passthrough).");
            } else {
                println!("No output ICC specified, using sRGB (passthrough).");
            }
            load_output_profile(
                opts.output_icc.as_ref(),
                embedded_icc.as_deref(),
                opts.default_wide_output_when_unset,
            )?
        }
    };

    let intent_name = match opts.intent {
        lcms2::Intent::Perceptual => "Perceptual",
        lcms2::Intent::RelativeColorimetric => "Relative Colorimetric",
        lcms2::Intent::Saturation => "Saturation",
        lcms2::Intent::AbsoluteColorimetric => "Absolute Colorimetric",
        _ => "Unknown",
    };
    println!(
        "Applying {} transform {} Black Point Compensation.",
        intent_name,
        if opts.bpc { "with" } else { "without" }
    );

    let depth_label = if opts.depth == 8 {
        "8-bit (dithered)"
    } else {
        "16-bit"
    };
    let sharpen_label = if opts.sharpen == 0 {
        "Off".to_string()
    } else {
        opts.sharpen.to_string()
    };
    let description = format!(
        "vibeprint | Engine: {} | Intent: {} | BPC: {} | DPI: {} | Sharpen: {} | Depth: {} | Output ICC: {}",
        opts.engine.display_name(),
        intent_name,
        if opts.bpc { "Enabled" } else { "Disabled" },
        opts.target_dpi,
        sharpen_label,
        depth_label,
        icc_filename,
    );

    let working_profile = load_prophoto_working_profile()?;

    let final_working_image = if let Some(ref layout) = opts.page_layout {
        let placed_source = if layout.rotate_cw {
            println!("VibePrint: Rotating image 90° CW for page layout.");
            rotate_90_cw_rgb16(&sharpened)
        } else {
            sharpened
        };
        let placed = transform_rgb16_icc(
            &placed_source,
            &input_profile,
            &working_profile,
            opts.intent,
            opts.bpc,
        )?;
        let (pw, ph) = (placed.width(), placed.height());
        let cx = layout.print_x + layout.print_w_px.saturating_sub(pw) / 2;
        let cy = layout.print_y + layout.print_h_px.saturating_sub(ph) / 2;
        println!(
            "VibePrint: Compositing {}x{} image at ({},{}) on {}x{} page.",
            pw, ph, cx, cy, layout.page_w_px, layout.page_h_px
        );
        let page_data = vec![65535u16; (layout.page_w_px * layout.page_h_px * 3) as usize];
        let mut page: Rgb16Image =
            ImageBuffer::from_raw(layout.page_w_px, layout.page_h_px, page_data)
                .context("failed to allocate page buffer")?;
        composite_rgb16(&mut page, &placed, cx, cy);
        page
    } else {
        transform_rgb16_icc(
            &sharpened,
            &input_profile,
            &working_profile,
            opts.intent,
            opts.bpc,
        )?
    };

    let final_image = transform_rgb16_icc(
        &final_working_image,
        &working_profile,
        &output_profile,
        opts.intent,
        opts.bpc,
    )?;

    if opts.depth == 8 {
        println!("VibePrint: Outputting 8-bit TIFF (with dithering).");
        let dithered = dither_rgb16_to_rgb8(&final_image);
        save_rgb8_tiff_with_dpi(
            &opts.output,
            &dithered,
            opts.target_dpi,
            &output_icc_bytes,
            &description,
        )?;
        println!("VibePrint: Saved 8-bit TIFF to {}", opts.output.display());
    } else {
        println!("VibePrint: Outputting 16-bit TIFF.");
        save_rgb16_tiff_with_dpi(
            &opts.output,
            &final_image,
            opts.target_dpi,
            &output_icc_bytes,
            &description,
        )?;
        println!("VibePrint: Saved 16-bit TIFF to {}", opts.output.display());
    }

    Ok(())
}

fn scaled_dimensions(w: u32, h: u32, source_dpi: f64, target_dpi: f64) -> (u32, u32) {
    // Use integer-based calculation to avoid floating-point precision loss
    let scale = target_dpi / source_dpi;
    let new_w = ((w as u64 * (scale * 10000.0) as u64) / 10000) as u32;
    let new_h = ((h as u64 * (scale * 10000.0) as u64) / 10000) as u32;
    (new_w, new_h)
}

fn rgb8_to_rgb16(img: &Rgb8Image) -> Rgb16Image {
    let mut out: Vec<u16> = Vec::with_capacity(img.as_raw().len());
    for &v in img.as_raw() {
        out.push((v as u16) * 257);
    }
    ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(img.width(), img.height(), out)
        .expect("rgb8_to_rgb16: dimensions mismatch")
}

fn dither_rgb16_to_rgb8(img: &Rgb16Image) -> Rgb8Image {
    let w = img.width();
    let h = img.height();
    // Working buffer in f32 to accumulate diffused error without clamping loss
    let mut work: Vec<f32> = img.as_raw().iter().map(|&v| v as f32).collect();

    for y in 0..h {
        for x in 0..w {
            for c in 0..3usize {
                let idx = ((y * w + x) * 3) as usize + c;
                let old_val = work[idx];
                // Quantize to nearest 8-bit level (re-expressed in 16-bit scale)
                let new_val_8 = ((old_val as u64 * 100) / 257) as u64 / 100;
                let new_val_16 = (new_val_8 * 257) as f32;
                work[idx] = new_val_16;
                let error = old_val - new_val_16;
                // Floyd-Steinberg diffusion
                if x + 1 < w {
                    let i = ((y * w + x + 1) * 3) as usize + c;
                    work[i] = (work[i] + error * (7.0 / 16.0)).clamp(0.0, 65535.0);
                }
                if y + 1 < h {
                    if x > 0 {
                        let i = (((y + 1) * w + x - 1) * 3) as usize + c;
                        work[i] = (work[i] + error * (3.0 / 16.0)).clamp(0.0, 65535.0);
                    }
                    let i = (((y + 1) * w + x) * 3) as usize + c;
                    work[i] = (work[i] + error * (5.0 / 16.0)).clamp(0.0, 65535.0);
                    if x + 1 < w {
                        let i = (((y + 1) * w + x + 1) * 3) as usize + c;
                        work[i] = (work[i] + error * (1.0 / 16.0)).clamp(0.0, 65535.0);
                    }
                }
            }
        }
    }

    let out: Vec<u8> = work
        .iter()
        .map(|&v| (((v as u64 * 100) / 257) as u64 / 100) as u8)
        .collect();
    ImageBuffer::from_raw(w, h, out).expect("dither_rgb16_to_rgb8: buffer size mismatch")
}

fn resize_rgb16(img: &Rgb16Image, new_w: u32, new_h: u32, engine: &ResampleEngine) -> Rgb16Image {
    if new_w == img.width() && new_h == img.height() {
        return img.clone();
    }
    match engine {
        ResampleEngine::Mks => image::imageops::resize(img, new_w, new_h, FilterType::CatmullRom),
        ResampleEngine::Lanczos3 => {
            image::imageops::resize(img, new_w, new_h, FilterType::Lanczos3)
        }
        ResampleEngine::IterativeStep => resize_iterative_step(img, new_w, new_h),
        ResampleEngine::MitchellEwa => resize_ewa_mitchell(img, new_w, new_h),
        ResampleEngine::MitchellEwaSharp => resize_ewa_mitchell_sharp(img, new_w, new_h),
    }
}

fn resize_iterative_step(img: &Rgb16Image, target_w: u32, target_h: u32) -> Rgb16Image {
    let mut current = img.clone();
    loop {
        let cur_w = current.width();
        let cur_h = current.height();
        if cur_w == target_w && cur_h == target_h {
            break;
        }
        let next_w = if target_w > cur_w {
            ((cur_w as u64 * 11000) / 10000) as u32
        } else {
            ((cur_w as u64 * 10000) / 11000).max(1) as u32
        };
        let next_h = if target_h > cur_h {
            ((cur_h as u64 * 11000) / 10000) as u32
        } else {
            ((cur_h as u64 * 10000) / 11000).max(1) as u32
        };
        let next_w = if target_w > cur_w {
            next_w.min(target_w)
        } else {
            next_w.max(target_w)
        };
        let next_h = if target_h > cur_h {
            next_h.min(target_h)
        } else {
            next_h.max(target_h)
        };
        current = image::imageops::resize(&current, next_w, next_h, FilterType::Lanczos3);
    }
    current
}

#[inline]
fn mitchell_kernel(t: f64) -> f64 {
    const B: f64 = 0.3782;
    const C: f64 = 0.3109;
    let t = t.abs();
    if t < 1.0 {
        ((12.0 - 9.0 * B - 6.0 * C) * t * t * t
            + (-18.0 + 12.0 * B + 6.0 * C) * t * t
            + (6.0 - 2.0 * B))
            / 6.0
    } else if t < 2.0 {
        ((-B - 6.0 * C) * t * t * t
            + (6.0 * B + 30.0 * C) * t * t
            + (-12.0 * B - 48.0 * C) * t
            + (8.0 * B + 24.0 * C))
            / 6.0
    } else {
        0.0
    }
}

#[inline]
fn mitchell_sharp_kernel(t: f64) -> f64 {
    const B: f64 = 0.30;
    const C: f64 = 0.35;
    let t = t.abs();
    if t < 1.0 {
        ((12.0 - 9.0 * B - 6.0 * C) * t * t * t
            + (-18.0 + 12.0 * B + 6.0 * C) * t * t
            + (6.0 - 2.0 * B))
            / 6.0
    } else if t < 2.0 {
        ((-B - 6.0 * C) * t * t * t
            + (6.0 * B + 30.0 * C) * t * t
            + (-12.0 * B - 48.0 * C) * t
            + (8.0 * B + 24.0 * C))
            / 6.0
    } else {
        0.0
    }
}

fn resize_ewa_mitchell(img: &Rgb16Image, dst_w: u32, dst_h: u32) -> Rgb16Image {
    resize_ewa_cubic(img, dst_w, dst_h, mitchell_kernel)
}

fn resize_ewa_mitchell_sharp(img: &Rgb16Image, dst_w: u32, dst_h: u32) -> Rgb16Image {
    resize_ewa_cubic(img, dst_w, dst_h, mitchell_sharp_kernel)
}

fn resize_ewa_cubic(
    img: &Rgb16Image,
    dst_w: u32,
    dst_h: u32,
    kernel: fn(f64) -> f64,
) -> Rgb16Image {
    use rayon::prelude::*;

    let src_w = img.width() as f64;
    let src_h = img.height() as f64;
    let scale_x = src_w / (dst_w as f64);
    let scale_y = src_h / (dst_h as f64);

    // Radius in input-pixel space: 2 for upscaling, 2*scale for downscaling (anti-alias)
    let radius_x = scale_x.max(1.0) * 2.0;
    let radius_y = scale_y.max(1.0) * 2.0;

    let src_max_x = img.width() as i64 - 1;
    let src_max_y = img.height() as i64 - 1;
    let src_stride = img.width() as usize * 3;
    let raw = img.as_raw();
    let mut output: Vec<u16> = vec![0u16; (dst_w * dst_h * 3) as usize];
    let row_len = dst_w as usize * 3;

    output.par_chunks_mut(row_len).enumerate().for_each(|(oy, row)| {
        for ox in 0..dst_w as usize {
            let ix = (ox as f64 + 0.5) * scale_x - 0.5;
            let iy = (oy as f64 + 0.5) * scale_y - 0.5;

            let x0 = (ix - radius_x).ceil() as i64;
            let x1 = (ix + radius_x).floor() as i64;
            let y0 = (iy - radius_y).ceil() as i64;
            let y1 = (iy + radius_y).floor() as i64;

            let mut sum_r = 0.0f64;
            let mut sum_g = 0.0f64;
            let mut sum_b = 0.0f64;
            let mut sum_w = 0.0f64;

            for sy in y0..=y1 {
                let dy = (sy as f64 - iy) / radius_y;
                let csy = sy.clamp(0, src_max_y) as usize;
                let row_base = csy * src_stride;
                for sx in x0..=x1 {
                    let dx = (sx as f64 - ix) / radius_x;
                    // EWA: circular support — skip samples outside the unit disc
                    let r2 = dx * dx + dy * dy;
                    if r2 >= 1.0 {
                        continue;
                    }
                    let w = kernel(r2.sqrt() * 2.0);
                    if w == 0.0 {
                        continue;
                    }
                    let csx = sx.clamp(0, src_max_x) as usize;
                    let pi = row_base + csx * 3;
                    sum_r += w * raw[pi] as f64;
                    sum_g += w * raw[pi + 1] as f64;
                    sum_b += w * raw[pi + 2] as f64;
                    sum_w += w;
                }
            }

            let idx = ox * 3;
            if sum_w > 1e-10 {
                row[idx] = (sum_r / sum_w).round().clamp(0.0, 65535.0) as u16;
                row[idx + 1] = (sum_g / sum_w).round().clamp(0.0, 65535.0) as u16;
                row[idx + 2] = (sum_b / sum_w).round().clamp(0.0, 65535.0) as u16;
            }
        }
    });

    ImageBuffer::from_raw(dst_w, dst_h, output).expect("resize_ewa_cubic: buffer size mismatch")
}

fn load_image_with_dpi_and_embedded_icc(
    path: &Path,
) -> Result<(LoadedImage, Option<f64>, Option<Vec<u8>>)> {
    let dyn_img =
        image::open(path).with_context(|| format!("failed to decode image: {}", path.display()))?;

    let (dpi, embedded_icc) = if is_tiff_path(path) {
        read_tiff_dpi_and_embedded_icc(path).unwrap_or((None, None))
    } else if is_jpeg_path(path) {
        read_jpeg_dpi_and_embedded_icc(path)
    } else if is_png_path(path) {
        read_png_dpi_and_embedded_icc(path)
    } else if is_webp_path(path) {
        (None, read_webp_embedded_icc(path))
    } else {
        (None, None)
    };

    let img = dynamic_to_rgb8_or_rgb16(dyn_img)?;
    Ok((img, dpi, embedded_icc))
}

fn is_tiff_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ext == "tif" || ext == "tiff"
}

fn is_jpeg_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ext == "jpg" || ext == "jpeg"
}

fn is_png_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("png"))
        .unwrap_or(false)
}

fn is_webp_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("webp"))
        .unwrap_or(false)
}

fn read_png_dpi_and_embedded_icc(path: &Path) -> (Option<f64>, Option<Vec<u8>>) {
    // ICC: delegate to image crate's PngDecoder — handles iCCP zlib decompression
    let icc = (|| -> Option<Vec<u8>> {
        let file = File::open(path).ok()?;
        let mut decoder = PngDecoder::new(BufReader::new(file)).ok()?;
        decoder.icc_profile().ok().flatten()
    })();

    // DPI: scan for pHYs chunk (pixels per unit + unit type)
    let dpi = (|| -> Option<f64> {
        let data = std::fs::read(path).ok()?;
        if data.len() < 8 || &data[0..8] != b"\x89PNG\r\n\x1a\n" {
            return None;
        }
        let mut i = 8usize;
        while i + 12 <= data.len() {
            let chunk_len = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
            let chunk_type = &data[i + 4..i + 8];
            if i + 8 + chunk_len > data.len() {
                break;
            }
            let chunk_data = &data[i + 8..i + 8 + chunk_len];
            i += 12 + chunk_len;
            if chunk_type == b"pHYs" && chunk_data.len() >= 9 {
                let px = u32::from_be_bytes(chunk_data[0..4].try_into().ok()?);
                let py = u32::from_be_bytes(chunk_data[4..8].try_into().ok()?);
                let unit = chunk_data[8];
                if unit == 1 && px > 0 && py > 0 {
                    // unit=1 means pixels per metre; convert to DPI
                    return Some((px as f64 + py as f64) / 2.0 * 0.0254);
                }
            }
            // pHYs appears before IDAT; stop once image data starts
            if chunk_type == b"IDAT" || chunk_type == b"IEND" {
                break;
            }
        }
        None
    })();

    (dpi, icc)
}

fn read_webp_embedded_icc(path: &Path) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    // RIFF header: "RIFF" + 4-byte LE size + "WEBP"
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return None;
    }
    let mut i = 12usize;
    while i + 8 <= data.len() {
        let chunk_id = &data[i..i + 4];
        let chunk_size = u32::from_le_bytes(data[i + 4..i + 8].try_into().ok()?) as usize;
        let payload_start = i + 8;
        if payload_start + chunk_size > data.len() {
            break;
        }
        if chunk_id == b"ICCP" {
            return Some(data[payload_start..payload_start + chunk_size].to_vec());
        }
        // Chunks are padded to even byte boundaries
        i = payload_start + chunk_size + (chunk_size & 1);
    }
    None
}

fn read_jpeg_dpi_and_embedded_icc(path: &Path) -> (Option<f64>, Option<Vec<u8>>) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return (None, None),
    };

    // Must start with JPEG SOI marker FF D8
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return (None, None);
    }

    let mut dpi: Option<f64> = None;
    let mut icc_segments: Vec<(u8, Vec<u8>)> = Vec::new(); // (seq_num, data)
    let mut i = 2usize;

    while i + 1 < data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];
        i += 2;

        // Markers without a length field
        if marker == 0xD8 || marker == 0xD9 || (marker >= 0xD0 && marker <= 0xD7) {
            continue;
        }
        // SOS starts compressed data — stop scanning
        if marker == 0xDA {
            break;
        }

        if i + 2 > data.len() {
            break;
        }
        let seg_len = ((data[i] as usize) << 8) | (data[i + 1] as usize);
        if seg_len < 2 || i + seg_len > data.len() {
            break;
        }
        // segment payload (after the 2 length bytes)
        let payload = &data[i + 2..i + seg_len];
        i += seg_len;

        // APP0 (FF E0): JFIF density
        if marker == 0xE0 && payload.len() >= 12 && &payload[0..5] == b"JFIF\0" {
            let units = payload[7];
            let xd = ((payload[8] as u16) << 8) | payload[9] as u16;
            let yd = ((payload[10] as u16) << 8) | payload[11] as u16;
            if units > 0 && xd > 0 && yd > 0 {
                let density = (xd as f64 + yd as f64) / 2.0;
                dpi = Some(match units {
                    1 => density,
                    2 => density * 2.54,
                    _ => density,
                });
            }
        }

        // APP2 (FF E2): ICC_PROFILE
        if marker == 0xE2 && payload.len() > 14 && &payload[0..12] == b"ICC_PROFILE\0" {
            let seq_num = payload[12];
            let chunk_data = payload[14..].to_vec();
            icc_segments.push((seq_num, chunk_data));
        }
    }

    // Reassemble ICC chunks sorted by sequence number
    let icc = if !icc_segments.is_empty() {
        icc_segments.sort_by_key(|(seq, _)| *seq);
        let mut combined = Vec::new();
        for (_, chunk) in icc_segments {
            combined.extend_from_slice(&chunk);
        }
        Some(combined)
    } else {
        None
    };

    (dpi, icc)
}

fn dynamic_to_rgb8_or_rgb16(img: DynamicImage) -> Result<LoadedImage> {
    Ok(match img {
        DynamicImage::ImageRgb16(im) => LoadedImage::Rgb16(im),
        DynamicImage::ImageRgba16(im) => {
            LoadedImage::Rgb16(image::DynamicImage::ImageRgba16(im).to_rgb16())
        }
        DynamicImage::ImageLuma16(im) => {
            LoadedImage::Rgb16(image::DynamicImage::ImageLuma16(im).to_rgb16())
        }
        DynamicImage::ImageLumaA16(im) => {
            LoadedImage::Rgb16(image::DynamicImage::ImageLumaA16(im).to_rgb16())
        }
        _ => LoadedImage::Rgb8(img.to_rgb8()),
    })
}

fn read_tiff_dpi_and_embedded_icc(path: &Path) -> Result<(Option<f64>, Option<Vec<u8>>)> {
    let file =
        File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))
        .context("failed to create TIFF decoder")?;
    let dpi = read_tiff_dpi(&mut decoder);
    let embedded_icc = read_tiff_embedded_icc(&mut decoder);
    Ok((dpi, embedded_icc))
}

fn read_tiff_dpi(decoder: &mut tiff::decoder::Decoder<BufReader<File>>) -> Option<f64> {
    use tiff::decoder::ifd::Value;
    use tiff::tags::Tag;

    let x_res = match decoder.get_tag(Tag::XResolution).ok()? {
        Value::Rational(n, d) if d != 0 => Some((n as f64) / (d as f64)),
        _ => None,
    };
    let y_res = match decoder.get_tag(Tag::YResolution).ok()? {
        Value::Rational(n, d) if d != 0 => Some((n as f64) / (d as f64)),
        _ => None,
    };

    let unit = match decoder.get_tag(Tag::ResolutionUnit).ok() {
        Some(Value::Short(v)) => Some(v),
        _ => None,
    };

    let mut dpi = match (x_res, y_res) {
        (Some(x), Some(y)) => Some((x + y) * 0.5),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        _ => None,
    };

    if let (Some(v), Some(3)) = (dpi, unit) {
        dpi = Some(v * 2.54);
    }

    dpi
}

fn read_tiff_embedded_icc(
    decoder: &mut tiff::decoder::Decoder<BufReader<File>>,
) -> Option<Vec<u8>> {
    use tiff::decoder::ifd::Value;
    use tiff::tags::Tag;

    let v = decoder.get_tag(Tag::IccProfile).ok()?;
    match v {
        Value::Byte(b) => Some(vec![b]),
        Value::SignedByte(b) => Some(vec![b as u8]),
        Value::List(values) => {
            let mut out = Vec::with_capacity(values.len());
            for item in values {
                match item {
                    Value::Byte(b) => out.push(b),
                    Value::SignedByte(b) => out.push(b as u8),
                    _ => return None,
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    }
}

fn transform_rgb16_icc(
    img: &Rgb16Image,
    input_profile: &lcms2::Profile,
    output_profile: &lcms2::Profile,
    intent: lcms2::Intent,
    bpc: bool,
) -> Result<Rgb16Image> {
    let flags = if bpc {
        lcms2::Flags::BLACKPOINT_COMPENSATION | lcms2::Flags::NO_CACHE
    } else {
        lcms2::Flags::NO_CACHE
    };
    let transform = lcms2::Transform::new_flags(
        input_profile,
        lcms2::PixelFormat::RGB_16,
        output_profile,
        lcms2::PixelFormat::RGB_16,
        intent,
        flags,
    )
    .context("failed to create lcms2 transform")?;

    let input_raw = img.as_raw();
    if input_raw.len() % 3 != 0 {
        bail!("expected interleaved RGB16 buffer length to be divisible by 3");
    }

    let pixel_count = input_raw.len() / 3;
    // SAFETY: Rgb16Pixel is #[repr(C)] with 3 u16 fields — identical layout to [u16; 3].
    // input_raw length is verified divisible by 3 above.
    let input_pixels: &[Rgb16Pixel] =
        unsafe { std::slice::from_raw_parts(input_raw.as_ptr() as *const Rgb16Pixel, pixel_count) };
    let mut output_pixels: Vec<Rgb16Pixel> =
        vec![Rgb16Pixel { r: 0, g: 0, b: 0 }; pixel_count];
    transform.transform_pixels(input_pixels, &mut output_pixels);

    // Reinterpret Vec<Rgb16Pixel> as Vec<u16> without copying
    let output_raw: Vec<u16> = {
        let mut v = std::mem::ManuallyDrop::new(output_pixels);
        let ptr = v.as_mut_ptr() as *mut u16;
        let len = v.len() * 3;
        let cap = v.capacity() * 3;
        // SAFETY: same layout guarantee as above, and Vec allocation is compatible
        unsafe { Vec::from_raw_parts(ptr, len, cap) }
    };

    let out = ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(img.width(), img.height(), output_raw)
        .context("failed to construct transformed RGB16 image")?;

    Ok(out)
}

fn save_rgb8_tiff_with_dpi(
    path: &Path,
    img: &Rgb8Image,
    dpi: f64,
    output_icc_bytes: &[u8],
    description: &str,
) -> Result<()> {
    use tiff::encoder::{colortype, Compression, DeflateLevel, TiffEncoder};
    use tiff::tags::Tag;

    if dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let file = File::create(path)
        .with_context(|| format!("failed to create output file: {}", path.display()))?;
    let mut encoder = TiffEncoder::new(file)
        .context("failed to create TIFF encoder")?
        .with_compression(Compression::Deflate(DeflateLevel::Balanced));
    let mut image = encoder
        .new_image::<colortype::RGB8>(img.width(), img.height())
        .context("failed to create 8-bit TIFF image")?;

    let (n, d) = dpi_to_rational(dpi);
    let _ = image
        .encoder()
        .write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
    let _ = image
        .encoder()
        .write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::ResolutionUnit, 2u16);
    let _ = image.encoder().write_tag(Tag::IccProfile, output_icc_bytes);
    let _ = image
        .encoder()
        .write_tag(Tag::from_u16_exhaustive(40961), 65535u16);
    let _ = image
        .encoder()
        .write_tag(Tag::ImageDescription, description);

    image
        .write_data(img.as_raw())
        .context("failed to write 8-bit TIFF pixel data")?;

    Ok(())
}

fn save_rgb16_tiff_with_dpi(
    path: &Path,
    img: &Rgb16Image,
    dpi: f64,
    output_icc_bytes: &[u8],
    description: &str,
) -> Result<()> {
    use tiff::encoder::{colortype, Compression, DeflateLevel, TiffEncoder};
    use tiff::tags::Tag;

    if dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let file = File::create(path)
        .with_context(|| format!("failed to create output file: {}", path.display()))?;
    let mut encoder = TiffEncoder::new(file)
        .context("failed to create TIFF encoder")?
        .with_compression(Compression::Deflate(DeflateLevel::Balanced));
    let mut image = encoder
        .new_image::<colortype::RGB16>(img.width(), img.height())
        .context("failed to create TIFF image")?;

    let (n, d) = dpi_to_rational(dpi);
    let _ = image
        .encoder()
        .write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
    let _ = image
        .encoder()
        .write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::ResolutionUnit, 2u16);

    let _ = image.encoder().write_tag(Tag::IccProfile, output_icc_bytes);
    let _ = image
        .encoder()
        .write_tag(Tag::from_u16_exhaustive(40961), 65535u16);
    let _ = image
        .encoder()
        .write_tag(Tag::ImageDescription, description);

    image
        .write_data(img.as_raw())
        .context("failed to write TIFF pixel data")?;

    Ok(())
}

const SHARPEN_SLIDER_NEUTRAL: f64 = 5.0;
const SHARPEN_SLIDER_MAX: f64 = 20.0;
const SHARPEN_NEUTRAL_AMOUNT: f64 = 0.8; // perceptual baseline USM strength
const SHARPEN_MAX_AMOUNT: f64 = 2.0; // existing USM upper bound

/// Maps the UI sharpening slider (0-20) to a normalized 0-1 strength while anchoring
/// slider value 5 to the calibrated neutral amount (~0.8). The curve is piecewise:
/// quadratic below 5 for fine control, cubic-ease above 5 for gentle growth.
fn map_sharpening_slider(value: f64) -> f64 {
    let v = value.clamp(0.0, SHARPEN_SLIDER_MAX);
    let neutral_norm = SHARPEN_NEUTRAL_AMOUNT / SHARPEN_MAX_AMOUNT;

    if v <= SHARPEN_SLIDER_NEUTRAL {
        let t = (v / SHARPEN_SLIDER_NEUTRAL).max(0.0);
        neutral_norm * t * t
    } else {
        let t = (v - SHARPEN_SLIDER_NEUTRAL) / (SHARPEN_SLIDER_MAX - SHARPEN_SLIDER_NEUTRAL);
        let eased = 1.0 - (1.0 - t).powi(3);
        neutral_norm + (1.0 - neutral_norm) * eased
    }
}

fn gaussian_blur_rgb16(img: &Rgb16Image, sigma: f64) -> Rgb16Image {
    use rayon::prelude::*;

    let w = img.width() as usize;
    let h = img.height() as usize;

    // Clamp sigma to avoid degenerate zero kernel
    let sigma = sigma.max(0.01);
    let radius = (sigma * 3.0).ceil() as usize;
    let kernel_size = 2 * radius + 1;

    let mut kernel = vec![0.0f64; kernel_size];
    let mut sum = 0.0f64;
    for i in 0..kernel_size {
        let x = i as f64 - radius as f64;
        kernel[i] = (-0.5 * x * x / (sigma * sigma)).exp();
        sum += kernel[i];
    }
    for k in kernel.iter_mut() {
        *k /= sum;
    }

    let raw = img.as_raw();
    let row_stride = w * 3;

    // Horizontal pass — store as f32 to avoid double rounding
    let mut horiz = vec![0.0f32; w * h * 3];
    horiz.par_chunks_mut(row_stride).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            for c in 0..3usize {
                let mut acc = 0.0f64;
                for (ki, &kv) in kernel.iter().enumerate() {
                    let sx = (x as i64 + ki as i64 - radius as i64).clamp(0, w as i64 - 1) as usize;
                    acc += kv * raw[(y * w + sx) * 3 + c] as f64;
                }
                row[x * 3 + c] = acc as f32;
            }
        }
    });

    // Vertical pass — output u16
    let mut result = vec![0u16; w * h * 3];
    result.par_chunks_mut(row_stride).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            for c in 0..3usize {
                let mut acc = 0.0f64;
                for (ki, &kv) in kernel.iter().enumerate() {
                    let sy = (y as i64 + ki as i64 - radius as i64).clamp(0, h as i64 - 1) as usize;
                    acc += kv * horiz[(sy * w + x) * 3 + c] as f64;
                }
                row[x * 3 + c] = acc.round().clamp(0.0, 65535.0) as u16;
            }
        }
    });

    ImageBuffer::from_raw(w as u32, h as u32, result).expect("gaussian_blur_rgb16: buffer mismatch")
}

fn unsharp_mask_rgb16(
    img: &Rgb16Image,
    sigma: f64,
    amount: f64,
    threshold_frac: f64,
) -> Rgb16Image {
    let blurred = gaussian_blur_rgb16(img, sigma);
    let threshold = threshold_frac * 65535.0;

    let w = img.width();
    let h = img.height();
    let mut result = vec![0u16; (w * h * 3) as usize];

    for y in 0..h {
        for x in 0..w {
            let orig = img.get_pixel(x, y);
            let blur = blurred.get_pixel(x, y);
            for c in 0..3usize {
                let o = orig[c] as f64;
                let b = blur[c] as f64;
                let diff = o - b;
                let idx = (y as usize * w as usize + x as usize) * 3 + c;
                result[idx] = if diff.abs() > threshold {
                    (o + amount * diff).round().clamp(0.0, 65535.0) as u16
                } else {
                    orig[c]
                };
            }
        }
    }

    ImageBuffer::from_raw(w, h, result).expect("unsharp_mask_rgb16: buffer mismatch")
}

fn dpi_to_rational(dpi: f64) -> (u32, u32) {
    // Use integer-based calculation to avoid floating-point precision loss
    let d = 10000u32;
    let n = ((dpi as u64 * 10000) * d as u64 / 10000) as u32;
    (n, d)
}

/// Rotate an Rgb16Image 90° clockwise.
/// New dimensions: new_w = orig_h, new_h = orig_w.
/// Mapping: new(nx, ny) = old(w-1-ny, nx)
fn rotate_90_cw_rgb16(img: &Rgb16Image) -> Rgb16Image {
    let (w, h) = (img.width(), img.height());
    let new_w = h;
    let new_h = w;
    let src = img.as_raw();
    let mut dst = vec![0u16; (new_w * new_h * 3) as usize];
    for ny in 0..new_h {
        for nx in 0..new_w {
            // For 90° CW rotation: new(nx, ny) = old(w-1-ny, nx)
            let ox = w - 1 - ny;
            let oy = nx;
            let si = ((oy * w + ox) * 3) as usize;
            let di = ((ny * new_w + nx) * 3) as usize;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
        }
    }
    ImageBuffer::from_raw(new_w, new_h, dst).expect("rotate_90_cw_rgb16: buffer mismatch")
}

/// Composite `img` onto `page` with its top-left at (x, y), clipping to page bounds.
fn composite_rgb16(page: &mut Rgb16Image, img: &Rgb16Image, x: u32, y: u32) {
    let page_w = page.width();
    let page_h = page.height();
    let img_w = img.width();
    let img_h = img.height();
    let src = img.as_raw();
    let dst = page.as_mut();
    for row in 0..img_h {
        let py = y + row;
        if py >= page_h {
            break;
        }
        let cols = img_w.min(page_w.saturating_sub(x));
        if cols == 0 {
            continue;
        }
        let si = (row * img_w * 3) as usize;
        let di = (py * page_w * 3 + x * 3) as usize;
        dst[di..di + (cols * 3) as usize].copy_from_slice(&src[si..si + (cols * 3) as usize]);
    }
}

/// Draw a black border in the gap between the image and its containing box.
/// For Inner borders: fills the gap between the smaller centered image and the original box edge.
/// For Outer borders: fills the gap between the image and the expanded outer box edge.
fn draw_border_in_gap_rgb16(
    page: &mut Rgb16Image,
    img_x: u32,
    img_y: u32,
    img_w: u32,
    img_h: u32,
    box_x: u32,
    box_y: u32,
    box_w: u32,
    box_h: u32,
    border_width_px: u32,
) {
    let page_w = page.width();
    let page_h = page.height();
    let dst = page.as_mut();

    // Calculate the gap between image and box
    let gap_top = img_y.saturating_sub(box_y);
    let gap_bottom = (box_y + box_h).saturating_sub(img_y + img_h);
    let gap_left = img_x.saturating_sub(box_x);
    let gap_right = (box_x + box_w).saturating_sub(img_x + img_w);

    // Clamp gaps to border width (don't draw more than the specified border)
    let border_top = gap_top.min(border_width_px);
    let border_bottom = gap_bottom.min(border_width_px);
    let border_left = gap_left.min(border_width_px);
    let border_right = gap_right.min(border_width_px);

    // Top strip: full width of box, height = gap_top
    for row in 0..border_top {
        let py = box_y + row;
        if py >= page_h {
            break;
        }
        for col in 0..box_w {
            let px = box_x + col;
            if px >= page_w {
                continue;
            }
            let di = ((py * page_w + px) * 3) as usize;
            dst[di] = 0;
            dst[di + 1] = 0;
            dst[di + 2] = 0;
        }
    }

    // Bottom strip: full width of box, height = gap_bottom
    for row in 0..border_bottom {
        let py = box_y + box_h - border_bottom + row;
        if py >= page_h {
            break;
        }
        for col in 0..box_w {
            let px = box_x + col;
            if px >= page_w {
                continue;
            }
            let di = ((py * page_w + px) * 3) as usize;
            dst[di] = 0;
            dst[di + 1] = 0;
            dst[di + 2] = 0;
        }
    }

    // Left strip: between top and bottom of image
    let left_strip_top = box_y + border_top;
    let left_strip_bottom = box_y + box_h - border_bottom;
    for row in left_strip_top..left_strip_bottom {
        let py = row;
        if py >= page_h {
            break;
        }
        for col in 0..border_left {
            let px = box_x + col;
            if px >= page_w {
                continue;
            }
            let di = ((py * page_w + px) * 3) as usize;
            dst[di] = 0;
            dst[di + 1] = 0;
            dst[di + 2] = 0;
        }
    }

    // Right strip: between top and bottom of image
    for row in left_strip_top..left_strip_bottom {
        let py = row;
        if py >= page_h {
            break;
        }
        for col in 0..border_right {
            let px = box_x + box_w - border_right + col;
            if px >= page_w {
                continue;
            }
            let di = ((py * page_w + px) * 3) as usize;
            dst[di] = 0;
            dst[di + 1] = 0;
            dst[di + 2] = 0;
        }
    }
}
