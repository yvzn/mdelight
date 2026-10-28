//! Find & replace bar over the editor (Ctrl+F; F3 / Shift+F3 to step through matches).

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{EM_SETSEL, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::app::{app, make_edit, App, Fonts, ID_FIND_EDIT, ID_REPLACE_EDIT};
use crate::sci;
use crate::theme::{self, Palette};
use crate::win::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Case,
    Regex,
    Prev,
    Next,
    Close,
    Replace,
    All,
}

pub struct FindBar {
    pub hwnd: HWND,
    find: HWND,
    replace: HWND,
    pub visible: bool,
    case: bool,
    regex: bool,
    btns: Vec<(RECT, Btn)>,
    hover: Option<Btn>,
    pal: Palette,
    brush: HBRUSH,
}

pub fn register(inst: HINSTANCE) {
    let cls = wide("MDelightFind");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(find_proc),
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

impl FindBar {
    pub fn create(parent: HWND) -> FindBar {
        let cls = wide("MDelightFind");
        let hwnd = unsafe {
            CreateWindowExW(0, cls.as_ptr(), std::ptr::null(), WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN, 0, 0, 10, 10, parent, std::ptr::null_mut(), GetModuleHandleW(std::ptr::null()), std::ptr::null())
        };
        let find = make_edit(hwnd, ID_FIND_EDIT, "Find");
        let replace = make_edit(hwnd, ID_REPLACE_EDIT, "Replace  (Enter: next · Ctrl+Enter: all)");
        show(find, true);
        show(replace, true);
        FindBar { hwnd, find, replace, visible: false, case: false, regex: false, btns: Vec::new(), hover: None, pal: theme::LIGHT, brush: std::ptr::null_mut() }
    }

    pub fn apply_theme(&mut self, p: &Palette) {
        self.pal = *p;
        unsafe {
            if !self.brush.is_null() {
                DeleteObject(self.brush as _);
            }
            self.brush = CreateSolidBrush(theme::cref(p.input_bg));
            InvalidateRect(self.hwnd, std::ptr::null(), 1);
        }
    }

    pub fn on_dpi(&self, fonts: &Fonts) {
        send(self.find, WM_SETFONT, fonts.ui as usize, 1);
        send(self.replace, WM_SETFONT, fonts.ui as usize, 1);
    }

    pub fn rect_in(&self, editor: &RECT, scale: f32) -> RECT {
        let w = ((430.0 * scale) as i32).min(editor.right - editor.left - (24.0 * scale) as i32);
        let h = (72.0 * scale) as i32;
        let right = editor.right - (24.0 * scale) as i32;
        let top = editor.top + (6.0 * scale) as i32;
        rect(right - w, top, right, top + h)
    }

    fn layout(&mut self) {
        let r = client_rect(self.hwnd);
        let s = dpi_scale(self.hwnd);
        let px = |v: f32| (v * s) as i32;
        let row = px(30.0);
        let b = px(26.0);
        let (y1, y2) = (px(4.0), px(4.0) + row + px(2.0));
        self.btns.clear();
        let mut x = r.right - px(6.0);
        for btn in [Btn::Close, Btn::Next, Btn::Prev, Btn::Regex, Btn::Case] {
            self.btns.push((rect(x - b, y1 + px(2.0), x, y1 + px(2.0) + b), btn));
            x -= b + px(2.0);
        }
        let find_right = x - px(4.0);
        let mut x2 = r.right - px(6.0);
        let aw = px(40.0);
        let rw = px(64.0);
        self.btns.push((rect(x2 - aw, y2 + px(2.0), x2, y2 + px(2.0) + b), Btn::All));
        x2 -= aw + px(2.0);
        self.btns.push((rect(x2 - rw, y2 + px(2.0), x2, y2 + px(2.0) + b), Btn::Replace));
        x2 -= rw + px(4.0);
        let ih = px(20.0);
        move_window(self.find, &rect(px(12.0), y1 + (row - ih) / 2, find_right - px(6.0), y1 + (row + ih) / 2));
        move_window(self.replace, &rect(px(12.0), y2 + (row - ih) / 2, x2 - px(6.0), y2 + (row + ih) / 2));
    }

    pub fn show(&mut self, seed: Option<&str>) {
        self.visible = true;
        if let Some(s) = seed.filter(|s| !s.is_empty()) {
            set_text(self.find, s);
        }
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
            SetFocus(self.find);
        }
        send(self.find, EM_SETSEL, 0, -1);
    }

    fn paint(&self) {
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(self.hwnd, &mut ps);
            let p = Painter { dc };
            let r = client_rect(self.hwnd);
            let pal = &self.pal;
            let a = app();
            p.fill(&r, pal.bar);
            p.frame(&r, pal.border_strong);
            for e in [self.find, self.replace] {
                let mut er: RECT = std::mem::zeroed();
                GetWindowRect(e, &mut er);
                let mut tl = POINT { x: er.left, y: er.top };
                let mut br = POINT { x: er.right, y: er.bottom };
                ScreenToClient(self.hwnd, &mut tl);
                ScreenToClient(self.hwnd, &mut br);
                let s = a.scale;
                let bx = rect(tl.x - (6.0 * s) as i32, tl.y - (5.0 * s) as i32, br.x + (4.0 * s) as i32, br.y + (5.0 * s) as i32);
                p.fill(&bx, pal.input_bg);
                p.frame(&bx, if GetFocus() == e { pal.accent } else { pal.border_strong });
            }
            for (br, b) in &self.btns {
                let (label, on, font) = match b {
                    Btn::Case => ("Aa", self.case, a.fonts.small),
                    Btn::Regex => (".*", self.regex, a.fonts.small),
                    Btn::Prev => ("\u{E70E}", false, a.fonts.icon),
                    Btn::Next => ("\u{E70D}", false, a.fonts.icon),
                    Btn::Close => ("\u{E711}", false, a.fonts.icon),
                    Btn::Replace => ("Replace", false, a.fonts.small),
                    Btn::All => ("All", false, a.fonts.small),
                };
                if on {
                    p.fill(br, pal.active_bar);
                } else if self.hover == Some(*b) {
                    p.fill(br, pal.hover_bar);
                }
                p.text(label, br, if on { pal.fg_active_bar } else { pal.fg }, font, CENTER_MID);
            }
            EndPaint(self.hwnd, &ps);
        }
    }
}

unsafe extern "system" fn find_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if !crate::app::app_ready() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let a = app();
    match msg {
        WM_SIZE => {
            a.find.layout();
            0
        }
        WM_PAINT => {
            a.find.paint();
            0
        }
        WM_ERASEBKGND => 1,
        WM_CTLCOLOREDIT => {
            let dc = wp as HDC;
            SetTextColor(dc, theme::cref(a.pal.fg));
            SetBkColor(dc, theme::cref(a.pal.input_bg));
            a.find.brush as LRESULT
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            let h = a.find.btns.iter().find(|(r, _)| contains(r, x, y)).map(|(_, b)| *b);
            if h != a.find.hover {
                a.find.hover = h;
                InvalidateRect(hwnd, std::ptr::null(), 0);
                let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut tme);
            }
            0
        }
        WM_MOUSELEAVE => {
            a.find.hover = None;
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            if let Some(b) = a.find.btns.iter().find(|(r, _)| contains(r, x, y)).map(|(_, b)| *b) {
                a.find_button(b);
            }
            0
        }
        WM_COMMAND => {
            let code = ((wp >> 16) & 0xffff) as u32;
            let id = wp & 0xffff;
            if id == ID_FIND_EDIT && code == EN_CHANGE {
                a.find_incremental();
            }
            if code == EN_SETFOCUS || code == EN_KILLFOCUS {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl App {
    fn find_button(&mut self, b: Btn) {
        match b {
            Btn::Case => self.find.case = !self.find.case,
            Btn::Regex => self.find.regex = !self.find.regex,
            Btn::Prev => self.find_next(false),
            Btn::Next => self.find_next(true),
            Btn::Close => self.hide_find(),
            Btn::Replace => self.replace_next(),
            Btn::All => self.replace_all(),
        }
        unsafe { InvalidateRect(self.find.hwnd, std::ptr::null(), 0) };
    }

    pub fn hide_find(&mut self) {
        self.find.visible = false;
        show(self.find.hwnd, false);
        self.editor.focus();
    }

    pub fn find_next(&mut self, forward: bool) {
        let q = get_text(self.find.find);
        if q.is_empty() {
            return;
        }
        if !self.editor.find(&q, forward, self.find.case, self.find.regex) {
            self.toast(&format!("No matches for {q}"));
        }
    }

    fn find_incremental(&mut self) {
        let q = get_text(self.find.find);
        if q.is_empty() {
            return;
        }
        let s = self.editor.msg(sci::SCI_GETSELECTIONSTART, 0, 0);
        self.editor.msg(sci::SCI_SETSEL, s as usize, s);
        self.editor.find(&q, true, self.find.case, self.find.regex);
    }

    fn replace_next(&mut self) {
        let (q, r) = (get_text(self.find.find), get_text(self.find.replace));
        self.editor.replace_next(&q, &r, self.find.case, self.find.regex);
    }

    fn replace_all(&mut self) {
        let (q, r) = (get_text(self.find.find), get_text(self.find.replace));
        let n = self.editor.replace_all(&q, &r, self.find.case, self.find.regex);
        self.toast(&format!("Replaced {n} occurrence{}", if n == 1 { "" } else { "s" }));
    }

    /// Keys in the find/replace edits; true when handled.
    pub fn find_key(&mut self, id: usize, vk: u16) -> bool {
        let shift = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
        match vk {
            VK_ESCAPE => {
                self.hide_find();
                true
            }
            VK_RETURN if id == ID_FIND_EDIT => {
                self.find_next(!shift);
                true
            }
            VK_RETURN => {
                if ctrl {
                    self.replace_all();
                } else {
                    self.replace_next();
                }
                true
            }
            VK_TAB => {
                unsafe { SetFocus(if id == ID_FIND_EDIT { self.find.replace } else { self.find.find }) };
                true
            }
            _ => false,
        }
    }
}
