//! Entry points of the C++ side (cpp/*.cpp and the vendored Scintilla).

use std::ffi::{c_char, c_void};
use windows_sys::Win32::Foundation::HWND;

/// Notifications from the preview control to its parent (see cpp/preview.cpp).
pub const PVN_SCROLL: u32 = 0x8000 + 0x100;
pub const PVN_LINK: u32 = 0x8000 + 0x101;
pub const PVN_TOAST: u32 = 0x8000 + 0x102;
pub const PVN_MERMAID: u32 = 0x8000 + 0x103;
pub const PVN_COPY: u32 = 0x8000 + 0x104;

/// PVN_COPY payload: the selection as an HTML fragment (UTF-8) and as plain text.
#[repr(C)]
pub struct PvCopy {
    pub html: *const c_char,
    pub text: *const u16,
    pub text_len: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PvAnchor {
    pub line: i32,
    pub top: f32,
}

extern "C" {
    pub fn Scintilla_RegisterClasses(hinstance: *mut c_void) -> i32;
    pub fn md_create_lexer() -> *mut c_void;
    pub fn pick_folder(owner: HWND, out: *mut u16, cap: i32) -> i32;

    pub fn pv_create(parent: HWND) -> HWND;
    pub fn pv_set_document(h: HWND, html: *const c_char, css: *const c_char, base_dir: *const u16, bg: u32, sel: u32, keep_scroll: i32);
    pub fn pv_scroll_y(h: HWND) -> f32;
    pub fn pv_set_scroll_y(h: HWND, y: f32);
    pub fn pv_view_height(h: HWND) -> f32;
    pub fn pv_content_height(h: HWND) -> f32;
    pub fn pv_anchors(h: HWND, out: *mut PvAnchor, cap: i32) -> i32;
    pub fn pv_find_id(h: HWND, id: *const c_char) -> f32;
    pub fn pv_clear(h: HWND);
    pub fn pv_set_background(h: HWND, bg: u32);
    pub fn pv_snapshot(h: HWND, path: *const u16) -> i32;
}
