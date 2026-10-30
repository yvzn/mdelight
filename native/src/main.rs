#![windows_subsystem = "windows"]

//! MDelight, native build: Win32 window, Scintilla editor, litehtml preview drawn with Direct2D.

mod app;
mod core;
mod copy;
mod crash;
mod ffi;
mod findbar;
mod fuzzy;
mod markdown;
mod mermaid;
mod palette;
mod panels;
mod paste;
mod sci;
mod settings;
mod theme;
mod win;

use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};

/// Whether a system font family is installed (DirectWrite falls back silently otherwise).
pub fn font_exists(family: &str) -> bool {
    use windows_sys::Win32::Graphics::Gdi::*;
    unsafe extern "system" fn found(_: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, flag: isize) -> i32 {
        *(flag as *mut bool) = true;
        0
    }
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    let w: Vec<u16> = family.encode_utf16().take(31).collect();
    lf.lfFaceName[..w.len()].copy_from_slice(&w);
    lf.lfCharSet = DEFAULT_CHARSET as u8;
    let mut hit = false;
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        EnumFontFamiliesExW(dc, &lf, Some(found), &mut hit as *mut bool as isize, 0);
        ReleaseDC(std::ptr::null_mut(), dc);
    }
    hit
}

fn main() {
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
    }
    crash::install();
    app::App::create();
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if app::app().pre_translate(&msg) {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
