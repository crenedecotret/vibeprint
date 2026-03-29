use std::{fs::File, io::BufReader, path::Path};

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, ImageBuffer, Rgb};

type Rgb16Image = ImageBuffer<Rgb<u16>, Vec<u16>>;

pub struct ProcessOptions {
    pub input: std::path::PathBuf,
    pub output: std::path::PathBuf,
    pub input_icc: std::path::PathBuf,
    pub output_icc: std::path::PathBuf,
    pub target_dpi: f64,
}

pub fn process(opts: ProcessOptions) -> Result<()> {
    if opts.target_dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let (img, source_dpi) = load_rgb16_tiff_with_dpi(&opts.input)?;

    let source_dpi = source_dpi.unwrap_or(opts.target_dpi);
    let (new_w, new_h) = scaled_dimensions(img.width(), img.height(), source_dpi, opts.target_dpi);

    let resized: Rgb16Image = if new_w != img.width() || new_h != img.height() {
        image::imageops::resize(&img, new_w, new_h, FilterType::Lanczos3)
    } else {
        img
    };

    let transformed = transform_rgb16_icc(&resized, &opts.input_icc, &opts.output_icc)?;

    save_rgb16_tiff_with_dpi(&opts.output, &transformed, opts.target_dpi)?;

    Ok(())
}

fn scaled_dimensions(w: u32, h: u32, source_dpi: f64, target_dpi: f64) -> (u32, u32) {
    let scale = target_dpi / source_dpi;
    let new_w = ((w as f64) * scale).round().max(1.0) as u32;
    let new_h = ((h as f64) * scale).round().max(1.0) as u32;
    (new_w, new_h)
}

fn load_rgb16_tiff_with_dpi(path: &Path) -> Result<(Rgb16Image, Option<f64>)> {
    let file = File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file)).context("failed to create TIFF decoder")?;

    let (w, h) = decoder.dimensions().context("failed to read TIFF dimensions")?;
    let color_type = decoder.colortype().context("failed to read TIFF color type")?;

    let dpi = read_tiff_dpi(&mut decoder);

    match color_type {
        tiff::ColorType::RGB(16) => {
            let data = decoder.read_image().context("failed to read TIFF image")?;
            let data: Vec<u16> = match data {
                tiff::decoder::DecodingResult::U16(v) => v,
                _ => bail!("unexpected TIFF decoding result; expected u16"),
            };
            let img: Rgb16Image = ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(w, h, data)
                .context("failed to construct RGB16 image buffer")?;
            Ok((img, dpi))
        }
        _ => bail!("only 16-bit RGB TIFF is supported for processing"),
    }
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

fn transform_rgb16_icc(img: &Rgb16Image, input_icc: &Path, output_icc: &Path) -> Result<Rgb16Image> {
    let in_prof = lcms2::Profile::new_file(input_icc)
        .with_context(|| format!("failed to load input ICC: {}", input_icc.display()))?;
    let out_prof = lcms2::Profile::new_file(output_icc)
        .with_context(|| format!("failed to load output ICC: {}", output_icc.display()))?;

    let transform = lcms2::Transform::new_flags(
        &in_prof,
        lcms2::PixelFormat::RGB_16,
        &out_prof,
        lcms2::PixelFormat::RGB_16,
        lcms2::Intent::Perceptual,
        lcms2::Flags::NO_CACHE,
    )
    .context("failed to create lcms2 transform")?;

    let input_raw: Vec<u16> = img.as_raw().clone();
    let mut output_raw: Vec<u16> = vec![0u16; input_raw.len()];

    transform.transform_pixels(&input_raw, &mut output_raw);

    let out = ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(img.width(), img.height(), output_raw)
        .context("failed to construct transformed RGB16 image")?;

    Ok(out)
}

fn save_rgb16_tiff_with_dpi(path: &Path, img: &Rgb16Image, dpi: f64) -> Result<()> {
    use tiff::encoder::{colortype, TiffEncoder};
    use tiff::tags::Tag;

    if dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let file = File::create(path).with_context(|| format!("failed to create output file: {}", path.display()))?;
    let mut encoder = TiffEncoder::new(file).context("failed to create TIFF encoder")?;
    let mut image = encoder
        .new_image::<colortype::RGB16>(img.width(), img.height())
        .context("failed to create TIFF image")?;

    let (n, d) = dpi_to_rational(dpi);
    let _ = image.encoder().write_tag(Tag::XResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::YResolution, tiff::encoder::Rational { n, d });
    let _ = image.encoder().write_tag(Tag::ResolutionUnit, 2u16);

    image
        .write_data(img.as_raw())
        .context("failed to write TIFF pixel data")?;

    Ok(())
}

fn dpi_to_rational(dpi: f64) -> (u32, u32) {
    let d = 10000u32;
    let n = (dpi * (d as f64)).round().max(0.0) as u32;
    (n, d)
}
