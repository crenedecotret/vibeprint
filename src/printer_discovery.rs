//! Hardware discovery layer — enumerates CUPS printer queues via libcups API.
//!
//! Uses CUPS C API (cupsGetDests, cupsCopyDestInfo) instead of PPD parsing for robust
//! handling of Gutenprint, Turboprint, driverless, and all printer types.
//!
//! Designed to be driven from a GUI thread: call [`spawn_discovery`] on startup and poll the
//! returned [`Receiver`] without blocking the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

mod cups_ffi;
use cups_ffi::{
    cups_dest_t, cups_dinfo_t,
    cupsGetDests, cupsFreeDests,
    get_dest_name, is_dest_default, get_dest_at,
};

// ── Public types ─────────────────────────────────────────────────────────────

/// Minimal information about a single CUPS printer queue.
#[derive(Debug, Clone)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
}

/// A paper size together with its printable (imageable) area.
/// All coordinates are in PostScript points (1 pt = 1/72 inch).
#[derive(Debug, Clone)]
pub struct PageSize {
    /// PPD key, e.g. `"A4"` or `"Letter"`.
    pub name: String,
    /// Human-readable label from the PPD, e.g. `"A4 (210 x 297 mm)"`.
    pub label: String,
    /// Physical sheet dimensions (width, height) in PostScript points.
    /// Sourced from `*PaperDimension`; falls back to imageable-area extents if unavailable.
    pub paper_size: (f32, f32),
    /// Printable bounds: Left, Bottom, Right, Top (PostScript points).
    pub imageable_area: (f32, f32, f32, f32),
}

/// A single CUPS/PPD option with all its selectable choices.
#[derive(Debug, Clone)]
pub struct CupsOption {
    /// PPD keyword, e.g. `"ColorModel"`, `"StpQuality"`.
    pub key: String,
    /// Human-readable option name, e.g. `"Color Model"`, `"Print Quality"`.
    pub label: String,
    /// `(ppd_key, human_label)` pairs for each selectable choice.
    pub choices: Vec<(String, String)>,
    /// Index into `choices` of the PPD/CUPS default.
    pub default_idx: usize,
}

/// Full hardware capabilities for one printer queue.
#[derive(Debug, Clone)]
pub struct PrinterCaps {
    pub name: String,
    /// Supported DPI values in ascending order, e.g. `[360, 720, 1440]`.
    pub resolutions: Vec<u32>,
    /// Media types as `(ipp_keyword, human_label)`, e.g. `[("photographic-glossy", "Glossy Photo Paper")]`.
    pub media_types: Vec<(String, String)>,
    /// Input slots as `(ipp_keyword, human_label)`, e.g. `[("auto", "Auto"), ("cd", "CD/DVD Tray")]`.
    pub input_slots: Vec<(String, String)>,
    /// All supported page sizes with per-size imageable areas.
    pub page_sizes: Vec<PageSize>,
    /// Printable area for the PPD's default page size (Left, Bottom, Right, Top in points).
    pub printable_area: (f32, f32, f32, f32),
    /// Every other PPD/CUPS option group not covered by the fields above.
    pub extra_options: Vec<CupsOption>,
}

/// Events emitted by the background discovery thread.
#[derive(Debug)]
pub enum DiscoveryEvent {
    /// Full list of printer queues found on the system.
    PrintersListed(Vec<PrinterInfo>),
    /// Parsed capabilities for one printer — emitted once per printer.
    CapsReady(PrinterCaps),
    /// Non-fatal warning, e.g. no PPD found for a specific printer.
    Warning(String),
    /// Fatal error that stopped discovery entirely.
    Error(String),
}

// ── Public API ───────────────────────────────────────────────────────────────

/// List all enabled CUPS printer queues and identify the system default.
///
/// Uses libcups `cupsGetDests()` API — works with all driver types including
/// Gutenprint, Turboprint, driverless/IPP Everywhere.
pub fn list_printers() -> Result<Vec<PrinterInfo>> {
    // Try CUPS API first
    let printers = list_printers_cups();
    if !printers.is_empty() {
        return Ok(printers);
    }
    
    // Fallback to lpstat if CUPS API fails
    list_printers_lpstat()
}

fn list_printers_cups() -> Vec<PrinterInfo> {
    unsafe {
        let mut dests: *mut cups_dest_t = ptr::null_mut();
        let num_dests = cupsGetDests(&mut dests);
        
        if num_dests <= 0 || dests.is_null() {
            return Vec::new();
        }
        
        let mut printers = Vec::new();
        for i in 0..num_dests {
            let dest = get_dest_at(dests, i);
            if let Some(name) = get_dest_name(dest) {
                let is_default = is_dest_default(dest);
                printers.push(PrinterInfo { name, is_default });
            }
        }
        
        cupsFreeDests(num_dests, dests);
        printers
    }
}

fn list_printers_lpstat() -> Result<Vec<PrinterInfo>> {
    let out = Command::new("lpstat")
        .arg("-e")
        .output()
        .context("failed to run `lpstat -e` — is CUPS installed?")?;

    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let default_name = detect_default_printer();

    Ok(names
        .into_iter()
        .map(|name| {
            let is_default = default_name.as_deref() == Some(name.as_str());
            PrinterInfo { name, is_default }
        })
        .collect())
}

/// Query capabilities for a named printer queue.
/// Never fails — returns minimal defaults if all methods fail.
///
/// Strategy: CUPS API for options (media types, slots, resolutions, color mode).
/// PPD for page sizes when available — the CUPS API only exposes what the IPP driver
/// declares, which for Gutenprint drivers is borderless-only. The PPD has the full list.
pub fn query_printer_caps(name: &str) -> Result<PrinterCaps> {
    // Get CUPS API caps first (options, slots, resolutions)
    let mut cups_caps = query_printer_caps_cups_api(name).ok();

    // Get PPD page sizes — overrides CUPS API page sizes when present
    if let Some(ppd_path) = find_ppd_path(name) {
        let parse_result = parse_ppd(name, &ppd_path);
        // Clean up temp file if it was written to /tmp by fetch_ppd_from_cups
        if ppd_path.starts_with("/tmp") {
            let _ = std::fs::remove_file(&ppd_path);
        }
        match parse_result {
            Ok(mut ppd_caps) => {
                fill_standard_imageable_areas(&mut ppd_caps.page_sizes);
                if let Some(ref mut cc) = cups_caps {
                    // Merge: use PPD page sizes + PPD extra options, keep CUPS media types/slots/resolutions
                    cc.page_sizes = ppd_caps.page_sizes;
                    cc.printable_area = ppd_caps.printable_area;
                    // Fill resolutions from PPD if CUPS only reported one
                    if cc.resolutions.len() <= 1 && !ppd_caps.resolutions.is_empty() {
                        cc.resolutions = ppd_caps.resolutions;
                    }
                    // Merge PPD-specific extra options not covered by CUPS API
                    for opt in ppd_caps.extra_options {
                        if !cc.extra_options.iter().any(|o| o.key == opt.key) {
                            cc.extra_options.push(opt);
                        }
                    }
                    return Ok(cc.clone());
                } else {
                    // No CUPS caps — supplement PPD with lpoptions resolutions
                    if ppd_caps.resolutions.is_empty() {
                        ppd_caps.resolutions = lpoptions_resolutions(name);
                    }
                    return Ok(ppd_caps);
                }
            }
            Err(e) => eprintln!("PPD parsing failed for '{}': {}", name, e),
        }
    }

    // No PPD — use CUPS API page sizes as-is (driverless/IPP Everywhere)
    if let Some(caps) = cups_caps {
        if !caps.page_sizes.is_empty() {
            return Ok(caps);
        }
    }

    // Fallback: lpoptions subprocess
    match caps_from_lpoptions(name) {
        Ok(caps) => return Ok(caps),
        Err(e) => eprintln!("lpoptions failed for '{}': {}", name, e),
    }

    // Last resort: minimal defaults so UI never gets stuck
    Ok(minimal_default_caps(name))
}

/// Query printer capabilities fully via CUPS API — no PPD parsing, no subprocesses.
/// Converts 1/100mm CUPS units to PostScript points (1pt = 25.4/72 mm).
fn query_printer_caps_cups_api(name: &str) -> Result<PrinterCaps> {
    use cups_ffi::*;
    use std::ffi::CString;

    unsafe {
        let _c_name = CString::new(name)?;

        // Get all destinations
        let mut dests: *mut cups_dest_t = ptr::null_mut();
        let num_dests = cupsGetDests(&mut dests);
        if num_dests <= 0 || dests.is_null() {
            anyhow::bail!("cupsGetDests returned no printers");
        }

        // Find this printer's destination entry
        let mut found_dest: *mut cups_dest_t = ptr::null_mut();
        for i in 0..num_dests {
            let d = get_dest_at(dests, i);
            if let Some(n) = get_dest_name(d) {
                if n == name {
                    found_dest = d;
                    break;
                }
            }
        }

        if found_dest.is_null() {
            cupsFreeDests(num_dests, dests);
            anyhow::bail!("printer '{}' not found in CUPS", name);
        }

        // Get capability info from CUPS scheduler
        let info = cupsCopyDestInfo(CUPS_HTTP_DEFAULT, found_dest);
        if info.is_null() {
            cupsFreeDests(num_dests, dests);
            anyhow::bail!("cupsCopyDestInfo failed for '{}'", name);
        }

        let caps = build_cups_caps(name, found_dest, info);

        cupsFreeDestInfo(info);
        cupsFreeDests(num_dests, dests);

        Ok(caps)
    }
}

/// 1/100 mm → PostScript points (72pt per inch, 25.4mm per inch)
#[inline]
fn hundredths_mm_to_pt(v: i32) -> f32 {
    v as f32 * 72.0 / 2540.0
}

/// IPP media-type keyword → human label mapping.
fn ipp_media_type_label(kw: &str) -> String {
    match kw {
        "stationery"            => "Plain Paper",
        "stationery-inkjet"     => "Inkjet Paper",
        "photographic"          => "Photo Paper",
        "photographic-glossy"   => "Glossy Photo Paper",
        "photographic-semi-gloss" => "Semi-Gloss Photo Paper",
        "photographic-matte"    => "Matte Photo Paper",
        "photographic-film"     => "Film",
        "transparency"          => "Transparency",
        "envelope"              => "Envelope",
        "envelope-plain"        => "Plain Envelope",
        "disc"                  => "CD/DVD",
        "labels"                => "Labels",
        "cardstock"             => "Card Stock",
        "postcard"              => "Postcard",
        "glossy-film"           => "Glossy Film",
        "back-film"             => "Back Light Film",
        _ => {
            // Convert kebab-case to Title Case as fallback
            return kw.split('-')
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }.to_string()
}

/// IPP media-source keyword → human label mapping.
fn ipp_media_source_label(kw: &str) -> String {
    match kw {
        "auto"          => "Auto",
        "main"          => "Main Tray",
        "manual"        => "Manual Feed",
        "alternate"     => "Alternate Tray",
        "top"           => "Top Tray",
        "bottom"        => "Bottom Tray",
        "large-capacity" => "Large Capacity",
        "envelope"      => "Envelope Feeder",
        "cd"            => "CD/DVD Tray",
        "velvet"        => "Roll (Velvet)",
        "matte"         => "Roll (Matte)",
        "main-roll"     => "Roll (Main)",
        "alternate-roll" => "Roll (Alternate)",
        "standard"      => "Standard",
        _ => {
            return kw.split('-')
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }.to_string()
}

/// PWG media name → short human label.
/// PWG keyword format: [namespace_]name_WxHunit[_borderless]
/// e.g. "na_letter_8.5x11in" → "Letter"
///      "iso_a4_210x297mm" → "A4"
///      "na_letter_8.5x11in_borderless" → "Letter (Borderless)"
///      "oe_photo-l_3.5x5in" → "Photo L"
///      "custom_2x6in_2x6in_borderless" → "2x6in (Borderless)"
fn pwg_media_label(pwg: &str) -> String {
    // Known namespace prefixes to strip
    let namespaces = ["na_", "iso_", "oe_", "jis_", "roc_", "asme_", "prc_"];

    let mut s = pwg;
    let mut borderless = false;

    // Strip _borderless suffix
    if let Some(base) = s.strip_suffix("_borderless") {
        s = base;
        borderless = true;
    }

    // Strip namespace prefix
    for ns in &namespaces {
        if let Some(rest) = s.strip_prefix(ns) {
            s = rest;
            break;
        }
    }

    // Remove trailing _WxHunit dimension (e.g. "_8.5x11in", "_210x297mm")
    // Find last underscore followed by digits
    let label = if let Some(pos) = s.rfind('_') {
        let after = &s[pos + 1..];
        // If it looks like a dimension (starts with digit), strip it
        if after.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            &s[..pos]
        } else {
            s
        }
    } else {
        s
    };

    let mut result = title_case(label);
    if borderless {
        result.push_str(" (Borderless)");
    }
    result
}

fn title_case(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

unsafe fn build_cups_caps(
    name: &str,
    dest: *mut cups_dest_t,
    info: *mut cups_dinfo_t,
) -> PrinterCaps {
    use cups_ffi::{CupsSize, cupsGetDestMediaCount, cupsGetDestMediaByIndex,
        cupsFindDestSupported, CUPS_HTTP_DEFAULT, CUPS_MEDIA_FLAGS_DEFAULT,
        cstr_from_array, ipp_attr_strings, ipp_attr_enums, ipp_attr_resolutions};
    use std::ffi::CString;

    // ── Page sizes ────────────────────────────────────────────────────────────
    let media_count = cupsGetDestMediaCount(CUPS_HTTP_DEFAULT, dest, info, CUPS_MEDIA_FLAGS_DEFAULT);
    let mut page_sizes: Vec<PageSize> = Vec::new();

    for i in 0..media_count {
        let mut size = CupsSize {
            media: [0; 128],
            width: 0,
            length: 0,
            bottom: 0,
            left: 0,
            right: 0,
            top: 0,
        };
        if cupsGetDestMediaByIndex(CUPS_HTTP_DEFAULT, dest, info, i, CUPS_MEDIA_FLAGS_DEFAULT, &mut size) == 0 {
            continue;
        }

        let media_name = cstr_from_array(&size.media);
        if media_name.is_empty() { continue; }

        // Convert 1/100mm → points
        let w_pt = hundredths_mm_to_pt(size.width);
        let h_pt = hundredths_mm_to_pt(size.length);
        let l_pt = hundredths_mm_to_pt(size.left);
        let b_pt = hundredths_mm_to_pt(size.bottom);
        let r_pt = hundredths_mm_to_pt(size.right);
        let t_pt = hundredths_mm_to_pt(size.top);

        // Imageable area: left, bottom, (width - right margin), (height - top margin)
        let ia = (l_pt, b_pt, w_pt - r_pt, h_pt - t_pt);

        let label = pwg_media_label(&media_name);

        page_sizes.push(PageSize {
            name: media_name,
            label,
            paper_size: (w_pt, h_pt),
            imageable_area: ia,
        });
    }

    // ── Resolutions ───────────────────────────────────────────────────────────
    let res_key = CString::new("printer-resolution").unwrap();
    let res_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, res_key.as_ptr());
    let mut resolutions = ipp_attr_resolutions(res_attr);
    resolutions.sort_unstable();
    resolutions.dedup();

    // ── Media types ───────────────────────────────────────────────────────────
    let mt_key = CString::new("media-type").unwrap();
    let mt_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, mt_key.as_ptr());
    let media_types: Vec<(String, String)> = ipp_attr_strings(mt_attr)
        .into_iter()
        .map(|kw| { let label = ipp_media_type_label(&kw); (kw, label) })
        .collect();

    // ── Input slots ───────────────────────────────────────────────────────────
    let ms_key = CString::new("media-source").unwrap();
    let ms_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, ms_key.as_ptr());
    let input_slots: Vec<(String, String)> = ipp_attr_strings(ms_attr)
        .into_iter()
        .map(|kw| { let label = ipp_media_source_label(&kw); (kw, label) })
        .collect();

    // ── Extra options (color mode, quality, etc.) ─────────────────────────────
    let mut extra_options: Vec<CupsOption> = Vec::new();

    let color_key = CString::new("print-color-mode").unwrap();
    let color_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, color_key.as_ptr());
    let color_vals = ipp_attr_strings(color_attr);
    if color_vals.len() > 1 {
        extra_options.push(CupsOption {
            key: "print-color-mode".to_string(),
            label: "Color Mode".to_string(),
            choices: color_vals.iter().map(|v| (v.clone(), title_case(v))).collect(),
            default_idx: color_vals.iter().position(|v| v == "color").unwrap_or(0),
        });
    }

    let sides_key = CString::new("sides").unwrap();
    let sides_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, sides_key.as_ptr());
    let sides_vals = ipp_attr_strings(sides_attr);
    if sides_vals.len() > 1 {
        let sides_labels: Vec<(String, String)> = sides_vals.iter().map(|v| {
            let label = match v.as_str() {
                "one-sided"            => "One Sided",
                "two-sided-long-edge"  => "Two Sided (Long Edge)",
                "two-sided-short-edge" => "Two Sided (Short Edge)",
                _ => v.as_str(),
            };
            (v.clone(), label.to_string())
        }).collect();
        extra_options.push(CupsOption {
            key: "sides".to_string(),
            label: "Duplex".to_string(),
            choices: sides_labels,
            default_idx: 0,
        });
    }

    // ── Print quality (IPP enum: 3=draft 4=normal 5=high) ────────────────────────
    let pq_key = CString::new("print-quality").unwrap();
    let pq_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, pq_key.as_ptr());
    // Try as keyword strings first (some drivers), then as enum integers
    let pq_vals = {
        let s = ipp_attr_strings(pq_attr);
        if s.is_empty() { ipp_attr_enums(pq_attr) } else { s }
    };
    if pq_vals.len() > 1 {
        let pq_labels: Vec<(String, String)> = pq_vals.iter().map(|v| {
            let label = match v.as_str() {
                "3" | "draft"  => "Draft",
                "4" | "normal" => "Normal",
                "5" | "high"   => "High",
                _ => v.as_str(),
            };
            (v.clone(), label.to_string())
        }).collect();
        let default_idx = pq_labels.iter().position(|(k, _)| k == "4" || k == "normal").unwrap_or(0);
        extra_options.push(CupsOption {
            key: "print-quality".to_string(),
            label: "Print Quality".to_string(),
            choices: pq_labels,
            default_idx,
        });
    }

    // ── Output bin ────────────────────────────────────────────────────────────
    let ob_key = CString::new("output-bin").unwrap();
    let ob_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, ob_key.as_ptr());
    let ob_vals = ipp_attr_strings(ob_attr);
    if ob_vals.len() > 1 {
        let ob_labels: Vec<(String, String)> = ob_vals.iter().map(|v| {
            let label = match v.as_str() {
                "auto"          => "Auto",
                "top"           => "Top Bin",
                "middle"        => "Middle Bin",
                "bottom"        => "Bottom Bin",
                "side"          => "Side Bin",
                "left"          => "Left Bin",
                "right"         => "Right Bin",
                "face-up"       => "Face Up",
                "face-down"     => "Face Down",
                "large-capacity" => "Large Capacity",
                _ => v.as_str(),
            };
            (v.clone(), label.to_string())
        }).collect();
        extra_options.push(CupsOption {
            key: "output-bin".to_string(),
            label: "Output Bin".to_string(),
            choices: ob_labels,
            default_idx: 0,
        });
    }

    // ── Print rendering intent ────────────────────────────────────────────────
    let ri_key = CString::new("print-rendering-intent").unwrap();
    let ri_attr = cupsFindDestSupported(CUPS_HTTP_DEFAULT, dest, info, ri_key.as_ptr());
    let ri_vals = ipp_attr_strings(ri_attr);
    if ri_vals.len() > 1 {
        let ri_labels: Vec<(String, String)> = ri_vals.iter().map(|v| {
            let label = match v.as_str() {
                "auto"             => "Auto",
                "perceptual"       => "Perceptual",
                "relative"         => "Relative Colorimetric",
                "relative-bpc"     => "Relative Colorimetric (BPC)",
                "saturation"       => "Saturation",
                "absolute"         => "Absolute Colorimetric",
                _ => v.as_str(),
            };
            (v.clone(), label.to_string())
        }).collect();
        let default_idx = ri_labels.iter().position(|(k, _)| k == "auto" || k == "perceptual").unwrap_or(0);
        extra_options.push(CupsOption {
            key: "print-rendering-intent".to_string(),
            label: "Rendering Intent".to_string(),
            choices: ri_labels,
            default_idx,
        });
    }

    // ── Default printable area (first page size) ──────────────────────────────
    let printable_area = page_sizes.first()
        .map(|p| p.imageable_area)
        .unwrap_or((12.0, 12.0, 600.0, 780.0));

    PrinterCaps {
        name: name.to_string(),
        resolutions,
        media_types,
        input_slots,
        page_sizes,
        printable_area,
        extra_options,
    }
}

fn minimal_default_caps(name: &str) -> PrinterCaps {
    PrinterCaps {
        name: name.to_string(),
        resolutions: vec![300, 600],
        media_types: vec![("stationery".to_string(), "Plain Paper".to_string())],
        input_slots: vec![("auto".to_string(), "Auto".to_string())],
        page_sizes: vec![
            PageSize {
                name: "Letter".to_string(),
                label: "Letter".to_string(),
                paper_size: (612.0, 792.0),
                imageable_area: (12.0, 12.0, 600.0, 780.0),
            },
            PageSize {
                name: "A4".to_string(),
                label: "A4".to_string(),
                paper_size: (595.0, 842.0),
                imageable_area: (12.0, 12.0, 583.0, 830.0),
            },
        ],
        printable_area: (12.0, 12.0, 600.0, 780.0),
        extra_options: Vec::new(),
    }
}

/// Fetch the PPD for `printer_name` from the CUPS HTTP scheduler
/// (`http://localhost:631/printers/<name>.ppd`) and write it to a temp file.
///
/// This is identical to what GTK/GIMP do: `ppdOpenFd(dup(fd))` on the
/// downloaded PPD. Works for all driver types including driverless/IPP
/// Everywhere, because CUPS synthesises a PPD for those too.
/// Returns the temp file path on success, `None` on any error.
pub fn find_ppd_path(printer_name: &str) -> Option<PathBuf> {
    fetch_ppd_from_cups(printer_name)
        .or_else(|| find_ppd_on_disk(printer_name))
}

/// Check all known on-disk PPD locations across distros.
/// - `/etc/cups/ppd/`                        — standard (Fedora, Debian, Ubuntu classic)
/// - `/var/snap/cups/common/etc/cups/ppd/`   — Ubuntu snap CUPS
fn find_ppd_on_disk(printer_name: &str) -> Option<PathBuf> {
    let candidates = [
        format!("/etc/cups/ppd/{}.ppd", printer_name),
        format!("/var/snap/cups/common/etc/cups/ppd/{}.ppd", printer_name),
    ];
    candidates.into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// Download the CUPS-synthesised PPD via HTTP and save to a temp file.
fn fetch_ppd_from_cups(printer_name: &str) -> Option<PathBuf> {
    use cups_ffi::{cupsServer, ippPort};
    use std::ffi::CStr;
    use std::io::Write;

    // Ask libcups for the actual server address and port — works with
    // localhost, Unix socket proxies, and remote CUPS servers alike.
    let (host, port) = unsafe {
        let h = cupsServer();
        let host = if h.is_null() {
            "localhost".to_string()
        } else {
            CStr::from_ptr(h).to_str().unwrap_or("localhost").to_string()
        };
        let port = ippPort();
        (host, port)
    };

    // Unix socket paths start with '/' — CUPS HTTP API is still reachable
    // via localhost in that case.
    let host = if host.starts_with('/') { "localhost".to_string() } else { host };
    let url = format!("http://{}:{}/printers/{}.ppd", host, port, printer_name);

    // Use curl with a short connect+transfer timeout — same as a subprocess
    // but without spawning lpstat which can block on CUPS being busy.
    let out = Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--max-time", "5",       // 5 s total timeout
            "--connect-timeout", "2", // 2 s connect timeout
            &url,
        ])
        .output()
        .ok()?;

    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }

    // Write to temp file so parse_ppd can read it as a path
    let mut tmp = tempfile::Builder::new()
        .prefix("vibeprint_ppd_")
        .suffix(".ppd")
        .tempfile()
        .ok()?;
    tmp.write_all(&out.stdout).ok()?;
    let (_, path) = tmp.keep().ok()?;
    Some(path)
}

// ── lpoptions-based capability query (driverless / IPP fallback) ─────────────

/// Build `PrinterCaps` entirely from `lpoptions -p <name> -l`.
/// Works for every CUPS queue regardless of driver type.
fn caps_from_lpoptions(name: &str) -> Result<PrinterCaps> {
    let out = Command::new("lpoptions")
        .args(["-p", name, "-l"])
        .output()
        .with_context(|| format!("failed to run lpoptions for '{name}'"))?;

    let text = String::from_utf8_lossy(&out.stdout);
    let mut resolutions: Vec<u32> = Vec::new();
    let mut media_types: Vec<(String, String)> = Vec::new();
    let mut input_slots: Vec<(String, String)> = Vec::new();
    let mut page_size_entries: Vec<(String, String)> = Vec::new();
    let mut extra_options: Vec<CupsOption> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        // Each line: "OptionKey/Option Label: [*]val1[/Val Label] [*]val2 ..."
        let (option_key, rest) = match line.split_once('/') {
            Some(p) => p,
            None => continue,
        };
        let (option_label, values_str) = match rest.split_once(':') {
            Some((l, v)) => (l.trim(), v.trim()),
            None => continue,
        };
        let key = option_key.trim();

        match key {
            "Resolution" | "Dpi" | "OutputResolution" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let k = v.split('/').next().unwrap_or(v);
                    if let Some(dpi) = parse_resolution_value(k) {
                        if !resolutions.contains(&dpi) {
                            resolutions.push(dpi);
                        }
                    }
                }
            }
            "MediaType" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let key = v.split('/').next().unwrap_or(v).trim().to_string();
                    let label = v.split('/').nth(1).unwrap_or(v).trim().to_string();
                    if !key.is_empty() && !media_types.iter().any(|(k, _)| k == &key) {
                        media_types.push((key, label));
                    }
                }
            }
            "InputSlot" | "MediaPosition" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let key = v.split('/').next().unwrap_or(v).trim().to_string();
                    let label = v.split('/').nth(1).unwrap_or(v).trim().to_string();
                    if !key.is_empty() && !input_slots.iter().any(|(k, _)| k == &key) {
                        input_slots.push((key, label));
                    }
                }
            }
            "PageSize" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let mut parts = v.splitn(2, '/');
                    let k = parts.next().unwrap_or(v).trim().to_string();
                    let label = parts
                        .next()
                        .map(|s| clean_paper_size_label(&s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| human_readable_label(&clean_paper_size_label(&k)));
                    if !k.is_empty() {
                        page_size_entries.push((k, label));
                    }
                }
            }
            _ => {
                // Generic option — capture as CupsOption
                let mut choices: Vec<(String, String)> = Vec::new();
                let mut default_idx = 0usize;
                for token in values_str.split_whitespace() {
                    let is_default = token.starts_with('*');
                    let v = token.trim_start_matches('*');
                    let (ck, cl) = if let Some((k, l)) = v.split_once('/') {
                        (k.to_string(), l.to_string())
                    } else {
                        (v.to_string(), v.to_string())
                    };
                    if is_default {
                        default_idx = choices.len();
                    }
                    if !ck.is_empty() {
                        choices.push((ck, cl));
                    }
                }
                if choices.len() >= 2 {
                    extra_options.push(CupsOption {
                        key: key.to_string(),
                        label: option_label.to_string(),
                        choices,
                        default_idx,
                    });
                }
            }
        }
    }

    if resolutions.is_empty() && media_types.is_empty() && page_size_entries.is_empty() {
        anyhow::bail!(
            "no capability data for '{name}': no PPD found and lpoptions returned nothing"
        );
    }

    resolutions.sort_unstable();

    let mut page_sizes: Vec<PageSize> = page_size_entries
        .into_iter()
        .map(|(name, label)| {
            let imageable_area = standard_imageable_area(&name).unwrap_or((0.0, 0.0, 612.0, 792.0));
            let paper_size =
                standard_paper_size(&name).unwrap_or((imageable_area.2, imageable_area.3));
            PageSize {
                name,
                label,
                paper_size,
                imageable_area,
            }
        })
        .collect();
    fill_standard_imageable_areas(&mut page_sizes);

    let printable_area = page_sizes
        .first()
        .map(|p| p.imageable_area)
        .unwrap_or((0.0, 0.0, 612.0, 792.0));

    Ok(PrinterCaps {
        name: name.to_string(),
        resolutions,
        media_types,
        input_slots,
        page_sizes,
        printable_area,
        extra_options,
    })
}

/// Extract only the `Resolution` values from `lpoptions -p <name> -l`.
/// Used to fill gaps when a PPD uses non-standard resolution keywords.
fn lpoptions_resolutions(name: &str) -> Vec<u32> {
    let Ok(out) = Command::new("lpoptions").args(["-p", name, "-l"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut resolutions = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let key = line.split('/').next().unwrap_or("");
        if key == "Resolution" || key == "OutputResolution" || key == "Dpi" {
            if let Some(vals) = line.split_once(':').map(|(_, v)| v) {
                for token in vals.split_whitespace() {
                    let v = token
                        .trim_start_matches('*')
                        .split('/')
                        .next()
                        .unwrap_or("");
                    if let Some(dpi) = parse_resolution_value(v) {
                        if !resolutions.contains(&dpi) {
                            resolutions.push(dpi);
                        }
                    }
                }
            }
        }
    }
    resolutions.sort_unstable();
    resolutions
}

/// Standard imageable area (Left, Bottom, Right, Top in PostScript points) for well-known
/// paper sizes. Used when no PPD margin data is available (driverless / IPP printers).
/// Values represent the physical page dimensions (i.e. effectively borderless bounds).
fn standard_imageable_area(size_name: &str) -> Option<(f32, f32, f32, f32)> {
    Some(match size_name {
        "Letter" => (0.0, 0.0, 612.0, 792.0),
        "Legal" => (0.0, 0.0, 612.0, 1008.0),
        "Tabloid" | "11x17" => (0.0, 0.0, 792.0, 1224.0),
        "Executive" => (0.0, 0.0, 522.0, 756.0),
        "Statement" => (0.0, 0.0, 396.0, 612.0),
        "A3" => (0.0, 0.0, 842.0, 1191.0),
        "A4" => (0.0, 0.0, 595.0, 842.0),
        "A5" => (0.0, 0.0, 420.0, 595.0),
        "A6" => (0.0, 0.0, 298.0, 420.0),
        "B4" => (0.0, 0.0, 729.0, 1032.0),
        "B5" => (0.0, 0.0, 516.0, 729.0),
        "SuperB" | "13x19" => (0.0, 0.0, 936.0, 1368.0),
        "w288h432" | "4x6" => (0.0, 0.0, 288.0, 432.0),
        "w360h504" | "5x7" => (0.0, 0.0, 360.0, 504.0),
        "w432h576" | "6x8" => (0.0, 0.0, 432.0, 576.0),
        "w576h720" | "8x10" => (0.0, 0.0, 576.0, 720.0),
        "w144h432" | "2x6" => (0.0, 0.0, 144.0, 432.0),
        "Postcard" => (0.0, 0.0, 283.0, 416.0),
        _ => return None,
    })
}

/// For any `PageSize` whose imageable area is all-zeros (unresolved), substitute the
/// standard area from the lookup table.
fn fill_standard_imageable_areas(sizes: &mut Vec<PageSize>) {
    for ps in sizes.iter_mut() {
        if ps.imageable_area == (0.0, 0.0, 0.0, 0.0) {
            if let Some(area) = standard_imageable_area(&ps.name) {
                ps.imageable_area = area;
            }
        }
    }
}

/// Spawn a background thread that enumerates all printers, then queries each one's
/// capabilities.  Events are sent over the returned [`Receiver`]; the caller never blocks.
///
/// ```no_run
/// use vibeprint::printer_discovery::{spawn_discovery, DiscoveryEvent};
///
/// let rx = spawn_discovery();
/// // poll from your GUI event loop:
/// while let Ok(event) = rx.try_recv() {
///     match event {
///         DiscoveryEvent::PrintersListed(printers) => { /* populate dropdown */ }
///         DiscoveryEvent::CapsReady(caps)          => { /* fill settings panel */ }
///         DiscoveryEvent::Warning(msg)             => eprintln!("warn: {msg}"),
///         DiscoveryEvent::Error(msg)               => eprintln!("error: {msg}"),
///     }
/// }
/// ```
pub fn spawn_discovery() -> Receiver<DiscoveryEvent> {
    let (tx, rx) = channel::<DiscoveryEvent>();
    thread::spawn(move || discovery_worker(tx));
    rx
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn detect_default_printer() -> Option<String> {
    // `lpstat -d` → "system default destination: <name>"
    let out = Command::new("lpstat").arg("-d").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    // strip everything up to and including the last ": "
    let name = line.rsplit(": ").next()?.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn discovery_worker(tx: Sender<DiscoveryEvent>) {
    let start_time = Instant::now();
    let timeout = Duration::from_secs(10); // Max total discovery time
    
    let printers = match list_printers() {
        Ok(p) if p.is_empty() => {
            let _ = tx.send(DiscoveryEvent::Warning(
                "no CUPS printer queues found".into(),
            ));
            return;
        }
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(DiscoveryEvent::Error(e.to_string()));
            return;
        }
    };

    let _ = tx.send(DiscoveryEvent::PrintersListed(printers.clone()));
    
    // Track completion status
    let printer_count = printers.len();
    let mut completed_count = 0;
    let mut first_caps_ready = false;
    
    // Query each printer with individual timeout
    for printer in &printers {
        // Check if we're over timeout
        if start_time.elapsed() > timeout {
            let _ = tx.send(DiscoveryEvent::Warning(
                format!("Discovery timeout: queried {}/{} printers", completed_count, printer_count)
            ));
            break;
        }
        
        // Query with timeout per printer
        let printer_name = printer.name.clone();
        let tx_clone = tx.clone();
        
        match query_printer_caps_with_timeout(&printer_name, Duration::from_secs(5)) {
            Ok(Some(caps)) => {
                let _ = tx_clone.send(DiscoveryEvent::CapsReady(caps));
                if !first_caps_ready {
                    first_caps_ready = true;
                    // Signal that we have at least one printer ready - UI can proceed
                    let _ = tx_clone.send(DiscoveryEvent::Warning(
                        "READY".to_string() // Special signal for app.rs
                    ));
                }
            }
            Ok(None) => {
                let _ = tx_clone.send(DiscoveryEvent::Warning(
                    format!("{}: timed out", printer_name)
                ));
            }
            Err(e) => {
                let _ = tx_clone.send(DiscoveryEvent::Warning(
                    format!("{}: {}", printer_name, e)
                ));
            }
        }
        completed_count += 1;
    }
}

/// Query printer caps with a timeout - uses thread to prevent blocking
fn query_printer_caps_with_timeout(name: &str, timeout: Duration) -> Result<Option<PrinterCaps>> {
    use std::sync::mpsc::RecvTimeoutError;
    
    let name = name.to_string();
    let (tx, rx) = channel::<Result<PrinterCaps>>();
    
    thread::spawn(move || {
        let result = query_printer_caps(&name);
        let _ = tx.send(result);
    });
    
    match rx.recv_timeout(timeout) {
        Ok(result) => result.map(Some),
        Err(RecvTimeoutError::Timeout) => Ok(None),
        Err(RecvTimeoutError::Disconnected) => {
            anyhow::bail!("Query thread panicked")
        }
    }
}

// ── PPD Parser ───────────────────────────────────────────────────────────────

/// Extract `(key, human_label)` from a `*OpenUI *Key/Human Label: PickOne` line.
fn parse_open_ui_key_label(line: &str) -> Option<(String, String)> {
    // Find the second '*' (first is the *OpenUI / *JCLOpenUI keyword itself)
    let second = line
        .char_indices()
        .filter(|(_, c)| *c == '*')
        .nth(1)
        .map(|(i, _)| i)?;
    let rest = &line[second + 1..]; // "Key/Human Label: PickOne"
    let colon = rest.find(':')?;
    let key_label = rest[..colon].trim(); // "Key/Human Label"
    if let Some((k, l)) = key_label.split_once('/') {
        Some((k.trim().to_string(), l.trim().to_string()))
    } else {
        let k = key_label.to_string();
        Some((k.clone(), k))
    }
}

/// Flush a pending `Generic` section into `extra_options`.
fn flush_generic(
    key: String,
    label: String,
    choices: Vec<(String, String)>,
    defaults: &HashMap<String, String>,
    extra_options: &mut Vec<CupsOption>,
) {
    if choices.len() < 2 {
        return;
    }
    let def = defaults.get(&key).map(|s| s.as_str()).unwrap_or("");
    let default_idx = choices.iter().position(|(k, _)| k == def).unwrap_or(0);
    extra_options.push(CupsOption {
        key,
        label,
        choices,
        default_idx,
    });
}

fn parse_ppd(printer_name: &str, path: &Path) -> Result<PrinterCaps> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read PPD: {}", path.display()))?;

    let mut resolutions: Vec<u32> = Vec::new();
    let mut media_types: Vec<(String, String)> = Vec::new();
    let mut input_slots: Vec<(String, String)> = Vec::new();
    let mut page_size_entries: Vec<(String, String)> = Vec::new();
    let mut imageable_areas: HashMap<String, (f32, f32, f32, f32)> = HashMap::new();
    let mut paper_dimensions: HashMap<String, (f32, f32)> = HashMap::new();
    let mut default_page_size: Option<String> = None;
    let mut extra_options: Vec<CupsOption> = Vec::new();
    // *Default<Key>: Value lines — used to set default_idx in extra_options
    let mut defaults: HashMap<String, String> = HashMap::new();

    // These option keys are handled by dedicated fields — skip for extra_options.
    const SKIP_KEYS: &[&str] = &[
        "Resolution",
        "MediaType",
        "InputSlot",
        "MediaPosition",
        "PageSize",
        "PageRegion",
    ];

    // Which *OpenUI section are we currently inside?
    enum Section {
        Resolution,
        MediaType,
        InputSlot,
        PageSize,
        Generic {
            key: String,
            label: String,
            choices: Vec<(String, String)>,
        },
        Other,
    }
    let mut section = Section::Other;

    for raw in text.lines() {
        let line = raw.trim();

        // ── Collect *Default<Key>: <Value> for later default_idx resolution ──
        if let Some(rest) = line.strip_prefix("*Default") {
            if let Some((k, v)) = rest.split_once(':') {
                defaults.insert(k.trim().to_string(), v.trim().to_string());
            }
            // Do not continue — may still be relevant for other matchers below
        }

        // ── Section open ─────────────────────────────────────────────────────
        if line.starts_with("*OpenUI") || line.starts_with("*JCLOpenUI") {
            // Flush any pending generic section first
            let prev = std::mem::replace(&mut section, Section::Other);
            if let Section::Generic {
                key,
                label,
                choices,
            } = prev
            {
                flush_generic(key, label, choices, &defaults, &mut extra_options);
            }

            if let Some((key, label)) = parse_open_ui_key_label(line) {
                section = match key.as_str() {
                    "Resolution" => Section::Resolution,
                    "MediaType" => Section::MediaType,
                    "InputSlot" | "MediaPosition" => Section::InputSlot,
                    "PageSize" | "PageRegion" => Section::PageSize,
                    _ => Section::Generic {
                        key,
                        label,
                        choices: Vec::new(),
                    },
                };
            }
            continue;
        }

        // ── Section close ────────────────────────────────────────────────────
        if line.starts_with("*CloseUI") || line.starts_with("*JCLCloseUI") {
            let prev = std::mem::replace(&mut section, Section::Other);
            if let Section::Generic {
                key,
                label,
                choices,
            } = prev
            {
                flush_generic(key, label, choices, &defaults, &mut extra_options);
            }
            continue;
        }

        // ── Default page size ────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("*DefaultPageSize:") {
            default_page_size = Some(rest.trim().to_string());
            continue;
        }
        if default_page_size.is_none() {
            if let Some(rest) = line.strip_prefix("*DefaultImageableArea:") {
                default_page_size = Some(rest.trim().to_string());
            }
        }

        // ── PaperDimension <key>[/<label>]: "width height" ───────────────────
        if let Some(rest) = line.strip_prefix("*PaperDimension ") {
            if let Some((key_part, val)) = rest.split_once(':') {
                let key = key_part
                    .trim()
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !key.is_empty() {
                    if let Some((w, h)) = parse_pair(val) {
                        paper_dimensions.insert(key, (w, h));
                    }
                }
            }
            continue;
        }

        // ── ImageableArea ─────────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("*ImageableArea ") {
            if let Some((key_part, val)) = rest.split_once(':') {
                let key = key_part
                    .trim()
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !key.is_empty() {
                    if let Some(area) = parse_quad(val) {
                        imageable_areas.insert(key, area);
                    }
                }
            }
            continue;
        }

        // ── Epson/Gutenprint *StpQuality / *PrintQuality HWResolution ────────
        // These embed HWResolution[x y] in PostScript instead of *OpenUI *Resolution.
        // Extract DPI but do NOT continue — fall through so the Generic section
        // handler below also captures the choice key/label.
        if (line.starts_with("*StpQuality ") || line.starts_with("*PrintQuality "))
            && !line.contains("Default")
        {
            if let Some(pos) = line.find("HWResolution[") {
                let inner = &line[pos + "HWResolution[".len()..];
                let close = inner.find(']').unwrap_or(inner.len());
                let dpi = inner[..close]
                    .split_whitespace()
                    .filter_map(|t| t.parse::<u32>().ok())
                    .max()
                    .unwrap_or(0);
                if dpi > 0 && !resolutions.contains(&dpi) {
                    resolutions.push(dpi);
                }
            }
            // fall through to section match below
        }

        // ── Options inside the active OpenUI section ─────────────────────────
        match &mut section {
            Section::Resolution => {
                if let Some(rest) = line.strip_prefix("*Resolution ") {
                    if rest.starts_with("Default") {
                        continue;
                    }
                    let value = rest.split('/').next().unwrap_or("").trim();
                    if let Some(dpi) = parse_resolution_value(value) {
                        if !resolutions.contains(&dpi) {
                            resolutions.push(dpi);
                        }
                    }
                }
            }
            Section::MediaType => {
                if let Some(rest) = line.strip_prefix("*MediaType ") {
                    if rest.starts_with("Default") {
                        continue;
                    }
                    // Split on ':' first to separate key from PostScript value
                    if let Some((key_part, _)) = rest.split_once(':') {
                        let key_part = key_part.trim();
                        // Now check if key_part has "/Label" separator
                        if let Some((key, label)) = key_part.split_once('/') {
                            let key = key.trim().to_string();
                            let label = label.trim().to_string();
                            let label = if label.is_empty() { key.clone() } else { label };
                            if !key.is_empty() && !media_types.iter().any(|(k, _)| k == &key) {
                                media_types.push((key, label));
                            }
                        } else {
                            // No "/" separator - use key as both key and label
                            let key = key_part.to_string();
                            if !key.is_empty() && !media_types.iter().any(|(k, _)| k == &key) {
                                media_types.push((key.clone(), key));
                            }
                        }
                    }
                }
            }
            Section::InputSlot => {
                for prefix in &["*InputSlot ", "*MediaPosition "] {
                    if let Some(rest) = line.strip_prefix(prefix) {
                        if rest.starts_with("Default") {
                            continue;
                        }
                        // Split on ':' first to separate key from PostScript value
                        if let Some((key_part, _)) = rest.split_once(':') {
                            let key_part = key_part.trim();
                            // Now check if key_part has "/Label" separator
                            if let Some((key, label)) = key_part.split_once('/') {
                                let key = key.trim().to_string();
                                let label = label.trim().to_string();
                                let label = if label.is_empty() { key.clone() } else { label };
                                if !key.is_empty() && !input_slots.iter().any(|(k, _)| k == &key) {
                                    input_slots.push((key, label));
                                }
                            } else {
                                // No "/" separator - use key as both key and label
                                let key = key_part.to_string();
                                if !key.is_empty() && !input_slots.iter().any(|(k, _)| k == &key) {
                                    input_slots.push((key.clone(), key));
                                }
                            }
                        }
                        break;
                    }
                }
            }
            Section::PageSize => {
                if let Some(rest) = line.strip_prefix("*PageSize ") {
                    if rest.starts_with("Default") {
                        continue;
                    }
                    // Split on ':' first to separate key from PostScript value
                    if let Some((key_part, _)) = rest.split_once(':') {
                        let key_part = key_part.trim();
                        // Now check if key_part has "/Label" separator
                        if let Some((key, label)) = key_part.split_once('/') {
                            let key = key.trim().to_string();
                            let label = clean_paper_size_label(&label.trim().to_string());
                            let label = if label.is_empty() {
                                human_readable_label(&clean_paper_size_label(&key))
                            } else {
                                label
                            };
                            page_size_entries.push((key, label));
                        } else {
                            // No "/" separator in key - use key directly
                            let key = key_part.to_string();
                            let label = human_readable_label(&clean_paper_size_label(&key));
                            page_size_entries.push((key, label));
                        }
                    }
                }
            }
            Section::Generic { key, choices, .. } => {
                // Choice lines: "*<Key> ChoiceKey/Choice Label: <postscript>"
                let prefix = format!("*{} ", key);
                if let Some(rest) = line.strip_prefix(prefix.as_str()) {
                    if rest.starts_with("Default") {
                        continue;
                    }
                    // Parse "ChoiceKey/Choice Label:" — stop at first ':'
                    if let Some(colon) = rest.find(':') {
                        let kl = rest[..colon].trim();
                        let (ck, cl) = if let Some((k, l)) = kl.split_once('/') {
                            (k.trim().to_string(), l.trim().to_string())
                        } else {
                            (kl.to_string(), kl.to_string())
                        };
                        // Sanity: skip empty or suspiciously long / postscript-looking keys
                        if !ck.is_empty() && ck.len() < 64 && !ck.contains('"') {
                            choices.push((ck, cl));
                        }
                    }
                }
            }
            Section::Other => {}
        }
    }

    // Flush any still-open generic section at end-of-file
    if let Section::Generic {
        key,
        label,
        choices,
    } = section
    {
        flush_generic(key, label, choices, &defaults, &mut extra_options);
    }

    resolutions.sort_unstable();

    let default_ps = default_page_size.as_deref().unwrap_or("");
    let fallback_area = (12.0f32, 12.0, 600.0, 780.0);
    let printable_area = imageable_areas
        .get(default_ps)
        .copied()
        .or_else(|| {
            page_size_entries
                .first()
                .and_then(|(k, _)| imageable_areas.get(k).copied())
        })
        .unwrap_or(fallback_area);

    let page_sizes: Vec<PageSize> = page_size_entries
        .into_iter()
        .map(|(name, label)| {
            let imageable_area = imageable_areas.get(&name).copied().unwrap_or(fallback_area);
            let paper_size = paper_dimensions
                .get(&name)
                .copied()
                .or_else(|| standard_paper_size(&name))
                .unwrap_or((imageable_area.2, imageable_area.3));
            PageSize {
                name,
                label,
                paper_size,
                imageable_area,
            }
        })
        .collect();

    // Suppress the unused variable warning — SKIP_KEYS is used conceptually above
    let _ = SKIP_KEYS;

    Ok(PrinterCaps {
        name: printer_name.to_string(),
        resolutions,
        media_types,
        input_slots,
        page_sizes,
        printable_area,
        extra_options,
    })
}

/// Parse `"L B R T"` or `L B R T` from a PPD value string.
fn parse_quad(s: &str) -> Option<(f32, f32, f32, f32)> {
    let s = s.trim().trim_matches('"');
    let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
    Some((it.next()?, it.next()?, it.next()?, it.next()?))
}

/// Parse `"W H"` or `W H` from a PPD value string (used by `*PaperDimension`).
fn parse_pair(s: &str) -> Option<(f32, f32)> {
    let s = s.trim().trim_matches('"');
    let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
    Some((it.next()?, it.next()?))
}

/// Physical sheet dimensions for well-known paper names (width × height, PostScript points).
fn standard_paper_size(name: &str) -> Option<(f32, f32)> {
    match name {
        "Letter" | "na_letter_8.5x11in" => Some((612.0, 792.0)),
        "Legal" | "na_legal_8.5x14in" => Some((612.0, 1008.0)),
        "Tabloid" | "11x17" => Some((792.0, 1224.0)),
        "Executive" => Some((522.0, 756.0)),
        "A3" | "iso_a3_297x420mm" => Some((842.0, 1191.0)),
        "A4" | "iso_a4_210x297mm" => Some((595.0, 842.0)),
        "A5" | "iso_a5_148x210mm" => Some((420.0, 595.0)),
        "B4" | "iso_b4_250x353mm" => Some((709.0, 1001.0)),
        "B5" | "iso_b5_176x250mm" => Some((499.0, 709.0)),
        "Postcard" => Some((284.0, 419.0)),
        "SuperB" | "13x19" => Some((936.0, 1368.0)),
        "Statement" => Some((396.0, 612.0)),
        _ => None,
    }
}

/// Clean up paper size labels by removing square characters and fixing symbols.
/// Keeps "(Borderless)" to distinguish regular vs borderless sizes.
fn clean_paper_size_label(label: &str) -> String {
    // Remove all square-like Unicode characters
    let squares = [
        '□', '■', '▪', '▫', '▬', '▭', '▮', '▯', '▰', '▱', '▢', '▣', '▤', '▥', '▦', '▧', '▨', '▩',
    ];
    let mut cleaned = label.to_string();

    // Replace inch symbol (″) with regular double quotes
    cleaned = cleaned.replace('″', "\"");

    for square in squares.iter() {
        cleaned = cleaned.replace(*square, "");
    }
    cleaned.trim().to_string()
}

/// Convert a technical page size name (e.g., from IPP/driverless PPDs) to a human-readable label.
/// Falls back to the original name if no mapping exists.
fn human_readable_label(key: &str) -> String {
    // Standard ISO/NA names from IPP/driverless printers
    let label = match key {
        // North American sizes
        "na_letter_8.5x11in" | "Letter" => "Letter",
        "na_legal_8.5x14in" | "Legal" => "Legal",
        "na_tabloid_11x17in" | "Tabloid" | "11x17" => "Tabloid (11x17)",
        "na_executive_7.25x10.5in" | "Executive" => "Executive",
        "na_foolscap_8.5x13in" => "Foolscap",
        "na_number-10_4.125x9.5in" => "Envelope #10",
        "na_monarch_3.875x7.5in" => "Envelope Monarch",
        "na_invoice_5.5x8.5in" | "Statement" => "Statement (5.5x8.5)",

        // ISO sizes
        "iso_a3_297x420mm" | "A3" => "A3",
        "iso_a4_210x297mm" | "A4" => "A4",
        "iso_a5_148x210mm" | "A5" => "A5",
        "iso_a6_105x148mm" | "A6" => "A6",
        "iso_b4_250x353mm" | "B4" => "B4",
        "iso_b5_176x250mm" | "B5" => "B5",
        "iso_c4_229x324mm" => "C4 Envelope",
        "iso_c5_162x229mm" => "C5 Envelope",
        "iso_c6_114x162mm" => "C6 Envelope",
        "iso_dl_110x220mm" => "DL Envelope",

        // Common photo/paper sizes
        "jis_b5_182x257mm" => "JIS B5",
        "jpn_hagaki_100x148mm" => "Hagaki",
        "jpn_oufuku_148x200mm" => "Oufuku",
        "om_small-photo_100x150mm" => "4x6 Photo",
        "oe_photo-l_89x127mm" => "3.5x5 Photo (L)",
        "oe_photo-2l_127x178mm" => "5x7 Photo (2L)",
        "na_index-3x5_3x5in" => "3x5 Index",
        "na_index-4x6_4x6in" => "4x6 Index",
        "na_index-5x8_5x8in" => "5x8 Index",
        "na_personal_3.625x6.5in" => "Personal",
        "na_quarto_8.5x10.83in" => "Quarto",
        "na_supera_8.94x14in" => "Super A",
        "na_superb_11.7x17.6in" | "SuperB" | "13x19" => "Super B (13x19)",

        // Roll paper (common for wide format)
        "roll_min_8.5in" => "Roll (min 8.5in)",
        "roll_max_36in" => "Roll (max 36in)",

        // Legacy w*h*432 format (Epson/Gutenprint style)
        "w288h432" | "4x6" => "4x6 Photo",
        "w360h504" | "5x7" => "5x7 Photo",
        "w432h576" | "6x8" => "6x8 Photo",
        "w576h720" | "8x10" => "8x10 Photo",
        "w720h1080" => "10x15 Photo",
        "w144h432" | "2x6" => "2x6 Photo",

        // Default: return the key itself
        _ => key,
    };

    // Handle the PostScript PageSize[...] format by extracting dimensions
    if key.starts_with("PageSize[") {
        if let Some(close) = key.find(']') {
            let inner = &key[9..close]; // Skip "PageSize["
                                        // Try to parse "W H" format
            let dims: Vec<&str> = inner.split_whitespace().collect();
            if dims.len() == 2 {
                if let (Ok(w), Ok(h)) = (dims[0].parse::<f32>(), dims[1].parse::<f32>()) {
                    // Convert points to inches for display
                    let w_in = w / 72.0;
                    let h_in = h / 72.0;
                    // Check for common sizes
                    let dims_str = format!("{:.1}x{:.1}in", w_in, h_in);
                    return match dims_str.as_str() {
                        "8.5x11.0in" | "8.5x11in" => "Letter".to_string(),
                        "8.5x14.0in" | "8.5x14in" => "Legal".to_string(),
                        "11.0x17.0in" | "11x17in" => "Tabloid (11x17)".to_string(),
                        "8.3x11.7in" => "A4".to_string(),
                        "11.7x16.5in" => "A3".to_string(),
                        _ => format!("{:.1} x {:.1} in", w_in, h_in),
                    };
                }
            }
            return format!("Custom ({inner})",);
        }
    }

    clean_paper_size_label(&label.to_string())
}

/// Parse a PPD resolution value string into a DPI integer.
///
/// Handles: `"360dpi"`, `"720x720dpi"`, `"1440x720dpi"`.
/// Returns the maximum axis value (relevant for asymmetric resolutions).
fn parse_resolution_value(s: &str) -> Option<u32> {
    let stripped = s
        .to_ascii_lowercase()
        .trim_end_matches("dpi")
        .trim()
        .to_string();
    stripped
        .split('x')
        .filter_map(|n| n.trim().parse::<u32>().ok())
        .max()
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resolution_value_simple() {
        assert_eq!(parse_resolution_value("360dpi"), Some(360));
        assert_eq!(parse_resolution_value("720dpi"), Some(720));
        assert_eq!(parse_resolution_value("1440dpi"), Some(1440));
    }

    #[test]
    fn parse_resolution_value_compound() {
        assert_eq!(parse_resolution_value("720x720dpi"), Some(720));
        assert_eq!(parse_resolution_value("1440x720dpi"), Some(1440));
        assert_eq!(parse_resolution_value("2880x1440dpi"), Some(2880));
    }

    #[test]
    fn parse_quad_quoted() {
        assert_eq!(
            parse_quad("\"12.0 12.0 600.0 780.0\""),
            Some((12.0, 12.0, 600.0, 780.0))
        );
        assert_eq!(
            parse_quad("12 12 600 780"),
            Some((12.0, 12.0, 600.0, 780.0))
        );
    }

    #[test]
    fn ppd_parser_synthetic() {
        let ppd = r#"
*OpenUI *Resolution/Output Resolution: PickOne
*DefaultResolution: 720dpi
*Resolution 360dpi/360 dpi: ""
*Resolution 720dpi/720 dpi: ""
*Resolution 1440dpi/1440 dpi: ""
*CloseUI: *Resolution

*OpenUI *MediaType/Media Type: PickOne
*DefaultMediaType: Plain
*MediaType Plain/Plain Paper: ""
*MediaType GlossyPhoto/Premium Glossy Photo: ""
*MediaType Matte/Ultra Premium Matte: ""
*CloseUI: *MediaType

*OpenUI *PageSize/Paper Size: PickOne
*DefaultPageSize: A4
*PageSize Letter/US Letter: ""
*PageSize A4/A4: ""
*CloseUI: *PageSize

*DefaultPageSize: A4
*ImageableArea Letter: "12.0 12.0 600.0 780.0"
*ImageableArea A4: "12.0 12.0 583.0 830.0"
"#;

        // Write to a temp file and parse
        let dir = tempfile::tempdir().unwrap();
        let ppd_path = dir.path().join("test.ppd");
        std::fs::write(&ppd_path, ppd).unwrap();

        let caps = parse_ppd("TestPrinter", &ppd_path).unwrap();

        assert_eq!(caps.resolutions, vec![360, 720, 1440]);
        assert_eq!(
            caps.media_types,
            vec![
                ("Plain".to_string(), "Plain Paper".to_string()),
                ("GlossyPhoto".to_string(), "Premium Glossy Photo".to_string()),
                ("Matte".to_string(), "Ultra Premium Matte".to_string()),
            ]
        );
        assert_eq!(caps.page_sizes.len(), 2);

        // Default page size is A4
        assert_eq!(caps.printable_area, (12.0, 12.0, 583.0, 830.0));

        // PageSize entries carry their own imageable areas
        let letter = caps.page_sizes.iter().find(|p| p.name == "Letter").unwrap();
        assert_eq!(letter.imageable_area, (12.0, 12.0, 600.0, 780.0));
        assert_eq!(letter.label, "US Letter");
    }

    #[test]
    fn ppd_parser_ipp_driverless() {
        // Simulate IPP/driverless PPD format without "/Label" separators
        let ppd = r#"
*OpenUI *PageSize: PickOne
*DefaultPageSize: na_letter_8.5x11in
*PageSize na_letter_8.5x11in: "<</PageSize[612 792]>>setpagedevice"
*PageSize iso_a4_210x297mm: "<</PageSize[595 842]>>setpagedevice"
*PageSize na_legal_8.5x14in: "<</PageSize[612 1008]>>setpagedevice"
*PageSize PageSize[252 360]>>setpagedevice: ""
*CloseUI: *PageSize

*DefaultImageableArea: na_letter_8.5x11in
*ImageableArea na_letter_8.5x11in: "12 12 600 780"
*ImageableArea iso_a4_210x297mm: "12 12 583 830"
"#;

        let dir = tempfile::tempdir().unwrap();
        let ppd_path = dir.path().join("ipp.ppd");
        std::fs::write(&ppd_path, ppd).unwrap();

        let caps = parse_ppd("IPPPrinter", &ppd_path).unwrap();

        assert_eq!(caps.page_sizes.len(), 4);

        // Check that IPP technical names get converted to readable labels
        let letter = caps
            .page_sizes
            .iter()
            .find(|p| p.name == "na_letter_8.5x11in")
            .unwrap();
        assert_eq!(letter.label, "Letter");

        let a4 = caps
            .page_sizes
            .iter()
            .find(|p| p.name == "iso_a4_210x297mm")
            .unwrap();
        assert_eq!(a4.label, "A4");

        let legal = caps
            .page_sizes
            .iter()
            .find(|p| p.name == "na_legal_8.5x14in")
            .unwrap();
        assert_eq!(legal.label, "Legal");

        // Check PageSize[...] format gets parsed as dimensions
        let custom = caps
            .page_sizes
            .iter()
            .find(|p| p.name.starts_with("PageSize["))
            .unwrap();
        assert!(custom.label.contains("x") || custom.label.contains("in"));
    }

    #[test]
    fn human_readable_label_mappings() {
        assert_eq!(human_readable_label("na_letter_8.5x11in"), "Letter");
        assert_eq!(human_readable_label("iso_a4_210x297mm"), "A4");
        assert_eq!(human_readable_label("iso_a3_297x420mm"), "A3");
        assert_eq!(human_readable_label("na_legal_8.5x14in"), "Legal");
        assert_eq!(human_readable_label("Letter"), "Letter");
        assert_eq!(human_readable_label("A4"), "A4");
        assert_eq!(human_readable_label("w288h432"), "4x6 Photo");
        assert_eq!(human_readable_label("4x6"), "4x6 Photo");

        // Test PostScript PageSize format
        let label = human_readable_label("PageSize[612 792]>>setpagedevice");
        assert_eq!(label, "Letter");

        let label = human_readable_label("PageSize[595 842]>>setpagedevice");
        assert_eq!(label, "A4");

        // Unknown key returns itself
        assert_eq!(human_readable_label("unknown_key"), "unknown_key");
    }
}
