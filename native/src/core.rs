//! Workspace model: file list, watcher, file operations and searches.

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

use crate::fuzzy::{self, FuzzyIndex};

pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_HITS: usize = 5000;
const MAX_LINE_CHARS: usize = 240;
const MAX_LINE_MATCHES: usize = 50;

#[derive(Default)]
struct WsState {
    root: Option<PathBuf>,
    files: Arc<Vec<String>>,
    file_set: HashSet<String>,
}

pub enum WatchEvent {
    /// Known files whose content changed on disk.
    Modified(Vec<String>),
    /// The file list changed (created, deleted or renamed files).
    Files(Vec<String>),
}

#[derive(Default)]
pub struct Core {
    ws: Arc<Mutex<WsState>>,
    watcher: Option<Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>>,
    search_gen: Arc<AtomicU64>,
    /// Fuzzy-search index of the open workspace, built on first use.
    fuzzy: Arc<Mutex<Option<FuzzyIndex>>>,
    /// Set by the watcher: the fuzzy index must check files before its next search.
    fuzzy_dirty: Arc<AtomicBool>,
}

// ---------------------------------------------------------------- workspace

/// Parallel walk respecting .gitignore / hidden files; returns sorted relative paths with `/` separators.
pub fn scan(root: &Path) -> Vec<String> {
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

fn set_files(ws: &mut WsState, files: Vec<String>) {
    ws.file_set = files.iter().cloned().collect();
    ws.files = Arc::new(files);
}

/// Split watcher events into "file list changed" and "content of known files changed".
fn classify(ws: &WsState, root: &Path, events: &[DebouncedEvent]) -> (bool, Vec<String>) {
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

impl Core {
    /// Open a folder and start watching it; `notify` runs on the watcher thread.
    pub fn open(&mut self, path: &Path, notify: impl Fn(WatchEvent) + Send + 'static) -> Result<Vec<String>, String> {
        if !path.is_dir() {
            return Err(format!("not a folder: {}", path.display()));
        }
        let root = path.to_path_buf();
        let files = scan(&root);
        {
            let mut ws = self.ws.lock().unwrap();
            ws.root = Some(root.clone());
            set_files(&mut ws, files.clone());
        }
        // Fresh slot: a search still running on the old workspace keeps (and then drops) the old one.
        self.fuzzy = Arc::default();
        self.fuzzy_dirty = Arc::new(AtomicBool::new(true));

        // Watch for created/removed files (fresh list) and edits made outside the app.
        let ws_arc = self.ws.clone();
        let fuzzy_dirty = self.fuzzy_dirty.clone();
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
            if rescan || !modified.is_empty() {
                fuzzy_dirty.store(true, Ordering::SeqCst);
            }
            if !modified.is_empty() {
                notify(WatchEvent::Modified(modified));
            }
            if rescan {
                let files = scan(&watch_root);
                set_files(&mut ws_arc.lock().unwrap(), files.clone());
                notify(WatchEvent::Files(files));
            }
        })
        .map_err(|e| e.to_string())?;
        debouncer.watcher().watch(&root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
        self.watcher = Some(debouncer);
        Ok(files)
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.ws.lock().unwrap().root.clone()
    }

    pub fn has_file(&self, rel: &str) -> bool {
        self.ws.lock().unwrap().file_set.contains(rel)
    }

    pub fn files(&self) -> Arc<Vec<String>> {
        self.ws.lock().unwrap().files.clone()
    }

    fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        let ws = self.ws.lock().unwrap();
        let root = ws.root.as_ref().ok_or("no workspace open")?;
        let p = root.join(rel);
        let canon = p.canonicalize().map_err(|e| e.to_string())?;
        let canon_root = root.canonicalize().map_err(|e| e.to_string())?;
        if !canon.starts_with(&canon_root) {
            return Err("path outside workspace".into());
        }
        Ok(canon)
    }

    /// Path for a file that may not exist yet: plain relative components only, inside the workspace.
    fn resolve_new(&self, rel: &str) -> Result<PathBuf, String> {
        resolve_new(self.ws.lock().unwrap().root.as_deref(), rel)
    }

    pub fn read(&self, rel: &str) -> Result<String, String> {
        let bytes = std::fs::read(self.resolve(rel)?).map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn write(&self, rel: &str, content: &[u8]) -> Result<(), String> {
        std::fs::write(self.resolve(rel)?, content).map_err(|e| e.to_string())
    }

    /// Rescan right away so the caller gets the new list without waiting for the watcher.
    fn refresh(&self) -> Vec<String> {
        let Some(root) = self.root() else { return Vec::new() };
        let files = scan(&root);
        set_files(&mut self.ws.lock().unwrap(), files.clone());
        files
    }

    pub fn create(&self, rel: &str) -> Result<Vec<String>, String> {
        let p = self.resolve_new(rel)?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { format!("{rel} already exists") } else { e.to_string() })?;
        Ok(self.refresh())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<Vec<String>, String> {
        let src = self.resolve(from)?;
        let dst = self.resolve_new(to)?;
        // Case-only renames hit the same file on Windows; allow those.
        if dst.exists() && dst.canonicalize().ok().as_deref() != Some(src.as_path()) {
            return Err(format!("{to} already exists"));
        }
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
        Ok(self.refresh())
    }

    /// Fuzzy file-name search; `indices` are char offsets into the path that matched.
    pub fn find_files(&self, query: &str, limit: usize) -> Vec<FileMatch> {
        let files = self.files();
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
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

        scored
            .into_iter()
            .map(|(_, i)| {
                let mut indices = Vec::new();
                pattern.indices(Utf32Str::new(&files[i], &mut buf), &mut matcher, &mut indices);
                indices.sort_unstable();
                indices.dedup();
                FileMatch { path: files[i].clone(), indices }
            })
            .collect()
    }

    /// Search file contents on worker threads; `sink` gets results as they come. A newer
    /// search (or an empty query) cancels the running one. `fuzzy` ignores `regex` and
    /// `case_sensitive`: typo- and accent-tolerant, best results first (see `fuzzy`).
    pub fn search(
        &self,
        query: &str,
        regex: bool,
        case_sensitive: bool,
        fuzzy: bool,
        sink: impl Fn(SearchEvent) + Send + Sync + 'static,
    ) -> Result<(), String> {
        let gen = self.search_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if query.is_empty() {
            return Ok(());
        }
        if fuzzy {
            return self.search_fuzzy(query, gen, sink);
        }
        let pattern = if regex { query.to_string() } else { regex::escape(query) };
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(!case_sensitive)
            .build(&pattern)
            .map_err(|e| e.to_string())?;
        let (root, files) = {
            let ws = self.ws.lock().unwrap();
            (ws.root.clone().ok_or("no workspace open")?, ws.files.clone())
        };
        let gen_ref = self.search_gen.clone();

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
                                    let mut found = Vec::new();
                                    let _ = matcher.find_iter(line.as_bytes(), |m| {
                                        if m.start() < m.end() {
                                            found.push((m.start(), m.end()));
                                        }
                                        found.len() < MAX_LINE_MATCHES
                                    });
                                    hits.push(make_hit(lnum, line, &found));
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
                                sink(SearchEvent::File { gen, path: rel.clone(), hits });
                            }
                        }
                    });
                }
            });

            if gen_ref.load(Ordering::Relaxed) == gen {
                sink(SearchEvent::Done {
                    gen,
                    files: file_count.load(Ordering::Relaxed),
                    hits: hit_count.load(Ordering::Relaxed),
                    ms: t0.elapsed().as_millis(),
                    truncated: truncated.load(Ordering::Relaxed),
                });
            }
        });
        Ok(())
    }

    fn search_fuzzy(&self, query: &str, gen: u64, sink: impl Fn(SearchEvent) + Send + Sync + 'static) -> Result<(), String> {
        let (root, files) = {
            let ws = self.ws.lock().unwrap();
            (ws.root.clone().ok_or("no workspace open")?, ws.files.clone())
        };
        let (slot, dirty, gen_ref) = (self.fuzzy.clone(), self.fuzzy_dirty.clone(), self.search_gen.clone());
        let query = query.to_string();
        std::thread::spawn(move || {
            fuzzy::run(&slot, &dirty, &root, &files, &query, gen, &|| gen_ref.load(Ordering::Relaxed), &sink);
        });
        Ok(())
    }

    /// Generation of the latest search; results from older ones are stale.
    pub fn search_gen(&self) -> u64 {
        self.search_gen.load(Ordering::SeqCst)
    }
}

fn resolve_new(root: Option<&Path>, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.trim().is_empty() || !rel_path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!("invalid file name: {rel}"));
    }
    Ok(root.ok_or("no workspace open")?.join(rel_path))
}

// ---------------------------------------------------------------- search results

pub struct FileMatch {
    pub path: String,
    pub indices: Vec<u32>,
}

pub struct LineHit {
    pub line: u64,
    pub text: String,
    /// Byte ranges of the matches in `text`.
    pub ranges: Vec<(usize, usize)>,
}

pub enum SearchEvent {
    File { gen: u64, path: String, hits: Vec<LineHit> },
    Done { gen: u64, files: usize, hits: usize, ms: u128, truncated: bool },
    Failed { gen: u64, error: String },
}

/// Trim a long line to a window around the first match.
pub fn make_hit(line_no: u64, line: &str, found: &[(usize, usize)]) -> LineHit {
    let line = line.trim_end_matches(['\r', '\n']);
    let first_start = found.first().map(|r| r.0).unwrap_or(0);
    let mut start = 0;
    if line.len() > MAX_LINE_CHARS * 2 && first_start > MAX_LINE_CHARS / 3 {
        start = first_start - MAX_LINE_CHARS / 3;
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
    let ranges = found.iter().filter(|(s, e)| *s >= start && *e <= end).map(|(s, e)| (s - start, e - start)).collect();
    LineHit { line: line_no, text: text.to_string(), ranges }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_are_windowed() {
        let h = make_hit(3, "  café needle needle\n", &[(8, 14), (15, 21)]);
        assert_eq!(h.text, "café needle needle");
        assert_eq!(h.ranges, vec![(6, 12), (13, 19)]);

        let long = format!("{}needle{}", "x".repeat(1000), "y".repeat(1000));
        let h = make_hit(1, &long, &[(1000, 1006)]);
        assert!(h.text.len() <= MAX_LINE_CHARS * 2);
        let (s, e) = h.ranges[0];
        assert_eq!(&h.text[s..e], "needle");
    }

    #[test]
    fn new_paths_stay_inside_workspace() {
        let root = Some(Path::new("C:\\ws"));
        assert!(resolve_new(root, "notes/new.md").is_ok());
        for bad in ["../x.md", "a/../../x.md", "C:\\x.md", "\\x.md", "", "  "] {
            assert!(resolve_new(root, bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn scan_lists_samples() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples");
        let files = scan(&root);
        assert!(files.contains(&"README.md".to_string()));
        assert!(files.contains(&"guide/intro.md".to_string()));
    }
}
