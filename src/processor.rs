use std::{fs::File, io::BufReader, path::Path};

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, DynamicImage, ImageBuffer, Rgb};

type Rgb16Image = ImageBuffer<Rgb<u16>, Vec<u16>>;
type Rgb8Image = ImageBuffer<Rgb<u8>, Vec<u8>>;

enum LoadedImage {
    Rgb8(Rgb8Image),
    Rgb16(Rgb16Image),
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
    RobidouxEwa,
}

impl ResampleEngine {
    pub fn display_name(&self) -> &'static str {
        match self {
            ResampleEngine::Mks           => "MKS (Magic Kernel Sharp)",
            ResampleEngine::Lanczos3      => "Lanczos3",
            ResampleEngine::IterativeStep => "Iterative-Step",
            ResampleEngine::RobidouxEwa   => "Robidoux-EWA",
        }
    }
}

pub struct ProcessOptions {
    pub input: std::path::PathBuf,
    pub output: std::path::PathBuf,
    pub input_icc: Option<std::path::PathBuf>,
    pub output_icc: std::path::PathBuf,
    pub target_dpi: f64,
    pub intent: lcms2::Intent,
    pub bpc: bool,
    pub engine: ResampleEngine,
}

pub fn process(opts: ProcessOptions) -> Result<()> {
    if opts.target_dpi <= 0.0 {
        bail!("dpi must be > 0");
    }

    let (img, source_dpi, embedded_icc) = load_image_with_dpi_and_embedded_icc(&opts.input)?;

    // Convert to 16-bit before resize so all engines operate at full depth
    let img16: Rgb16Image = match img {
        LoadedImage::Rgb8(im)  => rgb8_to_rgb16(&im),
        LoadedImage::Rgb16(im) => im,
    };

    let source_dpi = source_dpi.unwrap_or(opts.target_dpi);
    let (new_w, new_h) = scaled_dimensions(img16.width(), img16.height(), source_dpi, opts.target_dpi);

    println!("VibePrint Engine: {} initialized.", opts.engine.display_name());
    let resized = resize_rgb16(&img16, new_w, new_h, &opts.engine);

    let input_profile = match (opts.input_icc.as_ref(), embedded_icc.as_ref()) {
        (Some(path), _) => {
            println!("Using input ICC: {}", path.display());
            lcms2::Profile::new_file(path)
                .with_context(|| format!("failed to load input ICC: {}", path.display()))?
        }
        (None, Some(_)) => {
            println!("Using embedded ICC profile");
            lcms2::Profile::new_icc(embedded_icc.as_ref().unwrap())
                .context("failed to load embedded ICC profile")?
        }
        (None, None) => {
            println!("No profile found, defaulting to sRGB");
            lcms2::Profile::new_srgb()
        }
    };

    println!("Using destination ICC: {}", opts.output_icc.display());
    let output_icc_bytes = std::fs::read(&opts.output_icc)
        .with_context(|| format!("failed to read output ICC bytes: {}", opts.output_icc.display()))?;
    let output_profile = lcms2::Profile::new_file(&opts.output_icc)
        .with_context(|| format!("failed to load output ICC: {}", opts.output_icc.display()))?;

    let intent_name = match opts.intent {
        lcms2::Intent::Perceptual             => "Perceptual",
        lcms2::Intent::RelativeColorimetric   => "Relative Colorimetric",
        lcms2::Intent::Saturation             => "Saturation",
        lcms2::Intent::AbsoluteColorimetric   => "Absolute Colorimetric",
        _                                     => "Unknown",
    };
    println!(
        "Applying {} transform {} Black Point Compensation.",
        intent_name,
        if opts.bpc { "with" } else { "without" }
    );

    let icc_filename = opts.output_icc
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let description = format!(
        "vibeprint | Engine: {} | Intent: {} | BPC: {} | DPI: {} | Output ICC: {}",
        opts.engine.display_name(),
        intent_name,
        if opts.bpc { "Enabled" } else { "Disabled" },
        opts.target_dpi,
        icc_filename,
    );

    let transformed = transform_rgb16_icc(&resized, &input_profile, &output_profile, opts.intent, opts.bpc)?;
    save_rgb16_tiff_with_dpi(&opts.output, &transformed, opts.target_dpi, &output_icc_bytes, &description)?;

    Ok(())
}

fn scaled_dimensions(w: u32, h: u32, source_dpi: f64, target_dpi: f64) -> (u32, u32) {
    let scale = target_dpi / source_dpi;
    let new_w = ((w as f64) * scale).round().max(1.0) as u32;
    let new_h = ((h as f64) * scale).round().max(1.0) as u32;
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

fn resize_rgb16(img: &Rgb16Image, new_w: u32, new_h: u32, engine: &ResampleEngine) -> Rgb16Image {
    if new_w == img.width() && new_h == img.height() {
        return img.clone();
    }
    match engine {
        ResampleEngine::Mks           => image::imageops::resize(img, new_w, new_h, FilterType::CatmullRom),
        ResampleEngine::Lanczos3      => image::imageops::resize(img, new_w, new_h, FilterType::Lanczos3),
        ResampleEngine::IterativeStep => resize_iterative_step(img, new_w, new_h),
        ResampleEngine::RobidouxEwa   => resize_ewa_robidoux(img, new_w, new_h),
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
            ((cur_w as f64) * 1.1).round() as u32
        } else {
            ((cur_w as f64) / 1.1).round().max(1.0) as u32
        };
        let next_h = if target_h > cur_h {
            ((cur_h as f64) * 1.1).round() as u32
        } else {
            ((cur_h as f64) / 1.1).round().max(1.0) as u32
        };
        let next_w = if target_w > cur_w { next_w.min(target_w) } else { next_w.max(target_w) };
        let next_h = if target_h > cur_h { next_h.min(target_h) } else { next_h.max(target_h) };
        current = image::imageops::resize(&current, next_w, next_h, FilterType::Lanczos3);
    }
    current
}

#[inline]
fn robidoux_kernel(t: f64) -> f64 {
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

fn resize_ewa_robidoux(img: &Rgb16Image, dst_w: u32, dst_h: u32) -> Rgb16Image {
    let src_w = img.width() as f64;
    let src_h = img.height() as f64;
    let scale_x = src_w / (dst_w as f64);
    let scale_y = src_h / (dst_h as f64);

    // Radius in input-pixel space: 2 for upscaling, 2*scale for downscaling (anti-alias)
    let radius_x = scale_x.max(1.0) * 2.0;
    let radius_y = scale_y.max(1.0) * 2.0;

    let src_max_x = img.width() as i64 - 1;
    let src_max_y = img.height() as i64 - 1;
    let mut output: Vec<u16> = vec![0u16; (dst_w * dst_h * 3) as usize];

    for oy in 0..dst_h {
        for ox in 0..dst_w {
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
                for sx in x0..=x1 {
                    let dx = (sx as f64 - ix) / radius_x;
                    // EWA: circular support — skip samples outside the unit disc
                    let r2 = dx * dx + dy * dy;
                    if r2 >= 1.0 {
                        continue;
                    }
                    let w = robidoux_kernel(r2.sqrt() * 2.0);
                    if w == 0.0 {
                        continue;
                    }
                    let csx = sx.clamp(0, src_max_x) as u32;
                    let csy = sy.clamp(0, src_max_y) as u32;
                    let px = img.get_pixel(csx, csy);
                    sum_r += w * px[0] as f64;
                    sum_g += w * px[1] as f64;
                    sum_b += w * px[2] as f64;
                    sum_w += w;
                }
            }

            let idx = ((oy * dst_w + ox) * 3) as usize;
            if sum_w > 1e-10 {
                output[idx]     = (sum_r / sum_w).clamp(0.0, 65535.0).round() as u16;
                output[idx + 1] = (sum_g / sum_w).clamp(0.0, 65535.0).round() as u16;
                output[idx + 2] = (sum_b / sum_w).clamp(0.0, 65535.0).round() as u16;
            }
        }
    }

    ImageBuffer::from_raw(dst_w, dst_h, output).expect("ewa_robidoux: buffer size mismatch")
}

fn load_image_with_dpi_and_embedded_icc(path: &Path) -> Result<(LoadedImage, Option<f64>, Option<Vec<u8>>)> {
    let dyn_img = image::open(path).with_context(|| format!("failed to decode image: {}", path.display()))?;

    let (dpi, embedded_icc) = if is_tiff_path(path) {
        read_tiff_dpi_and_embedded_icc(path).unwrap_or((None, None))
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

fn dynamic_to_rgb8_or_rgb16(img: DynamicImage) -> Result<LoadedImage> {
    Ok(match img {
        DynamicImage::ImageRgb16(im) => LoadedImage::Rgb16(im),
        DynamicImage::ImageRgba16(im) => LoadedImage::Rgb16(image::DynamicImage::ImageRgba16(im).to_rgb16()),
        DynamicImage::ImageLuma16(im) => LoadedImage::Rgb16(image::DynamicImage::ImageLuma16(im).to_rgb16()),
        DynamicImage::ImageLumaA16(im) => LoadedImage::Rgb16(image::DynamicImage::ImageLumaA16(im).to_rgb16()),
        _ => LoadedImage::Rgb8(img.to_rgb8()),
    })
}

fn read_tiff_dpi_and_embedded_icc(path: &Path) -> Result<(Option<f64>, Option<Vec<u8>>)> {
    let file = File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file)).context("failed to create TIFF decoder")?;
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

fn read_tiff_embedded_icc(decoder: &mut tiff::decoder::Decoder<BufReader<File>>) -> Option<Vec<u8>> {
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

    let input_raw: Vec<u16> = img.as_raw().clone();
    if input_raw.len() % 3 != 0 {
        bail!("expected interleaved RGB16 buffer length to be divisible by 3");
    }

    let mut input_pixels: Vec<Rgb16Pixel> = Vec::with_capacity(input_raw.len() / 3);
    for ch in input_raw.chunks_exact(3) {
        input_pixels.push(Rgb16Pixel {
            r: ch[0],
            g: ch[1],
            b: ch[2],
        });
    }

    let mut output_pixels: Vec<Rgb16Pixel> = vec![Rgb16Pixel { r: 0, g: 0, b: 0 }; input_pixels.len()];
    transform.transform_pixels(&input_pixels, &mut output_pixels);

    let mut output_raw: Vec<u16> = Vec::with_capacity(input_raw.len());
    for px in output_pixels {
        output_raw.push(px.r);
        output_raw.push(px.g);
        output_raw.push(px.b);
    }

    let out = ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(img.width(), img.height(), output_raw)
        .context("failed to construct transformed RGB16 image")?;

    Ok(out)
}

fn save_rgb16_tiff_with_dpi(path: &Path, img: &Rgb16Image, dpi: f64, output_icc_bytes: &[u8], description: &str) -> Result<()> {
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

    let _ = image.encoder().write_tag(Tag::IccProfile, output_icc_bytes);
    let _ = image.encoder().write_tag(Tag::from_u16_exhaustive(40961), 65535u16);
    let _ = image.encoder().write_tag(Tag::ImageDescription, description);

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
