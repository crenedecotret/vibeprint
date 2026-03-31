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
    /// Human-readable media type labels, e.g. `["Plain Paper", "Premium Glossy Photo"]`.
    pub media_types: Vec<String>,
    /// Source tray / input slot labels, e.g. `["Auto", "Manual", "Roll"]`.
    pub input_slots: Vec<String>,
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
/// Checks two locations in order:
/// 1. The `Interface:` path reported by `lpstat -l -p` (handles non-standard driver layouts)
/// 2. The standard `/etc/cups/ppd/<name>.ppd` (driver-based installs)
///
/// Returns `None` for driverless / IPP Everywhere printers — use [`query_printer_caps`]
/// which falls back to `lpoptions` in that case.
pub fn find_ppd_path(printer_name: &str) -> Option<PathBuf> {
    // Primary: ask CUPS for the actual interface file via lpstat
    if let Some(p) = find_ppd_via_lpstat(printer_name) {
        return Some(p);
    }
    // Fallback: conventional location
    let p = PathBuf::from(format!("/etc/cups/ppd/{}.ppd", printer_name));
    if p.exists() { Some(p) } else { None }
}

fn find_ppd_via_lpstat(printer_name: &str) -> Option<PathBuf> {
    let out = Command::new("lpstat")
        .args(["-l", "-p", printer_name])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("Interface:") {
            let path = rest.trim();
            if path.starts_with('/') && path.ends_with(".ppd") {
                let p = PathBuf::from(path);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Query the full capability set for a named printer queue.
///
/// Uses a tiered strategy so it works for all printer types:
/// 1. **PPD** (driver-based: Epson / Canon / HP with full driver) — richest data.
/// 2. **`lpoptions -p <name> -l`** (driverless IPP Everywhere, remote queues, any CUPS queue)
///    — CUPS always synthesises this regardless of driver type.
/// 3. **Standard imageable-area table** — fills in printable bounds for well-known paper sizes
///    when no PPD margin data is available.
pub fn query_printer_caps(name: &str) -> Result<PrinterCaps> {
    match find_ppd_path(name) {
        Some(ppd_path) => {
            let mut caps = parse_ppd(name, &ppd_path)?;
            // Some PPDs (e.g. Gutenprint Epson) encode resolution via non-standard keywords;
            // if parse_ppd came up empty for resolutions, fill from lpoptions.
            if caps.resolutions.is_empty() {
                caps.resolutions = lpoptions_resolutions(name);
            }
            // Fill imageable areas that PPD left at the fallback value
            fill_standard_imageable_areas(&mut caps.page_sizes);
            Ok(caps)
        }
        None => {
            // Driverless / IPP Everywhere / remote queue — no static PPD file.
            caps_from_lpoptions(name)
        }
    }
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
    let mut media_types: Vec<String> = Vec::new();
    let mut input_slots: Vec<String> = Vec::new();
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
                        if !resolutions.contains(&dpi) { resolutions.push(dpi); }
                    }
                }
            }
            "MediaType" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let label = v.split('/').nth(1).unwrap_or(v).trim().to_string();
                    if !label.is_empty() && !media_types.contains(&label) {
                        media_types.push(label);
                    }
                }
            }
            "InputSlot" | "MediaPosition" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let label = v.split('/').nth(1).unwrap_or(v).trim().to_string();
                    if !label.is_empty() && !input_slots.contains(&label) {
                        input_slots.push(label);
                    }
                }
            }
            "PageSize" => {
                for token in values_str.split_whitespace() {
                    let v = token.trim_start_matches('*');
                    let mut parts = v.splitn(2, '/');
                    let k     = parts.next().unwrap_or(v).trim().to_string();
                    let label = parts.next().unwrap_or(&k).trim().to_string();
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
                    if is_default { default_idx = choices.len(); }
                    if !ck.is_empty() { choices.push((ck, cl)); }
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
            let imageable_area = standard_imageable_area(&name)
                .unwrap_or((0.0, 0.0, 612.0, 792.0));
            let paper_size = standard_paper_size(&name)
                .unwrap_or((imageable_area.2, imageable_area.3));
            PageSize { name, label, paper_size, imageable_area }
        })
        .collect();
    fill_standard_imageable_areas(&mut page_sizes);

    let printable_area = page_sizes.first()
        .map(|p| p.imageable_area)
        .unwrap_or((0.0, 0.0, 612.0, 792.0));

    Ok(PrinterCaps { name: name.to_string(), resolutions, media_types, input_slots, page_sizes, printable_area, extra_options })
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
                    let v = token.trim_start_matches('*').split('/').next().unwrap_or("");
                    if let Some(dpi) = parse_resolution_value(v) {
                        if !resolutions.contains(&dpi) { resolutions.push(dpi); }
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
        "Letter"               => (0.0, 0.0, 612.0,  792.0),
        "Legal"                => (0.0, 0.0, 612.0, 1008.0),
        "Tabloid" | "11x17"   => (0.0, 0.0, 792.0, 1224.0),
        "Executive"            => (0.0, 0.0, 522.0,  756.0),
        "Statement"            => (0.0, 0.0, 396.0,  612.0),
        "A3"                   => (0.0, 0.0, 842.0, 1191.0),
        "A4"                   => (0.0, 0.0, 595.0,  842.0),
        "A5"                   => (0.0, 0.0, 420.0,  595.0),
        "A6"                   => (0.0, 0.0, 298.0,  420.0),
        "B4"                   => (0.0, 0.0, 729.0, 1032.0),
        "B5"                   => (0.0, 0.0, 516.0,  729.0),
        "SuperB" | "13x19"    => (0.0, 0.0, 936.0, 1368.0),
        "w288h432" | "4x6"    => (0.0, 0.0, 288.0,  432.0),
        "w360h504" | "5x7"    => (0.0, 0.0, 360.0,  504.0),
        "w432h576" | "6x8"    => (0.0, 0.0, 432.0,  576.0),
        "w576h720" | "8x10"   => (0.0, 0.0, 576.0,  720.0),
        "w144h432" | "2x6"    => (0.0, 0.0, 144.0,  432.0),
        "Postcard"             => (0.0, 0.0, 283.0,  416.0),
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

/// Extract `(key, human_label)` from a `*OpenUI *Key/Human Label: PickOne` line.
fn parse_open_ui_key_label(line: &str) -> Option<(String, String)> {
    // Find the second '*' (first is the *OpenUI / *JCLOpenUI keyword itself)
    let second = line.char_indices()
        .filter(|(_, c)| *c == '*')
        .nth(1)
        .map(|(i, _)| i)?;
    let rest = &line[second + 1..];           // "Key/Human Label: PickOne"
    let colon = rest.find(':')?;
    let key_label = rest[..colon].trim();     // "Key/Human Label"
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
    if choices.len() < 2 { return; }
    let def = defaults.get(&key).map(|s| s.as_str()).unwrap_or("");
    let default_idx = choices.iter().position(|(k, _)| k == def).unwrap_or(0);
    extra_options.push(CupsOption { key, label, choices, default_idx });
}

fn parse_ppd(printer_name: &str, path: &Path) -> Result<PrinterCaps> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read PPD: {}", path.display()))?;

    let mut resolutions: Vec<u32> = Vec::new();
    let mut media_types: Vec<String> = Vec::new();
    let mut input_slots: Vec<String> = Vec::new();
    let mut page_size_entries: Vec<(String, String)> = Vec::new();
    let mut imageable_areas: HashMap<String, (f32, f32, f32, f32)> = HashMap::new();
    let mut paper_dimensions: HashMap<String, (f32, f32)> = HashMap::new();
    let mut default_page_size: Option<String> = None;
    let mut extra_options: Vec<CupsOption> = Vec::new();
    // *Default<Key>: Value lines — used to set default_idx in extra_options
    let mut defaults: HashMap<String, String> = HashMap::new();

    // These option keys are handled by dedicated fields — skip for extra_options.
    const SKIP_KEYS: &[&str] = &[
        "Resolution", "MediaType", "InputSlot", "MediaPosition",
        "PageSize", "PageRegion",
    ];

    // Which *OpenUI section are we currently inside?
    enum Section {
        Resolution,
        MediaType,
        InputSlot,
        PageSize,
        Generic { key: String, label: String, choices: Vec<(String, String)> },
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
            if let Section::Generic { key, label, choices } = prev {
                flush_generic(key, label, choices, &defaults, &mut extra_options);
            }

            if let Some((key, label)) = parse_open_ui_key_label(line) {
                section = match key.as_str() {
                    "Resolution" => Section::Resolution,
                    "MediaType"  => Section::MediaType,
                    "InputSlot" | "MediaPosition" => Section::InputSlot,
                    "PageSize"  | "PageRegion"    => Section::PageSize,
                    _ => Section::Generic { key, label, choices: Vec::new() },
                };
            }
            continue;
        }

        // ── Section close ────────────────────────────────────────────────────
        if line.starts_with("*CloseUI") || line.starts_with("*JCLCloseUI") {
            let prev = std::mem::replace(&mut section, Section::Other);
            if let Section::Generic { key, label, choices } = prev {
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
                let key = key_part.trim().split('/').next().unwrap_or("").trim().to_string();
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
                let key = key_part.trim().split('/').next().unwrap_or("").trim().to_string();
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
                    if rest.starts_with("Default") { continue; }
                    let value = rest.split('/').next().unwrap_or("").trim();
                    if let Some(dpi) = parse_resolution_value(value) {
                        if !resolutions.contains(&dpi) { resolutions.push(dpi); }
                    }
                }
            }
            Section::MediaType => {
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
            Section::InputSlot => {
                for prefix in &["*InputSlot ", "*MediaPosition "] {
                    if let Some(rest) = line.strip_prefix(prefix) {
                        if rest.starts_with("Default") { continue; }
                        if let Some((_, label_rest)) = rest.split_once('/') {
                            let label = label_rest.split(':').next().unwrap_or("").trim().to_string();
                            if !label.is_empty() && !input_slots.contains(&label) {
                                input_slots.push(label);
                            }
                        } else if let Some((key, _)) = rest.split_once(':') {
                            let key = key.trim().to_string();
                            if !key.is_empty() && !input_slots.contains(&key) {
                                input_slots.push(key);
                            }
                        }
                        break;
                    }
                }
            }
            Section::PageSize => {
                if let Some(rest) = line.strip_prefix("*PageSize ") {
                    if rest.starts_with("Default") { continue; }
                    if let Some((key, label_rest)) = rest.split_once('/') {
                        let key = key.trim().to_string();
                        let label = label_rest.split(':').next().unwrap_or("").trim().to_string();
                        page_size_entries.push((key, label));
                    }
                }
            }
            Section::Generic { key, choices, .. } => {
                // Choice lines: "*<Key> ChoiceKey/Choice Label: <postscript>"
                let prefix = format!("*{} ", key);
                if let Some(rest) = line.strip_prefix(prefix.as_str()) {
                    if rest.starts_with("Default") { continue; }
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
    if let Section::Generic { key, label, choices } = section {
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
            let paper_size = paper_dimensions.get(&name)
                .copied()
                .or_else(|| standard_paper_size(&name))
                .unwrap_or((imageable_area.2, imageable_area.3));
            PageSize { name, label, paper_size, imageable_area }
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
        "Letter"    | "na_letter_8.5x11in"  => Some((612.0,  792.0)),
        "Legal"     | "na_legal_8.5x14in"   => Some((612.0, 1008.0)),
        "Tabloid"   | "11x17"               => Some((792.0, 1224.0)),
        "Executive"                          => Some((522.0,  756.0)),
        "A3"        | "iso_a3_297x420mm"    => Some((842.0, 1191.0)),
        "A4"        | "iso_a4_210x297mm"    => Some((595.0,  842.0)),
        "A5"        | "iso_a5_148x210mm"    => Some((420.0,  595.0)),
        "B4"        | "iso_b4_250x353mm"    => Some((709.0, 1001.0)),
        "B5"        | "iso_b5_176x250mm"    => Some((499.0,  709.0)),
        "Postcard"                           => Some((284.0,  419.0)),
        "SuperB"    | "13x19"               => Some((936.0, 1368.0)),
        "Statement"                          => Some((396.0,  612.0)),
        _                                    => None,
    }
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
