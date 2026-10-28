//! Scintilla editor wrapper: setup, theming, text access, find, and the editing commands
//! the webview build had (move/duplicate/delete line, select next occurrence, comment).

#![allow(dead_code)]

use std::ffi::CString;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::ffi;
use crate::theme::Palette;
use crate::win::{send, wide};

pub const SCI_GETLENGTH: u32 = 2006;
pub const SCI_GETCURRENTPOS: u32 = 2008;
pub const SCI_GETANCHOR: u32 = 2009;
pub const SCI_SETSAVEPOINT: u32 = 2014;
pub const SCI_GOTOLINE: u32 = 2024;
pub const SCI_SETEOLMODE: u32 = 2031;
pub const SCI_SETTABWIDTH: u32 = 2036;
pub const SCI_SETCODEPAGE: u32 = 2037;
pub const SCI_STYLECLEARALL: u32 = 2050;
pub const SCI_STYLESETFORE: u32 = 2051;
pub const SCI_STYLESETBACK: u32 = 2052;
pub const SCI_STYLESETBOLD: u32 = 2053;
pub const SCI_STYLESETITALIC: u32 = 2054;
pub const SCI_STYLESETFONT: u32 = 2056;
pub const SCI_STYLESETEOLFILLED: u32 = 2057;
pub const SCI_STYLESETUNDERLINE: u32 = 2059;
pub const SCI_STYLESETSIZEFRACTIONAL: u32 = 2061;
pub const SCI_SETCARETFORE: u32 = 2069;
pub const SCI_ASSIGNCMDKEY: u32 = 2070;
pub const SCI_CLEARCMDKEY: u32 = 2071;
pub const SCI_BEGINUNDOACTION: u32 = 2078;
pub const SCI_ENDUNDOACTION: u32 = 2079;
pub const SCI_SETUSETABS: u32 = 2124;
pub const SCI_SETHSCROLLBAR: u32 = 2130;
pub const SCI_GETLINEENDPOSITION: u32 = 2136;
pub const SCI_GETSELECTIONSTART: u32 = 2143;
pub const SCI_GETSELECTIONEND: u32 = 2145;
pub const SCI_GETFIRSTVISIBLELINE: u32 = 2152;
pub const SCI_GETLINECOUNT: u32 = 2154;
pub const SCI_SETMARGINLEFT: u32 = 2155;
pub const SCI_SETMARGINRIGHT: u32 = 2157;
pub const SCI_SETSEL: u32 = 2160;
pub const SCI_LINEFROMPOSITION: u32 = 2166;
pub const SCI_POSITIONFROMLINE: u32 = 2167;
pub const SCI_SCROLLCARET: u32 = 2169;
pub const SCI_REPLACESEL: u32 = 2170;
pub const SCI_EMPTYUNDOBUFFER: u32 = 2175;
pub const SCI_SETTEXT: u32 = 2181;
pub const SCI_SETCARETWIDTH: u32 = 2188;
pub const SCI_SETTARGETSTART: u32 = 2190;
pub const SCI_GETTARGETSTART: u32 = 2191;
pub const SCI_SETTARGETEND: u32 = 2192;
pub const SCI_GETTARGETEND: u32 = 2193;
pub const SCI_REPLACETARGET: u32 = 2194;
pub const SCI_REPLACETARGETRE: u32 = 2195;
pub const SCI_SEARCHINTARGET: u32 = 2197;
pub const SCI_SETSEARCHFLAGS: u32 = 2198;
pub const SCI_VISIBLEFROMDOCLINE: u32 = 2220;
pub const SCI_DOCLINEFROMVISIBLE: u32 = 2221;
pub const SCI_SETMARGINTYPEN: u32 = 2240;
pub const SCI_SETMARGINWIDTHN: u32 = 2242;
pub const SCI_SETMARGINBACKN: u32 = 2250;
pub const SCI_SETMARGINS: u32 = 2252;
pub const SCI_SETWRAPMODE: u32 = 2268;
pub const SCI_SETLAYOUTCACHE: u32 = 2272;
pub const SCI_TEXTWIDTH: u32 = 2276;
pub const SCI_SETENDATLASTLINE: u32 = 2277;
pub const SCI_TAB: u32 = 2327;
pub const SCI_BACKTAB: u32 = 2328;
pub const SCI_LINEDELETE: u32 = 2338;
pub const SCI_SETMODEVENTMASK: u32 = 2359;
pub const SCI_LINESONSCREEN: u32 = 2370;
pub const SCI_USEPOPUP: u32 = 2371;
pub const SCI_GETFOCUS: u32 = 2381;
pub const SCI_GRABFOCUS: u32 = 2400;
pub const SCI_SETYCARETPOLICY: u32 = 2403;
pub const SCI_LINEDUPLICATE: u32 = 2404;
pub const SCI_SETWRAPINDENTMODE: u32 = 2472;
pub const SCI_SETSCROLLWIDTHTRACKING: u32 = 2516;
pub const SCI_GETCHARACTERPOINTER: u32 = 2520;
pub const SCI_SETEXTRAASCENT: u32 = 2525;
pub const SCI_SETEXTRADESCENT: u32 = 2527;
pub const SCI_SETMULTIPLESELECTION: u32 = 2563;
pub const SCI_SETADDITIONALSELECTIONTYPING: u32 = 2565;
pub const SCI_SCROLLRANGE: u32 = 2569;
pub const SCI_SETFIRSTVISIBLELINE: u32 = 2613;
pub const SCI_MOVESELECTEDLINESUP: u32 = 2620;
pub const SCI_MOVESELECTEDLINESDOWN: u32 = 2621;
pub const SCI_SETTECHNOLOGY: u32 = 2630;
pub const SCI_GETSELECTIONEMPTY: u32 = 2650;
pub const SCI_SETTARGETRANGE: u32 = 2686;
pub const SCI_MULTIPLESELECTADDNEXT: u32 = 2688;
pub const SCI_TARGETWHOLEDOCUMENT: u32 = 2690;
pub const SCI_SETIDLESTYLING: u32 = 2692;
pub const SCI_SETELEMENTCOLOUR: u32 = 2753;
pub const SCI_COLOURISE: u32 = 4003;
pub const SCI_SETILEXER: u32 = 4033;

pub const SCN_UPDATEUI: u32 = 2007;
pub const SCN_MODIFIED: u32 = 2008;
pub const SC_UPDATE_V_SCROLL: i32 = 0x4;
pub const SC_MOD_INSERTTEXT: i32 = 0x1;
pub const SC_MOD_DELETETEXT: i32 = 0x2;
pub const SCFIND_MATCHCASE: usize = 0x4;
pub const SCFIND_REGEXP: usize = 0x0020_0000;
pub const SCFIND_CXX11REGEX: usize = 0x0080_0000;

const STYLE_DEFAULT: usize = 32;
const STYLE_LINENUMBER: usize = 33;
const SCMOD_SHIFT: usize = 1;
const SCMOD_CTRL: usize = 2;
const SCMOD_ALT: usize = 4;
const SCK_DOWN: usize = 300;
const SCK_UP: usize = 301;

#[repr(C)]
pub struct SCNotification {
    pub hwnd_from: HWND,
    pub id_from: usize,
    pub code: u32,
    pub position: isize,
    pub ch: i32,
    pub modifiers: i32,
    pub modification_type: i32,
    pub text: *const u8,
    pub length: isize,
    pub lines_added: isize,
    pub message: i32,
    pub wparam: usize,
    pub lparam: isize,
    pub line: isize,
    pub fold_level_now: i32,
    pub fold_level_prev: i32,
    pub margin: i32,
    pub list_type: i32,
    pub x: i32,
    pub y: i32,
    pub token: i32,
    pub annotation_lines_added: isize,
    pub updated: i32,
    pub list_completion_method: i32,
    pub character_source: i32,
}

pub struct Editor {
    pub hwnd: HWND,
}

impl Editor {
    pub fn create(parent: HWND) -> Editor {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            ffi::Scintilla_RegisterClasses(inst as _);
            let cls = wide("Scintilla");
            let hwnd = CreateWindowExW(0, cls.as_ptr(), std::ptr::null(), WS_CHILD | WS_TABSTOP | WS_CLIPSIBLINGS, 0, 0, 100, 100, parent, std::ptr::null_mut(), inst, std::ptr::null());
            let e = Editor { hwnd };
            e.setup();
            e
        }
    }

    pub fn msg(&self, m: u32, wp: usize, lp: isize) -> isize {
        send(self.hwnd, m, wp, lp)
    }

    fn setup(&self) {
        self.msg(SCI_SETCODEPAGE, 65001, 0);
        self.msg(SCI_SETTECHNOLOGY, 1, 0); // DirectWrite
        self.msg(SCI_SETWRAPMODE, 1, 0); // word
        self.msg(SCI_SETWRAPINDENTMODE, 1, 0);
        self.msg(SCI_SETLAYOUTCACHE, 3, 0); // document
        self.msg(SCI_SETIDLESTYLING, 3, 0);
        self.msg(SCI_SETHSCROLLBAR, 0, 0);
        self.msg(SCI_SETSCROLLWIDTHTRACKING, 1, 0);
        self.msg(SCI_SETENDATLASTLINE, 0, 0);
        self.msg(SCI_SETTABWIDTH, 4, 0);
        self.msg(SCI_SETUSETABS, 0, 0);
        self.msg(SCI_SETMARGINS, 1, 0);
        self.msg(SCI_SETMARGINTYPEN, 0, 1); // line numbers
        self.msg(SCI_SETMARGINLEFT, 0, 6);
        self.msg(SCI_SETMARGINRIGHT, 0, 6);
        self.msg(SCI_SETEXTRAASCENT, 0, 2);
        self.msg(SCI_SETEXTRADESCENT, 0, 2);
        self.msg(SCI_SETCARETWIDTH, 2, 0);
        self.msg(SCI_SETMULTIPLESELECTION, 1, 0);
        self.msg(SCI_SETADDITIONALSELECTIONTYPING, 1, 0);
        self.msg(SCI_SETYCARETPOLICY, 0x01 | 0x04 | 0x08, 3); // slop | strict | even
        self.msg(SCI_SETMODEVENTMASK, (SC_MOD_INSERTTEXT | SC_MOD_DELETETEXT) as usize, 0);
        self.msg(SCI_SETILEXER, 0, unsafe { ffi::md_create_lexer() } as isize);

        // Keys from the webview build's help: Alt+↑/↓ move lines, Shift+Alt+↑/↓ duplicate,
        // Ctrl+Shift+K delete line. Ctrl+D (select next) and the rest are handled by the app.
        let key = |k: usize, m: usize| k | (m << 16);
        self.msg(SCI_ASSIGNCMDKEY, key(SCK_UP, SCMOD_ALT), SCI_MOVESELECTEDLINESUP as isize);
        self.msg(SCI_ASSIGNCMDKEY, key(SCK_DOWN, SCMOD_ALT), SCI_MOVESELECTEDLINESDOWN as isize);
        self.msg(SCI_ASSIGNCMDKEY, key(SCK_UP, SCMOD_ALT | SCMOD_SHIFT), SCI_LINEDUPLICATE as isize);
        self.msg(SCI_ASSIGNCMDKEY, key(SCK_DOWN, SCMOD_ALT | SCMOD_SHIFT), SCI_LINEDUPLICATE as isize);
        self.msg(SCI_ASSIGNCMDKEY, key('K' as usize, SCMOD_CTRL | SCMOD_SHIFT), SCI_LINEDELETE as isize);
        self.msg(SCI_CLEARCMDKEY, key('D' as usize, SCMOD_CTRL), 0);
    }

    fn set_font(&self, style: usize, family: &str, size: f32) {
        let f = CString::new(family).unwrap();
        self.msg(SCI_STYLESETFONT, style, f.as_ptr() as isize);
        self.msg(SCI_STYLESETSIZEFRACTIONAL, style, (size * 100.0) as isize);
    }

    fn style(&self, s: usize, fore: u32, back: Option<u32>, bold: bool, italic: bool) {
        use crate::theme::cref;
        self.msg(SCI_STYLESETFORE, s, cref(fore) as isize);
        if let Some(b) = back {
            self.msg(SCI_STYLESETBACK, s, cref(b) as isize);
        }
        self.msg(SCI_STYLESETBOLD, s, bold as isize);
        self.msg(SCI_STYLESETITALIC, s, italic as isize);
    }

    pub fn apply_theme(&self, p: &Palette) {
        use crate::theme::cref;
        let mono = if crate::font_exists("Cascadia Mono") { "Cascadia Mono" } else { "Consolas" };
        self.set_font(STYLE_DEFAULT, mono, 10.5);
        self.msg(SCI_STYLESETFORE, STYLE_DEFAULT, cref(p.fg) as isize);
        self.msg(SCI_STYLESETBACK, STYLE_DEFAULT, cref(p.bg) as isize);
        self.msg(SCI_STYLECLEARALL, 0, 0);
        self.msg(SCI_STYLESETFORE, STYLE_LINENUMBER, cref(p.fg_dim) as isize);
        self.msg(SCI_STYLESETBACK, STYLE_LINENUMBER, cref(p.bg) as isize);
        self.msg(SCI_SETMARGINBACKN, 0, cref(p.bg) as isize);
        self.msg(SCI_SETCARETFORE, cref(p.fg) as isize as usize, 0);
        // Element colours take 0xAABBGGRR.
        let opaque = |c: u32| (cref(c) | 0xff00_0000) as isize;
        self.msg(SCI_SETELEMENTCOLOUR, 11, opaque(p.active)); // selection
        self.msg(SCI_SETELEMENTCOLOUR, 13, opaque(p.active)); // additional selections
        self.msg(SCI_SETELEMENTCOLOUR, 17, opaque(p.active)); // inactive selection
        self.msg(SCI_SETELEMENTCOLOUR, 40, opaque(p.fg)); // caret
        let line = if p.dark { 0x2c313a } else { 0xf6f8fa };
        self.msg(SCI_SETELEMENTCOLOUR, 50, opaque(line)); // caret line

        // SCE_MARKDOWN_* styles.
        let head = if p.dark { 0xe5c07b } else { 0x0550ae };
        let code = if p.dark { 0x98c379 } else { 0x116329 };
        let list = if p.dark { 0xc678dd } else { 0x8250df };
        self.style(0, p.fg, None, false, false); // default
        self.style(1, p.fg, None, false, false); // line begin
        self.style(2, p.fg, None, true, false); // **strong**
        self.style(3, p.fg, None, true, false); // __strong__
        self.style(4, p.fg, None, false, true); // *em*
        self.style(5, p.fg, None, false, true); // _em_
        for h in 6..=11 {
            self.style(h, head, None, true, false); // headers
        }
        self.style(12, p.fg_dim, None, false, false); // prechar
        self.style(13, list, None, false, false); // ul item
        self.style(14, list, None, false, false); // ol item
        self.style(15, p.fg_dim, None, false, true); // blockquote
        self.style(16, p.fg_dim, None, false, false); // strikeout
        self.style(17, p.fg_dim, None, false, false); // hr
        self.style(18, p.accent, None, false, false); // link
        self.style(19, code, None, false, false); // `code`
        self.style(20, code, None, false, false); // ``code``
        self.style(21, code, None, false, false); // code block
        self.msg(SCI_COLOURISE, 0, -1);
        self.update_margin();
    }

    /// Line-number margin wide enough for the line count.
    pub fn update_margin(&self) {
        let digits = self.line_count().max(1).to_string().len().max(3);
        let sample = CString::new("9".repeat(digits + 1)).unwrap();
        let w = self.msg(SCI_TEXTWIDTH, STYLE_LINENUMBER, sample.as_ptr() as isize);
        self.msg(SCI_SETMARGINWIDTHN, 0, w);
    }

    pub fn set_text(&self, text: &str) {
        let crlf = text.contains("\r\n");
        self.msg(SCI_SETEOLMODE, if crlf { 0 } else { 2 }, 0);
        let c = CString::new(text.replace('\0', "")).unwrap();
        self.msg(SCI_SETTEXT, 0, c.as_ptr() as isize);
        self.msg(SCI_EMPTYUNDOBUFFER, 0, 0);
        self.msg(SCI_SETSAVEPOINT, 0, 0);
        self.msg(SCI_GOTOLINE, 0, 0);
        self.update_margin();
    }

    pub fn text(&self) -> String {
        let len = self.msg(SCI_GETLENGTH, 0, 0) as usize;
        let ptr = self.msg(SCI_GETCHARACTERPOINTER, 0, 0) as *const u8;
        if ptr.is_null() || len == 0 {
            return String::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Swap in new text by replacing only the differing middle, so caret and scroll stay put.
    pub fn replace_text(&self, text: &str) {
        let cur = self.text();
        let (a, b) = (cur.as_bytes(), text.as_bytes());
        let max = a.len().min(b.len());
        let mut s = 0;
        while s < max && a[s] == b[s] {
            s += 1;
        }
        while s > 0 && !text.is_char_boundary(s) {
            s -= 1;
        }
        let mut e = 0;
        while e < max - s && a[a.len() - 1 - e] == b[b.len() - 1 - e] {
            e += 1;
        }
        while e > 0 && !text.is_char_boundary(b.len() - e) {
            e -= 1;
        }
        self.msg(SCI_SETTARGETRANGE, s, (a.len() - e) as isize);
        let mid = &text[s..b.len() - e];
        self.msg(SCI_REPLACETARGET, mid.len(), mid.as_ptr() as isize);
        self.update_margin();
    }

    pub fn line_count(&self) -> usize {
        self.msg(SCI_GETLINECOUNT, 0, 0) as usize
    }

    /// 1-based document line at the top of the viewport (fractional within wrapped lines).
    pub fn top_line(&self) -> f64 {
        let vis = self.msg(SCI_GETFIRSTVISIBLELINE, 0, 0);
        let doc = self.msg(SCI_DOCLINEFROMVISIBLE, vis as usize, 0);
        let first_vis = self.msg(SCI_VISIBLEFROMDOCLINE, doc as usize, 0);
        let next_vis = self.msg(SCI_VISIBLEFROMDOCLINE, doc as usize + 1, 0);
        let span = (next_vis - first_vis).max(1) as f64;
        doc as f64 + 1.0 + (vis - first_vis) as f64 / span
    }

    pub fn scroll_to_line(&self, line: f64) {
        let n = line.floor().max(1.0) as usize;
        let first_vis = self.msg(SCI_VISIBLEFROMDOCLINE, n - 1, 0);
        let next_vis = self.msg(SCI_VISIBLEFROMDOCLINE, n, 0);
        let span = (next_vis - first_vis).max(1) as f64;
        let vis = first_vis + ((line - n as f64) * span).floor() as isize;
        self.msg(SCI_SETFIRSTVISIBLELINE, vis.max(0) as usize, 0);
    }

    /// Put the caret on a 1-based line and bring it into view (centered or at the top).
    pub fn goto_line(&self, line: usize, at_top: bool) {
        let l = line.clamp(1, self.line_count()) - 1;
        let pos = self.msg(SCI_POSITIONFROMLINE, l, 0);
        self.msg(SCI_SETSEL, pos as usize, pos);
        if at_top {
            let vis = self.msg(SCI_VISIBLEFROMDOCLINE, l, 0);
            self.msg(SCI_SETFIRSTVISIBLELINE, (vis - 1).max(0) as usize, 0);
        } else {
            let vis = self.msg(SCI_VISIBLEFROMDOCLINE, l, 0);
            let half = self.msg(SCI_LINESONSCREEN, 0, 0) / 2;
            self.msg(SCI_SETFIRSTVISIBLELINE, (vis - half).max(0) as usize, 0);
        }
        self.focus();
    }

    pub fn focus(&self) {
        self.msg(SCI_GRABFOCUS, 0, 0);
    }

    pub fn has_focus(&self) -> bool {
        unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus() == self.hwnd }
    }

    pub fn selection(&self) -> String {
        let s = self.msg(SCI_GETSELECTIONSTART, 0, 0) as usize;
        let e = self.msg(SCI_GETSELECTIONEND, 0, 0) as usize;
        let t = self.text();
        t.get(s..e).unwrap_or("").to_string()
    }

    /// Find `needle` from the selection, forwards or backwards, wrapping around. Selects it.
    pub fn find(&self, needle: &str, forward: bool, case: bool, regex: bool) -> bool {
        if needle.is_empty() {
            return false;
        }
        let mut flags = 0;
        if case {
            flags |= SCFIND_MATCHCASE;
        }
        if regex {
            flags |= SCFIND_REGEXP | SCFIND_CXX11REGEX;
        }
        self.msg(SCI_SETSEARCHFLAGS, flags, 0);
        let len = self.msg(SCI_GETLENGTH, 0, 0) as usize;
        let (s, e) = (self.msg(SCI_GETSELECTIONSTART, 0, 0) as usize, self.msg(SCI_GETSELECTIONEND, 0, 0) as usize);
        let tries: [(usize, usize); 2] = if forward { [(e, len), (0, e)] } else { [(s, 0), (len, s)] };
        for (from, to) in tries {
            self.msg(SCI_SETTARGETRANGE, from, to as isize);
            let r = self.msg(SCI_SEARCHINTARGET, needle.len(), needle.as_ptr() as isize);
            if r >= 0 {
                let ts = self.msg(SCI_GETTARGETSTART, 0, 0) as usize;
                let te = self.msg(SCI_GETTARGETEND, 0, 0) as usize;
                self.msg(SCI_SETSEL, ts, te as isize);
                self.msg(SCI_SCROLLRANGE, ts, te as isize);
                return true;
            }
        }
        false
    }

    /// Replace the current selection if it matches, then find the next one.
    pub fn replace_next(&self, needle: &str, with: &str, case: bool, regex: bool) -> bool {
        let sel = self.selection();
        let matches = if regex { !sel.is_empty() } else if case { sel == needle } else { sel.to_lowercase() == needle.to_lowercase() };
        if matches {
            let s = self.msg(SCI_GETSELECTIONSTART, 0, 0) as usize;
            let e = self.msg(SCI_GETSELECTIONEND, 0, 0);
            self.msg(SCI_SETTARGETRANGE, s, e);
            let w = CString::new(with).unwrap_or_default();
            let msg = if regex { SCI_REPLACETARGETRE } else { SCI_REPLACETARGET };
            let n = self.msg(msg, usize::MAX, w.as_ptr() as isize);
            self.msg(SCI_SETSEL, s + n as usize, (s + n as usize) as isize);
        }
        self.find(needle, true, case, regex)
    }

    pub fn replace_all(&self, needle: &str, with: &str, case: bool, regex: bool) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let mut flags = 0;
        if case {
            flags |= SCFIND_MATCHCASE;
        }
        if regex {
            flags |= SCFIND_REGEXP | SCFIND_CXX11REGEX;
        }
        self.msg(SCI_SETSEARCHFLAGS, flags, 0);
        let w = CString::new(with).unwrap_or_default();
        let msg = if regex { SCI_REPLACETARGETRE } else { SCI_REPLACETARGET };
        let mut count = 0;
        let mut pos = 0usize;
        self.msg(SCI_BEGINUNDOACTION, 0, 0);
        loop {
            let len = self.msg(SCI_GETLENGTH, 0, 0) as usize;
            self.msg(SCI_SETTARGETRANGE, pos, len as isize);
            if self.msg(SCI_SEARCHINTARGET, needle.len(), needle.as_ptr() as isize) < 0 {
                break;
            }
            let ts = self.msg(SCI_GETTARGETSTART, 0, 0) as usize;
            let te = self.msg(SCI_GETTARGETEND, 0, 0) as usize;
            let n = self.msg(msg, usize::MAX, w.as_ptr() as isize) as usize;
            count += 1;
            pos = ts + n + usize::from(te == ts); // empty regex matches must still advance
            if pos > len || count > 1_000_000 {
                break;
            }
        }
        self.msg(SCI_ENDUNDOACTION, 0, 0);
        count
    }

    pub fn select_next_occurrence(&self) {
        self.msg(SCI_TARGETWHOLEDOCUMENT, 0, 0);
        self.msg(SCI_SETSEARCHFLAGS, SCFIND_MATCHCASE, 0);
        self.msg(SCI_MULTIPLESELECTADDNEXT, 0, 0);
    }

    pub fn indent(&self, more: bool) {
        self.msg(if more { SCI_TAB } else { SCI_BACKTAB }, 0, 0);
    }

    /// Wrap the selected lines in an HTML comment, or unwrap them.
    pub fn toggle_comment(&self) {
        let s = self.msg(SCI_GETSELECTIONSTART, 0, 0) as usize;
        let e = self.msg(SCI_GETSELECTIONEND, 0, 0) as usize;
        let l0 = self.msg(SCI_LINEFROMPOSITION, s, 0) as usize;
        let l1 = self.msg(SCI_LINEFROMPOSITION, e, 0) as usize;
        let a = self.msg(SCI_POSITIONFROMLINE, l0, 0) as usize;
        let b = self.msg(SCI_GETLINEENDPOSITION, l1, 0) as usize;
        let text = self.text();
        let Some(block) = text.get(a..b) else { return };
        let t = block.trim();
        let out = if t.starts_with("<!--") && t.ends_with("-->") {
            let inner = &t[4..t.len() - 3];
            inner.strip_prefix(' ').unwrap_or(inner).strip_suffix(' ').unwrap_or(inner.strip_prefix(' ').unwrap_or(inner)).to_string()
        } else {
            format!("<!-- {block} -->")
        };
        self.msg(SCI_SETTARGETRANGE, a, b as isize);
        self.msg(SCI_REPLACETARGET, out.len(), out.as_ptr() as isize);
        self.msg(SCI_SETSEL, a, (a + out.len()) as isize);
    }
}
