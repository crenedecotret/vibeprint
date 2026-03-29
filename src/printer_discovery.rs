//! Hardware discovery layer — enumerates CUPS printer queues and parses their PPD files.
//!
//! Designed to be driven from a GUI thread: call [`spawn_discovery`] on startup and poll the
//! returned [`Receiver`] without blocking the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};

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
    /// Printable bounds: Left, Bottom, Right, Top (PostScript points).
    pub imageable_area: (f32, f32, f32, f32),
}

/// Full hardware capabilities for one printer queue.
#[derive(Debug, Clone)]
pub struct PrinterCaps {
    pub name: String,
    /// Supported DPI values in ascending order, e.g. `[360, 720, 1440]`.
    pub resolutions: Vec<u32>,
    /// Human-readable media type labels, e.g. `["Plain Paper", "Premium Glossy Photo"]`.
    pub media_types: Vec<String>,
    /// All supported page sizes with per-size imageable areas.
    pub page_sizes: Vec<PageSize>,
    /// Printable area for the PPD's default page size (Left, Bottom, Right, Top in points).
    /// This is the field used directly by the processing pipeline.
    pub printable_area: (f32, f32, f32, f32),
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
/// Requires `lpstat` (ships with CUPS on all Linux distros).
pub fn list_printers() -> Result<Vec<PrinterInfo>> {
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

/// Return the path to the PPD file CUPS has installed for `printer_name`.
///
/// CUPS stores PPDs at `/etc/cups/ppd/<name>.ppd` after driver installation.
pub fn find_ppd_path(printer_name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(format!("/etc/cups/ppd/{}.ppd", printer_name));
    if p.exists() { Some(p) } else { None }
}

/// Query the full capability set for a named printer queue by parsing its PPD.
pub fn query_printer_caps(name: &str) -> Result<PrinterCaps> {
    let ppd = find_ppd_path(name).with_context(|| {
        format!(
            "no PPD found for '{}' — printer may not be installed or CUPS may lack write access",
            name
        )
    })?;
    parse_ppd(name, &ppd)
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
    if name.is_empty() { None } else { Some(name) }
}

fn discovery_worker(tx: Sender<DiscoveryEvent>) {
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

    for printer in &printers {
        match query_printer_caps(&printer.name) {
            Ok(caps) => {
                let _ = tx.send(DiscoveryEvent::CapsReady(caps));
            }
            Err(e) => {
                let _ = tx.send(DiscoveryEvent::Warning(format!(
                    "{}: {}",
                    printer.name, e
                )));
            }
        }
    }
}

// ── PPD Parser ───────────────────────────────────────────────────────────────

fn parse_ppd(printer_name: &str, path: &Path) -> Result<PrinterCaps> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read PPD: {}", path.display()))?;

    let mut resolutions: Vec<u32> = Vec::new();
    let mut media_types: Vec<String> = Vec::new();
    let mut page_size_entries: Vec<(String, String)> = Vec::new(); // (name, label)
    let mut imageable_areas: HashMap<String, (f32, f32, f32, f32)> = HashMap::new();
    let mut default_page_size: Option<String> = None;

    // Which *OpenUI section are we currently inside?
    #[derive(PartialEq)]
    enum Section { Resolution, MediaType, PageSize, Other }
    let mut section = Section::Other;

    for raw in text.lines() {
        let line = raw.trim();

        // ── Section transitions ──────────────────────────────────────────────
        // Both *OpenUI and *JCLOpenUI wrap the same option types
        if (line.starts_with("*OpenUI") || line.starts_with("*JCLOpenUI"))
            && line.contains("*Resolution")
        {
            section = Section::Resolution;
            continue;
        }
        if (line.starts_with("*OpenUI") || line.starts_with("*JCLOpenUI"))
            && line.contains("*MediaType")
        {
            section = Section::MediaType;
            continue;
        }
        if (line.starts_with("*OpenUI") || line.starts_with("*JCLOpenUI"))
            && line.contains("*PageSize")
        {
            section = Section::PageSize;
            continue;
        }
        if line.starts_with("*CloseUI") || line.starts_with("*JCLCloseUI") {
            section = Section::Other;
            continue;
        }

        // ── Default page size ────────────────────────────────────────────────
        // Prefer *DefaultPageSize; fall back to *DefaultImageableArea (used by some Epson PPDs)
        if let Some(rest) = line.strip_prefix("*DefaultPageSize:") {
            default_page_size = Some(rest.trim().to_string());
            continue;
        }
        if default_page_size.is_none() {
            if let Some(rest) = line.strip_prefix("*DefaultImageableArea:") {
                default_page_size = Some(rest.trim().to_string());
            }
        }

        // ── ImageableArea <Key>[/<Label>]: "L B R T" ────────────────────────────────
        // Key may have an optional /Label suffix (e.g. "Letter/Letter:" or just "Letter:")
        if let Some(rest) = line.strip_prefix("*ImageableArea ") {
            if let Some((key_part, val)) = rest.split_once(':') {
                let key = key_part.trim().split('/').next().unwrap_or("").trim().to_string();
                if !key.is_empty() {
                    if let Some(area) = parse_quad(val) {
                        imageable_areas.insert(key, area);
                    }
                }
            }
            continue;
        }

        // ── Epson/Gutenprint *StpQuality / generic *PrintQuality ────────────
        // These embed HWResolution[x y] in PostScript code instead of using *OpenUI *Resolution
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
            continue;
        }

        // ── Options inside the active OpenUI section ─────────────────────────
        match section {
            Section::Resolution => {
                // "*Resolution 720dpi/720 dpi: ..." or "*Resolution 1440x720dpi/..."
                if let Some(rest) = line.strip_prefix("*Resolution ") {
                    // skip *DefaultResolution lines
                    if rest.starts_with("Default") { continue; }
                    let value = rest.split('/').next().unwrap_or("").trim();
                    if let Some(dpi) = parse_resolution_value(value) {
                        if !resolutions.contains(&dpi) {
                            resolutions.push(dpi);
                        }
                    }
                }
            }
            Section::MediaType => {
                // "*MediaType GlossyPhoto/Premium Glossy Photo: ..."
                if let Some(rest) = line.strip_prefix("*MediaType ") {
                    if rest.starts_with("Default") { continue; }
                    if let Some((_, label_rest)) = rest.split_once('/') {
                        let label = label_rest.split(':').next().unwrap_or("").trim().to_string();
                        if !label.is_empty() && !media_types.contains(&label) {
                            media_types.push(label);
                        }
                    }
                }
            }
            Section::PageSize => {
                // "*PageSize A4/A4: ..." or "*PageSize Letter/US Letter: ..."
                if let Some(rest) = line.strip_prefix("*PageSize ") {
                    if rest.starts_with("Default") { continue; }
                    if let Some((key, label_rest)) = rest.split_once('/') {
                        let key = key.trim().to_string();
                        let label = label_rest.split(':').next().unwrap_or("").trim().to_string();
                        page_size_entries.push((key, label));
                    }
                }
            }
            Section::Other => {}
        }
    }

    resolutions.sort_unstable();

    // Determine printable_area from the default page size
    let default_ps = default_page_size.as_deref().unwrap_or("");
    let fallback_area = (12.0f32, 12.0, 600.0, 780.0); // conservative Letter margins
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
            PageSize { name, label, imageable_area }
        })
        .collect();

    Ok(PrinterCaps {
        name: printer_name.to_string(),
        resolutions,
        media_types,
        page_sizes,
        printable_area,
    })
}

/// Parse `"L B R T"` or `L B R T` from a PPD value string.
fn parse_quad(s: &str) -> Option<(f32, f32, f32, f32)> {
    let s = s.trim().trim_matches('"');
    let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
    Some((it.next()?, it.next()?, it.next()?, it.next()?))
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
        assert_eq!(parse_quad("\"12.0 12.0 600.0 780.0\""), Some((12.0, 12.0, 600.0, 780.0)));
        assert_eq!(parse_quad("12 12 600 780"), Some((12.0, 12.0, 600.0, 780.0)));
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
        assert_eq!(caps.media_types, vec!["Plain Paper", "Premium Glossy Photo", "Ultra Premium Matte"]);
        assert_eq!(caps.page_sizes.len(), 2);

        // Default page size is A4
        assert_eq!(caps.printable_area, (12.0, 12.0, 583.0, 830.0));

        // PageSize entries carry their own imageable areas
        let letter = caps.page_sizes.iter().find(|p| p.name == "Letter").unwrap();
        assert_eq!(letter.imageable_area, (12.0, 12.0, 600.0, 780.0));
        assert_eq!(letter.label, "US Letter");
    }
}
