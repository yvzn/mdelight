//! Colors (same tokens as the webview build's style.css) and Windows dark-mode plumbing.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows_sys::Win32::UI::Controls::SetWindowTheme;

use crate::win::wide;

/// Colors as 0xRRGGBB.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bar_side: u32,
    pub bar: u32,
    pub side: u32,
    pub bg: u32,
    pub hover_bar: u32,
    pub hover_side: u32,
    pub active: u32,
    pub active_bar: u32,
    pub fg_active_bar: u32,
    pub fg: u32,
    pub fg_dim: u32,
    pub border: u32,
    pub border_strong: u32,
    pub accent: u32,
    pub accent_fill: u32,
    pub mark: u32,
    pub code_bg: u32,
    pub input_bg: u32,
}

pub const LIGHT: Palette = Palette {
    dark: false,
    bar_side: 0xdfe2e7,
    bar: 0xebedf0,
    side: 0xf3f4f6,
    bg: 0xffffff,
    hover_bar: 0xd2d5da,
    hover_side: 0xe4e5e7,
    active: 0xd4e8fc,
    active_bar: 0xc2ddf7,
    fg_active_bar: 0x01559f,
    fg: 0x1f2328,
    fg_dim: 0x656d76,
    border: 0xd8dee4,
    border_strong: 0xd0d7de,
    accent: 0x0267c1,
    accent_fill: 0x0267c1,
    mark: 0xffe58a,
    code_bg: 0xf3f4f6,
    input_bg: 0xffffff,
};

pub const DARK: Palette = Palette {
    dark: true,
    bar_side: 0x16181c,
    bar: 0x1d2025,
    side: 0x21252b,
    bg: 0x282c34,
    hover_bar: 0x2b2e33,
    hover_side: 0x2f3339,
    active: 0x20456a,
    active_bar: 0x1f507a,
    fg_active_bar: 0xa9d3fa,
    fg: 0xd7dae0,
    fg_dim: 0x8b929e,
    border: 0x181a1f,
    border_strong: 0x3a3f4b,
    accent: 0x5aa9ef,
    accent_fill: 0x0267c1,
    mark: 0x6b5a17,
    code_bg: 0x2f343e,
    input_bg: 0x1d2025,
};

/// 0xRRGGBB → GDI COLORREF (0x00BBGGRR).
pub const fn cref(c: u32) -> u32 {
    ((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThemePref {
    System,
    Light,
    Dark,
}

impl ThemePref {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            _ => Self::System,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Windows "app mode" setting.
pub fn system_dark() -> bool {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut v: u32 = 1;
    let mut size = 4u32;
    let ok = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut v as *mut u32 as _, &mut size)
    };
    ok == 0 && v == 0
}

pub fn resolve(pref: ThemePref) -> bool {
    match pref {
        ThemePref::System => system_dark(),
        ThemePref::Light => false,
        ThemePref::Dark => true,
    }
}

/// Dark title bar for a top-level window.
pub fn set_title_bar(hwnd: HWND, dark: bool) {
    let v: i32 = dark as i32;
    unsafe {
        DwmSetWindowAttribute(hwnd, 20 /* DWMWA_USE_IMMERSIVE_DARK_MODE */, &v as *const i32 as _, 4);
    }
}

/// Native popup menus follow the app theme. uxtheme's SetPreferredAppMode (ordinal 135) and
/// FlushMenuThemes (136) are undocumented but stable since Windows 10 1903.
pub fn set_menu_theme(dark: bool) {
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        if let Some(f) = GetProcAddress(lib, 135 as _) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(if dark { 2 } else { 3 }); // ForceDark / ForceLight
        }
        if let Some(f) = GetProcAddress(lib, 136 as _) {
            let flush: extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

/// Dark scrollbars and tree-view glyphs for a child control.
pub fn set_control_theme(hwnd: HWND, dark: bool) {
    let name = wide(if dark { "DarkMode_Explorer" } else { "Explorer" });
    unsafe {
        SetWindowTheme(hwnd, name.as_ptr(), std::ptr::null());
    }
}
