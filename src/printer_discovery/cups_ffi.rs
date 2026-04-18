//! FFI bindings for libcups C API
//! Full bindings for printer discovery and capability querying via CUPS API.

use std::ffi::{c_char, c_int, c_uint, CStr};
use std::ptr;

// ── Opaque C types ────────────────────────────────────────────────────────────

#[allow(non_camel_case_types)]
pub enum cups_dest_t {}

#[allow(non_camel_case_types)]
pub enum cups_dinfo_t {}

#[allow(non_camel_case_types)]
pub enum http_t {}

#[allow(non_camel_case_types)]
#[allow(dead_code)]
pub enum ipp_t {}

#[allow(non_camel_case_types)]
pub enum ipp_attribute_t {}

pub const CUPS_HTTP_DEFAULT: *mut http_t = ptr::null_mut();

// Media flags for cupsGetDestMediaByIndex
pub const CUPS_MEDIA_FLAGS_DEFAULT: c_uint = 0x0000;

// ── Concrete C structs ────────────────────────────────────────────────────────

/// Mirror of cups_dest_t C struct layout.
#[repr(C)]
pub struct CupsDest {
    pub name: *const c_char,
    pub _instance: *const c_char,
    pub is_default: c_int,
    pub num_options: c_int,
    pub options: *const CupsOption,
}

/// Mirror of cups_option_t C struct layout.
#[repr(C)]
pub struct CupsOption {
    pub name: *const c_char,
    pub value: *const c_char,
}

/// Mirror of cups_size_t C struct layout (page size + margins).
/// Matches typedef struct cups_size_s in cups/cups.h exactly.
/// Dimensions are in 1/100 mm.
#[repr(C)]
pub struct CupsSize {
    /// PWG media name (e.g. "na_letter_8.5x11in")
    pub media: [c_char; 128],
    /// Width in 1/100 mm
    pub width: c_int,
    /// Height in 1/100 mm
    pub length: c_int,
    /// Bottom margin in 1/100 mm
    pub bottom: c_int,
    /// Left margin in 1/100 mm
    pub left: c_int,
    /// Right margin in 1/100 mm
    pub right: c_int,
    /// Top margin in 1/100 mm
    pub top: c_int,
}

// IPP value tags (partial)
#[allow(dead_code)]
pub const IPP_TAG_KEYWORD: c_int = 0x44;
#[allow(dead_code)]
pub const IPP_TAG_RESOLUTION: c_int = 0x32;
pub const IPP_TAG_INTEGER: c_int = 0x21;
pub const IPP_TAG_ENUM: c_int = 0x23;

// IPP resolution units
pub const IPP_RES_PER_INCH: c_int = 3;

// ── FFI declarations ──────────────────────────────────────────────────────────

#[link(name = "cups")]
extern "C" {
    // Server info
    pub fn cupsServer() -> *const c_char;
    pub fn ippPort() -> c_int;

    // Destination listing
    pub fn cupsGetDests(dests: *mut *mut cups_dest_t) -> c_int;
    pub fn cupsFreeDests(num_dests: c_int, dests: *mut cups_dest_t);

    // Destination info (capabilities)
    pub fn cupsCopyDestInfo(http: *mut http_t, dest: *mut cups_dest_t) -> *mut cups_dinfo_t;
    pub fn cupsFreeDestInfo(info: *mut cups_dinfo_t);

    // Media (page size) enumeration
    pub fn cupsGetDestMediaCount(
        http: *mut http_t,
        dest: *mut cups_dest_t,
        info: *mut cups_dinfo_t,
        flags: c_uint,
    ) -> c_int;
    pub fn cupsGetDestMediaByIndex(
        http: *mut http_t,
        dest: *mut cups_dest_t,
        info: *mut cups_dinfo_t,
        n: c_int,
        flags: c_uint,
        size: *mut CupsSize,
    ) -> c_int;

    // Option support querying
    pub fn cupsFindDestSupported(
        http: *mut http_t,
        dest: *mut cups_dest_t,
        info: *mut cups_dinfo_t,
        option: *const c_char,
    ) -> *mut ipp_attribute_t;

    // IPP attribute accessors
    pub fn ippGetCount(attr: *mut ipp_attribute_t) -> c_int;
    pub fn ippGetValueTag(attr: *mut ipp_attribute_t) -> c_int;
    pub fn ippGetString(
        attr: *mut ipp_attribute_t,
        element: c_int,
        language: *mut *const c_char,
    ) -> *const c_char;
    pub fn ippGetInteger(attr: *mut ipp_attribute_t, element: c_int) -> c_int;
    pub fn ippGetResolution(
        attr: *mut ipp_attribute_t,
        element: c_int,
        yres: *mut c_int,
        units: *mut c_int,
    ) -> c_int;
}

// ── Safe helper wrappers ──────────────────────────────────────────────────────

/// Read the name field of a cups_dest_t pointer.
pub fn get_dest_name(dest: *const cups_dest_t) -> Option<String> {
    if dest.is_null() { return None; }
    unsafe {
        let d = &*(dest as *const CupsDest);
        if d.name.is_null() { return None; }
        CStr::from_ptr(d.name).to_str().ok().map(|s| s.to_string())
    }
}

/// Read the is_default field of a cups_dest_t pointer.
pub fn is_dest_default(dest: *const cups_dest_t) -> bool {
    if dest.is_null() { return false; }
    unsafe {
        let d = &*(dest as *const CupsDest);
        d.is_default != 0
    }
}

/// Index into a cups_dest_t array via the concrete CupsDest struct
/// so pointer arithmetic uses the correct element stride (not ZST stride).
pub fn get_dest_at(dests: *mut cups_dest_t, index: i32) -> *mut cups_dest_t {
    if dests.is_null() || index < 0 { return ptr::null_mut(); }
    unsafe {
        let base = dests as *mut CupsDest;
        base.add(index as usize) as *mut cups_dest_t
    }
}

/// Read a C string from a char array (null-terminated).
pub unsafe fn cstr_from_array(arr: &[c_char]) -> String {
    let bytes: Vec<u8> = arr.iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Collect all keyword string values from an IPP attribute.
pub unsafe fn ipp_attr_strings(attr: *mut ipp_attribute_t) -> Vec<String> {
    if attr.is_null() { return Vec::new(); }
    let n = ippGetCount(attr);
    (0..n).filter_map(|i| {
        let s = ippGetString(attr, i, ptr::null_mut());
        if s.is_null() { return None; }
        CStr::from_ptr(s).to_str().ok().map(|s| s.to_string())
    }).collect()
}

/// Collect all enum integer values from an IPP attribute, returned as decimal strings.
/// IPP print-quality is an enum: 3=draft, 4=normal, 5=high.
pub unsafe fn ipp_attr_enums(attr: *mut ipp_attribute_t) -> Vec<String> {
    if attr.is_null() { return Vec::new(); }
    let tag = ippGetValueTag(attr);
    if tag != IPP_TAG_ENUM && tag != IPP_TAG_INTEGER { return Vec::new(); }
    let n = ippGetCount(attr);
    (0..n).map(|i| ippGetInteger(attr, i).to_string()).collect()
}

/// Collect all resolution values from an IPP attribute, returning DPI as u32.
pub unsafe fn ipp_attr_resolutions(attr: *mut ipp_attribute_t) -> Vec<u32> {
    if attr.is_null() { return Vec::new(); }
    let n = ippGetCount(attr);
    (0..n).filter_map(|i| {
        let mut yres: c_int = 0;
        let mut units: c_int = 0;
        let xres = ippGetResolution(attr, i, &mut yres, &mut units);
        if units == IPP_RES_PER_INCH {
            Some(xres as u32)
        } else if units == 4 { // IPP_RES_PER_CM
            Some((xres as f32 * 2.54) as u32)
        } else {
            None
        }
    }).collect()
}
