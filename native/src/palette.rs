//! One overlay, three uses: quick open (Ctrl+P), go to heading (Ctrl+R),
//! and a single-line prompt (new file name, go to line).

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::app::{app, basename, make_edit, App, Fonts, Mode, ID_PALETTE_EDIT, WM_CHECK_FOCUS};
use crate::theme::{self, Palette};
use crate::win::*;

const ID_LIST: usize = 1;
const MAX_ROWS: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    NewFile,
    GotoLine,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Files,
    Headings,
    Ask(Ask),
}

enum Target {
    File(String),
    Line(usize),
}

struct Item {
    label: String,
    detail: String,
    label_idx: Vec<usize>,
    detail_idx: Vec<usize>,
    indent: usize,
    target: Target,
}

pub struct PaletteUi {
    pub hwnd: HWND,
    edit: HWND,
    list: HWND,
    pub open: bool,
    kind: Kind,
    items: Vec<Item>,
    sel: usize,
    pal: Palette,
    brush: HBRUSH,
    font: HFONT,
    font_bold: HFONT,
    font_small: HFONT,
    row_h: i32,
}

/// Subsequence match; matched char indices and a score favoring contiguous, early hits.
pub fn fuzzy(query: &str, text: &str) -> Option<(f64, Vec<usize>)> {
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut idx = Vec::new();
    let mut score = 0.0;
    let mut from = 0;
    for ch in query.to_lowercase().chars() {
        if ch == ' ' {
            continue;
        }
        let i = (from..t.len()).find(|&i| t[i] == ch)?;
        score += if i == from { 3.0 } else { 1.0 };
        if i == 0 || " \t-_/.".contains(t[i - 1]) {
            score += 2.0;
        }
        idx.push(i);
        from = i + 1;
    }
    Some((score - t.len() as f64 * 0.01, idx))
}

pub fn register(inst: HINSTANCE) {
    let cls = wide("MDelightPalette");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(palette_proc),
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

impl PaletteUi {
    pub fn create(parent: HWND) -> PaletteUi {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let cls = wide("MDelightPalette");
            let hwnd = CreateWindowExW(0, cls.as_ptr(), std::ptr::null(), WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN, 0, 0, 10, 10, parent, std::ptr::null_mut(), inst, std::ptr::null());
            let edit = make_edit(hwnd, ID_PALETTE_EDIT, "");
            show(edit, true);
            let lb = wide("LISTBOX");
            let list = CreateWindowExW(
                0,
                lb.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | (LBS_OWNERDRAWFIXED | LBS_NODATA | LBS_NOINTEGRALHEIGHT | LBS_NOTIFY) as u32,
                0,
                0,
                10,
                10,
                hwnd,
                ID_LIST as HMENU,
                inst,
                std::ptr::null(),
            );
            PaletteUi {
                hwnd,
                edit,
                list,
                open: false,
                kind: Kind::Files,
                items: Vec::new(),
                sel: 0,
                pal: theme::LIGHT,
                brush: std::ptr::null_mut(),
                font: std::ptr::null_mut(),
                font_bold: std::ptr::null_mut(),
                font_small: std::ptr::null_mut(),
                row_h: 28,
            }
        }
    }

    pub fn apply_theme(&mut self, p: &Palette) {
        self.pal = *p;
        unsafe {
            if !self.brush.is_null() {
                DeleteObject(self.brush as _);
            }
            self.brush = CreateSolidBrush(theme::cref(p.input_bg));
        }
        theme::set_control_theme(self.list, p.dark);
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 1) };
    }

    pub fn on_dpi(&mut self, fonts: &Fonts) {
        self.font = fonts.ui;
        self.font_bold = fonts.ui_bold;
        self.font_small = fonts.small;
        send(self.edit, WM_SETFONT, fonts.ui as usize, 1);
    }

    pub fn rect_in(&self, client: &RECT, scale: f32) -> RECT {
        let w = ((640.0 * scale) as i32).min(client.right - (40.0 * scale) as i32);
        let x = (client.right - w) / 2;
        let rows = if matches!(self.kind, Kind::Ask(_)) { 0 } else { self.items.len().clamp(1, MAX_ROWS) };
        let h = (46.0 * scale) as i32 + rows as i32 * self.row_h + if rows > 0 { (6.0 * scale) as i32 } else { 0 };
        let y = (48.0 * scale) as i32;
        rect(x, y, x + w, y + h)
    }

    pub fn place(&mut self, r: &RECT, scale: f32) {
        self.row_h = (28.0 * scale) as i32;
        move_window(self.hwnd, r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let pad = (12.0 * scale) as i32;
        move_window(self.edit, &rect(pad, (13.0 * scale) as i32, w - pad, (13.0 * scale) as i32 + (20.0 * scale) as i32));
        let list_top = (46.0 * scale) as i32;
        let ask = matches!(self.kind, Kind::Ask(_));
        move_window(self.list, &rect(1, list_top, w - 1, h - (4.0 * scale) as i32));
        show(self.list, !ask);
        send(self.list, LB_SETITEMHEIGHT, 0, self.row_h as isize);
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            InvalidateRect(self.hwnd, std::ptr::null(), 1);
        }
    }

    fn show_with(&mut self, kind: Kind, cue: &str, value: &str) {
        self.kind = kind;
        self.open = true;
        let c = wide(cue);
        send(self.edit, EM_SETCUEBANNER, 1, c.as_ptr() as isize);
        set_text(self.edit, value);
        show(self.hwnd, true);
        unsafe { SetFocus(self.edit) };
    }

    /// Ask for one line of text; `select` is the [start, end) byte range pre-selected in `value`.
    pub fn ask(&mut self, kind: Ask, cue: &str, value: &str, select: (usize, usize)) {
        self.items.clear();
        self.show_with(Kind::Ask(kind), cue, value);
        let u = |i: usize| value[..i].encode_utf16().count();
        send(self.edit, EM_SETSEL, u(select.0), u(select.1) as isize);
    }

    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        show(self.hwnd, false);
    }

    /// Focus left the palette (clicked elsewhere): close it.
    pub fn check_focus(&mut self) {
        let f = unsafe { GetFocus() };
        if self.open && f != self.edit && f != self.list && f != self.hwnd {
            self.close();
        }
    }

    fn set_items(&mut self, items: Vec<Item>) {
        self.items = items;
        self.sel = 0;
        send(self.list, LB_SETCOUNT, self.items.len(), 0);
        send(self.list, LB_SETCURSEL, 0, 0);
        unsafe { InvalidateRect(self.list, std::ptr::null(), 1) };
    }

    fn move_sel(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as isize;
        self.sel = ((self.sel as isize + delta).rem_euclid(n)) as usize;
        send(self.list, LB_SETCURSEL, self.sel, 0);
    }

    fn draw_item(&self, di: &DRAWITEMSTRUCT) {
        let p = Painter { dc: di.hDC };
        let pal = &self.pal;
        let r = di.rcItem;
        let selected = di.itemID as usize == self.sel;
        p.fill(&r, if selected { pal.active } else { pal.input_bg });
        let Some(it) = self.items.get(di.itemID as usize) else { return };
        let scale = self.row_h as f32 / 28.0;
        let mut x = r.left + ((14 + it.indent * 14) as f32 * scale) as i32;
        // Label with matched characters bold and accented.
        let chars: Vec<char> = it.label.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let hit = it.label_idx.contains(&i);
            let mut j = i;
            while j < chars.len() && it.label_idx.contains(&j) == hit {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            let font = if hit { self.font_bold } else { self.font };
            let w = p.text_width(&run, font);
            p.text(&run, &rect(x, r.top, x + w + 2, r.bottom), if hit { pal.accent } else { pal.fg }, font, LEFT_MID);
            x += w;
            i = j;
        }
        if !it.detail.is_empty() {
            x += (10.0 * scale) as i32;
            let chars: Vec<char> = it.detail.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let hit = it.detail_idx.contains(&i);
                let mut j = i;
                while j < chars.len() && it.detail_idx.contains(&j) == hit {
                    j += 1;
                }
                let run: String = chars[i..j].iter().collect();
                let w = p.text_width(&run, self.font_small);
                p.text(&run, &rect(x, r.top, (x + w + 2).min(r.right - 4), r.bottom), if hit { pal.accent } else { pal.fg_dim }, self.font_small, LEFT_MID);
                x += w;
                i = j;
            }
        }
    }
}

unsafe extern "system" fn palette_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if !crate::app::app_ready() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let a = app();
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let p = Painter { dc };
            let r = client_rect(hwnd);
            p.fill(&r, a.pal.input_bg);
            p.frame(&r, a.pal.border_strong);
            let s = a.scale;
            let pad = (6.0 * s) as i32;
            let er = rect(pad, pad, r.right - pad, (40.0 * s) as i32);
            p.frame(&er, a.pal.accent);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let dc = wp as HDC;
            SetTextColor(dc, theme::cref(a.pal.fg));
            SetBkColor(dc, theme::cref(a.pal.input_bg));
            a.palette.brush as LRESULT
        }
        WM_MEASUREITEM => {
            let mi = &mut *(lp as *mut MEASUREITEMSTRUCT);
            mi.itemHeight = a.palette.row_h as u32;
            1
        }
        WM_DRAWITEM => {
            a.palette.draw_item(&*(lp as *const DRAWITEMSTRUCT));
            1
        }
        WM_COMMAND => {
            let code = ((wp >> 16) & 0xffff) as u32;
            let id = wp & 0xffff;
            if id == ID_PALETTE_EDIT && code == EN_CHANGE && a.palette.open {
                a.palette_update();
            } else if id == ID_PALETTE_EDIT && code == EN_KILLFOCUS {
                PostMessageW(a.hwnd, WM_CHECK_FOCUS, 0, 0);
            } else if id == ID_LIST && code == LBN_SELCHANGE as u32 {
                let i = send(a.palette.list, LB_GETCURSEL, 0, 0);
                if i >= 0 && GetKeyState(VK_LBUTTON as i32) < 0 {
                    a.palette.sel = i as usize;
                    a.palette_choose();
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl App {
    pub fn open_file_palette(&mut self) {
        self.palette.show_with(Kind::Files, "Go to file…", "");
        self.palette_update();
    }

    pub fn open_heading_palette(&mut self) {
        self.palette.show_with(Kind::Headings, "Go to heading…", "");
        self.palette_update();
    }

    pub fn palette_update(&mut self) {
        let q = get_text(self.palette.edit).trim().to_string();
        let items = match self.palette.kind {
            Kind::Files => {
                let file_item = |path: &str, idx: &[u32]| {
                    let cut = path.rfind('/').map_or(0, |i| i + 1);
                    let cut_chars = path[..cut].chars().count();
                    Item {
                        label: path[cut..].to_string(),
                        detail: path[..cut].to_string(),
                        label_idx: idx.iter().map(|&i| i as usize).filter(|&i| i >= cut_chars).map(|i| i - cut_chars).collect(),
                        detail_idx: idx.iter().map(|&i| i as usize).filter(|&i| i < cut_chars).collect(),
                        indent: 0,
                        target: Target::File(path.to_string()),
                    }
                };
                if q.is_empty() {
                    let files = self.files_snapshot();
                    let mut out: Vec<String> = self.recent.clone();
                    out.extend(files.iter().filter(|f| !self.recent.contains(f)).take(50).cloned());
                    out.truncate(50);
                    out.iter().map(|p| file_item(p, &[])).collect()
                } else {
                    self.core.find_files(&q, 50).iter().map(|m| file_item(&m.path, &m.indices)).collect()
                }
            }
            Kind::Headings => {
                let min = self.headings.iter().map(|h| h.level).min().unwrap_or(1);
                let mut all: Vec<(f64, Item)> = self
                    .headings
                    .iter()
                    .filter_map(|h| {
                        let (score, idx) = if q.is_empty() { (0.0, vec![]) } else { fuzzy(&q, &h.text)? };
                        Some((
                            score,
                            Item {
                                label: h.text.clone(),
                                detail: String::new(),
                                label_idx: idx,
                                detail_idx: vec![],
                                indent: if q.is_empty() { (h.level - min) as usize } else { 0 },
                                target: Target::Line(h.line),
                            },
                        ))
                    })
                    .collect();
                if !q.is_empty() {
                    all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                }
                all.into_iter().map(|(_, it)| it).collect()
            }
            Kind::Ask(_) => Vec::new(),
        };
        self.palette.set_items(items);
        self.layout();
    }

    /// Keys in the palette's edit box; true when handled.
    pub fn palette_key(&mut self, vk: u16) -> bool {
        match vk {
            VK_ESCAPE => {
                self.palette.close();
                self.restore_focus();
                true
            }
            VK_RETURN => {
                if let Kind::Ask(kind) = self.palette.kind {
                    let v = get_text(self.palette.edit).trim().to_string();
                    self.palette.close();
                    self.restore_focus();
                    if !v.is_empty() {
                        match kind {
                            Ask::NewFile => self.create_file(&v),
                            Ask::GotoLine => {
                                if let Ok(n) = v.parse::<usize>() {
                                    self.reveal_line(n, false);
                                }
                            }
                        }
                    }
                } else {
                    self.palette_choose();
                }
                true
            }
            VK_DOWN | VK_UP if !matches!(self.palette.kind, Kind::Ask(_)) => {
                self.palette.move_sel(if vk == VK_DOWN { 1 } else { -1 });
                true
            }
            VK_NEXT | VK_PRIOR if !matches!(self.palette.kind, Kind::Ask(_)) => {
                self.palette.move_sel(if vk == VK_NEXT { MAX_ROWS as isize } else { -(MAX_ROWS as isize) });
                true
            }
            _ => false,
        }
    }

    fn palette_choose(&mut self) {
        let Some(it) = self.palette.items.get(self.palette.sel) else { return };
        let target = match &it.target {
            Target::File(p) => Target::File(p.clone()),
            Target::Line(l) => Target::Line(*l),
        };
        self.palette.close();
        self.restore_focus();
        match target {
            Target::File(p) => self.open_file(&p, None),
            Target::Line(l) => self.reveal_line(l, true),
        }
    }

    pub fn restore_focus(&mut self) {
        unsafe {
            if self.current.is_some() && self.effective_mode() != Mode::Preview {
                SetFocus(self.editor.hwnd);
            } else {
                SetFocus(self.preview);
            }
        }
        let _ = basename;
    }
}
