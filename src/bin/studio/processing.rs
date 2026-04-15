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
    _selected_page_size_idx: usize,
    _props_media_idx: usize,
    _props_slot_idx: usize,
    _extra_option_indices: &HashMap<String, usize>,
    log_tx: &Sender<String>,
) -> Result<(), String> {
    if temp_paths.is_empty() {
        return Err("No pages to print".into());
    }
    let _caps = caps.ok_or("No printer selected")?;
    let printer = printers.get(printer_idx).ok_or("No printer selected")?;

    for (i, temp_path) in temp_paths.iter().enumerate() {
        let _ = log_tx.send(format!(
            "Processing page {} of {}...",
            i + 1,
            temp_paths.len()
        ));

        // Generate unique temp file paths
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let pid = std::process::id();

        // Use the 16-bit TIFF directly (Ghostscript can handle it)
        let temp_path_q = shell_quote(&temp_path.display().to_string());
        let _ = log_tx.send(format!("Page {}: Converting to PDF...", i + 1));

        // Step 1: Convert TIFF to PDF using Ghostscript with color preservation
        let pdf_path = format!("/tmp/vibeprint_{}_{}.pdf", timestamp, pid);
        let pdf_q = shell_quote(&pdf_path);

        // Use tiff2ps piped to Ghostscript to convert TIFF to PDF with color preservation
        // -sColorConversionStrategy=LeaveColorUnchanged prevents color conversion
        // -dNOTRANSPARENCY flattens any transparency
        // -dVERBOSE provides progress output for high-DPI conversions
        let gs_cmd = format!(
            "tiff2ps {} | gs -dVERBOSE -o {} -sDEVICE=pdfwrite -sColorConversionStrategy=LeaveColorUnchanged -dNOTRANSPARENCY -",
            temp_path_q, pdf_q
        );

        let gs_output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&gs_cmd)
            .output()
            .map_err(|e| format!("Failed to run Ghostscript: {}", e))?;

        if !gs_output.status.success() {
            let stderr = String::from_utf8_lossy(&gs_output.stderr);
            return Err(format!(
                "PDF conversion failed (page {}): {}",
                i + 1,
                stderr
            ));
        }
        let _ = log_tx.send(format!("Page {}: Sending to printer...", i + 1));

        // Send PDF via LPR (no media options needed, just the file)
        let printer_q = shell_quote(&printer.name);
        let lpr_cmd = format!("lpr -P {} {}", printer_q, pdf_q);

        let lpr_result = std::process::Command::new("sh")
            .arg("-c")
            .arg(&lpr_cmd)
            .output();

        match lpr_result {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !output.status.success() {
                    return Err(format!("Print failed (page {}): {}", i + 1, stderr));
                }
            }
            Err(e) => {
                return Err(format!("Failed to submit print job: {}", e));
            }
        }
    }

    Ok(())
}
