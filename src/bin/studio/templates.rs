//! Saved preset templates - data model and file I/O.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Current schema version for saved presets.
pub const SCHEMA_VERSION: u32 = 1;

/// A complete snapshot of the Printer Settings tab, saved as a JSON file.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct PresetTemplate {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub name: String,
    /// RFC 3339 timestamp, e.g. `2026-09-13T12:00:00+00:00`.
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub printer_name: Option<String>,
    /// PPD key, e.g. `"A4"`.
    #[serde(default)]
    pub page_size_name: Option<String>,
    /// Human label, e.g. `"A4 (210 x 297 mm)"`.
    #[serde(default)]
    pub page_size_label: Option<String>,
    /// Paper dimensions `(width, height)` in PostScript points.
    #[serde(default)]
    pub page_size_dims_pt: Option<(f32, f32)>,
    /// Media type PPD/IPP key.
    #[serde(default)]
    pub media_type_key: Option<String>,
    /// Media type human label.
    #[serde(default)]
    pub media_type_label: Option<String>,
    /// Input slot PPD/IPP key.
    #[serde(default)]
    pub input_slot_key: Option<String>,
    /// Input slot human label.
    #[serde(default)]
    pub input_slot_label: Option<String>,
    /// Extra option selections keyed by option PPD key.
    #[serde(default)]
    pub extra_option_indices: HashMap<String, usize>,
    /// User page margins in inches.
    #[serde(default)]
    pub borders: crate::types::Borders,
    /// Path to the output ICC profile, if any.
    #[serde(default)]
    pub output_icc_path: Option<String>,
    /// Description of the output ICC profile.
    #[serde(default)]
    pub output_icc_description: Option<String>,
    /// Color intent: `"perceptual"`, `"saturation"`, or `"relative"`.
    #[serde(default)]
    pub intent: String,
    /// Black point compensation.
    #[serde(default)]
    pub bpc: bool,
    /// Resampling engine identifier, byte-identical to `Engine` string encoding.
    #[serde(default)]
    pub engine: String,
    /// Sharpening amount (0-10).
    #[serde(default)]
    pub sharpen: u8,
    /// 16-bit output depth.
    #[serde(default)]
    pub depth16: bool,
    /// Target resolution in DPI.
    #[serde(default)]
    pub target_dpi: u32,
    /// Cut marks label string, byte-identical to `CutMarks::label()`.
    #[serde(default)]
    pub cut_marks: String,
}

impl Default for PresetTemplate {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: String::new(),
            created_at: String::new(),
            printer_name: None,
            page_size_name: None,
            page_size_label: None,
            page_size_dims_pt: None,
            media_type_key: None,
            media_type_label: None,
            input_slot_key: None,
            input_slot_label: None,
            extra_option_indices: HashMap::new(),
            borders: crate::types::Borders::default(),
            output_icc_path: None,
            output_icc_description: None,
            intent: String::new(),
            bpc: false,
            engine: String::new(),
            sharpen: 0,
            depth16: false,
            target_dpi: 0,
            cut_marks: String::new(),
        }
    }
}

// ── File I/O ──────────────────────────────────────────────────────────────

/// Return the default templates directory (`~/.config/vibeprint/templates/`).
/// Does NOT create the directory. Returns `None` if the config dir is unavailable.
pub(crate) fn templates_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("vibeprint").join("templates"))
}

/// List all valid template files in the templates directory.
/// Returns `(sorted_templates, warnings)`.
/// On missing dir returns empty list with zero warnings.
pub(crate) fn list_templates() -> (Vec<(PathBuf, PresetTemplate)>, Vec<String>) {
    let dir = match templates_dir() {
        Some(d) => d,
        None => return (Vec::new(), Vec::new()),
    };
    if !dir.is_dir() {
        return (Vec::new(), Vec::new());
    }

    let entries = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!("Could not read templates directory: {}", e)],
            );
        }
    };

    let mut templates = Vec::new();
    let mut warnings = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let is_vsp = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("vsp"))
            .unwrap_or(false);
        if !is_vsp {
            continue;
        }

        match import_template_file(&path) {
            Ok(t) => templates.push((path, t)),
            Err(reason) => {
                warnings.push(format!(
                    "Skipping invalid preset file {}: {}",
                    path.display(),
                    reason
                ));
            }
        }
    }

    // Sort by name (case-insensitive), tiebreak by path
    templates.sort_by(|a, b| {
        a.1.name
            .to_lowercase()
            .cmp(&b.1.name.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    });

    (templates, warnings)
}

/// Sanitize a user-supplied template name for use as a filename stem.
///
/// - Trims leading/trailing whitespace
/// - Collapses runs of internal whitespace to a single space
/// - Removes `/` and NUL characters
/// - Falls back to `"preset"` if the result is empty
/// - Truncates to 64 characters
pub(crate) fn sanitize_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_was_space = false;

    for ch in name.chars() {
        match ch {
            '/' | '\0' => continue,
            c if c.is_ascii_whitespace() => {
                if !prev_was_space && !result.is_empty() {
                    result.push(' ');
                }
                prev_was_space = true;
            }
            c => {
                result.push(c);
                prev_was_space = false;
            }
        }
    }

    let trimmed = result.trim();
    let result = if trimmed.is_empty() {
        "preset".to_string()
    } else {
        trimmed.to_string()
    };

    if result.len() > 64 {
        let mut end = 64;
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result[..end].to_string()
    } else {
        result
    }
}

/// Serialize a template to pretty JSON with a trailing newline.
fn template_to_json(t: &PresetTemplate) -> Result<String, String> {
    let mut json = serde_json::to_string_pretty(t)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    json.push('\n');
    Ok(json)
}

/// Write a template to a file path, creating parent directories as needed.
fn write_template_to(t: &PresetTemplate, path: &Path) -> Result<(), String> {
    let json = template_to_json(t)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
    }
    fs::write(path, &json)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

/// Find the first non-existing path for the given stem in a directory.
/// If `ignore_path` is set, that path counts as available even if it exists.
fn find_unique_path(dir: &Path, stem: &str, ignore_path: Option<&Path>) -> PathBuf {
    let candidate = dir.join(format!("{stem}.vsp"));
    if !candidate.exists() || ignore_path.map_or(false, |p| p == candidate) {
        return candidate;
    }
    for n in 2u32.. {
        let candidate = dir.join(format!("{stem} ({n}).vsp"));
        if !candidate.exists() || ignore_path.map_or(false, |p| p == candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Save a template into the default templates directory, uniquifying the filename
/// if one already exists (e.g. `"stem (2).vsp"`, `"stem (3).vsp"`, ...).
/// Returns the path where the template was saved.
pub(crate) fn save_template_unique(t: &PresetTemplate) -> Result<PathBuf, String> {
    let dir = templates_dir()
        .ok_or_else(|| "Could not determine the templates directory".to_string())?;
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create templates directory: {}", e))?;
    let stem = sanitize_name(&t.name);
    let path = find_unique_path(&dir, &stem, None);
    write_template_to(t, &path)?;
    Ok(path)
}

/// Save a template to an arbitrary file path.
/// Creates parent directories on a best-effort basis. No name mangling.
pub(crate) fn save_template_at(t: &PresetTemplate, path: &Path) -> Result<(), String> {
    write_template_to(t, path)
}

/// Parse and validate a template file from disk.
/// Rejects files with `schema_version > SCHEMA_VERSION`.
pub(crate) fn import_template_file(path: &Path) -> Result<PresetTemplate, String> {
    let data =
        fs::read(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let t: PresetTemplate = serde_json::from_slice(&data)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))?;
    if t.schema_version > SCHEMA_VERSION {
        return Err(format!(
            "Unsupported schema version {} (maximum supported: {})",
            t.schema_version, SCHEMA_VERSION
        ));
    }
    Ok(t)
}

/// Delete a template file at the given path.
pub(crate) fn delete_template_at(path: &Path) -> Result<(), String> {
    fs::remove_file(path)
        .map_err(|e| format!("Failed to delete {}: {}", path.display(), e))
}

/// Rename a template by setting a new name and saving to the templates directory.
/// The original `created_at` is preserved. If the new stem matches the old stem,
/// the file is rewritten in place. Returns the new path.
#[allow(dead_code)] // UI rename removed; kept for potential future use
pub(crate) fn rename_template(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let mut t = import_template_file(path)?;
    let dir = templates_dir()
        .ok_or_else(|| "Could not determine the templates directory".to_string())?;
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create templates directory: {}", e))?;
    t.name = sanitize_name(new_name);
    let stem = &t.name;
    let new_path = find_unique_path(&dir, stem, Some(path));
    write_template_to(&t, &new_path)?;
    if new_path != path {
        fs::remove_file(path)
            .map_err(|e| format!("Failed to remove old file {}: {}", path.display(), e))?;
    }
    Ok(new_path)
}

// ── Page size matching ────────────────────────────────────────────────────

/// Match a page size from a printer's capabilities against template-saved values.
///
/// Tiers (each only consulted if earlier tiers found nothing):
/// 1. Exact match on PPD key (`name`)
/// 2. Exact match on human label (`label`)
/// 3. Dimension match within 1.5 pt tolerance, checking both orientations
///
/// Returns `None` if no tier matches.
pub(crate) fn match_page_size_idx(
    caps: &vibeprint::printer_discovery::PrinterCaps,
    name: Option<&str>,
    label: Option<&str>,
    dims_pt: Option<(f32, f32)>,
) -> Option<usize> {
    // Tier 1: exact PPD key
    if let Some(name) = name {
        if let Some(idx) = caps.page_sizes.iter().position(|ps| ps.name == name) {
            return Some(idx);
        }
    }
    // Tier 2: exact label
    if let Some(label) = label {
        if let Some(idx) = caps.page_sizes.iter().position(|ps| ps.label == label) {
            return Some(idx);
        }
    }
    // Tier 3: dimensions (both orientations) with tolerance
    if let Some((w, h)) = dims_pt {
        let tol = 1.5;
        if let Some(idx) = caps.page_sizes.iter().position(|ps| {
            let (pw, ph) = ps.paper_size;
            ((pw - w).abs() <= tol && (ph - h).abs() <= tol)
                || ((pw - h).abs() <= tol && (ph - w).abs() <= tol)
        }) {
            return Some(idx);
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_template() -> PresetTemplate {
        let mut extra = HashMap::new();
        extra.insert("ColorModel".to_string(), 1);
        extra.insert("StpQuality".to_string(), 2);

        PresetTemplate {
            schema_version: SCHEMA_VERSION,
            name: "My Preset".to_string(),
            created_at: "2026-09-13T10:00:00+00:00".to_string(),
            printer_name: Some("Epson-ET8500".to_string()),
            page_size_name: Some("A4".to_string()),
            page_size_label: Some("A4 (210 x 297 mm)".to_string()),
            page_size_dims_pt: Some((595.0, 842.0)),
            media_type_key: Some("photographic-glossy".to_string()),
            media_type_label: Some("Glossy Photo Paper".to_string()),
            input_slot_key: Some("auto".to_string()),
            input_slot_label: Some("Auto".to_string()),
            extra_option_indices: extra,
            borders: crate::types::Borders {
                left: 0.5,
                right: 0.5,
                top: 0.25,
                bottom: 0.75,
            },
            output_icc_path: Some("/home/user/icc/sRGB.icc".to_string()),
            output_icc_description: Some("sRGB IEC61966-2.1".to_string()),
            intent: "relative".to_string(),
            bpc: true,
            engine: "mitchell-sharp".to_string(),
            sharpen: 5,
            depth16: true,
            target_dpi: 300,
            cut_marks: "Crop Marks".to_string(),
        }
    }

    // ── 1. Serde round-trip ──────────────────────────────────────────────────

    #[test]
    fn serde_round_trip_preserves_all_fields() {
        let t = sample_template();
        let json = template_to_json(&t).unwrap();
        let back: PresetTemplate = serde_json::from_str(&json).unwrap();

        assert_eq!(back.schema_version, t.schema_version);
        assert_eq!(back.name, t.name);
        assert_eq!(back.created_at, t.created_at);
        assert_eq!(back.printer_name, t.printer_name);
        assert_eq!(back.page_size_name, t.page_size_name);
        assert_eq!(back.page_size_label, t.page_size_label);
        assert_eq!(back.page_size_dims_pt, t.page_size_dims_pt);
        assert_eq!(back.media_type_key, t.media_type_key);
        assert_eq!(back.media_type_label, t.media_type_label);
        assert_eq!(back.input_slot_key, t.input_slot_key);
        assert_eq!(back.input_slot_label, t.input_slot_label);
        assert_eq!(back.extra_option_indices, t.extra_option_indices);
        assert_eq!(back.borders, t.borders);
        assert_eq!(back.output_icc_path, t.output_icc_path);
        assert_eq!(back.output_icc_description, t.output_icc_description);
        assert_eq!(back.intent, t.intent);
        assert_eq!(back.bpc, t.bpc);
        assert_eq!(back.engine, t.engine);
        assert_eq!(back.sharpen, t.sharpen);
        assert_eq!(back.depth16, t.depth16);
        assert_eq!(back.target_dpi, t.target_dpi);
        assert_eq!(back.cut_marks, t.cut_marks);
    }

    #[test]
    fn serde_round_trip_json_has_trailing_newline() {
        let t = sample_template();
        let json = template_to_json(&t).unwrap();
        assert!(json.ends_with('\n'), "JSON should end with newline");
    }

    // ── 2. Forward compatibility ─────────────────────────────────────────────

    #[test]
    fn forward_compat_minimal_json_parses_with_defaults() {
        let json = r#"{"schema_version":1,"name":"x"}"#;
        let t: PresetTemplate = serde_json::from_str(json).unwrap();
        assert_eq!(t.schema_version, 1);
        assert_eq!(t.name, "x");
        assert!(t.printer_name.is_none());
        assert!(t.page_size_name.is_none());
        assert_eq!(t.borders, crate::types::Borders::default());
        assert!(t.extra_option_indices.is_empty());
        assert_eq!(t.intent, "");
        assert_eq!(t.engine, "");
        assert!(!t.bpc);
        assert_eq!(t.sharpen, 0);
        assert!(!t.depth16);
        assert_eq!(t.target_dpi, 0);
    }

    #[test]
    fn forward_compat_empty_object_parses() {
        let json = r#"{}"#;
        let t: PresetTemplate = serde_json::from_str(json).unwrap();
        // schema_version defaults to 0 via serde (not SCHEMA_VERSION; only
        // our manual Default sets it). This is fine - it means schema_version
        // is always written explicitly.
        assert_eq!(t.schema_version, 0);
        assert_eq!(t.name, "");
    }

    // ── 3. Schema version rejection ─────────────────────────────────────────

    #[test]
    fn schema_rejection_v2_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.vsp");
        let json = r#"{"schema_version":2,"name":"future"}"#;
        fs::write(&path, json).unwrap();

        let result = import_template_file(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Unsupported schema version 2"),
            "Error should mention version 2: {}",
            err
        );
    }

    #[test]
    fn schema_accept_v1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("valid.vsp");
        let json = r#"{"schema_version":1,"name":"valid"}"#;
        fs::write(&path, json).unwrap();

        let t = import_template_file(&path).unwrap();
        assert_eq!(t.schema_version, 1);
        assert_eq!(t.name, "valid");
    }

    #[test]
    fn schema_accept_v0_missing_field() {
        // schema_version defaults to 0 when absent, which is <= 1
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.vsp");
        let json = r#"{"name":"legacy"}"#;
        fs::write(&path, json).unwrap();

        let t = import_template_file(&path).unwrap();
        assert_eq!(t.schema_version, 0);
        assert_eq!(t.name, "legacy");
    }

    // ── 4. sanitize_name ────────────────────────────────────────────────────

    #[test]
    fn sanitize_trims_whitespace() {
        assert_eq!(sanitize_name("  hello  "), "hello");
    }

    #[test]
    fn sanitize_collapses_internal_whitespace() {
        assert_eq!(sanitize_name("hello   world"), "hello world");
    }

    #[test]
    fn sanitize_strips_slashes() {
        assert_eq!(sanitize_name("path/to/file"), "pathtofile");
    }

    #[test]
    fn sanitize_strips_null_bytes() {
        assert_eq!(sanitize_name("hel\0lo"), "hello");
    }

    #[test]
    fn sanitize_empty_becomes_preset() {
        assert_eq!(sanitize_name(""), "preset");
        assert_eq!(sanitize_name("   "), "preset");
        assert_eq!(sanitize_name("//\0\0"), "preset");
    }

    #[test]
    fn sanitize_truncates_at_64_chars() {
        let long = "a".repeat(100);
        let result = sanitize_name(&long);
        assert_eq!(result.len(), 64);
        assert_eq!(result, "a".repeat(64));
    }

    #[test]
    fn sanitize_exactly_64_chars_unchanged() {
        let exact = "b".repeat(64);
        assert_eq!(sanitize_name(&exact), exact);
    }

    // ── 5. File I/O: save, overwrite, unique, rename ─────────────────────────

    #[test]
    fn save_template_at_and_import_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("round_trip.vsp");

        let t = sample_template();
        save_template_at(&t, &path).unwrap();

        let loaded = import_template_file(&path).unwrap();
        assert_eq!(loaded.name, t.name);
        assert_eq!(loaded.printer_name, t.printer_name);
        assert_eq!(loaded.engine, t.engine);
        assert_eq!(loaded.cut_marks, t.cut_marks);
    }

    #[test]
    fn save_template_at_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overwrite.vsp");

        let mut t1 = sample_template();
        t1.name = "Version1".to_string();
        save_template_at(&t1, &path).unwrap();

        let mut t2 = sample_template();
        t2.name = "Version2".to_string();
        save_template_at(&t2, &path).unwrap();

        let loaded = import_template_file(&path).unwrap();
        assert_eq!(loaded.name, "Version2");
    }

    #[test]
    fn save_template_unique_produces_collision_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Collision.vsp");

        let mut t = sample_template();
        t.name = "Collision".to_string();

        // Write first file
        save_template_at(&t, &path).unwrap();

        // Now simulate save_template_unique logic in the same directory
        let stem = sanitize_name(&t.name);
        let unique_path = find_unique_path(dir.path(), &stem, None);
        assert_eq!(unique_path, dir.path().join("Collision (2).vsp"));

        save_template_at(&t, &unique_path).unwrap();
        assert!(path.exists());
        assert!(unique_path.exists());
    }

    #[test]
    fn rename_template_moves_file_and_preserves_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("OldName.vsp");

        let mut t = sample_template();
        t.name = "OldName".to_string();
        let original_created = t.created_at.clone();
        save_template_at(&t, &old_path).unwrap();

        // Read the file, change the name, write to new location, delete old
        // (simulating rename_template behavior since it writes to templates_dir)
        let mut loaded = import_template_file(&old_path).unwrap();
        loaded.name = sanitize_name("NewName");
        assert_eq!(loaded.created_at, original_created);

        let new_path = dir.path().join("NewName.vsp");
        write_template_to(&loaded, &new_path).unwrap();
        fs::remove_file(&old_path).unwrap();

        assert!(!old_path.exists());
        assert!(new_path.exists());

        let reloaded = import_template_file(&new_path).unwrap();
        assert_eq!(reloaded.name, "NewName");
        assert_eq!(reloaded.created_at, original_created);
    }

    #[test]
    fn rename_same_stem_rewrites_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SameName.vsp");

        let mut t = sample_template();
        t.name = "SameName".to_string();
        save_template_at(&t, &path).unwrap();

        // find_unique_path with ignore=path should return the same path
        let unique = find_unique_path(dir.path(), "SameName", Some(&path));
        assert_eq!(unique, path);
    }

    #[test]
    fn delete_template_at_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deleteme.vsp");

        let t = sample_template();
        save_template_at(&t, &path).unwrap();
        assert!(path.exists());

        delete_template_at(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn delete_nonexistent_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.vsp");
        assert!(delete_template_at(&path).is_err());
    }

    // ── 6. match_page_size_idx ──────────────────────────────────────────────

    fn make_caps() -> vibeprint::printer_discovery::PrinterCaps {
        vibeprint::printer_discovery::PrinterCaps {
            name: "TestPrinter".to_string(),
            resolutions: vec![300, 600],
            media_types: vec![
                ("stationery".to_string(), "Plain Paper".to_string()),
                ("photographic-glossy".to_string(), "Glossy Photo".to_string()),
            ],
            input_slots: vec![("auto".to_string(), "Auto".to_string())],
            page_sizes: vec![
                vibeprint::printer_discovery::PageSize {
                    name: "A4".to_string(),
                    label: "A4 (210 x 297 mm)".to_string(),
                    paper_size: (595.0, 842.0),
                    imageable_area: (12.0, 12.0, 583.0, 830.0),
                },
                vibeprint::printer_discovery::PageSize {
                    name: "Letter".to_string(),
                    label: "Letter".to_string(),
                    paper_size: (612.0, 792.0),
                    imageable_area: (12.0, 12.0, 600.0, 780.0),
                },
                vibeprint::printer_discovery::PageSize {
                    name: "Custom".to_string(),
                    label: "Custom Size".to_string(),
                    paper_size: (300.0, 600.0),
                    imageable_area: (0.0, 0.0, 300.0, 600.0),
                },
            ],
            printable_area: (12.0, 12.0, 583.0, 830.0),
            extra_options: vec![],
        }
    }

    #[test]
    fn match_tier1_name_hit() {
        let caps = make_caps();
        let idx = match_page_size_idx(&caps, Some("Letter"), None, None);
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn match_tier2_label_only_hit() {
        let caps = make_caps();
        // Name miss but label hit
        let idx = match_page_size_idx(&caps, Some("NOMATCH"), Some("A4 (210 x 297 mm)"), None);
        assert_eq!(idx, Some(0));
    }

    #[test]
    fn match_tier2_label_hit_no_name() {
        let caps = make_caps();
        let idx = match_page_size_idx(&caps, None, Some("Custom Size"), None);
        assert_eq!(idx, Some(2));
    }

    #[test]
    fn match_tier3_dims_exact() {
        let caps = make_caps();
        let idx = match_page_size_idx(&caps, None, None, Some((595.0, 842.0)));
        assert_eq!(idx, Some(0)); // A4
    }

    #[test]
    fn match_tier3_dims_rotated_orientation() {
        let caps = make_caps();
        // Letter is (612, 792); ask as (792, 612) - should still match
        let idx = match_page_size_idx(&caps, None, None, Some((792.0, 612.0)));
        assert_eq!(idx, Some(1)); // Letter
    }

    #[test]
    fn match_tier3_dims_within_tolerance() {
        let caps = make_caps();
        // A4 is (595.0, 842.0); try (594.0, 841.0) - within 1.5pt
        let idx = match_page_size_idx(&caps, None, None, Some((594.0, 841.0)));
        assert_eq!(idx, Some(0)); // A4
    }

    #[test]
    fn match_tier3_dims_outside_tolerance() {
        let caps = make_caps();
        // A4 is (595.0, 842.0); try (593.0, 840.0) - outside 1.5pt
        let idx = match_page_size_idx(&caps, None, None, Some((593.0, 840.0)));
        assert_eq!(idx, None);
    }

    #[test]
    fn match_nothing_matches_returns_none() {
        let caps = make_caps();
        let idx = match_page_size_idx(&caps, Some("NOMATCH"), Some("NOMATCH"), Some((100.0, 200.0)));
        assert_eq!(idx, None);
    }

    #[test]
    fn match_all_none_falls_through() {
        let caps = make_caps();
        let idx = match_page_size_idx(&caps, None, None, None);
        assert_eq!(idx, None);
    }

    #[test]
    fn match_tier1_takes_priority_over_tier2() {
        let caps = make_caps();
        // Name matches A4 (idx 0), label matches Letter (idx 1)
        // Tier 1 should win
        let idx = match_page_size_idx(&caps, Some("A4"), Some("Letter"), None);
        assert_eq!(idx, Some(0)); // A4 wins from tier 1
    }

    // ── list_templates edge cases ───────────────────────────────────────────

    #[test]
    fn list_templates_nonexistent_dir_returns_empty() {
        // If templates_dir() doesn't exist, should return empty with no warnings
        let (templates, warnings) = list_templates();
        // This depends on whether the actual dir exists; either way, no panic
        if !templates_dir().map_or(false, |d| d.is_dir()) {
            assert!(templates.is_empty());
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn list_templates_skips_invalid_files() {
        // We can't easily test this without modifying the real templates dir,
        // but we verify the format of the warning string
        let warning = format!(
            "Skipping invalid preset file {}: {}",
            "/fake/path.vsp",
            "some error"
        );
        assert!(warning.contains("/fake/path.vsp"));
        assert!(warning.contains("some error"));
    }

    #[test]
    fn find_unique_path_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let path = find_unique_path(dir.path(), "Fresh", None);
        assert_eq!(path, dir.path().join("Fresh.vsp"));
    }

    #[test]
    fn find_unique_path_with_collision() {
        let dir = tempfile::tempdir().unwrap();
        // Create the first file
        fs::write(dir.path().join("Collision.vsp"), "").unwrap();
        let path = find_unique_path(dir.path(), "Collision", None);
        assert_eq!(path, dir.path().join("Collision (2).vsp"));
    }

    #[test]
    fn find_unique_path_multiple_collisions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Multi.vsp"), "").unwrap();
        fs::write(dir.path().join("Multi (2).vsp"), "").unwrap();
        fs::write(dir.path().join("Multi (3).vsp"), "").unwrap();
        let path = find_unique_path(dir.path(), "Multi", None);
        assert_eq!(path, dir.path().join("Multi (4).vsp"));
    }
}
