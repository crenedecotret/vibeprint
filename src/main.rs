use std::{fs::File, io::BufReader, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use image::GenericImageView;
use vibeprint::processor;

#[derive(Parser, Debug)]
#[command(name = "vibeprint", version, about = "Image layout + color-managed printing (prototype)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::ValueEnum, Debug, Clone)]
enum CliEngine {
    /// Magic Kernel Sharp — Catmull-Rom cubic (default)
    Mks,
    /// Classic Lanczos3
    Lanczos3,
    /// Iterative 1.1x step upscaling (Fusion-style)
    #[value(name = "iterative-step")]
    IterativeStep,
    /// Elliptical Weighted Average with Robidoux coefficients
    #[value(name = "robidoux-ewa")]
    RobidouxEwa,
}

#[derive(clap::ValueEnum, Debug, Clone)]
enum OutputDepth {
    /// 8-bit output with Floyd-Steinberg dithering
    #[value(name = "8")]
    Eight,
    /// 16-bit output (default, full fidelity)
    #[value(name = "16")]
    Sixteen,
}

#[derive(clap::ValueEnum, Debug, Clone)]
enum ColorIntent {
    /// Perceptual rendering intent
    Perceptual,
    /// Relative colorimetric (default)
    Relative,
    /// Saturation rendering intent
    Saturation,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print metadata for an input image (TIFF/JPEG/PNG/etc)
    Meta {
        /// Path to image file
        input: PathBuf,
    },

    /// Resample image to a target DPI and apply ICC color transform, output 16-bit TIFF
    Process {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long = "input-icc")]
        input_icc: Option<PathBuf>,
        /// Output ICC profile (omit to use embedded/sRGB as passthrough)
        #[arg(long = "output-icc")]
        output_icc: Option<PathBuf>,
        #[arg(long = "dpi")]
        dpi: f64,
        /// Rendering intent for ICC color transform (default: relative)
        #[arg(long = "intent", default_value = "relative")]
        intent: ColorIntent,
        /// Explicitly enable Black Point Compensation (default: on for relative, off for others)
        #[arg(long = "bpc", conflicts_with = "no_bpc")]
        bpc: bool,
        /// Explicitly disable Black Point Compensation
        #[arg(long = "no-bpc", conflicts_with = "bpc")]
        no_bpc: bool,
        /// Resampling engine (default: mks)
        #[arg(long = "engine", default_value = "mks")]
        engine: CliEngine,
        /// Output bit depth: 8 (dithered) or 16 (default)
        #[arg(long = "depth", default_value = "16")]
        depth: OutputDepth,
        /// Unsharp mask sharpening level 0–20 (0=off, default 5)
        #[arg(long = "sharpen", default_value = "5", value_parser = clap::value_parser!(u8).range(0..=20))]
        sharpen: u8,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Meta { input } => print_metadata(&input),
        Command::Process {
            input,
            output,
            input_icc,
            output_icc,
            dpi,
            intent,
            bpc,
            no_bpc,
            engine,
            depth,
            sharpen,
        } => processor::process(processor::ProcessOptions {
            input,
            output,
            input_icc,
            output_icc,
            target_dpi: dpi,
            intent: match intent {
                ColorIntent::Perceptual => lcms2::Intent::Perceptual,
                ColorIntent::Relative   => lcms2::Intent::RelativeColorimetric,
                ColorIntent::Saturation => lcms2::Intent::Saturation,
            },
            bpc: if bpc { true } else if no_bpc { false } else { matches!(intent, ColorIntent::Relative) },
            engine: match engine {
                CliEngine::Mks           => processor::ResampleEngine::Mks,
                CliEngine::Lanczos3      => processor::ResampleEngine::Lanczos3,
                CliEngine::IterativeStep => processor::ResampleEngine::IterativeStep,
                CliEngine::RobidouxEwa   => processor::ResampleEngine::RobidouxEwa,
            },
            depth: match depth {
                OutputDepth::Eight   => 8,
                OutputDepth::Sixteen => 16,
            },
            sharpen,
        }),
    }
}

fn print_metadata(path: &PathBuf) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "tif" || ext == "tiff" {
        print_tiff_metadata(path)
    } else {
        print_generic_metadata(path)
    }
}

fn print_generic_metadata(path: &PathBuf) -> Result<()> {
    let reader = image::ImageReader::open(path)
        .with_context(|| format!("failed to open image: {}", path.display()))?
        .with_guessed_format()
        .context("failed to guess image format")?;

    let format = reader.format();
    let dyn_img = reader.decode().context("failed to decode image")?;

    let (w, h) = dyn_img.dimensions();

    println!("File: {}", path.display());
    println!("Format: {}", format.map(|f| format!("{f:?}")).unwrap_or("Unknown".to_string()));
    println!("Dimensions: {} x {} px", w, h);

    let (bit_depth, color_space_hint) = match dyn_img {
        image::DynamicImage::ImageLuma8(_) => (8u32, "Gray"),
        image::DynamicImage::ImageLumaA8(_) => (8u32, "Gray+Alpha"),
        image::DynamicImage::ImageRgb8(_) => (8u32, "RGB"),
        image::DynamicImage::ImageRgba8(_) => (8u32, "RGB+Alpha"),
        image::DynamicImage::ImageLuma16(_) => (16u32, "Gray"),
        image::DynamicImage::ImageLumaA16(_) => (16u32, "Gray+Alpha"),
        image::DynamicImage::ImageRgb16(_) => (16u32, "RGB"),
        image::DynamicImage::ImageRgba16(_) => (16u32, "RGB+Alpha"),
        _ => (0u32, "Unknown"),
    };

    println!("Bit depth: {}", if bit_depth == 0 { "Unknown".to_string() } else { format!("{}-bit", bit_depth) });
    println!("Color space hint: {}", color_space_hint);
    println!("DPI: Unknown (not available via image crate for this format)");

    Ok(())
}

fn print_tiff_metadata(path: &PathBuf) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to open TIFF: {}", path.display()))?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file)).context("failed to create TIFF decoder")?;

    let dims = decoder.dimensions().context("failed to read TIFF dimensions")?;
    let ct = decoder.colortype().context("failed to read TIFF color type")?;

    println!("File: {}", path.display());
    println!("Format: TIFF");
    println!("Dimensions: {} x {} px", dims.0, dims.1);

    // Bits/sample + color model from decoder's color type.
    let (bit_depth, color_space_hint) = match ct {
        tiff::ColorType::Gray(bps) => (bps as u32, "Gray"),
        tiff::ColorType::GrayA(bps) => (bps as u32, "Gray+Alpha"),
        tiff::ColorType::RGB(bps) => (bps as u32, "RGB"),
        tiff::ColorType::RGBA(bps) => (bps as u32, "RGB+Alpha"),
        tiff::ColorType::CMYK(bps) => (bps as u32, "CMYK"),
        tiff::ColorType::YCbCr(bps) => (bps as u32, "YCbCr"),
        tiff::ColorType::Palette(bps) => (bps as u32, "Palette"),
        _ => (0u32, "Unknown"),
    };

    if bit_depth == 0 {
        println!("Bit depth: Unknown");
    } else {
        println!("Bit depth: {}-bit", bit_depth);
    }
    println!("Color space hint: {}", color_space_hint);

    // DPI: from resolution tags if present.
    // NOTE: The tiff crate's tag API is limited; we best-effort parse XResolution/YResolution.
    let (x_dpi, y_dpi, unit) = read_tiff_resolution(&mut decoder).unwrap_or((None, None, None));

    match (x_dpi, y_dpi, unit) {
        (Some(x), Some(y), Some(u)) => println!("DPI: {:.4} x {:.4} ({})", x, y, u),
        (Some(x), Some(y), None) => println!("DPI: {:.4} x {:.4}", x, y),
        (Some(x), None, Some(u)) => println!("DPI: {:.4} ({})", x, u),
        (Some(x), None, None) => println!("DPI: {:.4}", x),
        _ => println!("DPI: Unknown"),
    }

    Ok(())
}

fn read_tiff_resolution(
    decoder: &mut tiff::decoder::Decoder<BufReader<File>>,
) -> Result<(Option<f64>, Option<f64>, Option<String>)> {
    use tiff::tags::Tag;

    // XResolution and YResolution are RATIONAL.
    let x = get_tag_rational_first(decoder, Tag::XResolution).map(rational_to_f64);
    let y = get_tag_rational_first(decoder, Tag::YResolution).map(rational_to_f64);

    // ResolutionUnit: 1=NoUnit, 2=Inch, 3=Centimeter
    let unit_raw: Option<u16> = get_tag_u16(decoder, Tag::ResolutionUnit);
    let unit = unit_raw.map(|v| match v {
        2 => "inch".to_string(),
        3 => "centimeter".to_string(),
        1 => "none".to_string(),
        _ => format!("unknown({})", v),
    });

    // Convert pixels/cm to pixels/in if needed.
    let (x, y) = match unit.as_deref() {
        Some("centimeter") => (x.map(|v| v * 2.54), y.map(|v| v * 2.54)),
        _ => (x, y),
    };

    Ok((x, y, unit))
}

fn get_tag_rational_first(
    decoder: &mut tiff::decoder::Decoder<BufReader<File>>,
    tag: tiff::tags::Tag,
) -> Option<(u32, u32)> {
    use tiff::decoder::ifd::Value;

    match decoder.get_tag(tag).ok()? {
        Value::Rational(n, d) => Some((n, d)),
        _ => None,
    }
}

fn get_tag_u16(
    decoder: &mut tiff::decoder::Decoder<BufReader<File>>,
    tag: tiff::tags::Tag,
) -> Option<u16> {
    use tiff::decoder::ifd::Value;

    match decoder.get_tag(tag).ok()? {
        Value::Short(v) => Some(v),
        _ => None,
    }
}

fn rational_to_f64(r: (u32, u32)) -> f64 {
    if r.1 == 0 {
        return f64::NAN;
    }
    (r.0 as f64) / (r.1 as f64)
}

#[allow(dead_code)]
fn _lcms2_smoke_test() -> Result<()> {
    let _ = lcms2::Profile::new_srgb();
    Ok(())
}

fn _ensure_path_exists(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        bail!("file does not exist: {}", path.display());
    }
    Ok(())
}
