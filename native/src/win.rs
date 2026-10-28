//! Thin helpers over raw Win32 calls.

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

pub fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, wp as WPARAM, lp as LPARAM) }
}

pub fn get_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
        from_wide(&buf)
    }
}

pub fn set_text(h: HWND, s: &str) {
    let w = wide(s);
    unsafe {
        SetWindowTextW(h, w.as_ptr());
    }
}

pub fn dpi_scale(h: HWND) -> f32 {
    let d = unsafe { GetDpiForWindow(h) };
    if d == 0 { 1.0 } else { d as f32 / 96.0 }
}

pub fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT { left: l, top: t, right: r, bottom: b }
}

pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub fn move_window(h: HWND, r: &RECT) {
    unsafe {
        MoveWindow(h, r.left, r.top, (r.right - r.left).max(0), (r.bottom - r.top).max(0), 1);
    }
}

pub fn show(h: HWND, visible: bool) {
    unsafe {
        ShowWindow(h, if visible { SW_SHOW } else { SW_HIDE });
    }
}

pub fn is_visible(h: HWND) -> bool {
    unsafe { IsWindowVisible(h) != 0 }
}

pub fn client_rect(h: HWND) -> RECT {
    let mut r = rect(0, 0, 0, 0);
    unsafe {
        GetClientRect(h, &mut r);
    }
    r
}

/// UI font at `px` CSS pixels (scaled for DPI).
pub fn font(family: &str, px: f32, weight: i32) -> HFONT {
    let fam = wide(family);
    unsafe {
        CreateFontW(
            -(px.round() as i32),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            0,
            fam.as_ptr(),
        )
    }
}

pub fn ui_font_family() -> &'static str {
    "Segoe UI Variable Text"
}

/// GDI painting helpers bound to one DC.
pub struct Painter {
    pub dc: HDC,
}

impl Painter {
    pub fn fill(&self, r: &RECT, rgb: u32) {
        unsafe {
            let b = CreateSolidBrush(crate::theme::cref(rgb));
            FillRect(self.dc, r, b);
            DeleteObject(b as _);
        }
    }

    pub fn round_fill(&self, r: &RECT, rgb: u32, radius: i32) {
        unsafe {
            let b = CreateSolidBrush(crate::theme::cref(rgb));
            let pen = CreatePen(PS_SOLID, 1, crate::theme::cref(rgb));
            let ob = SelectObject(self.dc, b as _);
            let op = SelectObject(self.dc, pen as _);
            RoundRect(self.dc, r.left, r.top, r.right, r.bottom, radius, radius);
            SelectObject(self.dc, ob);
            SelectObject(self.dc, op);
            DeleteObject(b as _);
            DeleteObject(pen as _);
        }
    }

    pub fn frame(&self, r: &RECT, rgb: u32) {
        unsafe {
            let b = CreateSolidBrush(crate::theme::cref(rgb));
            FrameRect(self.dc, r, b);
            DeleteObject(b as _);
        }
    }

    pub fn text(&self, s: &str, r: &RECT, rgb: u32, font: HFONT, flags: u32) {
        // An empty Vec's pointer is dangling, and DrawTextW (DT_END_ELLIPSIS) reads it anyway.
        if s.is_empty() {
            return;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut rc = *r;
        unsafe {
            let of = SelectObject(self.dc, font as _);
            SetTextColor(self.dc, crate::theme::cref(rgb));
            SetBkMode(self.dc, TRANSPARENT as i32);
            DrawTextW(self.dc, w.as_ptr(), w.len() as i32, &mut rc, flags | DT_NOPREFIX);
            SelectObject(self.dc, of);
        }
    }

    pub fn text_width(&self, s: &str, font: HFONT) -> i32 {
        if s.is_empty() {
            return 0;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut rc = rect(0, 0, 0, 0);
        unsafe {
            let of = SelectObject(self.dc, font as _);
            DrawTextW(self.dc, w.as_ptr(), w.len() as i32, &mut rc, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(self.dc, of);
        }
        rc.right - rc.left
    }
}

pub const LEFT_MID: u32 = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS;
pub const CENTER_MID: u32 = DT_CENTER | DT_VCENTER | DT_SINGLELINE;
