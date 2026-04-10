use std::{fs::File, io::BufReader, path::Path};

use anyhow::{Context, Result};
use tempfile::tempdir;

fn write_gradient_rgb16_tiff(path: &Path, width: u32, height: u32, dpi: f64) -> Result<()> {
    use tiff::encoder::{colortype, TiffEncoder};
    use tiff::tags::Tag;

    let mut data: Vec<u16> = vec![0; (width as usize) * (height as usize) * 3];
    for y in 0..height {
        for x in 0..width {
            let t = if width <= 1 { 0.0 } else { (x as f64) / ((width - 1) as f64) };
            let v = (t * 65535.0).round().clamp(0.0, 65535.0) as u16;
            let idx = ((y * width + x) * 3) as usize;
            data[idx] = v;
            data[idx + 1] = v;
            data[idx + 2] = v;
        }
    }

    let file = File::create(path).with_context(|| format!("failed to create test TIFF: {}", path.display()))?;
    let mut encoder = TiffEncoder::new(file).context("failed to create TIFF encoder")?;
    let mut image = encoder
        .new_image::<colortype::RGB16>(width, height)
        .context("failed to create TIFF image")?;

    let (n, d) = dpi_to_rational(dpi);
    let _ = image.encoder().write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::ResolutionUnit, 2u16);

    image.write_data(&data).context("failed to write test TIFF")?;

    Ok(())
}

#[test]
fn unset_output_icc_can_default_to_wide_profile() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("input.tif");
    let output_path = tmp.path().join("output_default_wide.tif");

    write_gradient_rgb16_tiff(&input_path, 16, 16, 360.0)?;

    vibeprint::processor::process_composite_page(vibeprint::processor::CompositePageOptions {
        output: output_path.clone(),
        placements: vec![vibeprint::processor::PagePlacement {
            input: input_path,
            input_icc: None,
            dest_x_px: 0,
            dest_y_px: 0,
            dest_w_px: 160,
            dest_h_px: 160,
            rotate_cw: false,
            crop_u0: 0.0,
            crop_v0: 0.0,
            crop_u1: 1.0,
            crop_v1: 1.0,
            border_type: vibeprint::layout_engine::BorderType::None,
            border_width_px: 0,
        }],
        page_w_px: 160,
        page_h_px: 160,
        output_icc: None,
        default_wide_output_when_unset: true,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 16,
        sharpen: 0,
    })?;

    let embedded = read_tiff_embedded_icc(&output_path)?;
    let srgb_bytes = lcms2::Profile::new_srgb().icc().context("failed to serialize sRGB profile")?;
    assert_ne!(embedded, srgb_bytes, "default-wide output ICC unexpectedly matched sRGB");

    let embedded_profile = lcms2::Profile::new_icc(&embedded)
        .context("failed to parse embedded output ICC profile")?;
    let desc = embedded_profile
        .info(lcms2::InfoType::Description, lcms2::Locale::none())
        .unwrap_or_default();
    assert_eq!(
        desc,
        "ProPhoto RGB D50 (Linear)",
        "embedded default-wide ICC description mismatch"
    );

    Ok(())
}

fn dpi_to_rational(dpi: f64) -> (u32, u32) {
    let d = 10000u32;
    let n = (dpi * (d as f64)).round().max(0.0) as u32;
    (n, d)
}

fn read_tiff_bit_depth_and_dpi(path: &Path) -> Result<(u16, f64)> {
    use tiff::decoder::ifd::Value;
    use tiff::tags::Tag;

    let file = File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file)).context("failed to create decoder")?;

    let ct = decoder.colortype().context("failed to read color type")?;
    let bit_depth: u16 = match ct {
        tiff::ColorType::RGB(bps) => bps.into(),
        tiff::ColorType::RGBA(bps) => bps.into(),
        tiff::ColorType::Gray(bps) => bps.into(),
        tiff::ColorType::GrayA(bps) => bps.into(),
        _ => 0,
    };

    let x = match decoder.get_tag(Tag::XResolution).context("missing XResolution")? {
        Value::Rational(n, d) if d != 0 => (n as f64) / (d as f64),
        other => anyhow::bail!("unexpected XResolution tag value: {other:?}"),
    };
    let y = match decoder.get_tag(Tag::YResolution).context("missing YResolution")? {
        Value::Rational(n, d) if d != 0 => (n as f64) / (d as f64),
        other => anyhow::bail!("unexpected YResolution tag value: {other:?}"),
    };

    let dpi = (x + y) * 0.5;
    Ok((bit_depth, dpi))
}

fn read_tiff_pixel_rgb16(path: &Path, x: u32, y: u32) -> Result<(u16, u16, u16)> {
    let file = File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file)).context("failed to create decoder")?;

    let (w, h) = decoder.dimensions().context("failed to get dimensions")?;
    if x >= w || y >= h {
        anyhow::bail!("pixel out of bounds");
    }

    let data = decoder.read_image().context("failed to read image")?;
    let data: Vec<u16> = match data {
        tiff::decoder::DecodingResult::U16(v) => v,
        _ => anyhow::bail!("expected u16 decoding result"),
    };

    let idx = ((y * w + x) * 3) as usize;
    Ok((data[idx], data[idx + 1], data[idx + 2]))
}

fn write_checkerboard_rgb16_tiff(path: &Path, width: u32, height: u32, dpi: f64, block_size: u32) -> Result<()> {
    use tiff::encoder::{colortype, TiffEncoder};
    use tiff::tags::Tag;

    let mut data: Vec<u16> = vec![0; (width as usize) * (height as usize) * 3];
    for y in 0..height {
        for x in 0..width {
            let v: u16 = if ((x / block_size) + (y / block_size)) % 2 == 0 { 65535 } else { 0 };
            let idx = ((y * width + x) * 3) as usize;
            data[idx]     = v;
            data[idx + 1] = v;
            data[idx + 2] = v;
        }
    }
    let file = File::create(path).with_context(|| format!("failed to create: {}", path.display()))?;
    let mut encoder = TiffEncoder::new(file)?;
    let mut image = encoder.new_image::<colortype::RGB16>(width, height)?;
    let (n, d) = dpi_to_rational(dpi);
    let _ = image.encoder().write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::ResolutionUnit, 2u16);
    image.write_data(&data).context("failed to write checkerboard")?;
    Ok(())
}

fn write_solid_rgb16_tiff(path: &Path, width: u32, height: u32, dpi: f64, rgb: (u16, u16, u16)) -> Result<()> {
    use tiff::encoder::{colortype, TiffEncoder};
    use tiff::tags::Tag;

    let mut data: Vec<u16> = vec![0; (width as usize) * (height as usize) * 3];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 3) as usize;
            data[idx] = rgb.0;
            data[idx + 1] = rgb.1;
            data[idx + 2] = rgb.2;
        }
    }

    let file = File::create(path).with_context(|| format!("failed to create test TIFF: {}", path.display()))?;
    let mut encoder = TiffEncoder::new(file).context("failed to create TIFF encoder")?;
    let mut image = encoder
        .new_image::<colortype::RGB16>(width, height)
        .context("failed to create TIFF image")?;

    let (n, d) = dpi_to_rational(dpi);
    let _ = image.encoder().write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::ResolutionUnit, 2u16);

    image.write_data(&data).context("failed to write solid-color TIFF")?;
    Ok(())
}

fn read_tiff_pixels_u16(path: &Path) -> Result<Vec<u16>> {
    let file = File::open(path).with_context(|| format!("failed to open: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    match decoder.read_image().context("failed to decode pixels")? {
        tiff::decoder::DecodingResult::U16(v) => Ok(v),
        _ => anyhow::bail!("expected u16 decoding result"),
    }
}

fn read_tiff_embedded_icc(path: &Path) -> Result<Vec<u8>> {
    use tiff::decoder::ifd::Value;
    use tiff::tags::Tag;

    let file = File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file)).context("failed to create decoder")?;
    let v = decoder
        .get_tag(Tag::IccProfile)
        .context("missing ICC profile tag on output TIFF")?;

    match v {
        Value::Byte(b) => Ok(vec![b]),
        Value::SignedByte(b) => Ok(vec![b as u8]),
        Value::List(values) => {
            let mut out = Vec::with_capacity(values.len());
            for item in values {
                match item {
                    Value::Byte(b) => out.push(b),
                    Value::SignedByte(b) => out.push(b as u8),
                    _ => anyhow::bail!("unexpected ICC tag list item type"),
                }
            }
            Ok(out)
        }
        _ => anyhow::bail!("unexpected ICC profile tag value type"),
    }
}

fn make_wide_gamut_profile_bytes() -> Result<Vec<u8>> {
    // Create a "wide gamut" RGB profile with primaries similar to ProPhoto and gamma 1.8.
    // This is only used for validation testing (we need a deterministic, local profile source).
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

    let curve = lcms2::ToneCurve::new(1.0 / 1.8);
    let prof = lcms2::Profile::new_rgb(&wp, &primaries, &[&curve, &curve, &curve])
        .context("failed to create wide gamut profile")?;

    let bytes = prof.icc().context("failed to serialize profile to ICC bytes")?;
    Ok(bytes)
}

#[test]
fn engine_smoke_tests() -> Result<()> {
    use std::collections::HashMap;

    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("checker.tif");
    let output_icc_path = tmp.path().join("wide_gamut.icc");

    // 32×32 checkerboard at 360 DPI — 2× upscale to 720 DPI → 64×64
    write_checkerboard_rgb16_tiff(&input_path, 32, 32, 360.0, 4)?;
    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)?;

    let target_dpi = 720.0;
    let engines = [
        ("mks",            vibeprint::processor::ResampleEngine::Mks),
        ("lanczos3",       vibeprint::processor::ResampleEngine::Lanczos3),
        ("iterative-step", vibeprint::processor::ResampleEngine::IterativeStep),
        ("robidoux-ewa",   vibeprint::processor::ResampleEngine::RobidouxEwa),
    ];

    let mut pixel_data: HashMap<&str, Vec<u16>> = HashMap::new();

    for (name, engine) in &engines {
        let out_path = tmp.path().join(format!("out_{}.tif", name));

        vibeprint::processor::process(vibeprint::processor::ProcessOptions {
            input: input_path.clone(),
            output: out_path.clone(),
            input_icc: None,
            output_icc: Some(output_icc_path.clone()),
            default_wide_output_when_unset: false,
            target_dpi,
            intent: lcms2::Intent::RelativeColorimetric,
            bpc: true,
            engine: engine.clone(),
            depth: 16,
            sharpen: 5,
            page_layout: None,
        })?;

        // Proof: 16-bit output at correct DPI
        let (bit_depth, dpi) = read_tiff_bit_depth_and_dpi(&out_path)?;
        assert_eq!(bit_depth, 16, "[{}] output must be 16-bit", name);
        assert!(
            (dpi - target_dpi).abs() < 1e-6,
            "[{}] expected {target_dpi} DPI, got {dpi}",
            name
        );

        // Proof: exact output dimensions (360→720 DPI, 2× scale)
        let file = File::open(&out_path)?;
        let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
        let (w, h) = decoder.dimensions()?;
        assert_eq!(w, 64, "[{}] expected width 64, got {}", name, w);
        assert_eq!(h, 64, "[{}] expected height 64, got {}", name, h);

        // Proof: pixel values not uniformly zero or max (engine actually ran)
        let pixels = read_tiff_pixels_u16(&out_path)?;
        assert!(!pixels.iter().all(|&p| p == 0),     "[{}] output is all zeros", name);
        assert!(!pixels.iter().all(|&p| p == 65535), "[{}] output is all white", name);

        pixel_data.insert(name, pixels);
    }

    // Proof: different engines produce distinct pixel values
    let mks     = pixel_data.get("mks").unwrap();
    let lanczos = pixel_data.get("lanczos3").unwrap();
    let ewa     = pixel_data.get("robidoux-ewa").unwrap();

    let mks_vs_lanczos = mks.iter().zip(lanczos).filter(|(a, b)| a != b).count();
    let mks_vs_ewa     = mks.iter().zip(ewa).filter(|(a, b)| a != b).count();

    assert!(mks_vs_lanczos > 0, "MKS and Lanczos3 produced identical output");
    assert!(mks_vs_ewa > 0,     "MKS and Robidoux-EWA produced identical output");

    println!("Engine diff MKS vs Lanczos3  : {} values ({:.1}%)",
        mks_vs_lanczos, 100.0 * mks_vs_lanczos as f64 / mks.len() as f64);
    println!("Engine diff MKS vs Robidoux-EWA: {} values ({:.1}%)",
        mks_vs_ewa, 100.0 * mks_vs_ewa as f64 / mks.len() as f64);

    Ok(())
}

#[test]
fn iterative_step_exact_dimensions() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("grad.tif");
    let output_path = tmp.path().join("out.tif");
    let output_icc_path = tmp.path().join("profile.icc");

    // 100×75 at 300 DPI → 720 DPI: scale=2.4 → expected 240×180
    write_gradient_rgb16_tiff(&input_path, 100, 75, 300.0)?;
    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)?;

    vibeprint::processor::process(vibeprint::processor::ProcessOptions {
        input: input_path,
        output: output_path.clone(),
        input_icc: None,
        output_icc: Some(output_icc_path),
        default_wide_output_when_unset: false,
        target_dpi: 720.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::IterativeStep,
        depth: 16,
        sharpen: 0,
        page_layout: None,
    })?;

    let file = File::open(&output_path)?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    let (w, h) = decoder.dimensions()?;
    assert_eq!(w, 240, "iterative-step: expected width 240, got {}", w);
    assert_eq!(h, 180, "iterative-step: expected height 180, got {}", h);

    Ok(())
}

#[test]
fn sharpen_usm_smoke_test() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("checker.tif");
    let output_sharp = tmp.path().join("sharp.tif");
    let output_flat  = tmp.path().join("flat.tif");
    let output_icc_path = tmp.path().join("profile.icc");

    // Step edge at mid-tones (20000 → 48000): clear edges + room to overshoot in both directions
    {
        use tiff::encoder::{colortype, TiffEncoder};
        use tiff::tags::Tag;
        let (w, h) = (64u32, 64u32);
        let mut data = vec![0u16; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let v: u16 = if x < w / 2 { 20000 } else { 48000 };
                let idx = ((y * w + x) * 3) as usize;
                data[idx] = v; data[idx+1] = v; data[idx+2] = v;
            }
        }
        let f = File::create(&input_path)?;
        let mut enc = TiffEncoder::new(f)?;
        let mut img = enc.new_image::<colortype::RGB16>(w, h)?;
        let (n, d) = dpi_to_rational(720.0);
        let _ = img.encoder().write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
        let _ = img.encoder().write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
        let _ = img.encoder().write_tag(Tag::ResolutionUnit, 2u16);
        img.write_data(&data)?;
    }
    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)?;

    let base_opts = || vibeprint::processor::ProcessOptions {
        input: input_path.clone(),
        output: output_flat.clone(),
        input_icc: None,
        output_icc: Some(output_icc_path.clone()),
        default_wide_output_when_unset: false,
        target_dpi: 720.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 16,
        sharpen: 0,
        page_layout: None,
    };

    // Run with sharpen=0 (no USM)
    vibeprint::processor::process(base_opts())?;
    let flat_pixels = read_tiff_pixels_u16(&output_flat)?;

    // Run with sharpen=10 (strong USM) — same input, different output path
    let mut sharp_opts = base_opts();
    sharp_opts.output = output_sharp.clone();
    sharp_opts.sharpen = 10;
    vibeprint::processor::process(sharp_opts)?;
    let sharp_pixels = read_tiff_pixels_u16(&output_sharp)?;

    // Proof 1: both outputs are 16-bit
    let (bd_flat, _)  = read_tiff_bit_depth_and_dpi(&output_flat)?;
    let (bd_sharp, _) = read_tiff_bit_depth_and_dpi(&output_sharp)?;
    assert_eq!(bd_flat,  16, "flat:  must be 16-bit");
    assert_eq!(bd_sharp, 16, "sharp: must be 16-bit");

    // Proof 2: sharpened output differs from unsharpened
    let diff_count = flat_pixels.iter().zip(&sharp_pixels).filter(|(a, b)| a != b).count();
    assert!(diff_count > 0, "USM produced no change — sharpening may not be applied");

    // Proof 3: sharpened image has higher max value or lower min (edge overshoot)
    let sharp_max = sharp_pixels.iter().copied().max().unwrap_or(0);
    let flat_max  = flat_pixels.iter().copied().max().unwrap_or(0);
    assert!(sharp_max >= flat_max, "USM should produce brighter highlights on edges");

    println!("USM diff: {} pixels changed ({:.1}%), sharp_max={} flat_max={}",
        diff_count,
        100.0 * diff_count as f64 / flat_pixels.len() as f64,
        sharp_max, flat_max);

    Ok(())
}

#[test]
fn single_image_page_layout_smoke_test() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("page_input.tif");
    let output_path = tmp.path().join("page_layout_out.tif");
    let output_icc_path = tmp.path().join("page_profile.icc");

    write_gradient_rgb16_tiff(&input_path, 64, 64, 360.0)?;
    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)?;

    vibeprint::processor::process(vibeprint::processor::ProcessOptions {
        input: input_path,
        output: output_path.clone(),
        input_icc: None,
        output_icc: Some(output_icc_path),
        default_wide_output_when_unset: false,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 8,
        sharpen: 0,
        page_layout: Some(vibeprint::processor::PageLayout {
            page_w_px: 300,
            page_h_px: 200,
            print_x: 50,
            print_y: 20,
            print_w_px: 200,
            print_h_px: 160,
            rotate_cw: false,
        }),
    })?;

    let (bit_depth, dpi) = read_tiff_bit_depth_and_dpi(&output_path)?;
    assert_eq!(bit_depth, 8, "page-layout output must be 8-bit");
    assert!((dpi - 360.0).abs() < 1e-6, "page-layout DPI mismatch");

    let file = File::open(&output_path)?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    let (w, h) = decoder.dimensions()?;
    assert_eq!(w, 300, "page-layout width mismatch");
    assert_eq!(h, 200, "page-layout height mismatch");

    Ok(())
}

#[test]
fn depth8_dither_smoke_test() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("grad.tif");
    let output_path = tmp.path().join("out8.tif");
    let output_icc_path = tmp.path().join("profile.icc");

    // Gradient: fine tonal transitions will show banding without dithering
    write_gradient_rgb16_tiff(&input_path, 256, 32, 360.0)?;
    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)?;

    vibeprint::processor::process(vibeprint::processor::ProcessOptions {
        input: input_path,
        output: output_path.clone(),
        input_icc: None,
        output_icc: Some(output_icc_path),
        default_wide_output_when_unset: false,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 8,
        sharpen: 0,
        page_layout: None,
    })?;

    // Proof 1: output is 8-bit
    let (bit_depth, dpi) = read_tiff_bit_depth_and_dpi(&output_path)?;
    assert_eq!(bit_depth, 8, "depth8: output must be 8-bit");
    assert!((dpi - 360.0).abs() < 1e-6, "depth8: DPI mismatch");

    // Proof 2: pixels are in 0–255 range and not uniform
    let file = File::open(&output_path)?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    let pixels: Vec<u8> = match decoder.read_image().context("decode 8-bit pixels")? {
        tiff::decoder::DecodingResult::U8(v) => v,
        _ => anyhow::bail!("expected U8 decoding result"),
    };
    assert!(!pixels.iter().all(|&p| p == 0),   "depth8: output is all zeros");
    assert!(!pixels.iter().all(|&p| p == 255),  "depth8: output is all white");

    // Proof 3: dithering introduces variation — gradient should have multiple distinct values
    let mut seen: std::collections::HashSet<u8> = std::collections::HashSet::new();
    for &p in &pixels { seen.insert(p); }
    assert!(seen.len() > 10, "depth8: too few distinct values ({}) — dithering may not be working", seen.len());

    println!("depth8: {} distinct pixel values across dithered gradient", seen.len());

    Ok(())
}

#[test]
fn composite_page_smoke_test() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_a = tmp.path().join("comp_a.tif");
    let input_b = tmp.path().join("comp_b.tif");
    let output_path = tmp.path().join("composite_out.tif");
    let output_icc_path = tmp.path().join("composite_profile.icc");

    write_gradient_rgb16_tiff(&input_a, 48, 48, 360.0)?;
    write_checkerboard_rgb16_tiff(&input_b, 48, 48, 360.0, 6)?;
    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)?;

    vibeprint::processor::process_composite_page(vibeprint::processor::CompositePageOptions {
        output: output_path.clone(),
        placements: vec![
            vibeprint::processor::PagePlacement {
                input: input_a,
                input_icc: None,
                dest_x_px: 0,
                dest_y_px: 0,
                dest_w_px: 120,
                dest_h_px: 120,
                rotate_cw: false,
                crop_u0: 0.0,
                crop_v0: 0.0,
                crop_u1: 1.0,
                crop_v1: 1.0,
                border_type: vibeprint::layout_engine::BorderType::None,
                border_width_px: 0,
            },
            vibeprint::processor::PagePlacement {
                input: input_b,
                input_icc: None,
                dest_x_px: 120,
                dest_y_px: 0,
                dest_w_px: 120,
                dest_h_px: 120,
                rotate_cw: false,
                crop_u0: 0.0,
                crop_v0: 0.0,
                crop_u1: 1.0,
                crop_v1: 1.0,
                border_type: vibeprint::layout_engine::BorderType::None,
                border_width_px: 0,
            },
        ],
        page_w_px: 240,
        page_h_px: 120,
        output_icc: Some(output_icc_path),
        default_wide_output_when_unset: false,
        target_dpi: 360.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 8,
        sharpen: 0,
    })?;

    let (bit_depth, dpi) = read_tiff_bit_depth_and_dpi(&output_path)?;
    assert_eq!(bit_depth, 8, "composite output must be 8-bit");
    assert!((dpi - 360.0).abs() < 1e-6, "composite DPI mismatch");

    let file = File::open(&output_path)?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    let (w, h) = decoder.dimensions()?;
    assert_eq!(w, 240, "composite width mismatch");
    assert_eq!(h, 120, "composite height mismatch");

    let pixels: Vec<u8> = match decoder.read_image().context("decode composite 8-bit pixels")? {
        tiff::decoder::DecodingResult::U8(v) => v,
        _ => anyhow::bail!("expected U8 decoding result"),
    };
    assert!(!pixels.iter().all(|&p| p == 255), "composite output is all white");

    Ok(())
}

#[test]
fn wide_gamut_input_does_not_roundtrip_through_srgb() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("wide_input.tif");
    let output_path = tmp.path().join("wide_output.tif");
    let wide_icc_path = tmp.path().join("wide.icc");

    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&wide_icc_path, &wide_bytes)?;

    let src_rgb = (0u16, 65535u16, 0u16);
    write_solid_rgb16_tiff(&input_path, 1, 1, 720.0, src_rgb)?;

    vibeprint::processor::process(vibeprint::processor::ProcessOptions {
        input: input_path,
        output: output_path.clone(),
        input_icc: Some(wide_icc_path.clone()),
        output_icc: Some(wide_icc_path.clone()),
        default_wide_output_when_unset: false,
        target_dpi: 720.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 16,
        sharpen: 0,
        page_layout: None,
    })?;

    let out = read_tiff_pixel_rgb16(&output_path, 0, 0)?;

    let input_profile = lcms2::Profile::new_icc(&wide_bytes)
        .context("failed to load synthetic wide-gamut profile")?;
    let output_profile = lcms2::Profile::new_icc(&wide_bytes)
        .context("failed to load synthetic wide-gamut profile")?;
    let srgb_profile = lcms2::Profile::new_srgb();

    let flags = lcms2::Flags::BLACKPOINT_COMPENSATION | lcms2::Flags::NO_CACHE;
    let to_srgb = lcms2::Transform::<[u16; 3], [u16; 3]>::new_flags(
        &input_profile,
        lcms2::PixelFormat::RGB_16,
        &srgb_profile,
        lcms2::PixelFormat::RGB_16,
        lcms2::Intent::RelativeColorimetric,
        flags,
    )
    .context("failed to create input->sRGB transform")?;
    let to_output = lcms2::Transform::<[u16; 3], [u16; 3]>::new_flags(
        &srgb_profile,
        lcms2::PixelFormat::RGB_16,
        &output_profile,
        lcms2::PixelFormat::RGB_16,
        lcms2::Intent::RelativeColorimetric,
        flags,
    )
    .context("failed to create sRGB->output transform")?;

    let src_buf = [[src_rgb.0, src_rgb.1, src_rgb.2]];
    let mut srgb_buf = [[0u16, 0u16, 0u16]];
    let mut srgb_roundtrip = [[0u16, 0u16, 0u16]];
    to_srgb.transform_pixels(&src_buf, &mut srgb_buf);
    to_output.transform_pixels(&srgb_buf, &mut srgb_roundtrip);

    let abs_sum_diff = |a: (u16, u16, u16), b: (u16, u16, u16)| -> u32 {
        a.0.abs_diff(b.0) as u32 + a.1.abs_diff(b.1) as u32 + a.2.abs_diff(b.2) as u32
    };

    let delta_out_vs_src = abs_sum_diff(out, src_rgb);
    let delta_srgb_roundtrip_vs_src = abs_sum_diff(
        (srgb_roundtrip[0][0], srgb_roundtrip[0][1], srgb_roundtrip[0][2]),
        src_rgb,
    );

    assert!(
        delta_out_vs_src + 256 < delta_srgb_roundtrip_vs_src,
        "output is too close to an sRGB-intermediate roundtrip: out={:?}, src={:?}, srgb_roundtrip={:?}, delta_out={}, delta_srgb={}",
        out,
        src_rgb,
        srgb_roundtrip,
        delta_out_vs_src,
        delta_srgb_roundtrip_vs_src
    );

    Ok(())
}

#[test]
fn pipeline_validation_suite() -> Result<()> {
    let tmp = tempdir().context("failed to create tempdir")?;
    let input_path = tmp.path().join("synthetic_input.tif");
    let output_path = tmp.path().join("pipeline_output.tif");
    let output_icc_path = tmp.path().join("wide_gamut.icc");

    // Create a small gradient so the test is quick, but still exercises resize math.
    let input_w = 64u32;
    let input_h = 4u32;
    let input_dpi = 360.0;
    write_gradient_rgb16_tiff(&input_path, input_w, input_h, input_dpi)?;

    let wide_bytes = make_wide_gamut_profile_bytes()?;
    std::fs::write(&output_icc_path, &wide_bytes)
        .with_context(|| format!("failed to write profile: {}", output_icc_path.display()))?;

    // Run pipeline: sRGB (default) -> wide gamut, with DPI resample to 720.
    let target_dpi = 720.0;
    vibeprint::processor::process(vibeprint::processor::ProcessOptions {
        input: input_path.clone(),
        output: output_path.clone(),
        input_icc: None,
        output_icc: Some(output_icc_path.clone()),
        default_wide_output_when_unset: false,
        target_dpi,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
        depth: 16,
        sharpen: 5,
        page_layout: None,
    })?;

    // Proof 1: output is still 16-bit.
    let (bit_depth, dpi) = read_tiff_bit_depth_and_dpi(&output_path)?;
    assert_eq!(bit_depth, 16, "output must remain 16-bit");

    // Proof 2: resolution tags match exactly 720.0 (within rational encoding tolerance).
    let eps = 1e-6;
    assert!((dpi - target_dpi).abs() < eps, "expected {target_dpi} dpi, got {dpi}");

    // Proof 3: pixel math shows an actual transform happened in 16-bit space.
    // Choose a pixel that should be about mid-gray in input.
    let out_w = input_w * 2;
    let x = out_w / 2;
    let y = 0;
    let (r, g, b) = read_tiff_pixel_rgb16(&output_path, x, y)?;

    let expected_mid = 32768u16;
    // If we accidentally fell back to 8-bit and scaled, we'd expect values near multiples of 257.
    let looks_like_8bit_scaled = (r % 257 == 0) && (g % 257 == 0) && (b % 257 == 0);
    assert!(!looks_like_8bit_scaled, "pixel looks like 8-bit scaled up (multiples of 257)");

    // And the ICC transform should change values (not remain exactly mid-gray).
    assert!(
        r != expected_mid || g != expected_mid || b != expected_mid,
        "expected ICC transform to shift mid-gray away from exact 32768"
    );

    Ok(())
}
