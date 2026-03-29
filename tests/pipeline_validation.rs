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

fn read_tiff_pixels_u16(path: &Path) -> Result<Vec<u16>> {
    let file = File::open(path).with_context(|| format!("failed to open: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    match decoder.read_image().context("failed to decode pixels")? {
        tiff::decoder::DecodingResult::U16(v) => Ok(v),
        _ => anyhow::bail!("expected u16 decoding result"),
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
            output_icc: output_icc_path.clone(),
            target_dpi,
            intent: lcms2::Intent::RelativeColorimetric,
            bpc: true,
            engine: engine.clone(),
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
        output_icc: output_icc_path,
        target_dpi: 720.0,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::IterativeStep,
    })?;

    let file = File::open(&output_path)?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))?;
    let (w, h) = decoder.dimensions()?;
    assert_eq!(w, 240, "iterative-step: expected width 240, got {}", w);
    assert_eq!(h, 180, "iterative-step: expected height 180, got {}", h);

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
        output_icc: output_icc_path.clone(),
        target_dpi,
        intent: lcms2::Intent::RelativeColorimetric,
        bpc: true,
        engine: vibeprint::processor::ResampleEngine::Mks,
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
