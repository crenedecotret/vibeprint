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
    std::fs::metadata(path)
        .map(|m| m.len())
        .unwrap_or(0)
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
