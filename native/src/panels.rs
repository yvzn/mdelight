//! Sidebar panels, each a native TreeView: files (lazy folders), search results, outline.

use std::collections::{HashMap, HashSet};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_RETURN};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::app::{is_markdown, make_edit, App, Fonts, Mode, ID_FILES_TREE, ID_OUTLINE_TREE, ID_SEARCH_EDIT, ID_SEARCH_TREE, T_SEARCH_FLUSH, WM_SEARCH};
use crate::core::{LineHit, SearchEvent};
use crate::markdown::Heading;
use crate::theme::{self, Palette};
use crate::win::*;

fn make_tree(parent: HWND, id: usize, buttons: bool) -> HWND {
    let cls = wide("SysTreeView32");
    let style = WS_CHILD | WS_CLIPSIBLINGS | WS_TABSTOP | TVS_FULLROWSELECT | TVS_SHOWSELALWAYS | TVS_NOHSCROLL | TVS_INFOTIP | if buttons { TVS_HASBUTTONS | TVS_LINESATROOT } else { 0 };
    unsafe {
        let h = CreateWindowExW(0, cls.as_ptr(), std::ptr::null(), style, 0, 0, 10, 10, parent, id as HMENU, GetModuleHandleW(std::ptr::null()), std::ptr::null());
        let ex = TVS_EX_DOUBLEBUFFER as usize;
        send(h, TVM_SETEXTENDEDSTYLE, ex, ex as isize);
        h
    }
}

fn tree_theme(h: HWND, p: &Palette) {
    theme::set_control_theme(h, p.dark);
    send(h, TVM_SETBKCOLOR, 0, theme::cref(p.side) as isize);
    send(h, TVM_SETTEXTCOLOR, 0, theme::cref(p.fg) as isize);
    unsafe { InvalidateRect(h, std::ptr::null(), 1) };
}

fn tree_font(h: HWND, scale: f32, fonts: &Fonts) {
    send(h, WM_SETFONT, fonts.ui as usize, 1);
    send(h, TVM_SETITEMHEIGHT, (24.0 * scale).round() as usize, 0);
    send(h, TVM_SETINDENT, (14.0 * scale).round() as usize, 0);
}

/// `children`: Some(..) forces the expand button (lazy folders); None lets the tree count real children.
fn insert(tree: HWND, parent: HTREEITEM, text: &str, param: isize, children: Option<bool>) -> HTREEITEM {
    insert_state(tree, parent, text, param, children, 0)
}

/// `insert` with initial item state, e.g. TVIS_EXPANDED (no expand round trip per item).
fn insert_state(tree: HWND, parent: HTREEITEM, text: &str, param: isize, children: Option<bool>, state: u32) -> HTREEITEM {
    let mut w = wide(text);
    let mut ins: TVINSERTSTRUCTW = unsafe { std::mem::zeroed() };
    ins.hParent = parent;
    ins.hInsertAfter = TVI_LAST;
    ins.Anonymous.item = TVITEMW {
        mask: TVIF_TEXT | TVIF_PARAM | TVIF_STATE | if children.is_some() { TVIF_CHILDREN } else { 0 },
        hItem: 0,
        state,
        stateMask: state,
        pszText: w.as_mut_ptr(),
        cchTextMax: 0,
        iImage: 0,
        iSelectedImage: 0,
        cChildren: children.unwrap_or(false) as i32,
        lParam: param,
    };
    send(tree, TVM_INSERTITEMW, 0, &ins as *const _ as isize) as HTREEITEM
}

fn item_param(tree: HWND, item: HTREEITEM) -> isize {
    let mut it: TVITEMW = unsafe { std::mem::zeroed() };
    it.mask = TVIF_PARAM;
    it.hItem = item;
    send(tree, TVM_GETITEMW, 0, &mut it as *mut _ as isize);
    it.lParam
}

fn clear(tree: HWND) {
    send(tree, TVM_DELETEITEM, 0, TVI_ROOT as isize);
}

/// Suspend painting (and scrollbar updates) around a bulk change.
fn freeze(tree: HWND) {
    send(tree, WM_SETREDRAW, 0, 0);
}

/// Resume painting; repaints the frame too so the scrollbar lands in one step.
fn thaw(tree: HWND) {
    send(tree, WM_SETREDRAW, 1, 0);
    unsafe { RedrawWindow(tree, std::ptr::null(), std::ptr::null_mut(), RDW_ERASE | RDW_FRAME | RDW_INVALIDATE) };
}

fn select(tree: HWND, item: HTREEITEM) {
    send(tree, TVM_SELECTITEM, TVGN_CARET as usize, item as isize);
    send(tree, TVM_ENSUREVISIBLE, 0, item as isize);
}

/// Item under the mouse, unless the click hit the expand button.
fn clicked_item(tree: HWND) -> Option<HTREEITEM> {
    let mut ht: TVHITTESTINFO = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut ht.pt);
        ScreenToClient(tree, &mut ht.pt);
    }
    let item = send(tree, TVM_HITTEST, 0, &mut ht as *mut _ as isize) as HTREEITEM;
    if item == 0 || ht.flags & TVHT_ONITEMBUTTON != 0 || ht.flags & (TVHT_ONITEM | TVHT_ONITEMRIGHT | TVHT_ONITEMINDENT) == 0 {
        return None;
    }
    Some(item)
}

// ---------------------------------------------------------------- files

struct Node {
    path: String,
    is_dir: bool,
    loaded: bool,
    item: HTREEITEM,
}

#[derive(Default)]
struct DirEntry {
    dirs: Vec<String>,
    files: Vec<String>,
}

/// Folder tree; folders fill in when first expanded, so huge workspaces stay cheap.
pub struct TreePanel {
    pub hwnd: HWND,
    nodes: Vec<Node>,
    by_path: HashMap<String, usize>,
    dirs: HashMap<String, DirEntry>,
    expanded: HashSet<String>,
    rebuilding: bool,
}

impl TreePanel {
    pub fn create(parent: HWND) -> TreePanel {
        TreePanel {
            hwnd: make_tree(parent, ID_FILES_TREE, true),
            nodes: Vec::new(),
            by_path: HashMap::new(),
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            rebuilding: false,
        }
    }

    pub fn apply_theme(&self, p: &Palette) {
        tree_theme(self.hwnd, p);
    }

    pub fn on_dpi(&self, scale: f32, fonts: &Fonts) {
        tree_font(self.hwnd, scale, fonts);
    }

    pub fn set_files(&mut self, files: &[String]) {
        self.dirs.clear();
        self.dirs.insert(String::new(), DirEntry::default());
        for f in files {
            let parts: Vec<&str> = f.split('/').collect();
            let mut parent = String::new();
            for i in 0..parts.len() - 1 {
                let path = parts[..=i].join("/");
                if !self.dirs.contains_key(&path) {
                    self.dirs.insert(path.clone(), DirEntry::default());
                    self.dirs.get_mut(&parent).unwrap().dirs.push(path.clone());
                }
                parent = path;
            }
            self.dirs.get_mut(&parent).unwrap().files.push(f.clone());
        }
        for d in self.dirs.values_mut() {
            d.dirs.sort_by_key(|p| crate::app::basename(p).to_lowercase());
        }

        self.rebuilding = true;
        send(self.hwnd, WM_SETREDRAW, 0, 0);
        clear(self.hwnd);
        self.nodes.clear();
        self.by_path.clear();
        self.fill(TVI_ROOT, "");
        let mut open: Vec<String> = self.expanded.iter().filter(|d| self.dirs.contains_key(*d)).cloned().collect();
        open.sort_by_key(|d| d.matches('/').count());
        self.expanded.clear();
        for d in open {
            self.expand(&d);
        }
        send(self.hwnd, WM_SETREDRAW, 1, 0);
        self.rebuilding = false;
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 1) };
    }

    fn fill(&mut self, parent: HTREEITEM, dir: &str) {
        let Some(entry) = self.dirs.get(dir) else { return };
        let (dirs, files) = (entry.dirs.clone(), entry.files.clone());
        for d in dirs {
            self.add(parent, d, true);
        }
        for f in files {
            self.add(parent, f, false);
        }
    }

    fn add(&mut self, parent: HTREEITEM, path: String, is_dir: bool) {
        let idx = self.nodes.len();
        let item = insert(self.hwnd, parent, crate::app::basename(&path), idx as isize, Some(is_dir));
        self.by_path.insert(path.clone(), idx);
        self.nodes.push(Node { path, is_dir, loaded: false, item });
    }

    fn load(&mut self, idx: usize) {
        if self.nodes[idx].loaded || !self.nodes[idx].is_dir {
            return;
        }
        self.nodes[idx].loaded = true;
        let (item, path) = (self.nodes[idx].item, self.nodes[idx].path.clone());
        self.fill(item, &path);
    }

    fn expand(&mut self, dir: &str) {
        if let Some(&i) = self.by_path.get(dir) {
            self.load(i);
            send(self.hwnd, TVM_EXPAND, TVE_EXPAND as usize, self.nodes[i].item as isize);
            self.expanded.insert(dir.to_string());
        }
    }

    pub fn set_active(&mut self, path: &str) {
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            let d = parts[..i].join("/");
            if !self.expanded.contains(&d) {
                self.expand(&d);
            }
        }
        if let Some(&i) = self.by_path.get(path) {
            self.rebuilding = true;
            select(self.hwnd, self.nodes[i].item);
            self.rebuilding = false;
        }
    }

    fn node_at(&self, item: HTREEITEM) -> Option<usize> {
        let i = item_param(self.hwnd, item) as usize;
        (i < self.nodes.len()).then_some(i)
    }
}

// ---------------------------------------------------------------- search

/// Workspace-wide content search; results stream in from the core's worker threads.
pub struct SearchPanel {
    pub edit: HWND,
    pub tree: HWND,
    pub case: bool,
    pub regex: bool,
    /// Typo- and accent-tolerant ranked search instead of exact matching.
    pub fuzzy: bool,
    pub status: String,
    hits: Vec<SearchHit>,
    /// Results received since the last flush into the tree (one redraw per batch).
    pending: Vec<(String, Vec<LineHit>)>,
    flush_armed: bool,
    /// The tree (and `hits`) still show the previous query; swapped out on the first flush.
    stale: bool,
    /// Query highlighted in the editor and preview, set by opening a result.
    pub mark: Option<Mark>,
}

struct SearchHit {
    path: String,
    line: usize,
    /// Item label (UTF-16) and the matches in it, for highlighting.
    label: Vec<u16>,
    ranges: Vec<(usize, usize)>,
}

pub struct Mark {
    query: String,
    regex: bool,
    case: bool,
    fuzzy: bool,
    pub re: regex::Regex,
}

/// Same semantics as the core's search (grep-regex uses the `regex` crate's syntax).
fn search_regex(query: &str, regex: bool, case: bool) -> Option<regex::Regex> {
    let pattern = if regex { query.to_string() } else { regex::escape(query) };
    regex::RegexBuilder::new(&pattern).case_insensitive(!case).build().ok()
}

/// For fuzzy results: the words that matched (as found, e.g. "Métier", "metiers"), longest first.
fn fuzzy_regex(hits: &[SearchHit]) -> Option<regex::Regex> {
    let mut words: Vec<String> = hits
        .iter()
        .flat_map(|h| h.ranges.iter().map(|&(a, b)| String::from_utf16_lossy(&h.label[a..b]).to_lowercase()))
        .collect::<HashSet<_>>()
        .into_iter()
        .take(500)
        .collect();
    if words.is_empty() {
        return None;
    }
    words.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    let alt = words.iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join("|");
    regex::RegexBuilder::new(&format!(r"\b(?:{alt})\b")).case_insensitive(true).build().ok()
}

/// Item label for a hit, with the match byte ranges mapped to UTF-16 offsets in it.
fn hit_label(h: &LineHit) -> (String, Vec<u16>, Vec<(usize, usize)>) {
    let prefix = format!("{}:  ", h.line);
    let text = format!("{prefix}{}", h.text);
    let label: Vec<u16> = text.encode_utf16().collect();
    let u16_at = |byte: usize| prefix.len() + h.text[..byte].encode_utf16().count();
    let ranges = h.ranges.iter().filter(|(s, e)| h.text.is_char_boundary(*s) && h.text.is_char_boundary(*e)).map(|&(s, e)| (u16_at(s), u16_at(e))).collect();
    (text, label, ranges)
}

impl SearchPanel {
    pub fn create(parent: HWND) -> SearchPanel {
        SearchPanel {
            edit: make_edit(parent, ID_SEARCH_EDIT, "Search in files"),
            tree: make_tree(parent, ID_SEARCH_TREE, true),
            case: false,
            regex: false,
            fuzzy: false,
            status: String::new(),
            hits: Vec::new(),
            pending: Vec::new(),
            flush_armed: false,
            stale: false,
            mark: None,
        }
    }

    pub fn apply_theme(&self, p: &Palette) {
        tree_theme(self.tree, p);
        theme::set_control_theme(self.edit, p.dark);
    }

    pub fn on_dpi(&self, scale: f32, fonts: &Fonts) {
        tree_font(self.tree, scale, fonts);
        send(self.edit, WM_SETFONT, fonts.ui as usize, 1);
        send(self.tree, TVM_SETINDENT, (8.0 * scale) as usize, 0);
    }
}

// ---------------------------------------------------------------- outline

/// Headings as a tree; the section at the top of the view stays selected.
pub struct OutlinePanel {
    pub tree: HWND,
    items: Vec<HTREEITEM>,
    active: Option<usize>,
    programmatic: bool,
}

impl OutlinePanel {
    pub fn create(parent: HWND) -> OutlinePanel {
        OutlinePanel { tree: make_tree(parent, ID_OUTLINE_TREE, false), items: Vec::new(), active: None, programmatic: false }
    }

    pub fn apply_theme(&self, p: &Palette) {
        tree_theme(self.tree, p);
    }

    pub fn on_dpi(&self, scale: f32, fonts: &Fonts) {
        tree_font(self.tree, scale, fonts);
    }

    pub fn set(&mut self, headings: &[Heading]) {
        send(self.tree, WM_SETREDRAW, 0, 0);
        clear(self.tree);
        self.items.clear();
        self.active = None;
        let mut stack: Vec<(u8, HTREEITEM)> = Vec::new();
        for (i, h) in headings.iter().enumerate() {
            while stack.last().is_some_and(|(l, _)| *l >= h.level) {
                stack.pop();
            }
            let parent = stack.last().map_or(TVI_ROOT, |(_, it)| *it);
            let text = if h.text.is_empty() { "(empty heading)" } else { &h.text };
            let item = insert(self.tree, parent, text, i as isize, None);
            stack.push((h.level, item));
            self.items.push(item);
        }
        for &it in &self.items {
            send(self.tree, TVM_EXPAND, TVE_EXPAND as usize, it as isize);
        }
        if headings.is_empty() {
            insert(self.tree, TVI_ROOT, "No headings", -1, None);
        }
        send(self.tree, WM_SETREDRAW, 1, 0);
        unsafe { InvalidateRect(self.tree, std::ptr::null(), 1) };
    }

    /// Mark the last heading at or above `line`.
    pub fn set_top_line(&mut self, line: f64, headings: &[Heading]) {
        let cur = headings.iter().rposition(|h| h.line as f64 <= line + 0.5);
        if cur == self.active {
            return;
        }
        self.active = cur;
        self.programmatic = true;
        match cur.and_then(|i| self.items.get(i)) {
            Some(&it) => select(self.tree, it),
            None => {
                send(self.tree, TVM_SELECTITEM, TVGN_CARET as usize, 0);
            }
        }
        self.programmatic = false;
    }
}

// ---------------------------------------------------------------- app glue

fn custom_draw(nm: *const NMHDR, color_for: impl Fn(isize) -> Option<u32>) -> Option<LRESULT> {
    let cd = unsafe { &mut *(nm as *mut NMTVCUSTOMDRAW) };
    match cd.nmcd.dwDrawStage {
        CDDS_PREPAINT => Some(CDRF_NOTIFYITEMDRAW as LRESULT),
        CDDS_ITEMPREPAINT => {
            if let Some(c) = color_for(cd.nmcd.lItemlParam) {
                cd.clrText = theme::cref(c);
            }
            Some(CDRF_DODEFAULT as LRESULT)
        }
        _ => Some(CDRF_DODEFAULT as LRESULT),
    }
}

/// Paint the matches of a result line over the tree's own text: a mark-colored box, then the
/// matched characters again on top. The label sits centered in its text rect.
fn draw_hit_marks(cd: &NMTVCUSTOMDRAW, tree: HWND, hit: &SearchHit, mark: u32, fg: u32) {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { *(&mut rc as *mut RECT as *mut HTREEITEM) = cd.nmcd.dwItemSpec as HTREEITEM };
    if send(tree, TVM_GETITEMRECT, 1, &mut rc as *mut _ as isize) == 0 {
        return;
    }
    let hdc = cd.nmcd.hdc;
    let label = &hit.label;
    unsafe {
        let old = SelectObject(hdc, send(tree, WM_GETFONT, 0, 0) as HGDIOBJ);
        let ext = |n: usize| {
            let mut sz = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(hdc, label.as_ptr(), n as i32, &mut sz);
            sz
        };
        let full = ext(label.len());
        let x0 = rc.left + (rc.right - rc.left - full.cx) / 2;
        let top = rc.top + (rc.bottom - rc.top - full.cy) / 2;
        let brush = CreateSolidBrush(theme::cref(mark));
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, theme::cref(fg));
        for &(a, b) in &hit.ranges {
            let (l, r) = (x0 + ext(a).cx, x0 + ext(b).cx);
            let bg = RECT { left: l, top: top - 1, right: r, bottom: top + full.cy + 1 };
            FillRect(hdc, &bg, brush);
            let mut t = RECT { left: l, top: rc.top, right: r, bottom: rc.bottom };
            DrawTextW(hdc, label[a..].as_ptr(), (b - a) as i32, &mut t, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_NOCLIP);
        }
        DeleteObject(brush as HGDIOBJ);
        SelectObject(hdc, old);
    }
}

impl App {
    pub fn tree_notify(&mut self, nm: *const NMHDR) -> Option<LRESULT> {
        let code = unsafe { (*nm).code };
        match code {
            TVN_ITEMEXPANDINGW => {
                let tv = unsafe { &*(nm as *const NMTREEVIEWW) };
                if tv.action == TVE_EXPAND {
                    if let Some(i) = self.tree.node_at(tv.itemNew.hItem) {
                        self.tree.load(i);
                    }
                }
                Some(0)
            }
            TVN_ITEMEXPANDEDW => {
                let tv = unsafe { &*(nm as *const NMTREEVIEWW) };
                if let (false, Some(i)) = (self.tree.rebuilding, self.tree.node_at(tv.itemNew.hItem)) {
                    let p = self.tree.nodes[i].path.clone();
                    if tv.action == TVE_EXPAND {
                        self.tree.expanded.insert(p);
                    } else {
                        self.tree.expanded.remove(&p);
                    }
                }
                Some(0)
            }
            NM_CLICK => {
                let item = clicked_item(self.tree.hwnd)?;
                self.tree_activate(item);
                Some(0)
            }
            TVN_KEYDOWN => {
                let kd = unsafe { &*(nm as *const NMTVKEYDOWN) };
                if kd.wVKey == VK_RETURN {
                    let sel = send(self.tree.hwnd, TVM_GETNEXTITEM, TVGN_CARET as usize, 0) as HTREEITEM;
                    if sel != 0 {
                        self.tree_activate(sel);
                    }
                    return Some(1);
                }
                Some(0)
            }
            NM_CUSTOMDRAW => {
                let (dim, nodes) = (self.pal.fg_dim, &self.tree.nodes);
                custom_draw(nm, |p| nodes.get(p as usize).filter(|n| !n.is_dir && !is_markdown(&n.path)).map(|_| dim))
            }
            _ => None,
        }
    }

    fn tree_activate(&mut self, item: HTREEITEM) {
        let Some(i) = self.tree.node_at(item) else { return };
        if self.tree.nodes[i].is_dir {
            send(self.tree.hwnd, TVM_EXPAND, TVE_TOGGLE as usize, item as isize);
        } else {
            let p = self.tree.nodes[i].path.clone();
            self.open_file(&p, None);
        }
    }

    // ---------------------------------------------------------------- search

    pub fn focus_search(&mut self, seed: Option<&str>) {
        if let Some(s) = seed.filter(|s| !s.is_empty() && !s.contains('\n')) {
            set_text(self.search.edit, s);
            self.run_search();
        }
        send(self.search.edit, EM_SETSEL, 0, -1);
        unsafe { SetFocus(self.search.edit) };
    }

    pub fn rerun_search(&mut self) {
        if !get_text(self.search.edit).is_empty() {
            self.run_search();
        }
    }

    /// Search as you type (after the debounce): one-character queries match nearly every
    /// line, so they wait for Enter instead.
    pub fn run_search_typed(&mut self) {
        let query = get_text(self.search.edit);
        if query.chars().count() == 1 && (self.search.fuzzy || !self.search.regex) {
            let _ = self.core.search("", false, false, false, |_| {}); // cancel any running search
            self.reset_results();
            self.search.status = "Type 2+ characters, or press Enter".into();
            self.invalidate();
            return;
        }
        self.run_search();
    }

    fn reset_results(&mut self) {
        self.cancel_pending();
        self.clear_results();
    }

    fn cancel_pending(&mut self) {
        self.search.pending.clear();
        self.search.flush_armed = false;
        unsafe { KillTimer(self.hwnd, T_SEARCH_FLUSH) };
    }

    fn clear_results(&mut self) {
        let tree = self.search.tree;
        freeze(tree);
        clear(tree);
        thaw(tree);
        self.search.hits.clear();
        self.search.stale = false;
    }

    pub fn run_search(&mut self) {
        let query = get_text(self.search.edit);
        let (regex, case, fuzzy) = (self.search.regex, self.search.case, self.search.fuzzy);
        if self.search.mark.as_ref().is_some_and(|m| (m.query.as_str(), m.regex, m.case, m.fuzzy) != (query.as_str(), regex, case, fuzzy)) {
            self.search.mark = None;
            self.apply_search_mark();
        }
        if query.is_empty() {
            self.reset_results();
        } else {
            // Keep the old results on screen until the new ones arrive, then swap in one
            // repaint: no empty flash, no scrollbar shrinking and growing back.
            self.cancel_pending();
            self.search.stale = true;
        }
        self.search.status = if query.is_empty() { String::new() } else { "Searching…".into() };
        let hwnd = self.hwnd as usize;
        let res = self.core.search(&query, regex, case, fuzzy, move |ev| unsafe {
            PostMessageW(hwnd as HWND, WM_SEARCH, 0, Box::into_raw(Box::new(ev)) as isize);
        });
        if let Err(e) = res {
            self.clear_results();
            self.search.status = e;
        }
        self.invalidate();
    }

    pub fn flush_search_results(&mut self) {
        unsafe { KillTimer(self.hwnd, T_SEARCH_FLUSH) };
        self.search.flush_armed = false;
        if self.search.pending.is_empty() {
            return;
        }
        let tree = self.search.tree;
        freeze(tree);
        if self.search.stale {
            clear(tree);
            self.search.hits.clear();
            self.search.stale = false;
        }
        for (path, hits) in std::mem::take(&mut self.search.pending) {
            let group = insert_state(tree, TVI_ROOT, &format!("{path}   ({})", hits.len()), 0, None, TVIS_EXPANDED);
            for h in hits {
                let (text, label, ranges) = hit_label(&h);
                self.search.hits.push(SearchHit { path: path.clone(), line: h.line as usize, label, ranges });
                insert(tree, group, &text, self.search.hits.len() as isize, None);
            }
        }
        thaw(tree);
    }

    pub fn on_search_event(&mut self, ev: SearchEvent) {
        let latest = self.core.search_gen();
        match ev {
            SearchEvent::File { gen, path, hits } if gen == latest => {
                // Results stream in per file; add them to the tree in batches, one redraw each.
                self.search.pending.push((path, hits));
                if !self.search.flush_armed {
                    self.search.flush_armed = true;
                    // While old results still show, wait longer: most searches finish first
                    // and land in a single swap.
                    let ms = if self.search.stale { 300 } else { 100 };
                    unsafe { SetTimer(self.hwnd, T_SEARCH_FLUSH, ms, None) };
                }
            }
            SearchEvent::Done { gen, files, hits, ms, truncated } if gen == latest => {
                self.flush_search_results();
                if self.search.stale {
                    self.clear_results(); // no matches
                }
                let s = |n: usize| if n == 1 { "" } else { "s" };
                self.search.status = format!("{hits} result{} in {files} file{} · {ms} ms{}", s(hits), s(files), if truncated { " (truncated)" } else { "" });
                self.invalidate();
            }
            SearchEvent::Failed { gen, error } if gen == latest => {
                self.cancel_pending();
                self.clear_results();
                self.search.status = error;
                self.invalidate();
            }
            _ => {}
        }
    }

    pub fn search_notify(&mut self, nm: *const NMHDR) -> Option<LRESULT> {
        let code = unsafe { (*nm).code };
        match code {
            NM_CLICK => {
                let item = clicked_item(self.search.tree)?;
                self.search_activate(item);
                Some(0)
            }
            TVN_KEYDOWN => {
                let kd = unsafe { &*(nm as *const NMTVKEYDOWN) };
                if kd.wVKey == VK_RETURN {
                    let sel = send(self.search.tree, TVM_GETNEXTITEM, TVGN_CARET as usize, 0) as HTREEITEM;
                    if sel != 0 {
                        self.search_activate(sel);
                    }
                    return Some(1);
                }
                Some(0)
            }
            NM_CUSTOMDRAW => {
                let cd = unsafe { &mut *(nm as *mut NMTVCUSTOMDRAW) };
                let p = cd.nmcd.lItemlParam;
                let hit = (p > 0).then(|| self.search.hits.get(p as usize - 1)).flatten().filter(|h| !h.ranges.is_empty());
                Some(match cd.nmcd.dwDrawStage {
                    CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW,
                    CDDS_ITEMPREPAINT if p == 0 => {
                        cd.clrText = theme::cref(self.pal.fg_dim);
                        CDRF_DODEFAULT
                    }
                    CDDS_ITEMPREPAINT if hit.is_some() => CDRF_NOTIFYPOSTPAINT,
                    CDDS_ITEMPOSTPAINT => {
                        if let Some(h) = hit {
                            draw_hit_marks(cd, self.search.tree, h, self.pal.mark, self.pal.fg);
                        }
                        CDRF_DODEFAULT
                    }
                    _ => CDRF_DODEFAULT,
                } as LRESULT)
            }
            _ => None,
        }
    }

    fn search_activate(&mut self, item: HTREEITEM) {
        let p = item_param(self.search.tree, item);
        if p == 0 {
            send(self.search.tree, TVM_EXPAND, TVE_TOGGLE as usize, item as isize);
            return;
        }
        let Some((path, line)) = self.search.hits.get(p as usize - 1).map(|h| (h.path.clone(), h.line)) else { return };
        if self.search.mark.is_none() {
            let (query, regex, case, fuzzy) = (get_text(self.search.edit), self.search.regex, self.search.case, self.search.fuzzy);
            let re = if fuzzy { fuzzy_regex(&self.search.hits) } else { search_regex(&query, regex, case) };
            self.search.mark = re.map(|re| Mark { query, regex, case, fuzzy, re });
            // A newly opened file picks the mark up as it loads.
            if self.current.as_deref() == Some(path.as_str()) {
                self.apply_search_mark();
            }
        }
        self.open_file(&path, Some(line));
    }

    /// Show the search mark (or its absence) in the open file's editor and preview.
    pub fn apply_search_mark(&mut self) {
        if self.current.is_none() {
            return;
        }
        self.editor.mark_matches(self.search.mark.as_ref().map(|m| &m.re));
        self.refresh_preview(true);
    }

    // ---------------------------------------------------------------- outline

    pub fn outline_notify(&mut self, nm: *const NMHDR) -> Option<LRESULT> {
        let code = unsafe { (*nm).code };
        let go = |app: &mut App, item: HTREEITEM| {
            let i = item_param(app.outline.tree, item);
            if let Some(h) = (i >= 0).then(|| app.headings.get(i as usize)).flatten() {
                let line = h.line;
                app.outline.active = Some(i as usize);
                app.reveal_line(line, true);
                if app.effective_mode() == Mode::Preview {
                    unsafe { SetFocus(app.preview) };
                }
            }
        };
        match code {
            NM_CLICK => {
                let item = clicked_item(self.outline.tree)?;
                go(self, item);
                Some(0)
            }
            TVN_KEYDOWN => {
                let kd = unsafe { &*(nm as *const NMTVKEYDOWN) };
                if kd.wVKey == VK_RETURN {
                    let sel = send(self.outline.tree, TVM_GETNEXTITEM, TVGN_CARET as usize, 0) as HTREEITEM;
                    if sel != 0 {
                        go(self, sel);
                    }
                    return Some(1);
                }
                Some(0)
            }
            _ => None,
        }
    }
}
