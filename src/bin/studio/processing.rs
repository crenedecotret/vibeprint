use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use vibeprint::printer_discovery::{PrinterCaps, PrinterInfo};

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
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

    // Page dimensions in PostScript points from the selected paper size.
    // paper_size is stored in pts (72 pts/inch) — use directly for gs.
    let (page_w_pts, page_h_pts) = caps
        .page_sizes
        .get(selected_page_size_idx)
        .map(|ps| (ps.paper_size.0.round() as u32, ps.paper_size.1.round() as u32))
        .unwrap_or((612, 792)); // fallback: Letter

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

        let gs_cmd = format!(
            "tiff2ps {} | gs -q -o {} -sDEVICE=pdfwrite \
             -sColorConversionStrategy=LeaveColorUnchanged \
             -dNOTRANSPARENCY \
             -dDEVICEWIDTHPOINTS={} -dDEVICEHEIGHTPOINTS={} -dFIXEDMEDIA \
             -dAutoFilterColorImages=false -sColorImageFilter=FlateEncode \
             -dAutoFilterGrayImages=false -sGrayImageFilter=FlateEncode \
             -dDownsampleColorImages=false -dDownsampleGrayImages=false \
             -",
            temp_path_q, pdf_q, page_w_pts, page_h_pts
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
