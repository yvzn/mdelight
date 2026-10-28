//! Sidebar panels, each a native TreeView: files (lazy folders), search results, outline.

use std::collections::{HashMap, HashSet};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient};
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
    let mut w = wide(text);
    let mut ins: TVINSERTSTRUCTW = unsafe { std::mem::zeroed() };
    ins.hParent = parent;
    ins.hInsertAfter = TVI_LAST;
    ins.Anonymous.item = TVITEMW {
        mask: TVIF_TEXT | TVIF_PARAM | if children.is_some() { TVIF_CHILDREN } else { 0 },
        hItem: 0,
        state: 0,
        stateMask: 0,
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
    pub status: String,
    hits: Vec<(String, usize)>,
    /// Results received since the last flush into the tree (one redraw per batch).
    pending: Vec<(String, Vec<LineHit>)>,
    flush_armed: bool,
}

impl SearchPanel {
    pub fn create(parent: HWND) -> SearchPanel {
        SearchPanel {
            edit: make_edit(parent, ID_SEARCH_EDIT, "Search in files"),
            tree: make_tree(parent, ID_SEARCH_TREE, true),
            case: false,
            regex: false,
            status: String::new(),
            hits: Vec::new(),
            pending: Vec::new(),
            flush_armed: false,
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
        if query.chars().count() == 1 && !self.search.regex {
            let _ = self.core.search("", false, false, |_| {}); // cancel any running search
            self.reset_results();
            self.search.status = "Type 2+ characters, or press Enter".into();
            self.invalidate();
            return;
        }
        self.run_search();
    }

    fn reset_results(&mut self) {
        clear(self.search.tree);
        self.search.hits.clear();
        self.search.pending.clear();
        self.search.flush_armed = false;
        unsafe { KillTimer(self.hwnd, T_SEARCH_FLUSH) };
    }

    pub fn run_search(&mut self) {
        let query = get_text(self.search.edit);
        self.reset_results();
        self.search.status = if query.is_empty() { String::new() } else { "Searching…".into() };
        let hwnd = self.hwnd as usize;
        let res = self.core.search(&query, self.search.regex, self.search.case, move |ev| unsafe {
            PostMessageW(hwnd as HWND, WM_SEARCH, 0, Box::into_raw(Box::new(ev)) as isize);
        });
        if let Err(e) = res {
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
        send(tree, WM_SETREDRAW, 0, 0);
        for (path, hits) in std::mem::take(&mut self.search.pending) {
            let group = insert(tree, TVI_ROOT, &format!("{path}   ({})", hits.len()), 0, None);
            for h in hits {
                self.search.hits.push((path.clone(), h.line as usize));
                let text = format!("{}:  {}", h.line, h.text);
                insert(tree, group, &text, self.search.hits.len() as isize, None);
            }
            send(tree, TVM_EXPAND, TVE_EXPAND as usize, group as isize);
        }
        send(tree, WM_SETREDRAW, 1, 0);
        unsafe { InvalidateRect(tree, std::ptr::null(), 1) };
    }

    pub fn on_search_event(&mut self, ev: SearchEvent) {
        let latest = self.core.search_gen();
        match ev {
            SearchEvent::File { gen, path, hits } if gen == latest => {
                // Results stream in per file; add them to the tree in batches, one redraw each.
                self.search.pending.push((path, hits));
                if !self.search.flush_armed {
                    self.search.flush_armed = true;
                    unsafe { SetTimer(self.hwnd, T_SEARCH_FLUSH, 100, None) };
                }
            }
            SearchEvent::Done { gen, files, hits, ms, truncated } if gen == latest => {
                self.flush_search_results();
                let s = |n: usize| if n == 1 { "" } else { "s" };
                self.search.status = format!("{hits} result{} in {files} file{} · {ms} ms{}", s(hits), s(files), if truncated { " (truncated)" } else { "" });
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
                let dim = self.pal.fg_dim;
                custom_draw(nm, |p| (p == 0).then_some(dim))
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
        if let Some((path, line)) = self.search.hits.get(p as usize - 1).cloned() {
            self.open_file(&path, Some(line));
        }
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
