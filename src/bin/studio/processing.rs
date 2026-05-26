use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use vibeprint::printer_discovery::{PrinterCaps, PrinterInfo};

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Calculate the PostScript translate offset to place a TIFF (sized to the
/// imageable area) at the correct position on the physical page.
/// `imageable_area` is `(left_pt, bottom_pt, right_pt, top_pt)` from CUPS.
/// Returns `(offset_x_pts, offset_y_pts)` — the translation in PostScript points.
fn calc_print_offset(imageable_area: (f32, f32, f32, f32)) -> (f32, f32) {
    (imageable_area.0, imageable_area.1)
}

/// Print job submission (sync version for background thread)
pub(crate) fn submit_print_jobs_sync(
    temp_paths: &[PathBuf],
    caps: Option<PrinterCaps>,
    printer_idx: usize,
    printers: &[PrinterInfo],
    selected_page_size_idx: usize,
    props_media_idx: usize,
    props_slot_idx: usize,
    extra_option_indices: &HashMap<String, usize>,
    log_tx: &Sender<String>,
) -> Result<(), String> {
    if temp_paths.is_empty() {
        return Err("No pages to print".into());
    }
    let caps = caps.ok_or("No printer selected")?;
    let printer = printers.get(printer_idx).ok_or("No printer selected")?;

    // Build the lpr -o option list from user selections
    let mut lpr_opts: Vec<String> = Vec::new();

    // Prevent CUPS auto-scaling — our TIFF is already sized to imageable area
    lpr_opts.push("-o print-scaling=none".to_string());

    // Paper size: use the PWG media name (e.g. "na_letter_8.5x11in")
    if let Some(ps) = caps.page_sizes.get(selected_page_size_idx) {
        lpr_opts.push(format!("-o media={}", ps.name));
    }

    // Media type: use the IPP keyword (e.g. "photographic-glossy")
    if let Some((key, _)) = caps.media_types.get(props_media_idx) {
        lpr_opts.push(format!("-o media-type={}", key));
    }

    // Input slot: use the IPP keyword (e.g. "auto", "cd")
    if let Some((key, _)) = caps.input_slots.get(props_slot_idx) {
        lpr_opts.push(format!("-o media-source={}", key));
    }

    // Extra options (color mode, duplex, quality, etc.)
    for opt in &caps.extra_options {
        if let Some(&idx) = extra_option_indices.get(&opt.key) {
            if let Some((choice_key, _)) = opt.choices.get(idx) {
                lpr_opts.push(format!("-o {}={}", opt.key, choice_key));
            }
        }
    }

    let opts_str = lpr_opts.join(" ");

    for (i, temp_path) in temp_paths.iter().enumerate() {
        let _ = log_tx.send(format!("Processing page {} of {}...", i + 1, temp_paths.len()));

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let pid = std::process::id();

        let temp_path_q = shell_quote(&temp_path.display().to_string());
        let _ = log_tx.send(format!("Page {}: Converting to PDF...", i + 1));

        let pdf_path = format!("/tmp/vibeprint_{}_{}.pdf", timestamp, pid);
        let pdf_q = shell_quote(&pdf_path);

        // PDF page = full physical paper size in pts.
        // The TIFF is sized to the imageable area (paper minus borders).
        // tiff2ps places the image at PostScript origin (0,0) = bottom-left.
        // We wrap the PS with a translate to offset the image by the border amount
        // so it sits correctly on the physical sheet.
        let (paper_w_pts, paper_h_pts) = caps
            .page_sizes
            .get(selected_page_size_idx)
            .map(|ps| (ps.paper_size.0, ps.paper_size.1))
            .unwrap_or((612.0, 792.0));

        // Derive TIFF image size in pts from its pixel dimensions and embedded DPI.
        #[allow(unused_variables)]
        let (img_w_pts, img_h_pts) = {
            let mut w = paper_w_pts;
            let mut h = paper_h_pts;
            if let Ok(file) = std::fs::File::open(temp_path) {
                if let Ok(mut dec) = tiff::decoder::Decoder::new(file) {
                    if let Ok((px_w, px_h)) = dec.dimensions() {
                        let res_unit = dec.get_tag_u32(tiff::tags::Tag::ResolutionUnit).unwrap_or(2);
                        let xres = dec.get_tag_f32_vec(tiff::tags::Tag::XResolution)
                            .ok().and_then(|v| v.into_iter().next()).unwrap_or(72.0);
                        let dpi = if res_unit == 3 { xres * 2.54 } else { xres };
                        if dpi > 0.0 {
                            w = px_w as f32 / dpi * 72.0;
                            h = px_h as f32 / dpi * 72.0;
                        }
                    }
                }
            }
            (w, h)
        };

        // Offset in pts: place the TIFF at the reported border origin
        // (left_pt, bottom_pt) from the CUPS imageable area.
        let (offset_x, offset_y) = caps
            .page_sizes
            .get(selected_page_size_idx)
            .map(|ps| calc_print_offset(ps.imageable_area))
            .unwrap_or((0.0, 0.0));

        // Pipe: tiff2ps → prepend a translate → gs with full paper size
        let gs_cmd = format!(
            "{{ echo 'gsave {:.4} {:.4} translate'; tiff2ps {}; echo 'grestore'; }} \
             | gs -q -o {} -sDEVICE=pdfwrite \
             -sColorConversionStrategy=LeaveColorUnchanged \
             -dNOTRANSPARENCY \
             -dDEVICEWIDTHPOINTS={:.4} -dDEVICEHEIGHTPOINTS={:.4} -dFIXEDMEDIA \
             -dAutoFilterColorImages=false -sColorImageFilter=FlateEncode \
             -dAutoFilterGrayImages=false -sGrayImageFilter=FlateEncode \
             -dDownsampleColorImages=false -dDownsampleGrayImages=false \
             -",
            offset_x, offset_y, temp_path_q, pdf_q, paper_w_pts, paper_h_pts
        );

        let gs_output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&gs_cmd)
            .output()
            .map_err(|e| format!("Failed to run Ghostscript: {}", e))?;

        if !gs_output.status.success() {
            let stderr = String::from_utf8_lossy(&gs_output.stderr);
            return Err(format!("PDF conversion failed (page {}): {}", i + 1, stderr));
        }

        let _ = log_tx.send(format!("Page {}: Sending to printer...", i + 1));

        let printer_q = shell_quote(&printer.name);
        let lpr_cmd = format!("lpr -P {} {} {}", printer_q, opts_str, pdf_q);

        let lpr_result = std::process::Command::new("sh")
            .arg("-c")
            .arg(&lpr_cmd)
            .output()
            .map_err(|e| format!("Failed to run lpr: {}", e))?;

        if !lpr_result.status.success() {
            let stderr = String::from_utf8_lossy(&lpr_result.stderr);
            let _ = std::fs::remove_file(&pdf_path);
            return Err(format!("lpr failed (page {}): {}", i + 1, stderr));
        }

        let _ = std::fs::remove_file(&pdf_path);
    }

    Ok(())
}

/// Direct TIFF print job submission for safe 8-bit path (no PS/PDF conversion)
pub(crate) fn submit_print_jobs_direct_tiff(
    temp_paths: &[PathBuf],
    caps: Option<PrinterCaps>,
    printer_idx: usize,
    printers: &[PrinterInfo],
    selected_page_size_idx: usize,
    props_media_idx: usize,
    props_slot_idx: usize,
    extra_option_indices: &HashMap<String, usize>,
    log_tx: &Sender<String>,
) -> Result<(), String> {
    if temp_paths.is_empty() {
        return Err("No pages to print".into());
    }
    let caps = caps.ok_or("No printer selected")?;
    let printer = printers.get(printer_idx).ok_or("No printer selected")?;

    // Build the lpr -o option list from user selections (same as standard path)
    let mut lpr_opts: Vec<String> = Vec::new();

    // Prevent CUPS auto-scaling — our TIFF is already sized to imageable area
    lpr_opts.push("-o print-scaling=none".to_string());

    // Paper size: use the PWG media name (e.g. "na_letter_8.5x11in")
    if let Some(ps) = caps.page_sizes.get(selected_page_size_idx) {
        lpr_opts.push(format!("-o media={}", ps.name));
    }

    // Media type: use the IPP keyword (e.g. "photographic-glossy")
    if let Some((key, _)) = caps.media_types.get(props_media_idx) {
        lpr_opts.push(format!("-o media-type={}", key));
    }

    // Input slot: use the IPP keyword (e.g. "auto", "cd")
    if let Some((key, _)) = caps.input_slots.get(props_slot_idx) {
        lpr_opts.push(format!("-o media-source={}", key));
    }

    // Extra options (color mode, duplex, quality, etc.)
    for opt in &caps.extra_options {
        if let Some(&idx) = extra_option_indices.get(&opt.key) {
            if let Some((choice_key, _)) = opt.choices.get(idx) {
                lpr_opts.push(format!("-o {}={}", opt.key, choice_key));
            }
        }
    }

    let opts_str = lpr_opts.join(" ");
    let printer_q = shell_quote(&printer.name);

    // Submit each TIFF directly to lpr (no PS/PDF conversion)
    for (i, temp_path) in temp_paths.iter().enumerate() {
        let temp_path_q = shell_quote(&temp_path.display().to_string());

        let lpr_cmd = format!("lpr -P {} {} {}", printer_q, opts_str, temp_path_q);

        let log_msg = format!(
            "Safe 8-bit TIFF: Sending page {} of {} directly to lpr (no PDF conversion)",
            i + 1,
            temp_paths.len()
        );
        let _ = log_tx.send(log_msg);

        let lpr_result = std::process::Command::new("sh")
            .arg("-c")
            .arg(&lpr_cmd)
            .output()
            .map_err(|e| format!("Failed to run lpr: {}", e))?;

        if !lpr_result.status.success() {
            let stderr = String::from_utf8_lossy(&lpr_result.stderr);
            return Err(format!(
                "lpr failed (page {} of {}): {}",
                i + 1,
                temp_paths.len(),
                stderr
            ));
        }

        let success_msg = format!("Safe 8-bit TIFF: Page {} submitted successfully", i + 1);
        let _ = log_tx.send(success_msg);
    }

    let _ = log_tx.send(format!(
        "Safe 8-bit TIFF: All {} page(s) submitted to {}",
        temp_paths.len(),
        printer.name
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::calc_print_offset;

    #[test]
    fn asymmetric_imageable_area_offset() {
        // Letter paper: 612 × 792 pts.
        // Asymmetric reported borders: left=14.4, bottom=18.0, right=28.8, top=21.6 pts
        let imageable_area = (14.4, 18.0, 583.2, 770.4);
        let (offset_x, offset_y) = calc_print_offset(imageable_area);
        assert_eq!(offset_x, 14.4);
        assert_eq!(offset_y, 18.0);
    }

    #[test]
    fn symmetric_imageable_area_offset() {
        // Same imageable area on all sides: 12 pts.
        let imageable_area = (12.0, 12.0, 600.0, 780.0);
        let (offset_x, offset_y) = calc_print_offset(imageable_area);
        assert_eq!(offset_x, 12.0);
        assert_eq!(offset_y, 12.0);
    }

    #[test]
    fn zero_imageable_area_offset() {
        // Borderless: imageable area fills the full paper.
        let imageable_area = (0.0, 0.0, 612.0, 792.0);
        let (offset_x, offset_y) = calc_print_offset(imageable_area);
        assert_eq!(offset_x, 0.0);
        assert_eq!(offset_y, 0.0);
    }
}
