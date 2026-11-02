//! F1 help: a sheet over the whole window with a card of shortcuts.
//! Task on the left, keys right-aligned as keycaps, two columns when there is room.

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::app::{app, WM_CHECK_FOCUS};
use crate::theme::{self, Palette};
use crate::win::*;

/// (title, column in the two-column layout, rows). Keys: "+" joins caps, " / " separates
/// alternatives; a leading "~" marks a mouse gesture, drawn as plain text instead of caps.
const SECTIONS: &[(&str, usize, &[(&str, &str)])] = &[
    ("Files & folders", 0, &[("Open folder", "Ctrl+Shift+O"), ("New file", "Ctrl+N"), ("Rename / move file", "F2"), ("Save (autosave is on)", "Ctrl+S")]),
    ("Navigate", 0, &[("Go to file", "Ctrl+P / Ctrl+O"), ("Go to heading", "Ctrl+R"), ("Search in all files", "Ctrl+Shift+F"), ("Show file list", "Ctrl+Shift+E")]),
    ("View", 0, &[("Code / Split / Preview", "Ctrl+1 / 2 / 3"), ("Show / hide sidebar", "Ctrl+B"), ("Switch light / dark", "Ctrl+Shift+L"), ("This help", "F1")]),
    ("Preview", 0, &[("Select text by word, then Ctrl+C", "~Drag"), ("Copy code / link / paragraph", "~Right-click"), ("Open image", "~Double-click"), ("View diagram in browser", "~Double-click")]),
    (
        "Editor",
        1,
        &[
            ("Find & replace", "Ctrl+F"),
            ("Next / previous match", "F3 / Shift+F3"),
            ("Go to line", "Ctrl+Alt+G"),
            ("Select next occurrence", "Ctrl+D"),
            ("Undo / redo", "Ctrl+Z / Ctrl+Y"),
            ("Move line up / down", "Alt+↑ / ↓"),
            ("Duplicate line", "Ctrl+Shift+D"),
            ("Delete line", "Ctrl+Shift+K"),
            ("Column (block) selection", "Shift+Alt+Arrows"),
            ("Indent / outdent", "Ctrl+] / ["),
            ("Toggle comment", "Ctrl+/"),
            ("Paste web, Word, images as Markdown", "Ctrl+V"),
            ("Paste as plain text", "Ctrl+Shift+V"),
            ("Copy as rich text", "Ctrl+Shift+C"),
        ],
    ),
];

// Metrics in CSS pixels.
const MARGIN: f32 = 24.0;
const PAD: f32 = 28.0;
const COL_W: f32 = 360.0;
const GAP: f32 = 48.0;
const HEAD_H: f32 = 64.0;
const FOOT_H: f32 = 44.0;
const TITLE_H: f32 = 34.0;
const SECTION_GAP: f32 = 18.0;
const ROW_H: f32 = 30.0;

/// Painted behind the card, then swapped for a translucent scrim; never used by the card itself.
const BACKDROP_KEY: u32 = 0x010203;

pub struct HelpUi {
    pub hwnd: HWND,
    pub open: bool,
    scroll: i32,
    max_scroll: i32,
    close_btn: RECT,
    card: RECT,
    hover_close: bool,
    font_title: HFONT,
    font_section: HFONT,
    font_key: HFONT,
}

pub fn register(inst: HINSTANCE) {
    let cls = wide("MDelightHelp");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(help_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: inst,
        hIcon: std::ptr::null_mut(),
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: cls.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    unsafe { RegisterClassExW(&wc) };
}

/// One run of a key column: a keycap, a separator ("/"), or a mouse gesture.
enum Part<'a> {
    Cap(&'a str),
    Sep(&'a str),
    Gesture(&'a str),
}

fn parts(keys: &str) -> Vec<Part<'_>> {
    if let Some(g) = keys.strip_prefix('~') {
        return vec![Part::Gesture(g)];
    }
    let mut out = Vec::new();
    for (i, alt) in keys.split(" / ").enumerate() {
        if i > 0 {
            out.push(Part::Sep("/"));
        }
        // "Ctrl+/" and "Ctrl++" keep their last character as a key.
        let mut rest = alt;
        while let Some(j) = rest.find('+').filter(|&j| j > 0 && j + 1 < rest.len()) {
            out.push(Part::Cap(&rest[..j]));
            rest = &rest[j + 1..];
        }
        out.push(Part::Cap(rest));
    }
    out
}

impl HelpUi {
    pub fn create(parent: HWND) -> HelpUi {
        let hwnd = unsafe {
            let cls = wide("MDelightHelp");
            CreateWindowExW(WS_EX_LAYERED, cls.as_ptr(), std::ptr::null(), WS_CHILD, 0, 0, 10, 10, parent, std::ptr::null_mut(), GetModuleHandleW(std::ptr::null()), std::ptr::null())
        };
        HelpUi {
            hwnd,
            open: false,
            scroll: 0,
            max_scroll: 0,
            close_btn: rect(0, 0, 0, 0),
            card: rect(0, 0, 0, 0),
            hover_close: false,
            font_title: std::ptr::null_mut(),
            font_section: std::ptr::null_mut(),
            font_key: std::ptr::null_mut(),
        }
    }

    pub fn on_dpi(&mut self, scale: f32) {
        for f in [self.font_title, self.font_section, self.font_key] {
            if !f.is_null() {
                unsafe { DeleteObject(f as _) };
            }
        }
        let fam = ui_font_family();
        self.font_title = font(fam, 20.0 * scale, 600);
        self.font_section = font(fam, 11.5 * scale, 700);
        self.font_key = font(fam, 11.5 * scale, 600);
    }

    pub fn show(&mut self, client: &RECT) {
        self.open = true;
        self.scroll = 0;
        move_window(self.hwnd, client);
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
            SetFocus(self.hwnd);
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
            UpdateWindow(self.hwnd);
        }
    }

    pub fn place(&mut self, client: &RECT) {
        move_window(self.hwnd, client);
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
    }

    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        show(self.hwnd, false);
    }

    /// Focus left the sheet (another window took it): close it.
    pub fn check_focus(&mut self) {
        if self.open && unsafe { GetFocus() } != self.hwnd {
            self.close();
        }
    }

    fn scroll_by(&mut self, dy: i32) {
        let s = (self.scroll + dy).clamp(0, self.max_scroll);
        if s != self.scroll {
            self.scroll = s;
            unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
        }
    }

    fn content_height(cols: usize, px: &dyn Fn(f32) -> i32) -> i32 {
        let mut h = vec![0; cols];
        for (_, col, rows) in SECTIONS {
            let c = if cols == 1 { 0 } else { *col };
            if h[c] > 0 {
                h[c] += px(SECTION_GAP);
            }
            h[c] += px(TITLE_H) + rows.len() as i32 * px(ROW_H);
        }
        h.into_iter().max().unwrap_or(0)
    }

    fn paint(&mut self, p: &Painter, pal: &Palette, fonts: &crate::app::Fonts, scale: f32) {
        let px = |v: f32| (v * scale).round() as i32;
        let r = client_rect(self.hwnd);
        p.fill(&r, BACKDROP_KEY);

        let avail_w = r.right - 2 * px(MARGIN);
        let cols = if avail_w >= px(2.0 * COL_W + GAP + 2.0 * PAD) { 2 } else { 1 };
        let col_w = if cols == 2 { px(COL_W) } else { (avail_w - 2 * px(PAD)).min(px(COL_W + 40.0)) };
        let card_w = cols as i32 * col_w + (cols as i32 - 1) * px(GAP) + 2 * px(PAD);
        let body_h = Self::content_height(cols, &px);
        let full_h = px(HEAD_H) + body_h + px(FOOT_H);
        let card_h = full_h.min(r.bottom - 2 * px(MARGIN)).max(px(HEAD_H + FOOT_H));
        let view_h = card_h - px(HEAD_H) - px(FOOT_H);
        self.max_scroll = (body_h - view_h).max(0);
        self.scroll = self.scroll.min(self.max_scroll);
        let cx = (r.right - card_w) / 2;
        let cy = (r.bottom - card_h) / 2;
        let card = rect(cx, cy, cx + card_w, cy + card_h);
        self.card = card;

        round_box(p, &card, pal.input_bg, pal.border_strong, px(12.0));

        // Header: title, close button, rule.
        let left = card.left + px(PAD);
        let right = card.right - px(PAD);
        p.text("Keyboard shortcuts", &rect(left, card.top, right, card.top + px(HEAD_H)), pal.fg, self.font_title, LEFT_MID);
        let b = px(32.0);
        self.close_btn = rect(card.right - px(16.0) - b, card.top + (px(HEAD_H) - b) / 2, card.right - px(16.0), card.top + (px(HEAD_H) + b) / 2);
        if self.hover_close {
            round_box(p, &self.close_btn, pal.hover_side, pal.hover_side, px(6.0));
        }
        p.text("\u{E711}", &self.close_btn, pal.fg_dim, fonts.icon, CENTER_MID);
        p.fill(&rect(card.left + 1, card.top + px(HEAD_H) - 1, card.right - 1, card.top + px(HEAD_H)), pal.border);

        // Footer: how to get back here, how to leave.
        let foot = rect(left, card.bottom - px(FOOT_H), right, card.bottom);
        p.fill(&rect(card.left + 1, foot.top, card.right - 1, foot.top + 1), pal.border);
        let mut x = left;
        for (t, cap) in [("Press", false), ("F1", true), ("anytime to open this,", false), ("Esc", true), ("to close", false)] {
            x += if cap { self.keycap(p, pal, t, x, foot.top, foot.bottom, px) } else { self.label(p, pal.fg_dim, fonts.small, t, x, foot.top, foot.bottom) };
            x += px(6.0);
        }

        // Body, clipped to the space between header and footer.
        let body = rect(card.left + 1, card.top + px(HEAD_H), card.right - 1, foot.top);
        unsafe {
            let rgn = CreateRectRgn(body.left, body.top, body.right, body.bottom);
            SelectClipRgn(p.dc, rgn);
            DeleteObject(rgn as _);
        }
        let mut y = vec![body.top - self.scroll; cols];
        for (title, col, rows) in SECTIONS {
            let c = if cols == 1 { 0 } else { *col };
            let x0 = left + c as i32 * (col_w + px(GAP));
            let x1 = x0 + col_w;
            if y[c] > body.top - self.scroll {
                y[c] += px(SECTION_GAP);
            }
            // Section title: small caps in the accent color over a hairline.
            let t = rect(x0, y[c] + px(8.0), x1, y[c] + px(TITLE_H) - px(6.0));
            p.text(&title.to_uppercase(), &t, pal.accent, self.font_section, LEFT_MID);
            p.fill(&rect(x0, t.bottom, x1, t.bottom + 1), pal.border_strong);
            y[c] += px(TITLE_H);
            for (what, keys) in rows.iter() {
                let (top, bottom) = (y[c], y[c] + px(ROW_H));
                // Keys first, right-aligned, so the label gets whatever is left.
                let ps = parts(keys);
                let widths: Vec<i32> = ps.iter().map(|q| self.part_width(p, fonts, q, px)).collect();
                let keys_w: i32 = widths.iter().sum::<i32>() + px(4.0) * (ps.len() as i32 - 1);
                let mut kx = x1 - keys_w;
                p.text(what, &rect(x0, top, kx - px(12.0), bottom), pal.fg, fonts.ui, LEFT_MID);
                for (q, w) in ps.iter().zip(widths) {
                    match q {
                        Part::Cap(k) => {
                            self.keycap(p, pal, k, kx, top, bottom, px);
                        }
                        Part::Sep(s) => {
                            p.text(s, &rect(kx, top, kx + w, bottom), pal.fg_dim, fonts.small, CENTER_MID);
                        }
                        Part::Gesture(g) => {
                            p.text(g, &rect(kx, top, kx + w, bottom), pal.fg_dim, fonts.ui, LEFT_MID);
                        }
                    }
                    kx += w + px(4.0);
                }
                y[c] = bottom;
            }
        }
        unsafe { SelectClipRgn(p.dc, std::ptr::null_mut()) };

        // Scroll hint: a fade-free thumb on the card's right edge.
        if self.max_scroll > 0 {
            let track = body.bottom - body.top - px(8.0);
            let thumb = (track as i64 * view_h as i64 / body_h as i64) as i32;
            let top = body.top + px(4.0) + ((track - thumb) as i64 * self.scroll as i64 / self.max_scroll as i64) as i32;
            round_box(p, &rect(card.right - px(8.0), top, card.right - px(4.0), top + thumb), pal.border_strong, pal.border_strong, px(4.0));
        }
    }

    fn part_width(&self, p: &Painter, fonts: &crate::app::Fonts, q: &Part, px: impl Fn(f32) -> i32) -> i32 {
        match q {
            Part::Cap(k) => (p.text_width(k, self.font_key) + px(14.0)).max(px(24.0)),
            Part::Sep(s) => p.text_width(s, fonts.small) + px(2.0),
            Part::Gesture(g) => p.text_width(g, fonts.ui),
        }
    }

    /// A keycap at `x`, vertically centered in [top, bottom); returns its width.
    fn keycap(&self, p: &Painter, pal: &Palette, k: &str, x: i32, top: i32, bottom: i32, px: impl Fn(f32) -> i32) -> i32 {
        let w = (p.text_width(k, self.font_key) + px(14.0)).max(px(24.0));
        let h = px(22.0);
        let y = (top + bottom - h) / 2;
        // The darker bottom edge reads as a physical key.
        round_box(p, &rect(x, y, x + w, y + h), pal.border_strong, pal.border_strong, px(5.0));
        round_box(p, &rect(x, y, x + w, y + h - px(2.0)), pal.code_bg, pal.border_strong, px(5.0));
        p.text(k, &rect(x, y, x + w, y + h - px(2.0)), pal.fg, self.font_key, CENTER_MID);
        w
    }

    fn label(&self, p: &Painter, rgb: u32, f: HFONT, s: &str, x: i32, top: i32, bottom: i32) -> i32 {
        let w = p.text_width(s, f);
        p.text(s, &rect(x, top, x + w + 1, bottom), rgb, f, LEFT_MID);
        w
    }
}

fn round_box(p: &Painter, r: &RECT, fill: u32, border: u32, radius: i32) {
    unsafe {
        let b = CreateSolidBrush(theme::cref(fill));
        let pen = CreatePen(PS_SOLID, 1, theme::cref(border));
        let ob = SelectObject(p.dc, b as _);
        let op = SelectObject(p.dc, pen as _);
        RoundRect(p.dc, r.left, r.top, r.right, r.bottom, radius, radius);
        SelectObject(p.dc, ob);
        SelectObject(p.dc, op);
        DeleteObject(b as _);
        DeleteObject(pen as _);
    }
}

unsafe extern "system" fn help_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if !crate::app::app_ready() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let a = app();
    let pt = || ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            // A layered window: draw into a 32-bit DIB, then hand it over with per-pixel alpha.
            let r = client_rect(hwnd);
            let (w, h) = (r.right.max(1), r.bottom.max(1));
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let mem = CreateCompatibleDC(dc);
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if !bmp.is_null() {
                let old = SelectObject(mem, bmp as _);
                let (pal, scale) = (a.pal, a.scale);
                a.help.paint(&Painter { dc: mem }, &pal, &a.fonts, scale);
                GdiFlush();
                // GDI leaves alpha at 0: the backdrop key becomes a translucent black scrim
                // (premultiplied, so only the alpha byte is set), everything else opaque.
                let scrim = (crate::scrim::alpha(pal.dark) as u32) << 24;
                for px in std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize) {
                    *px = if *px & 0xffffff == BACKDROP_KEY { scrim } else { *px | 0xff00_0000 };
                }
                let size = SIZE { cx: w, cy: h };
                let src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
                UpdateLayeredWindow(hwnd, std::ptr::null_mut(), std::ptr::null(), &size, mem, &src, 0, &blend, ULW_ALPHA);
                SelectObject(mem, old);
                DeleteObject(bmp as _);
            }
            DeleteDC(mem);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_GETDLGCODE => DLGC_WANTALLKEYS as LRESULT,
        WM_KEYDOWN => {
            let line = (ROW_H * a.scale) as i32;
            match wp as u16 {
                VK_ESCAPE | VK_RETURN | VK_SPACE => {
                    a.help.close();
                    a.restore_focus();
                }
                VK_DOWN => a.help.scroll_by(line),
                VK_UP => a.help.scroll_by(-line),
                VK_NEXT => a.help.scroll_by(line * 8),
                VK_PRIOR => a.help.scroll_by(-line * 8),
                VK_HOME => a.help.scroll_by(-a.help.max_scroll),
                VK_END => a.help.scroll_by(a.help.max_scroll),
                _ => {}
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xffff) as i16 as i32;
            a.help.scroll_by(-delta * (ROW_H * 3.0 * a.scale) as i32 / 120);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = pt();
            let hover = contains(&a.help.close_btn, x, y);
            if hover != a.help.hover_close {
                a.help.hover_close = hover;
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_LBUTTONDOWN => {
            // The close button, or anywhere outside the card, closes.
            let (x, y) = pt();
            if contains(&a.help.close_btn, x, y) || !contains(&a.help.card, x, y) {
                a.help.close();
                a.restore_focus();
            }
            0
        }
        WM_KILLFOCUS => {
            PostMessageW(a.hwnd, WM_CHECK_FOCUS, 0, 0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(keys: &str) -> Vec<String> {
        parts(keys)
            .into_iter()
            .map(|p| match p {
                Part::Cap(k) => format!("[{k}]"),
                Part::Sep(s) => s.to_string(),
                Part::Gesture(g) => g.to_string(),
            })
            .collect()
    }

    #[test]
    fn keys_split_into_caps() {
        assert_eq!(caps("Ctrl+Shift+O"), ["[Ctrl]", "[Shift]", "[O]"]);
        assert_eq!(caps("Ctrl+/"), ["[Ctrl]", "[/]"]);
        assert_eq!(caps("Ctrl+] / ["), ["[Ctrl]", "[]]", "/", "[[]"]);
        assert_eq!(caps("Ctrl+1 / 2 / 3"), ["[Ctrl]", "[1]", "/", "[2]", "/", "[3]"]);
        assert_eq!(caps("~Double-click"), ["Double-click"]);
    }
}
