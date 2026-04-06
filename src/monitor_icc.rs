//! Monitor ICC profile discovery for X11.
//!
//! Reads the monitor's ICC profile from the X11 _ICC_PROFILE atom on the root window.
//! This is the standard location set by calibration software (ArgyllCMS, DisplayCAL, etc.).

#[cfg(feature = "monitor-icc")]
use std::ffi::CStr;
#[cfg(feature = "monitor-icc")]
use x11::xlib;

/// Get the monitor ICC profile from X11 _ICC_PROFILE atom.
/// Returns the raw ICC profile bytes, or None if not available.
#[cfg(feature = "monitor-icc")]
pub fn get_monitor_profile() -> Option<Vec<u8>> {
    unsafe {
        // Open default X11 display
        let display = xlib::XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return None;
        }

        // Get the root window
        let screen = xlib::XDefaultScreen(display);
        let root_window = xlib::XRootWindow(display, screen);

        // Get the _ICC_PROFILE atom
        let icc_profile_atom = xlib::XInternAtom(display, CStr::from_bytes_with_nul(b"_ICC_PROFILE\0").unwrap().as_ptr(), xlib::True);
        if icc_profile_atom == 0 {
            xlib::XCloseDisplay(display);
            return None;
        }

        // Read the property
        let mut actual_type: xlib::Atom = 0;
        let mut actual_format: i32 = 0;
        let mut nitems: u64 = 0;
        let mut bytes_after: u64 = 0;
        let mut prop_data: *mut u8 = std::ptr::null_mut();

        let result = xlib::XGetWindowProperty(
            display,
            root_window,
            icc_profile_atom,
            0,                    // long_offset
            i32::MAX as i64,      // long_length (read all)
            xlib::False,          // delete
            xlib::AnyPropertyType as u64,  // req_type
            &mut actual_type,
            &mut actual_format,
            &mut nitems,
            &mut bytes_after,
            &mut prop_data as *mut *mut u8,
        );

        if result != xlib::Success as i32 || prop_data.is_null() || nitems == 0 {
            xlib::XCloseDisplay(display);
            return None;
        }

        // Copy the data
        let profile_data = std::slice::from_raw_parts(prop_data, nitems as usize).to_vec();

        // Free X11 resources
        xlib::XFree(prop_data as *mut libc::c_void);
        xlib::XCloseDisplay(display);

        Some(profile_data)
    }
}

#[cfg(not(feature = "monitor-icc"))]
pub fn get_monitor_profile() -> Option<Vec<u8>> {
    None
}

/// Apply monitor ICC profile to image pixels for color-accurate display.
/// Transforms from image's embedded profile (or sRGB fallback) to monitor profile.
/// Returns transformed pixels, or None if transform fails.
pub fn apply_monitor_profile(
    monitor_profile_data: &[u8],
    source_profile_data: Option<&[u8]>,  // Image embedded profile, None = use sRGB
    pixels: &mut [u8],  // RGB triplets
    intent: lcms2::Intent,
    bpc: bool,
) -> Option<()> {
    use lcms2::{PixelFormat, Profile, Transform};
    
    // Verify monitor profile
    if monitor_profile_data.len() < 128 {
        eprintln!("Monitor ICC: Profile too small ({} bytes)", monitor_profile_data.len());
        return None;
    }
    
    // Load source profile (image embedded or sRGB fallback)
    let source_profile = if let Some(src_data) = source_profile_data {
        eprintln!("Monitor ICC: Using image embedded profile ({} bytes)", src_data.len());
        match Profile::new_icc(src_data) {
            Ok(p) => {
                eprintln!("Monitor ICC: Image profile loaded successfully");
                p
            }
            Err(e) => {
                eprintln!("Monitor ICC: Failed to load image profile: {:?}, falling back to sRGB", e);
                Profile::new_srgb()
            }
        }
    } else {
        eprintln!("Monitor ICC: No image profile, using sRGB fallback");
        Profile::new_srgb()
    };
    
    // Load monitor profile
    let monitor_profile = match Profile::new_icc(monitor_profile_data) {
        Ok(p) => {
            eprintln!("Monitor ICC: Monitor profile loaded successfully");
            p
        }
        Err(e) => {
            eprintln!("Monitor ICC: Failed to load monitor profile: {:?}", e);
            return None;
        }
    };
    
    eprintln!("Monitor ICC: Using intent {:?}, BPC={}", intent, bpc);
    
    // Create transform: Source profile → Monitor profile
    // Note: BPC is handled by lcms2 based on the profile and intent
    let transform = match Transform::new(
        &source_profile,
        PixelFormat::RGB_8,
        &monitor_profile,
        PixelFormat::RGB_8,
        intent,
    ) {
        Ok(t) => {
            eprintln!("Monitor ICC: Transform created successfully");
            t
        }
        Err(e) => {
            eprintln!("Monitor ICC: Failed to create transform: {:?}", e);
            return None;
        }
    };
    
    // Sample a few pixels before transform
    if !pixels.is_empty() {
        eprintln!("Monitor ICC: First pixel before: R={} G={} B={}", pixels[0], pixels[1], pixels[2]);
    }
    
    // Apply transform
    let src = pixels.to_vec();
    transform.transform_pixels(&src, pixels);
    
    // Sample after
    if !pixels.is_empty() {
        eprintln!("Monitor ICC: First pixel after: R={} G={} B={}", pixels[0], pixels[1], pixels[2]);
    }
    
    eprintln!("Monitor ICC: Transform applied to {} pixels", pixels.len() / 3);
    
    Some(())
}

/// Get a human-readable description from ICC profile bytes.
/// Parses the profile header to extract device/model info.
pub fn profile_description(profile_data: &[u8]) -> Option<String> {
    if profile_data.len() < 128 {
        return None;
    }

    // ICC profile header structure (first 128 bytes):
    // 0-3:   Profile size (big-endian u32)
    // 4-7:   CMM type signature
    // 8-11:  Profile version
    // 12-15: Profile/device class (mntr, moni, etc.)
    // 16-19: Color space (RGB, XYZ, etc.)
    // 20-23: PCS (profile connection space)
    // 24-35: Date/time (12 bytes)
    // 36-39: Profile file signature ('acsp')
    // 40-43: Primary platform
    // 44-47: Profile flags
    // 48-51: Device manufacturer
    // 52-55: Device model
    // ...etc

    let header = &profile_data[..128];
    
    // Extract 4-byte signatures as strings
    let sig_to_str = |offset: usize| {
        let bytes = &header[offset..offset+4];
        String::from_utf8_lossy(bytes).trim_end().to_string()
    };
    
    let device_class = sig_to_str(12);  // mntr, moni, etc.
    let color_space = sig_to_str(16);   // RGB, XYZ, etc.
    let platform = sig_to_str(40);      // APPL, MSFT, etc.
    let manufacturer = sig_to_str(48);  // Device manufacturer
    let model = sig_to_str(52);         // Device model
    
    // Build description from available info
    let mut parts = vec![];
    
    if !device_class.is_empty() && device_class.chars().all(|c| c.is_ascii_alphabetic()) {
        let class_desc = match device_class.as_str() {
            "mntr" | "moni" => "Monitor",
            "prtr" => "Printer", 
            "scnr" => "Scanner",
            "spac" => "Colorspace",
            "link" => "Device Link",
            "abst" => "Abstract",
            "nmcl" => "Named Color",
            _ => &device_class,
        };
        parts.push(class_desc.to_string());
    }
    
    if !color_space.is_empty() && color_space.chars().all(|c| c.is_ascii_alphabetic()) {
        parts.push(color_space);
    }
    
    let mut details = vec![];
    if !manufacturer.is_empty() && manufacturer != "\0\0\0\0" {
        details.push(format!("mfg: {}", manufacturer));
    }
    if !model.is_empty() && model != "\0\0\0\0" {
        details.push(format!("model: {}", model));
    }
    if !platform.is_empty() && platform != "\0\0\0\0" {
        details.push(format!("platform: {}", platform));
    }
    
    let size_kb = profile_data.len() / 1024;
    
    let main_desc = if parts.is_empty() {
        "ICC Profile".to_string()
    } else {
        parts.join(" ")
    };
    
    if details.is_empty() {
        Some(format!("{} ({}KB)", main_desc, size_kb))
    } else {
        Some(format!("{} ({}KB, {})", main_desc, size_kb, details.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_description() {
        let fake_profile = vec![0u8; 2048]; // 2KB fake profile
        let desc = profile_description(&fake_profile);
        assert!(desc.is_some());
        assert!(desc.unwrap().contains("2KB"));
    }
}
