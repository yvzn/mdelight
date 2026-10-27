use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{sinks, BinaryDetection, SearcherBuilder};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebouncedEvent, Debouncer};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HITS: usize = 5000;
const MAX_LINE_CHARS: usize = 240;

#[derive(Default)]
struct Workspace {
    root: Option<PathBuf>,
    files: Arc<Vec<String>>,
    file_set: HashSet<String>,
}

#[derive(Default)]
struct AppState {
    ws: Arc<Mutex<Workspace>>,
    watcher: Mutex<Option<Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>>>,
    search_gen: Arc<AtomicU64>,
}

// ---------------------------------------------------------------- workspace

/// Parallel walk respecting .gitignore / hidden files; returns sorted relative paths with `/` separators.
fn scan(root: &Path) -> Vec<String> {
    let out = Mutex::new(Vec::with_capacity(4096));
    ignore::WalkBuilder::new(root)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "node_modules")
        .build_parallel()
        .run(|| {
            let out = &out;
            Box::new(move |entry| {
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|t| t.is_file()) {
                        if let Ok(rel) = e.path().strip_prefix(root) {
                            let rel = rel.to_string_lossy().replace('\\', "/");
                            out.lock().unwrap().push(rel);
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    let mut files = out.into_inner().unwrap();
    files.sort_unstable_by_key(|f| f.to_lowercase());
    files
}

fn set_files(ws: &mut Workspace, files: Vec<String>) {
    ws.file_set = files.iter().cloned().collect();
    ws.files = Arc::new(files);
}

/// Split watcher events into "file list changed" and "content of known files changed".
fn classify(ws: &Workspace, root: &Path, events: &[DebouncedEvent]) -> (bool, Vec<String>) {
    let mut rescan = false;
    let mut modified = Vec::new();
    for ev in events {
        let Ok(rel) = ev.path.strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.starts_with(".git/") || rel == ".git" {
            continue;
        }
        if !ev.path.exists() {
            rescan = true;
        } else if ev.path.is_file() {
            if ws.file_set.contains(&rel) {
                modified.push(rel);
            } else {
                rescan = true;
            }
        }
    }
    (rescan, modified)
}

#[tauri::command]
async fn open_workspace(app: AppHandle, state: State<'_, AppState>, path: String) -> Result<Vec<String>, String> {
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    let files = {
        let r = root.clone();
        tauri::async_runtime::spawn_blocking(move || scan(&r)).await.map_err(|e| e.to_string())?
    };
    {
        let mut ws = state.ws.lock().unwrap();
        ws.root = Some(root.clone());
        set_files(&mut ws, files.clone());
    }

    // Watch for created/removed files (fresh list) and edits made outside the app.
    let ws_arc = state.ws.clone();
    let watch_root = root.clone();
    let mut debouncer = new_debouncer(Duration::from_millis(250), move |res: Result<Vec<DebouncedEvent>, _>| {
        let Ok(events) = res else { return };
        let (rescan, modified) = {
            let ws = ws_arc.lock().unwrap();
            if ws.root.as_deref() != Some(watch_root.as_path()) {
                return;
            }
            classify(&ws, &watch_root, &events)
        };
        if !modified.is_empty() {
            let _ = app.emit("file-changed", modified);
        }
        if rescan {
            let files = scan(&watch_root);
            set_files(&mut ws_arc.lock().unwrap(), files.clone());
            let _ = app.emit("files-changed", files);
        }
    })
    .map_err(|e| e.to_string())?;
    debouncer.watcher().watch(&root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    *state.watcher.lock().unwrap() = Some(debouncer);

    Ok(files)
}

fn resolve(state: &AppState, rel: &str) -> Result<PathBuf, String> {
    let ws = state.ws.lock().unwrap();
    let root = ws.root.as_ref().ok_or("no workspace open")?;
    let p = root.join(rel);
    let canon = p.canonicalize().map_err(|e| e.to_string())?;
    let canon_root = root.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(&canon_root) {
        return Err("path outside workspace".into());
    }
    Ok(canon)
}

#[tauri::command]
async fn read_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let p = resolve(&state, &path)?;
    let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[tauri::command]
async fn write_file(state: State<'_, AppState>, path: String, content: String) -> Result<(), String> {
    let p = resolve(&state, &path)?;
    std::fs::write(p, content).map_err(|e| e.to_string())
}

/// Path for a file that may not exist yet: plain relative components only, inside the workspace.
fn resolve_new(state: &AppState, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.trim().is_empty() || !rel_path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!("invalid file name: {rel}"));
    }
    let ws = state.ws.lock().unwrap();
    Ok(ws.root.as_ref().ok_or("no workspace open")?.join(rel_path))
}

/// Rescan right away so the caller gets the new list without waiting for the watcher.
fn refresh(state: &AppState) -> Vec<String> {
    let Some(root) = state.ws.lock().unwrap().root.clone() else { return Vec::new() };
    let files = scan(&root);
    set_files(&mut state.ws.lock().unwrap(), files.clone());
    files
}

#[tauri::command]
async fn create_file(state: State<'_, AppState>, path: String) -> Result<Vec<String>, String> {
    let p = resolve_new(&state, &path)?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { format!("{path} already exists") } else { e.to_string() })?;
    Ok(refresh(&state))
}

#[tauri::command]
async fn rename_file(state: State<'_, AppState>, from: String, to: String) -> Result<Vec<String>, String> {
    let src = resolve(&state, &from)?;
    let dst = resolve_new(&state, &to)?;
    // Case-only renames hit the same file on Windows; allow those.
    if dst.exists() && dst.canonicalize().ok().as_deref() != Some(src.as_path()) {
        return Err(format!("{to} already exists"));
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(refresh(&state))
}

#[tauri::command]
fn initial_path() -> Option<String> {
    std::env::args().nth(1).filter(|a| Path::new(a).is_dir())
}

// ---------------------------------------------------------------- file name search

#[derive(Serialize)]
struct FileMatch {
    path: String,
    indices: Vec<u32>,
}

#[tauri::command]
async fn find_files(state: State<'_, AppState>, query: String, limit: usize) -> Result<Vec<FileMatch>, String> {
    let files = state.ws.lock().unwrap().files.clone();
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();

    let mut scored: Vec<(u32, usize)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let s = pattern.score(Utf32Str::new(f, &mut buf), &mut matcher)?;
            // Small bonus for markdown files: they're what this editor is for.
            let bonus = if f.ends_with(".md") || f.ends_with(".markdown") { 8 } else { 0 };
            Some((s + bonus, i))
        })
        .collect();
    scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(files[a.1].len().cmp(&files[b.1].len())));
    scored.truncate(limit);

    Ok(scored
        .into_iter()
        .map(|(_, i)| {
            let mut indices = Vec::new();
            pattern.indices(Utf32Str::new(&files[i], &mut buf), &mut matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            FileMatch { path: files[i].clone(), indices }
        })
        .collect())
}

// ---------------------------------------------------------------- content search

#[derive(Serialize, Clone)]
struct LineHit {
    line: u64,
    text: String,
    /// UTF-16 offsets into `text`, ready for JS string slicing.
    ranges: Vec<(usize, usize)>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum SearchEvent {
    File { path: String, hits: Vec<LineHit> },
    Done { files: usize, hits: usize, ms: u128, truncated: bool },
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Trim a long line to a window around the first match, and convert byte ranges to UTF-16 offsets.
fn make_hit(line_no: u64, line: &str, byte_ranges: &[(usize, usize)]) -> LineHit {
    let line = line.trim_end_matches(['\r', '\n']);
    let first = byte_ranges.first().map(|r| r.0).unwrap_or(0);
    let mut start = 0;
    if line.len() > MAX_LINE_CHARS * 2 && first > MAX_LINE_CHARS / 3 {
        start = first - MAX_LINE_CHARS / 3;
        while !line.is_char_boundary(start) {
            start -= 1;
        }
    }
    let mut end = (start + MAX_LINE_CHARS * 2).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let lead = line[start..end].len() - line[start..end].trim_start().len();
    let start = start + lead;
    let text = &line[start..end];
    let ranges = byte_ranges
        .iter()
        .filter(|(s, e)| *s >= start && *e <= end)
        .map(|(s, e)| (utf16_len(&line[start..*s]), utf16_len(&line[start..*e])))
        .collect();
    LineHit { line: line_no, text: text.to_string(), ranges }
}

#[tauri::command]
fn search_content(
    state: State<'_, AppState>,
    query: String,
    regex: bool,
    case_sensitive: bool,
    on_event: Channel<SearchEvent>,
) -> Result<(), String> {
    let gen = state.search_gen.fetch_add(1, Ordering::SeqCst) + 1;
    if query.is_empty() {
        return Ok(());
    }
    let pattern = if regex { query } else { regex::escape(&query) };
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .build(&pattern)
        .map_err(|e| e.to_string())?;
    let (root, files) = {
        let ws = state.ws.lock().unwrap();
        (ws.root.clone().ok_or("no workspace open")?, ws.files.clone())
    };
    let gen_ref = state.search_gen.clone();

    std::thread::spawn(move || {
        let t0 = Instant::now();
        let next = AtomicUsize::new(0);
        let hit_count = AtomicUsize::new(0);
        let file_count = AtomicUsize::new(0);
        let truncated = AtomicBool::new(false);
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    let mut searcher = SearcherBuilder::new()
                        .binary_detection(BinaryDetection::quit(0))
                        .line_number(true)
                        .build();
                    let matcher: RegexMatcher = matcher.clone();
                    loop {
                        if gen_ref.load(Ordering::Relaxed) != gen || truncated.load(Ordering::Relaxed) {
                            return;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rel) = files.get(i) else { return };
                        let path = root.join(rel);
                        if std::fs::metadata(&path).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
                            continue;
                        }
                        let mut hits = Vec::new();
                        let _ = searcher.search_path(
                            &matcher,
                            &path,
                            sinks::Lossy(|lnum, line| {
                                let mut ranges = Vec::new();
                                let _ = matcher.find_iter(line.as_bytes(), |m| {
                                    ranges.push((m.start(), m.end()));
                                    ranges.len() < 50
                                });
                                hits.push(make_hit(lnum, line, &ranges));
                                Ok(hits.len() < 200)
                            }),
                        );
                        if hits.is_empty() {
                            continue;
                        }
                        let total = hit_count.fetch_add(hits.len(), Ordering::Relaxed) + hits.len();
                        file_count.fetch_add(1, Ordering::Relaxed);
                        if total >= MAX_HITS {
                            truncated.store(true, Ordering::Relaxed);
                        }
                        if gen_ref.load(Ordering::Relaxed) == gen {
                            let _ = on_event.send(SearchEvent::File { path: rel.clone(), hits });
                        }
                    }
                });
            }
        });

        if gen_ref.load(Ordering::Relaxed) == gen {
            let _ = on_event.send(SearchEvent::Done {
                files: file_count.load(Ordering::Relaxed),
                hits: hit_count.load(Ordering::Relaxed),
                ms: t0.elapsed().as_millis(),
                truncated: truncated.load(Ordering::Relaxed),
            });
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- markdown

#[tauri::command]
async fn render_markdown(text: String) -> String {
    let mut opts = comrak::Options::default();
    opts.extension.strikethrough = true;
    opts.extension.table = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.footnotes = true;
    opts.extension.header_id_prefix = Some(String::new());
    // Raw HTML stays escaped: the preview shares a webview with the IPC bridge.
    opts.render.r#unsafe = false;
    opts.render.sourcepos = true;
    comrak::markdown_to_html(&text, &opts)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            open_workspace,
            read_file,
            write_file,
            create_file,
            rename_file,
            initial_path,
            find_files,
            search_content,
            render_markdown
        ])
        .on_window_event(|window, event| {
            // Minimizing fires a resize (to 0×0 on Windows), restoring fires another.
            if let tauri::WindowEvent::Resized(_) = event {
                let minimized = window.is_minimized().unwrap_or(false);
                if let Some(webview) = window.app_handle().get_webview_window(window.label()) {
                    set_low_memory(&webview, minimized);
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------------------------------------------------------------- memory

/// Ask WebView2 to trim caches and page memory while the window is minimized, and
/// to return to normal when it comes back. Only called on state changes.
#[cfg(windows)]
fn set_low_memory(webview: &tauri::WebviewWindow, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    use windows_core::Interface;

    static IS_LOW: AtomicBool = AtomicBool::new(false);
    if IS_LOW.swap(low, Ordering::Relaxed) == low {
        return;
    }
    let level = if low { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL };
    let _ = webview.with_webview(move |wv| unsafe {
        // ICoreWebView2_19 needs WebView2 runtime 114+; older runtimes simply keep the default.
        if let Ok(core) = wv.controller().CoreWebView2().and_then(|c| c.cast::<ICoreWebView2_19>()) {
            let _ = core.SetMemoryUsageTargetLevel(level);
        }
    });
}

#[cfg(not(windows))]
fn set_low_memory(_webview: &tauri::WebviewWindow, _low: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_ranges_are_utf16_and_windowed() {
        let h = make_hit(3, "  café needle\n", &[(8, 14)]);
        assert_eq!(h.text, "café needle");
        assert_eq!(h.ranges, vec![(5, 11)]);

        let long = format!("{}needle{}", "x".repeat(1000), "y".repeat(1000));
        let h = make_hit(1, &long, &[(1000, 1006)]);
        assert!(h.text.len() <= MAX_LINE_CHARS * 2);
        let (s, e) = h.ranges[0];
        assert_eq!(&h.text[s..e], "needle");
    }

    #[test]
    fn new_paths_stay_inside_workspace() {
        let state = AppState::default();
        state.ws.lock().unwrap().root = Some(PathBuf::from("C:\\ws"));
        assert!(resolve_new(&state, "notes/new.md").is_ok());
        for bad in ["../x.md", "a/../../x.md", "C:\\x.md", "\\x.md", "", "  "] {
            assert!(resolve_new(&state, bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn scan_lists_samples_and_markdown_renders_with_sourcepos() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples");
        let files = scan(&root);
        assert!(files.contains(&"README.md".to_string()));
        assert!(files.contains(&"guide/intro.md".to_string()));

        let html = tauri::async_runtime::block_on(render_markdown("# Hi\n\n<script>x</script>\n".into()));
        assert!(html.contains("data-sourcepos=\"1:1"));
        assert!(!html.contains("<script>"));
    }
}
