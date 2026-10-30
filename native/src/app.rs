//! Main window: state, layout, painting, keyboard shortcuts, and the document lifecycle
//! (open, autosave, external edits, preview refresh, scroll sync).

use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::core::{Core, SearchEvent, WatchEvent};
use crate::ffi::{self, PvAnchor};
use crate::findbar::FindBar;
use crate::markdown::{self, Heading};
use crate::mermaid;
use crate::palette::PaletteUi;
use crate::panels::{OutlinePanel, SearchPanel, TreePanel};
use crate::sci::{self, Editor};
use crate::settings::Settings;
use crate::theme::{self, Palette, ThemePref};
use crate::win::*;

pub const WM_WATCH: u32 = WM_APP + 1;
pub const WM_SEARCH: u32 = WM_APP + 2;
pub const WM_CHECK_FOCUS: u32 = WM_APP + 3;

const T_SAVE: usize = 1;
const T_PREVIEW: usize = 2;
const T_OUTLINE: usize = 3;
const T_TOAST: usize = 4;
const T_LOCK: usize = 5;
pub const T_SEARCH: usize = 6;
const T_SNAPSHOT: usize = 7;
pub const T_SEARCH_FLUSH: usize = 8;

pub const ID_SEARCH_EDIT: usize = 101;
pub const ID_RENAME_EDIT: usize = 102;
pub const ID_PALETTE_EDIT: usize = 103;
pub const ID_FIND_EDIT: usize = 104;
pub const ID_REPLACE_EDIT: usize = 105;
pub const ID_FILES_TREE: usize = 201;
pub const ID_SEARCH_TREE: usize = 202;
pub const ID_OUTLINE_TREE: usize = 203;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Code,
    Split,
    Preview,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Files,
    Search,
    Outline,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Editor,
    Preview,
}

/// Clickable regions painted by the main window.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    ToggleSidebar,
    Title,
    Mode(Mode),
    Theme,
    Tab(Panel),
    WsMenu,
    NewFile,
    SearchFuzzy,
    SearchCase,
    SearchRegex,
    WelcomeOpen,
    SideSplitter,
}

pub struct Fonts {
    pub ui: HFONT,
    pub ui_bold: HFONT,
    pub small: HFONT,
    pub big: HFONT,
    pub icon: HFONT,
    pub mono: HFONT,
}

impl Fonts {
    fn new(s: f32) -> Fonts {
        let fam = ui_font_family();
        let icons = if crate::font_exists("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        Fonts {
            ui: font(fam, 13.0 * s, 400),
            ui_bold: font(fam, 13.0 * s, 600),
            small: font(fam, 12.0 * s, 400),
            big: font(fam, 30.0 * s, 700),
            icon: font(icons, 14.0 * s, 400),
            mono: font("Cascadia Mono", 12.0 * s, 400),
        }
    }

    fn delete(&self) {
        for f in [self.ui, self.ui_bold, self.small, self.big, self.icon, self.mono] {
            unsafe {
                DeleteObject(f as _);
            }
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Layout {
    side: RECT,
    tabs: RECT,
    files_head: RECT,
    search_box: RECT,
    search_status: RECT,
    toolbar: RECT,
    panes: RECT,
    editor: RECT,
    preview: RECT,
}

pub struct App {
    pub hwnd: HWND,
    pub core: Core,
    pub settings: Settings,
    pub pref: ThemePref,
    pub pal: Palette,
    pub scale: f32,
    pub fonts: Fonts,
    pub input_brush: HBRUSH,

    pub editor: Editor,
    pub preview: HWND,
    pub tree: TreePanel,
    pub search: SearchPanel,
    pub outline: OutlinePanel,
    pub palette: PaletteUi,
    pub find: FindBar,
    rename_edit: HWND,
    toast: HWND,
    toast_text: String,

    pub root: Option<PathBuf>,
    pub current: Option<String>,
    /// Last text known to be on disk; tells our own saves apart from external edits.
    disk: String,
    pub dirty: bool,
    pub mode: Mode,
    pub recent: Vec<String>,
    pub panel: Panel,
    pub sidebar: bool,
    side_w: f32,
    pub headings: Vec<Heading>,

    loading: bool,
    anchors: Vec<PvAnchor>,
    anchors_dirty: bool,
    lock: Option<Side>,
    last_render_ms: f64,

    lay: Layout,
    hits: Vec<(RECT, Hit)>,
    hover: Option<Hit>,
    drag_side: Option<i32>,
}

static mut APP: *mut App = std::ptr::null_mut();

/// The single app instance. Win32 dispatches everything on the UI thread, so this is never shared.
#[allow(static_mut_refs)]
pub fn app() -> &'static mut App {
    unsafe { &mut *APP }
}

/// False while the app is still being built (child windows already get messages then).
pub fn app_ready() -> bool {
    unsafe { !APP.is_null() }
}

pub fn is_markdown(p: &str) -> bool {
    let l = p.to_lowercase();
    [".md", ".markdown", ".mdx", ".mdown", ".mkd"].iter().any(|e| l.ends_with(e))
}

pub fn basename(p: &str) -> &str {
    &p[p.rfind(['/', '\\']).map_or(0, |i| i + 1)..]
}

pub fn dirname(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

/// Resolve `href` against workspace-relative dir `base`; None if it escapes the workspace.
pub fn resolve_rel(base: &str, href: &str) -> Option<String> {
    let mut parts: Vec<&str> = if href.starts_with('/') || base.is_empty() { vec![] } else { base.split('/').collect() };
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn interpolate(anchors: &[PvAnchor], value: f64, by_line: bool) -> f64 {
    if anchors.len() < 2 {
        return if by_line { 0.0 } else { 1.0 };
    }
    let key = |a: &PvAnchor| if by_line { a.line as f64 } else { a.top as f64 };
    let val = |a: &PvAnchor| if by_line { a.top as f64 } else { a.line as f64 };
    let (mut lo, mut hi) = (0, anchors.len() - 1);
    while lo < hi - 1 {
        let mid = (lo + hi) / 2;
        if key(&anchors[mid]) <= value {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (a, b) = (&anchors[lo], &anchors[hi]);
    let span = key(b) - key(a);
    let t = if span == 0.0 { 0.0 } else { ((value - key(a)) / span).clamp(0.0, 1.0) };
    val(a) + t * (val(b) - val(a))
}

unsafe extern "system" fn edit_subclass(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            if app().on_edit_key(id, wp as u16) {
                return 0;
            }
        }
        // Swallow the beeps single-line edits make for Enter/Esc, and Ctrl+Backspace's box glyph.
        WM_CHAR if wp == 13 || wp == 27 || wp == 0x7f => return 0,
        WM_GETDLGCODE => return DLGC_WANTALLKEYS as LRESULT,
        _ => {}
    }
    DefSubclassProc(h, msg, wp, lp)
}

pub fn make_edit(parent: HWND, id: usize, cue: &str) -> HWND {
    unsafe {
        let cls = wide("EDIT");
        let h = CreateWindowExW(
            0,
            cls.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_CLIPSIBLINGS | ES_AUTOHSCROLL as u32,
            0,
            0,
            10,
            10,
            parent,
            id as HMENU,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        let c = wide(cue);
        send(h, EM_SETCUEBANNER, 1, c.as_ptr() as isize);
        SetWindowSubclass(h, Some(edit_subclass), id, 0);
        h
    }
}

impl App {
    pub fn create() {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_TREEVIEW_CLASSES | ICC_STANDARD_CLASSES };
            InitCommonControlsEx(&icc);

            let cls = wide("MDelightMain");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(main_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: inst,
                hIcon: LoadIconW(inst, 1 as _),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: cls.as_ptr(),
                hIconSm: LoadIconW(inst, 1 as _),
            };
            RegisterClassExW(&wc);
            crate::findbar::register(inst);
            crate::palette::register(inst);
            register_toast(inst);

            let settings = Settings::load();
            let pref = ThemePref::parse(settings.get("theme"));
            let dark = theme::resolve(pref);
            theme::set_menu_theme(dark);
            let title = wide("MDelight");
            let hwnd = CreateWindowExW(
                0,
                cls.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1280,
                820,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                inst,
                std::ptr::null(),
            );
            let scale = dpi_scale(hwnd);
            let editor = Editor::create(hwnd);
            let preview = ffi::pv_create(hwnd);
            SetWindowLongPtrW(preview, GWL_STYLE, GetWindowLongPtrW(preview, GWL_STYLE) | WS_CLIPSIBLINGS as isize);
            let a = Box::new(App {
                hwnd,
                core: Core::default(),
                pref,
                pal: if dark { theme::DARK } else { theme::LIGHT },
                scale,
                fonts: Fonts::new(scale),
                input_brush: std::ptr::null_mut(),
                editor,
                preview,
                tree: TreePanel::create(hwnd),
                search: SearchPanel::create(hwnd),
                outline: OutlinePanel::create(hwnd),
                palette: PaletteUi::create(hwnd),
                find: FindBar::create(hwnd),
                rename_edit: make_edit(hwnd, ID_RENAME_EDIT, ""),
                toast: create_toast(hwnd),
                toast_text: String::new(),
                root: None,
                current: None,
                disk: String::new(),
                dirty: false,
                mode: Mode::Preview, // every launch starts as a reader; Ctrl+1/2 switch to editing
                recent: Vec::new(),
                panel: Panel::Files,
                sidebar: settings.get("sidebar") != Some("0"),
                side_w: settings.get("sideWidth").and_then(|v| v.parse().ok()).unwrap_or(260.0),
                settings,
                headings: Vec::new(),
                loading: false,
                anchors: Vec::new(),
                anchors_dirty: true,
                lock: None,
                last_render_ms: 0.0,
                lay: Layout::default(),
                hits: Vec::new(),
                hover: None,
                drag_side: None,
            });
            APP = Box::into_raw(a);
            let app = app();
            app.apply_fonts();
            app.apply_theme();
            app.restore_placement();
            app.layout();

            let arg = std::env::args().nth(1).filter(|a| std::path::Path::new(a).is_dir());
            let ws = arg.clone().or_else(|| app.settings.get("workspace").map(String::from));
            if let Some(ws) = ws {
                app.open_workspace(PathBuf::from(ws), arg.is_none());
            }
            // Test hook: MDELIGHT_SNAPSHOT=<png> renders the preview pane to a file shortly after start.
            if std::env::var_os("MDELIGHT_SNAPSHOT").is_some() {
                SetTimer(hwnd, T_SNAPSHOT, 800, None);
                if let Ok(t) = std::env::var("MDELIGHT_THEME") {
                    app.pref = ThemePref::parse(Some(&t));
                    app.apply_theme();
                }
                match std::env::var("MDELIGHT_MODE").as_deref() {
                    Ok("code") => app.set_mode(Mode::Code),
                    Ok("split") => app.set_mode(Mode::Split),
                    _ => {}
                }
            }
            app.update_title();
            app.layout();
        }
    }

    // ---------------------------------------------------------------- theme

    fn apply_fonts(&mut self) {
        self.tree.on_dpi(self.scale, &self.fonts);
        self.search.on_dpi(self.scale, &self.fonts);
        self.outline.on_dpi(self.scale, &self.fonts);
        self.palette.on_dpi(&self.fonts);
        self.find.on_dpi(&self.fonts);
        send(self.rename_edit, WM_SETFONT, self.fonts.ui as usize, 1);
        self.editor.update_margin();
    }

    pub fn apply_theme(&mut self) {
        let dark = theme::resolve(self.pref);
        self.pal = if dark { theme::DARK } else { theme::LIGHT };
        theme::set_title_bar(self.hwnd, dark);
        theme::set_menu_theme(dark);
        unsafe {
            if !self.input_brush.is_null() {
                DeleteObject(self.input_brush as _);
            }
            self.input_brush = CreateSolidBrush(theme::cref(self.pal.input_bg));
        }
        self.editor.apply_theme(&self.pal);
        theme::set_control_theme(self.editor.hwnd, dark);
        theme::set_control_theme(self.preview, dark);
        self.tree.apply_theme(&self.pal);
        self.search.apply_theme(&self.pal);
        self.outline.apply_theme(&self.pal);
        self.palette.apply_theme(&self.pal);
        self.find.apply_theme(&self.pal);
        unsafe {
            ffi::pv_set_background(self.preview, self.pal.bg);
            InvalidateRect(self.hwnd, std::ptr::null(), 1);
            RedrawWindow(self.hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_FRAME);
        }
        self.refresh_preview(true);
    }

    fn set_pref(&mut self, p: ThemePref) {
        self.pref = p;
        self.settings.set("theme", p.as_str());
        self.apply_theme();
    }

    fn theme_menu(&mut self, at: POINT) {
        unsafe {
            let m = CreatePopupMenu();
            for (i, (p, label)) in [(ThemePref::Light, "Light"), (ThemePref::Dark, "Dark"), (ThemePref::System, "Match Windows")].iter().enumerate() {
                let l = wide(label);
                AppendMenuW(m, MF_STRING | if *p == self.pref { MF_CHECKED } else { 0 }, i + 1, l.as_ptr());
            }
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTALIGN, at.x, at.y, 0, self.hwnd, std::ptr::null());
            DestroyMenu(m);
            match cmd {
                1 => self.set_pref(ThemePref::Light),
                2 => self.set_pref(ThemePref::Dark),
                3 => self.set_pref(ThemePref::System),
                _ => {}
            }
        }
    }

    // ---------------------------------------------------------------- layout & painting

    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    pub fn has_ws(&self) -> bool {
        self.root.is_some()
    }

    pub fn effective_mode(&self) -> Mode {
        match &self.current {
            Some(c) if is_markdown(c) => self.mode,
            _ => Mode::Code,
        }
    }

    fn preview_visible(&self) -> bool {
        self.effective_mode() != Mode::Code
    }

    pub fn layout(&mut self) {
        let cr = client_rect(self.hwnd);
        let (w, h) = (cr.right, cr.bottom);
        let bar = self.px(40.0);
        let show_side = self.sidebar && self.has_ws();
        let sw = if show_side { self.px(self.side_w).clamp(self.px(160.0), (w - self.px(300.0)).max(self.px(160.0))) } else { 0 };
        let mut l = Layout::default();
        l.side = rect(0, 0, sw, h);
        l.tabs = rect(0, 0, sw, bar);
        let body_top = bar;
        l.files_head = rect(0, body_top, sw, body_top + self.px(34.0));
        l.search_box = rect(self.px(8.0), body_top + self.px(8.0), sw - self.px(8.0), body_top + self.px(36.0));
        l.search_status = rect(self.px(12.0), body_top + self.px(38.0), sw - self.px(8.0), body_top + self.px(60.0));
        let main_left = if show_side { sw + 1 } else { 0 };
        l.toolbar = rect(main_left, 0, w, bar);
        l.panes = rect(main_left, bar, w, h);
        let mode = self.effective_mode();
        let has_doc = self.has_ws();
        match mode {
            Mode::Code => {
                l.editor = l.panes;
                l.preview = rect(0, 0, 0, 0);
            }
            Mode::Preview => {
                l.editor = rect(0, 0, 0, 0);
                l.preview = l.panes;
            }
            Mode::Split => {
                let mid = (l.panes.left + l.panes.right) / 2;
                l.editor = rect(l.panes.left, l.panes.top, mid, l.panes.bottom);
                l.preview = rect(mid + 1, l.panes.top, l.panes.right, l.panes.bottom);
            }
        }
        self.lay = l;

        let vis = |r: &RECT| has_doc && r.right > r.left;
        move_window(self.editor.hwnd, &l.editor);
        show(self.editor.hwnd, vis(&l.editor));
        move_window(self.preview, &l.preview);
        show(self.preview, vis(&l.preview));

        let panel_rect_tree = rect(0, l.files_head.bottom, sw, h);
        move_window(self.tree.hwnd, &panel_rect_tree);
        show(self.tree.hwnd, show_side && self.panel == Panel::Files);
        let inner = self.px(6.0);
        let edit_r = rect(l.search_box.left + inner, l.search_box.top + self.px(5.0), l.search_box.right - self.px(86.0), l.search_box.bottom - self.px(5.0));
        move_window(self.search.edit, &edit_r);
        show(self.search.edit, show_side && self.panel == Panel::Search);
        move_window(self.search.tree, &rect(0, l.search_status.bottom + self.px(2.0), sw, h));
        show(self.search.tree, show_side && self.panel == Panel::Search);
        move_window(self.outline.tree, &rect(0, body_top + self.px(4.0), sw, h));
        show(self.outline.tree, show_side && self.panel == Panel::Outline);

        if self.find.visible {
            let r = self.find.rect_in(&l.editor, self.scale);
            move_window(self.find.hwnd, &r);
            show(self.find.hwnd, vis(&l.editor));
        }
        if self.palette.open {
            let r = self.palette.rect_in(&cr, self.scale);
            self.palette.place(&r, self.scale);
        }
        self.anchors_dirty = true;
        self.position_toast();
        unsafe {
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let cr = client_rect(self.hwnd);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, cr.right.max(1), cr.bottom.max(1));
            let old = SelectObject(mem, bmp as _);
            let p = Painter { dc: mem };
            self.hits.clear();
            self.paint_into(&p, &cr);
            BitBlt(hdc, 0, 0, cr.right, cr.bottom, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            DeleteObject(bmp as _);
            DeleteDC(mem);
            EndPaint(self.hwnd, &ps);
        }
    }

    fn button(&mut self, p: &Painter, r: RECT, hit: Hit, label: &str, font: HFONT, active: bool, enabled: bool, base_bg: u32) {
        let pal = self.pal;
        let hover = self.hover == Some(hit) && enabled;
        if active {
            p.round_fill(&r, pal.active_bar, self.px(8.0));
        } else if hover {
            p.round_fill(&r, if base_bg == pal.side { pal.hover_side } else { pal.hover_bar }, self.px(8.0));
        }
        let fg = if !enabled {
            pal.fg_dim
        } else if active {
            pal.fg_active_bar
        } else {
            pal.fg
        };
        p.text(label, &r, fg, font, CENTER_MID);
        self.hits.push((r, hit));
    }

    fn paint_into(&mut self, p: &Painter, cr: &RECT) {
        let pal = self.pal;
        let l = self.lay;
        p.fill(cr, pal.bg);
        if !self.has_ws() {
            self.paint_welcome(p, cr);
            return;
        }

        // Sidebar.
        if l.side.right > 0 {
            p.fill(&l.side, pal.side);
            p.fill(&l.tabs, pal.bar_side);
            p.fill(&rect(l.side.right, 0, l.side.right + 1, cr.bottom), pal.border);
            let tabs = [(Panel::Files, "Files"), (Panel::Search, "Search"), (Panel::Outline, "Outline")];
            let mut x = l.tabs.left + self.px(8.0);
            for (panel, label) in tabs {
                let w = p.text_width(label, self.fonts.ui) + self.px(20.0);
                let r = rect(x, self.px(7.0), x + w, l.tabs.bottom - self.px(7.0));
                let active = self.panel == panel;
                self.button(p, r, Hit::Tab(panel), label, self.fonts.ui, active, true, pal.bar_side);
                x += w + self.px(2.0);
            }
            match self.panel {
                Panel::Files => {
                    let head = l.files_head;
                    let name = self.root.as_ref().map(|r| r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| r.display().to_string())).unwrap_or_default();
                    let new_w = p.text_width("+ New", self.fonts.ui) + self.px(16.0);
                    let new_r = rect(head.right - new_w - self.px(8.0), head.top + self.px(5.0), head.right - self.px(8.0), head.bottom - self.px(5.0));
                    let label = format!("{name}  \u{25BE}");
                    let name_w = (p.text_width(&label, self.fonts.ui_bold) + self.px(16.0)).min(new_r.left - head.left - self.px(12.0));
                    let name_r = rect(head.left + self.px(6.0), new_r.top, head.left + self.px(6.0) + name_w, new_r.bottom);
                    if self.hover == Some(Hit::WsMenu) {
                        p.round_fill(&name_r, pal.hover_side, self.px(8.0));
                    }
                    p.text(&label, &rect(name_r.left + self.px(8.0), name_r.top, name_r.right - self.px(4.0), name_r.bottom), pal.fg, self.fonts.ui_bold, LEFT_MID);
                    self.hits.push((name_r, Hit::WsMenu));
                    self.button(p, new_r, Hit::NewFile, "+ New", self.fonts.ui, false, true, pal.side);
                }
                Panel::Search => {
                    let b = l.search_box;
                    p.round_fill(&b, pal.border_strong, self.px(8.0));
                    p.round_fill(&rect(b.left + 1, b.top + 1, b.right - 1, b.bottom - 1), pal.input_bg, self.px(8.0));
                    let sz = self.px(24.0);
                    let rx = rect(b.right - self.px(3.0) - sz, b.top + self.px(2.0), b.right - self.px(3.0), b.bottom - self.px(2.0));
                    let rc = rect(rx.left - sz - self.px(2.0), rx.top, rx.left - self.px(2.0), rx.bottom);
                    let rf = rect(rc.left - sz - self.px(2.0), rx.top, rc.left - self.px(2.0), rx.bottom);
                    // Fuzzy search ignores case and regex.
                    let (fuzzy, exact) = (self.search.fuzzy, !self.search.fuzzy);
                    self.button(p, rf, Hit::SearchFuzzy, "~", self.fonts.small, fuzzy, true, pal.side);
                    self.button(p, rc, Hit::SearchCase, "Aa", self.fonts.small, self.search.case && exact, exact, pal.side);
                    self.button(p, rx, Hit::SearchRegex, ".*", self.fonts.small, self.search.regex && exact, exact, pal.side);
                    p.text(&self.search.status, &l.search_status, pal.fg_dim, self.fonts.small, LEFT_MID);
                }
                Panel::Outline => {}
            }
            self.hits.push((rect(l.side.right - self.px(3.0), 0, l.side.right + self.px(3.0), cr.bottom), Hit::SideSplitter));
        }

        // Toolbar.
        let tb = l.toolbar;
        p.fill(&tb, pal.bar);
        let ib = self.px(28.0);
        let ty = tb.top + (tb.bottom - tb.top - ib) / 2;
        let toggle = rect(tb.left + self.px(8.0), ty, tb.left + self.px(8.0) + ib, ty + ib);
        self.button(p, toggle, Hit::ToggleSidebar, "\u{E90C}", self.fonts.icon, false, true, pal.bar);

        let theme_r = rect(tb.right - self.px(8.0) - ib, ty, tb.right - self.px(8.0), ty + ib);
        let glyph = match self.pref {
            ThemePref::Light => "\u{E706}",
            ThemePref::Dark => "\u{E708}",
            ThemePref::System => "\u{E7F4}",
        };
        self.button(p, theme_r, Hit::Theme, glyph, self.fonts.icon, false, true, pal.bar);
        let sep_x = theme_r.left - self.px(8.0);
        p.fill(&rect(sep_x, ty + self.px(6.0), sep_x + 1, ty + ib - self.px(6.0)), pal.border_strong);

        let md = self.current.as_deref().is_some_and(is_markdown);
        let mut x = sep_x - self.px(8.0);
        let mode = self.effective_mode();
        for (m, label) in [(Mode::Preview, "Preview"), (Mode::Split, "Split"), (Mode::Code, "Code")] {
            let w = p.text_width(label, self.fonts.ui) + self.px(20.0);
            let r = rect(x - w, ty, x, ty + ib);
            self.button(p, r, Hit::Mode(m), label, self.fonts.ui, md && m == mode, md, pal.bar);
            x -= w + self.px(2.0);
        }
        let title_left = toggle.right + self.px(10.0);
        let title_right = x - self.px(12.0);
        if !crate::win::is_visible(self.rename_edit) {
            let title = self.current.clone().unwrap_or_default();
            let tw = p.text_width(&title, self.fonts.ui).min(title_right - title_left);
            let tr = rect(title_left, ty, title_left + tw + self.px(4.0), ty + ib);
            p.text(&title, &tr, pal.fg, self.fonts.ui, LEFT_MID);
            if self.current.is_some() {
                self.hits.push((tr, Hit::Title));
            }
            if self.dirty {
                p.text("\u{25CF}", &rect(tr.right + self.px(4.0), ty, tr.right + self.px(20.0), ty + ib), pal.fg_dim, self.fonts.small, LEFT_MID);
            }
        }
        // Divider between split panes.
        if mode == Mode::Split {
            p.fill(&rect(l.editor.right, l.panes.top, l.editor.right + 1, l.panes.bottom), pal.border_strong);
        }
        p.fill(&rect(tb.left, tb.bottom - 1, tb.right, tb.bottom), pal.border);
        if self.current.is_none() {
            let r = l.panes;
            p.text("Open a file from the sidebar  ·  Ctrl+P", &r, pal.fg_dim, self.fonts.ui, CENTER_MID);
        }
    }

    fn paint_welcome(&mut self, p: &Painter, cr: &RECT) {
        let pal = self.pal;
        let cy = cr.bottom / 2;
        p.text("#", &rect(0, cy - self.px(150.0), cr.right, cy - self.px(80.0)), 0xff8a1f, self.fonts.big, CENTER_MID);
        p.text("MDelight", &rect(0, cy - self.px(80.0), cr.right, cy - self.px(30.0)), pal.fg, self.fonts.big, CENTER_MID);
        let bw = self.px(140.0);
        let br = rect(cr.right / 2 - bw / 2, cy - self.px(10.0), cr.right / 2 + bw / 2, cy + self.px(24.0));
        let hover = self.hover == Some(Hit::WelcomeOpen);
        p.round_fill(&br, if hover { 0x01559f } else { pal.accent_fill }, self.px(8.0));
        p.text("Open folder", &br, 0xffffff, self.fonts.ui_bold, CENTER_MID);
        self.hits.push((br, Hit::WelcomeOpen));
        let lines = [
            "Ctrl+P quick open  ·  Ctrl+Shift+F search  ·  Ctrl+1/2/3 code / split / preview",
            "Ctrl+N new file  ·  F2 rename  ·  Ctrl+B sidebar  ·  Ctrl+R go to heading",
            "F1 all shortcuts",
        ];
        for (i, l) in lines.iter().enumerate() {
            let y = cy + self.px(50.0 + i as f32 * 22.0);
            p.text(l, &rect(0, y, cr.right, y + self.px(20.0)), pal.fg_dim, self.fonts.small, CENTER_MID);
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| contains(r, x, y)).map(|(_, h)| *h)
    }

    fn on_click(&mut self, hit: Hit, x: i32, y: i32) {
        match hit {
            Hit::ToggleSidebar => self.set_sidebar(!self.sidebar),
            Hit::Title => self.start_rename(),
            Hit::Mode(m) => self.set_mode(m),
            Hit::Theme => {
                let mut pt = POINT { x: self.lay.toolbar.right - self.px(8.0), y: self.lay.toolbar.bottom };
                unsafe { ClientToScreen(self.hwnd, &mut pt) };
                self.theme_menu(pt);
            }
            Hit::Tab(p) => {
                self.show_panel(p);
                if p == Panel::Search {
                    self.focus_search(None);
                }
            }
            Hit::WsMenu => self.ws_menu(),
            Hit::NewFile => self.new_file(),
            Hit::SearchFuzzy => {
                self.search.fuzzy = !self.search.fuzzy;
                self.run_search();
                self.invalidate();
            }
            Hit::SearchCase if self.search.fuzzy => {}
            Hit::SearchRegex if self.search.fuzzy => {}
            Hit::SearchCase => {
                self.search.case = !self.search.case;
                self.run_search();
                self.invalidate();
            }
            Hit::SearchRegex => {
                self.search.regex = !self.search.regex;
                self.run_search();
                self.invalidate();
            }
            Hit::WelcomeOpen => self.pick_folder(),
            Hit::SideSplitter => {
                self.drag_side = Some(x - self.lay.side.right);
                let _ = y;
                unsafe { SetCapture(self.hwnd) };
            }
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
        }
    }

    // ---------------------------------------------------------------- toast

    pub fn toast(&mut self, msg: &str) {
        self.toast_text = msg.to_string();
        set_text(self.toast, msg);
        self.position_toast();
        unsafe {
            ShowWindow(self.toast, SW_SHOWNOACTIVATE);
            InvalidateRect(self.toast, std::ptr::null(), 1);
            SetTimer(self.hwnd, T_TOAST, 4000, None);
        }
    }

    fn position_toast(&self) {
        if self.toast.is_null() {
            return;
        }
        let cr = client_rect(self.hwnd);
        let w = ((self.toast_text.chars().count() as f32 * 7.2 + 40.0) * self.scale).min(cr.right as f32 - 40.0) as i32;
        let h = self.px(36.0);
        let mut pt = POINT { x: (cr.right - w) / 2, y: cr.bottom - h - self.px(24.0) };
        unsafe {
            ClientToScreen(self.hwnd, &mut pt);
            SetWindowPos(self.toast, std::ptr::null_mut(), pt.x, pt.y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
        }
    }

    // ---------------------------------------------------------------- workspace

    pub fn pick_folder(&mut self) {
        let mut buf = vec![0u16; 1024];
        let n = unsafe { ffi::pick_folder(self.hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if n > 0 {
            self.open_workspace(PathBuf::from(from_wide(&buf)), false);
        }
    }

    pub fn open_workspace(&mut self, path: PathBuf, silent: bool) {
        let hwnd = self.hwnd as usize;
        let res = self.core.open(&path, move |ev| unsafe {
            PostMessageW(hwnd as HWND, WM_WATCH, 0, Box::into_raw(Box::new(ev)) as isize);
        });
        let files = match res {
            Ok(f) => f,
            Err(e) => {
                if !silent {
                    self.toast(&e);
                }
                return;
            }
        };
        self.flush_save();
        let p = path.to_string_lossy().into_owned();
        self.root = Some(path);
        self.current = None;
        self.recent.clear();
        self.headings.clear();
        self.outline.set(&[]);
        unsafe { ffi::pv_clear(self.preview) };
        self.editor.set_text("");
        self.tree.set_files(&files);
        self.settings.set("workspace", &p);
        let mut recent = self.settings.list("recentWorkspaces");
        recent.retain(|r| r != &p);
        recent.insert(0, p.clone());
        recent.truncate(6);
        self.settings.set_list("recentWorkspaces", &recent);
        let last = self.settings.get(&format!("last:{p}")).map(String::from).filter(|l| self.core.has_file(l));
        let first = last
            .or_else(|| files.iter().find(|f| f.eq_ignore_ascii_case("readme.md")).cloned())
            .or_else(|| files.iter().find(|f| is_markdown(f)).cloned());
        self.layout();
        if let Some(f) = first {
            self.open_file(&f, None);
        }
        self.update_title();
        self.rerun_search();
    }

    fn ws_menu(&mut self) {
        let others: Vec<String> = self.settings.list("recentWorkspaces").into_iter().filter(|p| Some(p.as_str()) != self.root.as_ref().and_then(|r| r.to_str())).collect();
        unsafe {
            let m = CreatePopupMenu();
            let open = wide("Open folder…\tCtrl+Shift+O");
            AppendMenuW(m, MF_STRING, 1, open.as_ptr());
            if !others.is_empty() {
                AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
                for (i, p) in others.iter().enumerate() {
                    let name = basename(p.trim_end_matches(['\\', '/']));
                    let l = wide(&format!("{name}\t{p}"));
                    AppendMenuW(m, MF_STRING, 10 + i, l.as_ptr());
                }
            }
            let mut pt = POINT { x: self.lay.files_head.left + self.px(6.0), y: self.lay.files_head.bottom };
            ClientToScreen(self.hwnd, &mut pt);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD, pt.x, pt.y, 0, self.hwnd, std::ptr::null()) as usize;
            DestroyMenu(m);
            if cmd == 1 {
                self.pick_folder();
            } else if cmd >= 10 {
                if let Some(p) = others.get(cmd - 10) {
                    self.open_workspace(PathBuf::from(p), false);
                }
            }
        }
    }

    pub fn set_files(&mut self, files: Vec<String>) {
        self.tree.set_files(&files);
        if let Some(c) = self.current.clone() {
            self.tree.set_active(&c);
        }
    }

    // ---------------------------------------------------------------- documents

    pub fn open_file(&mut self, path: &str, line: Option<usize>) {
        if self.current.as_deref() != Some(path) {
            self.flush_save();
            let text = match self.core.read(path) {
                Ok(t) => t,
                Err(e) => {
                    self.toast(&e);
                    return;
                }
            };
            self.current = Some(path.to_string());
            self.disk = text.clone();
            self.dirty = false;
            self.loading = true;
            self.editor.set_text(&text);
            self.editor.mark_matches(self.search.mark.as_ref().map(|m| &m.re));
            self.loading = false;
            self.recent.retain(|p| p != path);
            self.recent.insert(0, path.to_string());
            self.recent.truncate(30);
            if let Some(r) = self.root.as_ref().and_then(|r| r.to_str()).map(String::from) {
                self.settings.set(&format!("last:{r}"), path);
            }
            self.tree.set_active(path);
            self.update_outline();
            self.layout();
            self.update_title();
            self.refresh_preview(false);
            self.track_top();
        }
        if let Some(l) = line {
            self.reveal_line(l, false);
        }
    }

    /// Jump to a source line in whichever pane is showing.
    pub fn reveal_line(&mut self, line: usize, at_top: bool) {
        if self.effective_mode() == Mode::Preview {
            let top = interpolate(self.get_anchors(), line as f64, true) as f32;
            let vh = unsafe { ffi::pv_view_height(self.preview) };
            let y = if at_top { top - 8.0 } else { top - vh / 3.0 };
            unsafe { ffi::pv_set_scroll_y(self.preview, y.max(0.0)) };
            self.track_top();
        } else {
            self.editor.goto_line(line, at_top);
        }
    }

    pub fn update_title(&mut self) {
        let mut parts = Vec::new();
        if let Some(c) = &self.current {
            parts.push(basename(c).to_string());
        }
        if let Some(r) = &self.root {
            parts.push(r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
        parts.push("MDelight".into());
        set_text(self.hwnd, &parts.join(" — "));
        self.invalidate();
    }

    pub fn flush_save(&mut self) {
        unsafe { KillTimer(self.hwnd, T_SAVE) };
        let Some(path) = self.current.clone() else { return };
        if !self.dirty {
            return;
        }
        let content = self.editor.text();
        match self.core.write(&path, content.as_bytes()) {
            Ok(()) => {
                self.disk = content;
                self.dirty = false;
            }
            Err(e) => self.toast(&format!("Save failed: {e}")),
        }
        self.update_title();
    }

    fn on_change(&mut self) {
        if self.loading || self.current.is_none() {
            return;
        }
        if !self.dirty {
            self.dirty = true;
            self.invalidate();
        }
        unsafe {
            SetTimer(self.hwnd, T_SAVE, 800, None);
            SetTimer(self.hwnd, T_OUTLINE, 200, None);
        }
        self.schedule_preview();
    }

    fn update_outline(&mut self) {
        self.headings = markdown::headings(&self.editor.text());
        self.outline.set(&self.headings);
        self.editor.update_margin();
    }

    /// Edited outside the app (another editor, OneDrive/git sync): reload in place.
    fn on_watch(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::Files(files) => self.set_files(files),
            WatchEvent::Modified(paths) => {
                let Some(path) = self.current.clone() else { return };
                if !paths.contains(&path) {
                    return;
                }
                let Ok(text) = self.core.read(&path) else { return };
                if text == self.disk {
                    return; // our own save
                }
                if self.dirty {
                    self.toast(&format!("{} changed on disk while you were editing; keeping your version", basename(&path)));
                    return;
                }
                self.disk = text.clone();
                self.loading = true;
                self.editor.replace_text(&text);
                self.loading = false;
                self.update_outline();
                self.refresh_preview(true);
            }
        }
    }

    // ---------------------------------------------------------------- new / rename

    pub fn new_file(&mut self) {
        if !self.has_ws() {
            return;
        }
        let dir = self.current.as_deref().map(dirname).unwrap_or("").to_string();
        let initial = if dir.is_empty() { "untitled.md".to_string() } else { format!("{dir}/untitled.md") };
        let start = initial.len() - "untitled.md".len();
        self.palette.ask(crate::palette::Ask::NewFile, "New file path (relative to the folder)", &initial, (start, start + "untitled".len()));
        self.layout();
    }

    pub fn create_file(&mut self, name: &str) {
        let mut name = name.replace('\\', "/").trim_start_matches('/').to_string();
        if !basename(&name).contains('.') {
            name.push_str(".md");
        }
        match self.core.create(&name) {
            Ok(files) => self.set_files(files),
            Err(e) => return self.toast(&e),
        }
        self.open_file(&name, None);
        if self.effective_mode() == Mode::Preview {
            self.set_mode(Mode::Split);
        }
        self.editor.focus();
    }

    /// Click the title (or F2) to edit the path in place: Enter/focus loss commits, Esc cancels.
    pub fn start_rename(&mut self) {
        let Some(from) = self.current.clone() else { return };
        if is_visible(self.rename_edit) {
            return;
        }
        let tb = self.lay.toolbar;
        let x = tb.left + self.px(46.0);
        let r = rect(x, tb.top + self.px(9.0), (x + self.px(420.0)).min(tb.right - self.px(260.0)), tb.bottom - self.px(9.0));
        move_window(self.rename_edit, &r);
        set_text(self.rename_edit, &from);
        unsafe {
            send(self.rename_edit, WM_SETFONT, self.fonts.ui as usize, 0);
            SetWindowPos(self.rename_edit, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        }
        show(self.rename_edit, true);
        let cut = from.rfind('/').map_or(0, |i| i + 1);
        let dot = from.rfind('.').filter(|&d| d > cut).unwrap_or(from.len());
        let u16_at = |i: usize| from[..i].encode_utf16().count();
        send(self.rename_edit, EM_SETSEL, u16_at(cut), u16_at(dot) as isize);
        unsafe { SetFocus(self.rename_edit) };
        self.invalidate();
    }

    fn finish_rename(&mut self, commit: bool) {
        if !is_visible(self.rename_edit) {
            return;
        }
        let to = get_text(self.rename_edit).trim().to_string();
        show(self.rename_edit, false);
        self.invalidate();
        let Some(from) = self.current.clone() else { return };
        if commit && !to.is_empty() {
            self.rename_file(&from, &to);
        }
        if self.effective_mode() != Mode::Preview {
            self.editor.focus();
        }
    }

    fn rename_file(&mut self, from: &str, to: &str) {
        let to = to.replace('\\', "/").trim_start_matches('/').to_string();
        if to == from {
            return;
        }
        self.flush_save();
        match self.core.rename(from, &to) {
            Ok(files) => self.set_files(files),
            Err(e) => return self.toast(&e),
        }
        if self.current.as_deref() == Some(from) {
            self.current = Some(to.clone());
            for p in &mut self.recent {
                if p == from {
                    *p = to.clone();
                }
            }
            if let Some(r) = self.root.as_ref().and_then(|r| r.to_str()).map(String::from) {
                self.settings.set(&format!("last:{r}"), &to);
            }
            self.tree.set_active(&to);
            self.update_title();
            self.layout();
            self.refresh_preview(true);
        }
    }

    // ---------------------------------------------------------------- preview & scroll sync

    fn schedule_preview(&mut self) {
        if !self.preview_visible() {
            return;
        }
        // Adaptive debounce: big documents render less often while typing.
        let ms = (self.last_render_ms * 2.0).clamp(16.0, 300.0) as u32;
        unsafe { SetTimer(self.hwnd, T_PREVIEW, ms, None) };
    }

    pub fn refresh_preview(&mut self, keep_scroll: bool) {
        unsafe { KillTimer(self.hwnd, T_PREVIEW) };
        let (Some(cur), Some(root)) = (self.current.clone(), self.root.clone()) else { return };
        if !self.preview_visible() {
            return;
        }
        let t0 = Instant::now();
        let mut body = markdown::render(&self.editor.text(), self.pal.dark);
        if let Some(m) = &self.search.mark {
            body = markdown::mark_matches(&body, &m.re);
        }
        let html = markdown::page(&body);
        let css = markdown::css(&self.pal);
        let base = root.join(dirname(&cur).replace('/', "\\"));
        let base_w = wide(&base.to_string_lossy());
        let html_c = CString::new(html.replace('\0', "")).unwrap();
        let css_c = CString::new(css).unwrap();
        unsafe {
            ffi::pv_set_document(self.preview, html_c.as_ptr(), css_c.as_ptr(), base_w.as_ptr(), self.pal.bg, self.pal.accent, keep_scroll as i32);
        }
        self.last_render_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.anchors_dirty = true;
    }

    fn get_anchors(&mut self) -> &[PvAnchor] {
        if self.anchors_dirty {
            let n = unsafe { ffi::pv_anchors(self.preview, std::ptr::null_mut(), 0) };
            let mut raw = vec![PvAnchor::default(); n.max(0) as usize];
            unsafe { ffi::pv_anchors(self.preview, raw.as_mut_ptr(), n) };
            let mut out = vec![PvAnchor { line: 1, top: 0.0 }];
            for a in raw {
                let last = *out.last().unwrap();
                if a.line > last.line && a.top > last.top {
                    out.push(a);
                }
            }
            let last = *out.last().unwrap();
            let total = self.editor.line_count() as i32;
            let height = unsafe { ffi::pv_content_height(self.preview) };
            out.push(PvAnchor { line: (total + 1).max(last.line + 1), top: height.max(last.top + 1.0) });
            self.anchors = out;
            self.anchors_dirty = false;
        }
        &self.anchors
    }

    fn hold_lock(&mut self, side: Side) {
        self.lock = Some(side);
        unsafe { SetTimer(self.hwnd, T_LOCK, 120, None) };
    }

    fn on_editor_scroll(&mut self) {
        self.track_top();
        if self.effective_mode() != Mode::Split || self.lock == Some(Side::Preview) {
            return;
        }
        self.hold_lock(Side::Editor);
        let line = self.editor.top_line();
        let top = interpolate(self.get_anchors(), line, true) as f32;
        unsafe { ffi::pv_set_scroll_y(self.preview, top) };
    }

    fn on_preview_scroll(&mut self) {
        self.track_top();
        if self.effective_mode() != Mode::Split || self.lock == Some(Side::Editor) {
            return;
        }
        self.hold_lock(Side::Preview);
        let y = unsafe { ffi::pv_scroll_y(self.preview) } as f64;
        let line = interpolate(self.get_anchors(), y, false);
        self.editor.scroll_to_line(line);
    }

    /// Highlight the outline entry for the section at the top of the visible pane.
    pub fn track_top(&mut self) {
        let line = if self.effective_mode() == Mode::Preview {
            let y = unsafe { ffi::pv_scroll_y(self.preview) } as f64;
            interpolate(self.get_anchors(), y, false)
        } else {
            self.editor.top_line()
        };
        self.outline.set_top_line(line, &self.headings);
    }

    fn open_mermaid(&mut self, code: &str) {
        match mermaid::write_page(code, self.pal.dark) {
            Ok(page) => {
                let w = wide(&page.to_string_lossy());
                let op = wide("open");
                unsafe { ShellExecuteW(self.hwnd, op.as_ptr(), w.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
            }
            Err(e) => self.toast(&format!("Can't open diagram: {e}")),
        }
    }

    fn on_link(&mut self, href: &str) {
        if let Some(id) = href.strip_prefix('#') {
            let id = CString::new(percent_decode(id)).unwrap_or_default();
            let top = unsafe { ffi::pv_find_id(self.preview, id.as_ptr()) };
            if top >= 0.0 {
                unsafe { ffi::pv_set_scroll_y(self.preview, (top - 8.0).max(0.0)) };
                self.on_preview_scroll();
            }
            return;
        }
        let lower = href.to_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
            let w = wide(href);
            let op = wide("open");
            unsafe { ShellExecuteW(self.hwnd, op.as_ptr(), w.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
            return;
        }
        if lower.split_once(':').is_some_and(|(s, _)| s.len() > 1 && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))) {
            self.toast(&format!("Not opening {href}"));
            return;
        }
        let Some(cur) = self.current.clone() else { return };
        let target = href.split('#').next().unwrap_or("");
        let rel = resolve_rel(dirname(&cur), &percent_decode(target));
        match rel {
            Some(r) if self.core.has_file(&r) => self.open_file(&r, None),
            _ => self.toast(&format!("File not found: {target}")),
        }
    }

    // ---------------------------------------------------------------- modes & panels

    pub fn set_mode(&mut self, m: Mode) {
        self.mode = m;
        self.layout();
        self.refresh_preview(true);
        self.invalidate();
    }

    pub fn show_panel(&mut self, p: Panel) {
        if !self.sidebar {
            self.set_sidebar(true);
        }
        self.panel = p;
        self.layout();
        if p == Panel::Outline {
            self.track_top();
        }
    }

    pub fn set_sidebar(&mut self, visible: bool) {
        self.sidebar = visible;
        self.settings.set("sidebar", if visible { "1" } else { "0" });
        self.layout();
    }

    fn help(&self) {
        let text = "Files & folders\n  Ctrl+Shift+O\tOpen folder\n  Ctrl+N\tNew file\n  F2\tRename / move file\n  Ctrl+S\tSave now (autosave is on)\n\n\
Navigate\n  Ctrl+P / Ctrl+O\tGo to file\n  Ctrl+R\tGo to heading\n  Ctrl+Shift+F\tSearch in all files\n  Ctrl+Shift+E\tShow file list\n\n\
View\n  Ctrl+1 / 2 / 3\tCode / Split / Preview\n  Ctrl+B\tShow / hide sidebar\n  Ctrl+Shift+L\tSwitch light / dark\n  F1\tThis help\n\n\
Editor\n  Ctrl+F\tFind & replace in file\n  F3 / Shift+F3\tNext / previous match\n  Ctrl+Alt+G\tGo to line\n  Ctrl+D\tSelect next occurrence\n  Ctrl+Z / Ctrl+Y\tUndo / redo\n  Alt+↑ / ↓\tMove line up / down\n  Ctrl+Shift+D\tDuplicate line\n  Shift+Alt+arrows\tColumn (block) selection\n  Ctrl+Shift+K\tDelete line\n  Ctrl+] / [\tIndent / outdent\n  Ctrl+/\tToggle comment\n\n\
Preview\n  Drag\tSelect text (by word), Ctrl+C to copy\n  Right-click\tCopy code / link / paragraph\n  Double-click image\tOpen it\n  Double-click diagram\tView it in the browser";
        let t = wide(text);
        let c = wide("Keyboard shortcuts");
        unsafe { MessageBoxW(self.hwnd, t.as_ptr(), c.as_ptr(), MB_OK) };
    }

    // ---------------------------------------------------------------- keyboard

    /// App-wide shortcuts, checked before the focused control sees the key.
    pub fn pre_translate(&mut self, msg: &MSG) -> bool {
        if msg.message != WM_KEYDOWN && msg.message != WM_SYSKEYDOWN {
            return false;
        }
        let key = msg.wParam as u16;
        let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk as i32) } < 0;
        let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
        if key == VK_F1 {
            self.help();
            return true;
        }
        if key == VK_F2 && !ctrl && !shift && !alt {
            self.start_rename();
            return true;
        }
        if key == VK_F3 && !ctrl && !alt {
            self.find_next(!shift);
            return true;
        }
        if !ctrl {
            return false;
        }
        let in_editor = self.editor.has_focus();
        match (key, shift, alt) {
            (0x50 /* P */ | 0x4F /* O */, false, false) => {
                if self.has_ws() {
                    self.open_file_palette();
                }
            }
            (0x52 /* R */, false, false) => {
                if self.current.is_some() {
                    self.open_heading_palette();
                }
            }
            (0x4F, true, false) => self.pick_folder(),
            (0x4E /* N */, false, false) => self.new_file(),
            (0x4C /* L */, true, false) => {
                let dark = theme::resolve(self.pref);
                self.set_pref(if dark { ThemePref::Light } else { ThemePref::Dark });
            }
            (0x42 /* B */, false, false) => self.set_sidebar(!self.sidebar),
            (0x46 /* F */, true, false) => {
                self.show_panel(Panel::Search);
                let sel = if in_editor { Some(self.editor.selection()) } else { None };
                self.focus_search(sel.as_deref());
            }
            (0x46, false, false) => {
                if self.current.is_some() {
                    if self.effective_mode() == Mode::Preview {
                        self.set_mode(Mode::Split);
                    }
                    let sel = self.editor.selection();
                    self.find.show(if sel.contains('\n') { None } else { Some(sel.as_str()) });
                    self.layout();
                }
            }
            (0x45 /* E */, true, false) => self.show_panel(Panel::Files),
            (0x53 /* S */, false, false) => self.flush_save(),
            (0x31 | 0x32 | 0x33, false, false) => self.set_mode([Mode::Code, Mode::Split, Mode::Preview][(key - 0x31) as usize]),
            (0x47 /* G */, false, true) => {
                if self.current.is_some() {
                    self.palette.ask(crate::palette::Ask::GotoLine, "Go to line", "", (0, 0));
                    self.layout();
                }
            }
            (0x44 /* D */, false, false) if in_editor => self.editor.select_next_occurrence(),
            (0xDD /* ] */, false, false) if in_editor => self.editor.indent(true),
            (0xDB /* [ */, false, false) if in_editor => self.editor.indent(false),
            (0xBF /* / */, false, false) if in_editor => self.editor.toggle_comment(),
            _ => return false,
        }
        true
    }

    /// Keys in our single-line edits; true when handled.
    pub fn on_edit_key(&mut self, id: usize, vk: u16) -> bool {
        match id {
            ID_RENAME_EDIT => match vk {
                VK_RETURN => {
                    self.finish_rename(true);
                    true
                }
                VK_ESCAPE => {
                    self.finish_rename(false);
                    true
                }
                _ => false,
            },
            ID_SEARCH_EDIT => match vk {
                VK_RETURN => {
                    self.run_search();
                    true
                }
                VK_ESCAPE => {
                    if self.current.is_some() && self.effective_mode() != Mode::Preview {
                        self.editor.focus();
                    }
                    true
                }
                VK_DOWN => {
                    unsafe { SetFocus(self.search.tree) };
                    true
                }
                _ => false,
            },
            ID_PALETTE_EDIT => self.palette_key(vk),
            ID_FIND_EDIT | ID_REPLACE_EDIT => self.find_key(id, vk),
            _ => false,
        }
    }

    // ---------------------------------------------------------------- window placement

    fn restore_placement(&mut self) {
        let v: Vec<i32> = self.settings.get("window").unwrap_or("").split(',').filter_map(|s| s.parse().ok()).collect();
        unsafe {
            if v.len() == 5 {
                let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
                wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
                wp.rcNormalPosition = rect(v[0], v[1], v[2], v[3]);
                wp.showCmd = if v[4] == 1 { SW_SHOWMAXIMIZED as u32 } else { SW_SHOWNORMAL as u32 };
                SetWindowPlacement(self.hwnd, &wp);
            } else {
                ShowWindow(self.hwnd, SW_SHOWNORMAL);
            }
        }
    }

    fn save_placement(&mut self) {
        unsafe {
            let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
            wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if GetWindowPlacement(self.hwnd, &mut wp) != 0 {
                let r = wp.rcNormalPosition;
                let max = (wp.showCmd == SW_SHOWMAXIMIZED as u32) as i32;
                self.settings.set("window", &format!("{},{},{},{},{}", r.left, r.top, r.right, r.bottom, max));
            }
        }
    }

    // ---------------------------------------------------------------- messages

    fn on_notify(&mut self, nm: *const NMHDR) -> Option<LRESULT> {
        let hdr = unsafe { &*nm };
        if hdr.hwndFrom == self.editor.hwnd {
            let n = unsafe { &*(nm as *const sci::SCNotification) };
            match n.code {
                sci::SCN_MODIFIED if n.modification_type & (sci::SC_MOD_INSERTTEXT | sci::SC_MOD_DELETETEXT) != 0 => self.on_change(),
                sci::SCN_UPDATEUI if n.updated & sci::SC_UPDATE_V_SCROLL != 0 => self.on_editor_scroll(),
                _ => {}
            }
            return None;
        }
        if hdr.hwndFrom == self.tree.hwnd {
            return self.tree_notify(nm);
        }
        if hdr.hwndFrom == self.search.tree {
            return self.search_notify(nm);
        }
        if hdr.hwndFrom == self.outline.tree {
            return self.outline_notify(nm);
        }
        None
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                Some(0)
            }
            WM_PRINTCLIENT => {
                let cr = client_rect(self.hwnd);
                self.hits.clear();
                self.paint_into(&Painter { dc: wp as HDC }, &cr);
                Some(0)
            }
            WM_ERASEBKGND => Some(1),
            WM_SIZE => {
                self.layout();
                Some(0)
            }
            WM_MOVE => {
                self.position_toast();
                None
            }
            WM_GETMINMAXINFO => {
                let mm = unsafe { &mut *(lp as *mut MINMAXINFO) };
                mm.ptMinTrackSize = POINT { x: self.px(520.0), y: self.px(360.0) };
                Some(0)
            }
            WM_DPICHANGED => {
                self.scale = (wp & 0xffff) as f32 / 96.0;
                self.fonts.delete();
                self.fonts = Fonts::new(self.scale);
                self.apply_fonts();
                let r = unsafe { &*(lp as *const RECT) };
                unsafe { SetWindowPos(self.hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE) };
                self.layout();
                Some(0)
            }
            WM_SETTINGCHANGE => {
                if lp != 0 {
                    let s = unsafe { std::slice::from_raw_parts(lp as *const u16, 64) };
                    if from_wide(s).starts_with("ImmersiveColorSet") && self.pref == ThemePref::System {
                        self.apply_theme();
                    }
                }
                None
            }
            WM_ACTIVATE => {
                // Remember focus? Win32 restores it for us when the window was the last active one.
                None
            }
            WM_SETFOCUS => {
                if self.current.is_some() && self.effective_mode() != Mode::Preview {
                    self.editor.focus();
                } else if self.current.is_some() {
                    unsafe { SetFocus(self.preview) };
                }
                Some(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
                if let Some(off) = self.drag_side {
                    self.side_w = ((x - off) as f32 / self.scale).clamp(160.0, 600.0);
                    self.layout();
                    return Some(0);
                }
                let h = self.hit_at(x, y);
                if h != self.hover {
                    self.hover = h;
                    self.invalidate();
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                    unsafe { TrackMouseEvent(&mut tme) };
                }
                Some(0)
            }
            WM_MOUSELEAVE => {
                if self.hover.is_some() {
                    self.hover = None;
                    self.invalidate();
                }
                Some(0)
            }
            WM_SETCURSOR => {
                if (lp & 0xffff) as u32 == HTCLIENT && (self.hover == Some(Hit::SideSplitter) || self.drag_side.is_some()) {
                    unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_SIZEWE)) };
                    return Some(1);
                }
                None
            }
            WM_LBUTTONDOWN => {
                let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
                if let Some(h) = self.hit_at(x, y) {
                    self.on_click(h, x, y);
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                if self.drag_side.take().is_some() {
                    unsafe { ReleaseCapture() };
                    self.settings.set("sideWidth", &format!("{}", self.side_w.round()));
                }
                Some(0)
            }
            WM_NOTIFY => self.on_notify(lp as *const NMHDR),
            WM_COMMAND => {
                let code = ((wp >> 16) & 0xffff) as u32;
                let id = wp & 0xffff;
                if id == ID_SEARCH_EDIT && code == EN_CHANGE {
                    // Wait for typing to settle before searching.
                    unsafe { SetTimer(self.hwnd, T_SEARCH, 300, None) };
                } else if id == ID_RENAME_EDIT && code == EN_KILLFOCUS {
                    self.finish_rename(true);
                }
                Some(0)
            }
            WM_CTLCOLOREDIT => {
                let dc = wp as HDC;
                unsafe {
                    SetTextColor(dc, theme::cref(self.pal.fg));
                    SetBkColor(dc, theme::cref(self.pal.input_bg));
                }
                Some(self.input_brush as LRESULT)
            }
            WM_TIMER => {
                match wp {
                    T_SAVE => self.flush_save(),
                    T_PREVIEW => self.refresh_preview(true),
                    T_OUTLINE => {
                        unsafe { KillTimer(self.hwnd, T_OUTLINE) };
                        self.update_outline();
                        self.track_top();
                    }
                    T_TOAST => {
                        unsafe { KillTimer(self.hwnd, T_TOAST) };
                        show(self.toast, false);
                    }
                    T_LOCK => {
                        unsafe { KillTimer(self.hwnd, T_LOCK) };
                        self.lock = None;
                    }
                    T_SEARCH => {
                        unsafe { KillTimer(self.hwnd, T_SEARCH) };
                        self.run_search_typed();
                    }
                    T_SEARCH_FLUSH => self.flush_search_results(),
                    T_SNAPSHOT => {
                        unsafe { KillTimer(self.hwnd, T_SNAPSHOT) };
                        if let Some(p) = std::env::var_os("MDELIGHT_SNAPSHOT") {
                            // MDELIGHT_HOVER=x,y (preview client pixels): hover there first.
                            if let Some((x, y)) = std::env::var("MDELIGHT_HOVER").ok().and_then(|v| {
                                let (x, y) = v.split_once(',')?;
                                Some((x.trim().parse::<isize>().ok()?, y.trim().parse::<isize>().ok()?))
                            }) {
                                send(self.preview, WM_MOUSEMOVE, 0, (y << 16) | x);
                            }
                            let w = wide(&p.to_string_lossy());
                            unsafe { ffi::pv_snapshot(self.preview, w.as_ptr()) };
                        }
                    }
                    _ => {}
                }
                Some(0)
            }
            WM_WATCH => {
                let ev = unsafe { Box::from_raw(lp as *mut WatchEvent) };
                self.on_watch(*ev);
                Some(0)
            }
            WM_SEARCH => {
                let ev = unsafe { Box::from_raw(lp as *mut SearchEvent) };
                self.on_search_event(*ev);
                Some(0)
            }
            WM_CHECK_FOCUS => {
                self.palette.check_focus();
                Some(0)
            }
            ffi::PVN_SCROLL => {
                self.on_preview_scroll();
                Some(0)
            }
            ffi::PVN_LINK => {
                let href = unsafe { std::ffi::CStr::from_ptr(lp as *const std::ffi::c_char) }.to_string_lossy().into_owned();
                self.on_link(&href);
                Some(0)
            }
            ffi::PVN_TOAST => {
                let t = unsafe { std::ffi::CStr::from_ptr(lp as *const std::ffi::c_char) }.to_string_lossy().into_owned();
                self.toast(&t);
                Some(0)
            }
            ffi::PVN_MERMAID => {
                let code = unsafe { std::ffi::CStr::from_ptr(lp as *const std::ffi::c_char) }.to_string_lossy().into_owned();
                self.open_mermaid(&code);
                Some(0)
            }
            WM_CLOSE => {
                self.flush_save();
                self.save_placement();
                unsafe { DestroyWindow(self.hwnd) };
                Some(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                Some(0)
            }
            _ => None,
        }
    }

    pub fn files_snapshot(&self) -> Arc<Vec<String>> {
        self.core.files()
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if !APP.is_null() {
        if let Some(r) = app().handle(msg, wp, lp) {
            return r;
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

// ---------------------------------------------------------------- toast window

unsafe extern "system" fn toast_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT if !APP.is_null() => {
            let a = app();
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let p = Painter { dc };
            let r = client_rect(hwnd);
            let (bg, fg) = if a.pal.dark { (0x3a3f4b, 0xf5f6f8) } else { (0x1f2328, 0xffffff) };
            p.fill(&r, bg);
            p.text(&a.toast_text, &r, fg, a.fonts.ui, CENTER_MID | DT_END_ELLIPSIS);
            EndPaint(hwnd, &ps);
            0
        }
        WM_LBUTTONDOWN => {
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_NCHITTEST => HTCLIENT as LRESULT,
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn register_toast(inst: HINSTANCE) {
    let cls = wide("MDelightToast");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(toast_proc),
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

fn create_toast(owner: HWND) -> HWND {
    let cls = wide("MDelightToast");
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            owner,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_links_resolve_inside_workspace() {
        assert_eq!(resolve_rel("guide", "note-1.md").as_deref(), Some("guide/note-1.md"));
        assert_eq!(resolve_rel("guide", "../README.md").as_deref(), Some("README.md"));
        assert_eq!(resolve_rel("", "../x.md"), None);
        assert_eq!(resolve_rel("a/b", "/c.md").as_deref(), Some("c.md"));
        assert_eq!(percent_decode("my%20note.md"), "my note.md");
    }

    #[test]
    fn interpolation_maps_both_ways() {
        let a = [PvAnchor { line: 1, top: 0.0 }, PvAnchor { line: 11, top: 100.0 }, PvAnchor { line: 21, top: 400.0 }];
        assert_eq!(interpolate(&a, 6.0, true), 50.0);
        assert_eq!(interpolate(&a, 250.0, false), 16.0);
    }
}
