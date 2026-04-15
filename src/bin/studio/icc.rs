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

/// Scan ICC directories and send results through channel
pub(crate) fn scan_icc_directories(tx: Sender<Vec<IccProfileEntry>>) {
    let mut profiles = Vec::new();

    // Standard Linux ICC profile directories (system)
    let system_dirs = vec![
        PathBuf::from("/usr/share/color/icc"),
        PathBuf::from("/usr/local/share/color/icc"),
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

    // Sort by description for consistent ordering
    profiles.sort_by(|a, b| {
        a.description
            .to_lowercase()
            .cmp(&b.description.to_lowercase())
    });

    let _ = tx.send(profiles);
}

fn scan_directory(dir: &PathBuf, source: IccProfileSource, profiles: &mut Vec<IccProfileEntry>) {
    use lcms2::Profile;

    if !dir.exists() {
        return;
    }

    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());

            if extension.as_deref() != Some("icc") && extension.as_deref() != Some("icm") {
                continue;
            }

            // Try to extract the internal profile description and date
            let (description, date) = if let Ok(bytes) = std::fs::read(&path) {
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
                    (desc, file_date)
                } else {
                    // Fallback to filename if profile loading fails
                    let desc = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let file_date = extract_file_date(&path);
                    (desc, file_date)
                }
            } else {
                // Fallback to filename if file read fails
                let desc = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let file_date = extract_file_date(&path);
                (desc, file_date)
            };

            profiles.push(IccProfileEntry {
                path,
                description,
                date,
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
    let flags = if bpc {
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
            flags,
        )
        .ok()?;

        let mut output_space = vec![0u8; pixels.len()];
        let src = pixels.to_vec();
        to_output.transform_pixels(&src, &mut output_space);

        let to_monitor = Transform::new_flags(
            &output_profile,
            PixelFormat::RGB_8,
            &monitor_profile,
            PixelFormat::RGB_8,
            intent,
            flags,
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
        flags,
    )
    .ok()?;
    let src = pixels.to_vec();
    to_monitor.transform_pixels(&src, pixels);
    Some(())
}
