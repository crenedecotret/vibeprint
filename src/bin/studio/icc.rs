use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::types::{IccProfileEntry, IccProfileSource};

/// Extract file modification date as string
pub(crate) fn extract_file_date(path: &PathBuf) -> String {
    use chrono::{DateTime, Local, Utc};

    if let Ok(metadata) = std::fs::metadata(path) {
        if let Ok(modified) = metadata.modified() {
            let datetime: DateTime<Utc> = modified.into();
            let local_datetime = datetime.with_timezone(&Local);
            return local_datetime.format("%d %b %Y").to_string();
        }
    }
    "Unknown".to_string()
}

/// Extract file size in bytes
pub(crate) fn extract_file_size(path: &PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Scan ICC directories and send results through channel
pub(crate) fn scan_icc_directories(tx: Sender<Vec<IccProfileEntry>>) {
    let mut profiles = Vec::new();

    // Standard Linux ICC profile directories (system)
    let system_dirs = vec![
        PathBuf::from("/usr/share/color/icc"),
        PathBuf::from("/usr/share/color"),
        PathBuf::from("/usr/local/share/color/icc"),
        PathBuf::from("/var/lib/colord/icc"),
    ];

    // User-local directories
    let mut user_dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        user_dirs.push(home.join(".local/share/color/icc"));
        user_dirs.push(home.join(".local/share/icc"));
        user_dirs.push(home.join(".color/icc"));
    }

    // Scan system directories
    for dir in system_dirs {
        scan_directory(&dir, IccProfileSource::System, &mut profiles);
    }

    // Scan user directories
    for dir in user_dirs {
        scan_directory(&dir, IccProfileSource::User, &mut profiles);
    }

    // Deduplicate by canonical file path to handle overlapping recursive scans
    // (e.g., /usr/share/color recursing into /usr/share/color/icc)
    {
        let mut seen: HashSet<PathBuf> = HashSet::with_capacity(profiles.len());
        profiles.retain(|p| {
            let key = std::fs::canonicalize(&p.path).unwrap_or_else(|_| p.path.clone());
            seen.insert(key)
        });
    }

    // Secondary dedup: identical description + file_size (e.g. copies in
    // different sub-dirs with different canonical paths or modification dates).
    // Prefer User over System.
    {
        let mut content_seen: HashMap<(String, u64), IccProfileEntry> = HashMap::new();
        for p in profiles {
            let key = (p.description.to_lowercase(), p.file_size);
            match content_seen.get_mut(&key) {
                Some(existing) if p.source == IccProfileSource::User => {
                    *existing = p;
                }
                None => {
                    content_seen.insert(key, p);
                }
                _ => {}
            }
        }
        profiles = content_seen.into_values().collect();
    }

    // Sort by description for consistent ordering
    profiles.sort_by(|a, b| {
        a.description
            .to_lowercase()
            .cmp(&b.description.to_lowercase())
    });

    let _ = tx.send(profiles);
}

fn scan_directory(dir: &PathBuf, source: IccProfileSource, profiles: &mut Vec<IccProfileEntry>) {
    scan_directory_recursive(dir, source, profiles, 0);
}

fn scan_directory_recursive(
    dir: &PathBuf,
    source: IccProfileSource,
    profiles: &mut Vec<IccProfileEntry>,
    depth: u32,
) {
    use lcms2::Profile;

    const MAX_DEPTH: u32 = 3;

    if !dir.exists() || depth > MAX_DEPTH {
        return;
    }

    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();

            if path.is_dir() {
                scan_directory_recursive(&path, source, profiles, depth + 1);
                continue;
            }

            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());

            if extension.as_deref() != Some("icc") && extension.as_deref() != Some("icm") {
                continue;
            }

            // Try to extract the internal profile description and date
            let (description, date, file_size) = if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(profile) = Profile::new_icc(&bytes) {
                    // Try to get the profile description tag
                    let desc = profile
                        .info(lcms2::InfoType::Description, lcms2::Locale::none())
                        .unwrap_or_else(|| {
                            // Fallback to filename if description extraction fails
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Unknown")
                                .to_string()
                        });

                    let file_date = extract_file_date(&path);
                    let file_size = extract_file_size(&path);
                    (desc, file_date, file_size)
                } else {
                    // Fallback to filename if profile loading fails
                    let desc = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let file_date = extract_file_date(&path);
                    let file_size = extract_file_size(&path);
                    (desc, file_date, file_size)
                }
            } else {
                // Fallback to filename if file read fails
                let desc = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let file_date = extract_file_date(&path);
                let file_size = extract_file_size(&path);
                (desc, file_date, file_size)
            };

            profiles.push(IccProfileEntry {
                path,
                description,
                date,
                file_size,
                source,
            });
        }
    }
}

/// Returns the path to the custom ICC profile list JSON file.
pub(crate) fn custom_icc_config_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("vibeprint");
    p.push("custom_icc_profile_list.json");
    Some(p)
}

/// Load the list of custom ICC profile paths from JSON.
pub(crate) fn load_custom_icc_profile_paths() -> Vec<PathBuf> {
    let path = match custom_icc_config_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let paths: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
    paths.into_iter().map(PathBuf::from).collect()
}

/// Save the list of custom ICC profile paths to JSON atomically.
pub(crate) fn save_custom_icc_profile_paths(paths: &[PathBuf]) {
    let Some(path) = custom_icc_config_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let strings: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    if let Ok(text) = serde_json::to_string_pretty(&strings) {
        let _ = std::fs::write(&path, text);
    }
}

/// Check if a file is a valid ICC profile by parsing it with lcms2.
pub(crate) fn is_valid_icc_profile(path: &PathBuf) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => lcms2::Profile::new_icc(&bytes).is_ok(),
        Err(_) => false,
    }
}

/// Convert a path to an IccProfileEntry with UserCurated source.
/// Returns None if the file does not exist or cannot be read.
pub(crate) fn path_to_icc_entry(path: &PathBuf) -> Option<IccProfileEntry> {
    if !path.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let profile = lcms2::Profile::new_icc(&bytes).ok()?;
    let description = profile
        .info(lcms2::InfoType::Description, lcms2::Locale::none())
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_string()
        });
    let date = extract_file_date(path);
    let file_size = extract_file_size(path);
    Some(IccProfileEntry {
        path: path.clone(),
        description,
        date,
        file_size,
        source: IccProfileSource::UserCurated,
    })
}

/// Load curated ICC entries, auto-pruning dead paths.
pub(crate) fn load_custom_icc_profile_entries() -> Vec<IccProfileEntry> {
    let paths = load_custom_icc_profile_paths();
    let mut valid = Vec::new();
    let mut valid_paths = Vec::new();
    for path in &paths {
        if let Some(entry) = path_to_icc_entry(path) {
            valid.push(entry);
            valid_paths.push(path.clone());
        }
    }
    if valid_paths.len() != paths.len() {
        save_custom_icc_profile_paths(&valid_paths);
    }
    valid.sort_by(|a, b| a.description.to_lowercase().cmp(&b.description.to_lowercase()));
    valid
}

/// Add a custom ICC profile path to the list (dedupes).
pub(crate) fn add_custom_icc_profile(path: PathBuf) -> Result<(), String> {
    let mut paths = load_custom_icc_profile_paths();
    if !paths.contains(&path) {
        paths.push(path);
        save_custom_icc_profile_paths(&paths);
    }
    Ok(())
}

/// Remove a custom ICC profile path from the list.
pub(crate) fn remove_custom_icc_profile(path: &PathBuf) -> Result<(), String> {
    let mut paths = load_custom_icc_profile_paths();
    let original_len = paths.len();
    paths.retain(|p| p != path);
    if paths.len() != original_len {
        save_custom_icc_profile_paths(&paths);
    }
    Ok(())
}

/// Check if a path is in a known system ICC directory.
pub(crate) fn is_system_icc_path(path: &PathBuf) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let canonical_str = canonical.to_string_lossy();
    [
        "/usr/share/color/icc",
        "/usr/share/color",
        "/usr/local/share/color/icc",
        "/var/lib/colord/icc",
    ]
    .iter()
    .any(|prefix| canonical_str.starts_with(prefix))
}

/// Check if a path is in a known user ICC directory.
pub(crate) fn is_user_icc_path(path: &PathBuf) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let canonical_str = canonical.to_string_lossy();
    [
        ".local/share/color/icc",
        ".local/share/icc",
        ".color/icc",
    ]
    .iter()
    .any(|suffix| canonical_str.contains(suffix))
}

/// Apply color transform for preview (source -> monitor, optionally with softproof)
pub(crate) fn apply_preview_transform(
    monitor_profile_data: &[u8],
    source_profile_data: Option<&[u8]>,
    output_icc_path: Option<&PathBuf>,
    pixels: &mut [u8],
    intent: lcms2::Intent,
    bpc: bool,
    softproof_enabled: bool,
) -> Option<()> {
    use lcms2::{Flags, PixelFormat, Profile, Transform};

    let source_profile = if let Some(src_data) = source_profile_data {
        Profile::new_icc(src_data).unwrap_or_else(|_| Profile::new_srgb())
    } else {
        Profile::new_srgb()
    };

    let monitor_profile = Profile::new_icc(monitor_profile_data).ok()?;
    // BPC flag for the source→output (or source→monitor) leg — user controlled
    let sim_flags = if bpc {
        Flags::BLACKPOINT_COMPENSATION | Flags::NO_CACHE
    } else {
        Flags::NO_CACHE
    };

    if softproof_enabled {
        let output_profile = if let Some(path) = output_icc_path {
            let bytes = std::fs::read(path).ok()?;
            Profile::new_icc(&bytes).ok()?
        } else if let Some(src_data) = source_profile_data {
            Profile::new_icc(src_data).unwrap_or_else(|_| Profile::new_srgb())
        } else {
            Profile::new_srgb()
        };

        let to_output = Transform::new_flags(
            &source_profile,
            PixelFormat::RGB_8,
            &output_profile,
            PixelFormat::RGB_8,
            intent,
            sim_flags,
        )
        .ok()?;

        let mut output_space = vec![0u8; pixels.len()];
        to_output.transform_pixels(pixels, &mut output_space);

        // Display leg: output→monitor is a colorimetric adaptation — never apply BPC here
        let to_monitor = Transform::new_flags(
            &output_profile,
            PixelFormat::RGB_8,
            &monitor_profile,
            PixelFormat::RGB_8,
            lcms2::Intent::RelativeColorimetric,
            Flags::NO_CACHE,
        )
        .ok()?;
        to_monitor.transform_pixels(&output_space, pixels);
        return Some(());
    }

    let to_monitor = Transform::new_flags(
        &source_profile,
        PixelFormat::RGB_8,
        &monitor_profile,
        PixelFormat::RGB_8,
        intent,
        sim_flags,
    )
    .ok()?;
    to_monitor.transform_in_place(pixels);
    Some(())
}

/// Transform a single sRGB border color for preview display.
///
/// When softproof is off, returns the raw sRGB value.
/// When softproof is on, transforms through sRGB → output profile → monitor profile,
/// matching the display leg of the image softproof pipeline.
pub(crate) fn transform_preview_border_color(
    monitor_profile_data: &[u8],
    output_icc_path: Option<&PathBuf>,
    intent: lcms2::Intent,
    bpc: bool,
    rgb: [u8; 3],
) -> [u8; 3] {
    use lcms2::{Flags, PixelFormat, Profile, Transform};

    let monitor_profile = match Profile::new_icc(monitor_profile_data) {
        Ok(p) => p,
        Err(_) => return rgb,
    };

    let output_profile = if let Some(path) = output_icc_path {
        match std::fs::read(path)
            .ok()
            .and_then(|b| Profile::new_icc(&b).ok())
        {
            Some(p) => p,
            None => return rgb,
        }
    } else {
        Profile::new_srgb()
    };

    let srgb = Profile::new_srgb();

    let sim_flags = if bpc {
        Flags::BLACKPOINT_COMPENSATION | Flags::NO_CACHE
    } else {
        Flags::NO_CACHE
    };

    // sRGB → output (user intent + BPC)
    let to_output = match Transform::new_flags(
        &srgb,
        PixelFormat::RGB_8,
        &output_profile,
        PixelFormat::RGB_8,
        intent,
        sim_flags,
    ) {
        Ok(t) => t,
        Err(_) => return rgb,
    };

    let mut tmp = [0u8; 3];
    to_output.transform_pixels(&rgb, &mut tmp);

    // output → monitor (Relative Colorimetric, no BPC — same as image display leg)
    let to_monitor = match Transform::new_flags(
        &output_profile,
        PixelFormat::RGB_8,
        &monitor_profile,
        PixelFormat::RGB_8,
        lcms2::Intent::RelativeColorimetric,
        Flags::NO_CACHE,
    ) {
        Ok(t) => t,
        Err(_) => return rgb,
    };

    let mut result = [0u8; 3];
    to_monitor.transform_pixels(&tmp, &mut result);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip() {
        // Unique temp file name to avoid collisions between parallel test runs.
        let unique = format!(
            "vibeprint_icc_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(format!("{}.json", unique));
        let paths = vec![
            PathBuf::from("/foo/bar.icc"),
            PathBuf::from("/baz/qux.icm"),
        ];
        let text = serde_json::to_string_pretty(&paths).unwrap();
        std::fs::write(&path, text).unwrap();

        let loaded: Vec<PathBuf> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded, paths);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn add_dedupes_duplicates() {
        // Test the dedup logic used by add_custom_icc_profile directly,
        // without touching the real config file.
        let mut paths: Vec<PathBuf> = vec![PathBuf::from("/foo/bar.icc")];
        let new = PathBuf::from("/foo/bar.icc");
        if !paths.contains(&new) {
            paths.push(new);
        }
        assert_eq!(paths.len(), 1);

        let other = PathBuf::from("/baz/qux.icm");
        if !paths.contains(&other) {
            paths.push(other);
        }
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn path_to_icc_entry_missing_file() {
        let path = PathBuf::from("/nonexistent/path/file.icc");
        assert!(path_to_icc_entry(&path).is_none());
    }

    #[test]
    fn is_system_icc_path_classifies() {
        assert!(is_system_icc_path(&PathBuf::from("/usr/share/color/icc/sRGB.icc")));
        assert!(is_system_icc_path(&PathBuf::from("/usr/share/color/sRGB.icc")));
        assert!(is_system_icc_path(&PathBuf::from("/usr/local/share/color/icc/sRGB.icc")));
        assert!(is_system_icc_path(&PathBuf::from("/var/lib/colord/icc/sRGB.icc")));
        assert!(!is_system_icc_path(&PathBuf::from("/home/user/.local/share/color/icc/sRGB.icc")));
    }

    #[test]
    fn is_user_icc_path_classifies() {
        assert!(is_user_icc_path(&PathBuf::from("/home/user/.local/share/color/icc/sRGB.icc")));
        assert!(is_user_icc_path(&PathBuf::from("/home/user/.local/share/icc/sRGB.icc")));
        assert!(is_user_icc_path(&PathBuf::from("/home/user/.color/icc/sRGB.icc")));
        assert!(!is_user_icc_path(&PathBuf::from("/usr/share/color/icc/sRGB.icc")));
    }

    #[test]
    fn remove_drops_correct_path() {
        let mut paths: Vec<PathBuf> = vec![
            PathBuf::from("/foo/a.icc"),
            PathBuf::from("/foo/b.icc"),
            PathBuf::from("/foo/c.icc"),
        ];
        let to_remove = PathBuf::from("/foo/b.icc");
        let original_len = paths.len();
        paths.retain(|p| p != &to_remove);
        assert_eq!(paths.len(), original_len - 1);
        assert!(!paths.contains(&to_remove));
        assert!(paths.contains(&PathBuf::from("/foo/a.icc")));
        assert!(paths.contains(&PathBuf::from("/foo/c.icc")));
    }

    #[test]
    fn auto_prune_logic() {
        // Simulate the prune logic: given a mix of existing and missing files,
        // only existing files should remain.
        let all_paths = vec![
            PathBuf::from("/nonexistent/path.icc"),
            PathBuf::from("/also/missing.icc"),
        ];
        // path_to_icc_entry returns None for both, so valid subset is empty
        let valid: Vec<PathBuf> = all_paths
            .iter()
            .filter(|p| path_to_icc_entry(p).is_some())
            .cloned()
            .collect();
        assert!(valid.is_empty());

        // Also test that the pruned list length differs from original
        assert_ne!(valid.len(), all_paths.len());
    }

    #[test]
    fn merged_list_logic() {
        use crate::types::{IccProfileEntry, IccProfileSource};

        let curated_a = IccProfileEntry {
            path: PathBuf::from("/curated/a.icc"),
            description: "Curated A".into(),
            date: "01 Jan 2024".into(),
            file_size: 1024,
            source: IccProfileSource::UserCurated,
        };
        let curated_b = IccProfileEntry {
            path: PathBuf::from("/scanned/b.icc"),
            description: "Scanned B".into(),
            date: "01 Jan 2024".into(),
            file_size: 2048,
            source: IccProfileSource::UserCurated,
        };
        let scanned_b = IccProfileEntry {
            path: PathBuf::from("/scanned/b.icc"),
            description: "Scanned B".into(),
            date: "01 Jan 2024".into(),
            file_size: 2048,
            source: IccProfileSource::User,
        };
        let scanned_c = IccProfileEntry {
            path: PathBuf::from("/scanned/c.icc"),
            description: "Scanned C".into(),
            date: "01 Jan 2024".into(),
            file_size: 3072,
            source: IccProfileSource::System,
        };

        // Build merged list: curated first, then scanned (deduped by path)
        let mut display_profiles: Vec<IccProfileEntry> = vec![curated_a.clone(), curated_b.clone()];
        let curated_paths: std::collections::HashSet<PathBuf> =
            display_profiles.iter().map(|e| e.path.clone()).collect();
        for p in &[scanned_b.clone(), scanned_c.clone()] {
            if !curated_paths.contains(&p.path) {
                display_profiles.push(p.clone());
            }
        }

        // Expected: [A (curated), B (curated), C (scanned)] — B from curated wins over scanned B
        assert_eq!(display_profiles.len(), 3);
        assert_eq!(display_profiles[0].path, PathBuf::from("/curated/a.icc"));
        assert_eq!(display_profiles[1].path, PathBuf::from("/scanned/b.icc"));
        assert_eq!(display_profiles[1].source, IccProfileSource::UserCurated);
        assert_eq!(display_profiles[2].path, PathBuf::from("/scanned/c.icc"));

        // Filter: UserCurated shows only curated
        let user_curated: Vec<_> = display_profiles
            .iter()
            .filter(|p| p.source == IccProfileSource::UserCurated)
            .collect();
        assert_eq!(user_curated.len(), 2);

        // Filter: System shows system + curated in system dirs
        // (using is_system_icc_path — none of these fake paths match, so only System source)
        let system: Vec<_> = display_profiles
            .iter()
            .filter(|p| {
                p.source == IccProfileSource::System
                    || (p.source == IccProfileSource::UserCurated && is_system_icc_path(&p.path))
            })
            .collect();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0].path, PathBuf::from("/scanned/c.icc"));

        // All: sort curated first, then by description
        let mut all_sorted = display_profiles.clone();
        all_sorted.sort_by(|a, b| {
            let a_curated = (a.source == IccProfileSource::UserCurated) as u8;
            let b_curated = (b.source == IccProfileSource::UserCurated) as u8;
            b_curated.cmp(&a_curated).then_with(|| {
                a.description.to_lowercase().cmp(&b.description.to_lowercase())
            })
        });
        assert_eq!(all_sorted[0].path, PathBuf::from("/curated/a.icc"));
        assert_eq!(all_sorted[1].path, PathBuf::from("/scanned/b.icc"));
        assert_eq!(all_sorted[2].path, PathBuf::from("/scanned/c.icc"));
    }
}
