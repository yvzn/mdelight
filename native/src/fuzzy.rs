//! Typo- and accent-tolerant workspace search: a tantivy index of markdown sections, ranked so
//! exact matches always come before near ones.
//!
//! The index lives in a temp folder and is memory-mapped, so it costs page cache rather than
//! heap. It is built on the first fuzzy search and brought up to date (by modification time)
//! before each later one once the watcher has seen a change.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::{DocAddress, Index, IndexReader, ReloadPolicy, Score, TantivyDocument, Term};

use crate::core::{make_hit, LineHit, SearchEvent, MAX_FILE_BYTES, MAX_HITS};

const TOKENIZER: &str = "fold";
/// Writer arena; tantivy's minimum. The writer only exists while (re)indexing.
const WRITER_BYTES: usize = 15_000_000;
/// Candidate sections taken from each of the exact and fuzzy queries.
const CANDIDATES: usize = 400;
/// A section ends at a heading or after this many lines, so long notes still rank by region.
const MAX_SECTION_LINES: usize = 60;
const MAX_FILE_HITS: usize = 200;
const HEADING_BOOST: Score = 2.0;

/// Lowercase, strip accents (é→e, œ→oe, ß→ss), split on non-alphanumerics.
fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// Folded tokens of `text` with their byte ranges in it.
fn tokens(analyzer: &mut TextAnalyzer, text: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut ts = analyzer.token_stream(text);
    ts.process(&mut |t| out.push((t.text.clone(), t.offset_from, t.offset_to)));
    out
}

// ---------------------------------------------------------------- sections

struct Section {
    start: usize,
    end: usize,
    /// 0-based line of `start`.
    line: usize,
    heading: String,
}

/// Split markdown at ATX headings (outside fenced code), and every `MAX_SECTION_LINES` lines.
fn sections(text: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let mut cur = Section { start: 0, end: 0, line: 0, heading: String::new() };
    let mut fence: Option<&str> = None;
    let (mut pos, mut line) = (0, 0);
    for l in text.split_inclusive('\n') {
        let t = l.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| t.starts_with(m));
        match (fence, marker) {
            (None, Some(m)) => fence = Some(m),
            (Some(f), Some(m)) if f == m => fence = None,
            _ => {}
        }
        let heading = fence.is_none() && marker.is_none() && t.starts_with('#') && t.trim_start_matches('#').starts_with([' ', '\t', '\r', '\n']);
        if pos > cur.start && (heading || line - cur.line >= MAX_SECTION_LINES) {
            cur.end = pos;
            out.push(std::mem::replace(&mut cur, Section { start: pos, end: 0, line, heading: String::new() }));
        }
        if heading {
            cur.heading = t.trim_start_matches('#').trim().to_string();
        }
        pos += l.len();
        line += 1;
    }
    if pos > cur.start {
        cur.end = pos;
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------- matching

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Exact,
    Prefix,
    Fuzzy,
}

struct QTerm {
    text: String,
    chars: Vec<char>,
    dist: u8,
    prefix: bool,
}

/// Allowed typos: none for short words, one up to 7 chars, two beyond.
fn max_dist(len: usize) -> u8 {
    match len {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

/// Optimal-string-alignment distance (a swap counts as one edit). With `prefix`, the best
/// distance between `q` and any prefix of `t`, as tantivy's fuzzy prefix query does.
fn osa(q: &[char], t: &[char], prefix: bool) -> usize {
    let (n, m) = (q.len(), t.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = (q[i - 1] != t[j - 1]) as usize;
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && q[i - 1] == t[j - 2] && q[i - 2] == t[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    if prefix {
        *d[n].iter().min().unwrap()
    } else {
        d[n][m]
    }
}

fn classify(tok: &str, q: &QTerm) -> Option<Kind> {
    if tok == q.text {
        return Some(Kind::Exact);
    }
    if q.prefix && tok.starts_with(&q.text) {
        return Some(Kind::Prefix);
    }
    if q.dist > 0 {
        let t: Vec<char> = tok.chars().collect();
        if t.len().abs_diff(q.chars.len()) <= q.dist as usize || q.prefix {
            if osa(&q.chars, &t, q.prefix) <= q.dist as usize {
                return Some(Kind::Fuzzy);
            }
        }
    }
    None
}

/// Rank of a section: more query terms matched exactly (or as a prefix of the word being
/// typed) first, then more terms matched at all, then fewer prefix-only, then BM25.
#[derive(Clone, Copy)]
struct Rank {
    exact: usize,
    matched: usize,
    prefix: usize,
    score: Score,
}

impl Rank {
    fn of(best: &[Option<Kind>], score: Score) -> Rank {
        Rank {
            exact: best.iter().filter(|k| matches!(k, Some(Kind::Exact | Kind::Prefix))).count(),
            matched: best.iter().filter(|k| k.is_some()).count(),
            prefix: best.iter().filter(|k| **k == Some(Kind::Prefix)).count(),
            score,
        }
    }

    /// Better ranks sort first.
    fn cmp(&self, o: &Rank) -> CmpOrdering {
        o.exact.cmp(&self.exact).then(o.matched.cmp(&self.matched)).then(self.prefix.cmp(&o.prefix)).then(o.score.total_cmp(&self.score))
    }
}

// ---------------------------------------------------------------- index

/// Removes the index folder once everything else in `FuzzyIndex` (the mmaps) is dropped.
struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct FuzzyIndex {
    index: Index,
    reader: IndexReader,
    path: Field,
    body: Field,
    head: Field,
    start: Field,
    end: Field,
    line: Field,
    analyzer: TextAnalyzer,
    /// Indexed files and their modification time.
    indexed: HashMap<String, SystemTime>,
    _dir: DirGuard,
}

const DIR_PREFIX: &str = "mdelight-fuzzy-";

fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

impl FuzzyIndex {
    fn create() -> Result<FuzzyIndex, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::temp_dir();
        // Leftovers from runs that crashed; other running instances keep theirs.
        if let Ok(rd) = std::fs::read_dir(&tmp) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let pid = name.strip_prefix(DIR_PREFIX).and_then(|r| r.split('-').next()).and_then(|p| p.parse::<u32>().ok());
                if pid.is_some_and(|p| p != std::process::id() && !process_alive(p)) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let dir = tmp.join(format!("{DIR_PREFIX}{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let guard = DirGuard(dir.clone());

        let text = TextOptions::default().set_indexing_options(TextFieldIndexing::default().set_tokenizer(TOKENIZER).set_index_option(IndexRecordOption::WithFreqs));
        let mut sb = Schema::builder();
        let path = sb.add_text_field("path", STRING | STORED);
        let body = sb.add_text_field("body", text.clone());
        let head = sb.add_text_field("head", text);
        let start = sb.add_u64_field("start", STORED);
        let end = sb.add_u64_field("end", STORED);
        let line = sb.add_u64_field("line", STORED);
        let index = Index::create_in_dir(&dir, sb.build()).map_err(|e| e.to_string())?;
        index.tokenizers().register(TOKENIZER, analyzer());
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().map_err(|e| e.to_string())?;
        Ok(FuzzyIndex { index, reader, path, body, head, start, end, line, analyzer: analyzer(), indexed: HashMap::new(), _dir: guard })
    }

    /// Bring the index in line with `files` on disk: (re)index new and modified markdown files,
    /// drop removed ones.
    fn sync(&mut self, root: &Path, files: &[String]) -> Result<(), String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut changed = Vec::new();
        for f in files.iter().filter(|f| crate::app::is_markdown(f)) {
            let Ok(meta) = std::fs::metadata(root.join(f)) else { continue };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            seen.insert(f.as_str());
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if self.indexed.get(f) != Some(&mtime) {
                changed.push((f.clone(), mtime));
            }
        }
        let removed: Vec<String> = self.indexed.keys().filter(|f| !seen.contains::<str>(f)).cloned().collect();
        if changed.is_empty() && removed.is_empty() {
            return Ok(());
        }

        let mut writer = self.index.writer_with_num_threads::<TantivyDocument>(1, WRITER_BYTES).map_err(|e| e.to_string())?;
        for f in &removed {
            writer.delete_term(Term::from_field_text(self.path, f));
            self.indexed.remove(f);
        }
        for (f, mtime) in changed {
            writer.delete_term(Term::from_field_text(self.path, &f));
            let Ok(bytes) = std::fs::read(root.join(&f)) else {
                self.indexed.remove(&f);
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for s in sections(&text) {
                let mut doc = TantivyDocument::default();
                doc.add_text(self.path, &f);
                doc.add_text(self.body, &text[s.start..s.end]);
                doc.add_text(self.head, &s.heading);
                doc.add_u64(self.start, s.start as u64);
                doc.add_u64(self.end, s.end as u64);
                doc.add_u64(self.line, s.line as u64);
                writer.add_document(doc).map_err(|e| e.to_string())?;
            }
            self.indexed.insert(f, mtime);
        }
        writer.commit().map_err(|e| e.to_string())?;
        writer.wait_merging_threads().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())
    }

    fn query_terms(&mut self, query: &str) -> Vec<QTerm> {
        // The word under the cursor is still being typed: let it match as a prefix.
        let typing = !query.ends_with(char::is_whitespace);
        let mut seen = HashSet::new();
        let words: Vec<String> = tokens(&mut self.analyzer, query).into_iter().map(|t| t.0).filter(|w| seen.insert(w.clone())).collect();
        let n = words.len();
        words
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let chars: Vec<char> = text.chars().collect();
                QTerm { dist: max_dist(chars.len()), prefix: typing && i + 1 == n && chars.len() >= 2, text, chars }
            })
            .collect()
    }

    /// Exact query (`fuzzy` false) or near-match query over body and heading.
    fn build_query(&self, terms: &[QTerm], fuzzy: bool) -> BooleanQuery {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for q in terms {
            for (field, boost) in [(self.body, 1.0), (self.head, HEADING_BOOST)] {
                let term = Term::from_field_text(field, &q.text);
                let sub: Box<dyn Query> = match (fuzzy, q.prefix) {
                    (false, false) => Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)),
                    (false, true) => Box::new(FuzzyTermQuery::new_prefix(term, 0, true)),
                    (true, _) if q.dist == 0 => continue,
                    (true, false) => Box::new(FuzzyTermQuery::new(term, q.dist, true)),
                    (true, true) => Box::new(FuzzyTermQuery::new_prefix(term, q.dist, true)),
                };
                clauses.push((Occur::Should, Box::new(BoostQuery::new(sub, boost))));
            }
        }
        BooleanQuery::new(clauses)
    }

    /// Ranked results, one entry per file, best file first; lines within a file by best match
    /// kind, then position. `cancelled` is polled between files.
    fn search(&mut self, root: &Path, query: &str, cancelled: &dyn Fn() -> bool) -> Result<(Vec<(String, Vec<LineHit>)>, bool), String> {
        let terms = self.query_terms(query);
        if terms.is_empty() {
            return Ok((Vec::new(), false));
        }
        let searcher = self.reader.searcher();
        let mut cands: HashMap<DocAddress, Score> = HashMap::new();
        for fuzzy in [false, true] {
            let q = self.build_query(&terms, fuzzy);
            let top = searcher.search(&q, &TopDocs::with_limit(CANDIDATES).order_by_score()).map_err(|e| e.to_string())?;
            for (score, addr) in top {
                cands.entry(addr).or_insert(score);
            }
        }

        // Group candidate sections by file so each file is read once.
        let mut by_file: HashMap<String, Vec<(usize, usize, usize, Score)>> = HashMap::new();
        for (addr, score) in cands {
            let doc: TantivyDocument = searcher.doc(addr).map_err(|e| e.to_string())?;
            let u = |f: Field| doc.get_first(f).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let Some(path) = doc.get_first(self.path).and_then(|v| v.as_str()).map(str::to_string) else { continue };
            by_file.entry(path).or_default().push((u(self.start), u(self.end), u(self.line), score));
        }

        let mut files: Vec<(Rank, String, Vec<LineHit>)> = Vec::new();
        for (path, secs) in by_file {
            if cancelled() {
                return Ok((Vec::new(), false));
            }
            let Ok(bytes) = std::fs::read(root.join(&path)) else { continue };
            let text = String::from_utf8_lossy(&bytes);
            let mut best: Option<Rank> = None;
            let mut lines: Vec<(Kind, LineHit)> = Vec::new();
            for (start, end, line0, score) in secs {
                let Some(sec) = text.get(start..end) else { continue }; // changed since indexed
                let mut sec_best = vec![None; terms.len()];
                for (i, l) in sec.split_inclusive('\n').enumerate() {
                    let mut found = Vec::new();
                    let mut line_best: Option<Kind> = None;
                    for (tok, a, b) in tokens(&mut self.analyzer, l) {
                        let mut hit = None;
                        for (qi, q) in terms.iter().enumerate() {
                            if let Some(k) = classify(&tok, q) {
                                sec_best[qi] = Some(sec_best[qi].map_or(k, |b: Kind| b.min(k)));
                                hit = Some(hit.map_or(k, |h: Kind| h.min(k)));
                            }
                        }
                        if let Some(k) = hit {
                            found.push((a, b));
                            line_best = Some(line_best.map_or(k, |b| b.min(k)));
                        }
                    }
                    if let Some(k) = line_best {
                        lines.push((k, make_hit((line0 + i + 1) as u64, l, &found)));
                    }
                }
                let r = Rank::of(&sec_best, score);
                if r.matched > 0 && best.is_none_or(|b| r.cmp(&b).is_lt()) {
                    best = Some(r);
                }
            }
            let Some(best) = best else { continue };
            lines.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.line.cmp(&b.1.line)));
            lines.truncate(MAX_FILE_HITS);
            files.push((best, path, lines.into_iter().map(|l| l.1).collect()));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let mut total = 0;
        let mut truncated = false;
        let mut out = Vec::new();
        for (_, path, hits) in files {
            if total >= MAX_HITS {
                truncated = true;
                break;
            }
            total += hits.len();
            out.push((path, hits));
        }
        Ok((out, truncated))
    }
}

/// Run a fuzzy search (building or refreshing the index first if needed) and stream the
/// results, best file first, to `sink`.
pub fn run(
    slot: &std::sync::Mutex<Option<FuzzyIndex>>,
    dirty: &std::sync::atomic::AtomicBool,
    root: &Path,
    files: &[String],
    query: &str,
    gen: u64,
    current: &dyn Fn() -> u64,
    sink: &dyn Fn(SearchEvent),
) {
    let t0 = Instant::now();
    let mut slot = slot.lock().unwrap();
    if current() != gen {
        return;
    }
    let result = (|| {
        if slot.is_none() {
            *slot = Some(FuzzyIndex::create()?);
            dirty.store(true, Ordering::SeqCst);
        }
        let ix = slot.as_mut().unwrap();
        if dirty.swap(false, Ordering::SeqCst) {
            if let Err(e) = ix.sync(root, files) {
                dirty.store(true, Ordering::SeqCst);
                return Err(e);
            }
        }
        ix.search(root, query, &|| current() != gen)
    })();
    let (results, truncated) = match result {
        Ok(r) => r,
        Err(e) => {
            // A broken index (e.g. temp folder removed): start over on the next search.
            *slot = None;
            if current() == gen {
                sink(SearchEvent::Failed { gen, error: format!("Fuzzy search failed: {e}") });
            }
            return;
        }
    };
    drop(slot);
    let (mut nfiles, mut nhits) = (0, 0);
    for (path, hits) in results {
        if current() != gen {
            return;
        }
        nfiles += 1;
        nhits += hits.len();
        sink(SearchEvent::File { gen, path, hits });
    }
    if current() == gen {
        sink(SearchEvent::Done { gen, files: nfiles, hits: nhits, ms: t0.elapsed().as_millis(), truncated });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qt(text: &str, prefix: bool) -> QTerm {
        let chars: Vec<char> = text.chars().collect();
        QTerm { dist: max_dist(chars.len()), prefix, text: text.into(), chars }
    }

    #[test]
    fn folds_accents_and_ligatures() {
        let mut a = analyzer();
        let t: Vec<String> = tokens(&mut a, "Métier, CŒUR straße").into_iter().map(|t| t.0).collect();
        assert_eq!(t, ["metier", "coeur", "strasse"]);
        // Offsets point into the original text, accents included.
        let (_, s, e) = tokens(&mut a, "le Métier").remove(1);
        assert_eq!(&"le Métier"[s..e], "Métier");
    }

    #[test]
    fn classifies_matches() {
        assert_eq!(classify("metier", &qt("metier", false)), Some(Kind::Exact));
        assert_eq!(classify("metiers", &qt("metier", true)), Some(Kind::Prefix));
        assert_eq!(classify("metier", &qt("meiter", false)), Some(Kind::Fuzzy)); // swap
        assert_eq!(classify("search", &qt("serach", false)), Some(Kind::Fuzzy));
        assert_eq!(classify("cat", &qt("car", false)), None); // short: no typos
        assert_eq!(classify("documentation", &qt("documnt", true)), Some(Kind::Fuzzy));
    }

    #[test]
    fn exact_outranks_fuzzy_whatever_the_score() {
        let exact = Rank::of(&[Some(Kind::Exact)], 0.1);
        let prefix = Rank::of(&[Some(Kind::Prefix)], 50.0);
        let fuzzy = Rank::of(&[Some(Kind::Fuzzy)], 99.0);
        assert!(exact.cmp(&prefix).is_lt());
        assert!(prefix.cmp(&fuzzy).is_lt());
        let two_fuzzy = Rank::of(&[Some(Kind::Fuzzy), Some(Kind::Fuzzy)], 99.0);
        let one_exact = Rank::of(&[Some(Kind::Exact), None], 0.1);
        assert!(one_exact.cmp(&two_fuzzy).is_lt());
    }

    #[test]
    fn splits_at_headings_outside_code() {
        let text = "intro\n# One\na\n```\n# not a heading\n```\n## Two\nb\n#hashtag\n";
        let s = sections(text);
        assert_eq!(s.iter().map(|s| (s.line, s.heading.as_str())).collect::<Vec<_>>(), [(0, ""), (1, "One"), (6, "Two")]);
        assert_eq!(s.last().unwrap().end, text.len());
    }

    #[test]
    fn index_ranks_exact_first() {
        let root = std::env::temp_dir().join(format!("mdelight-fuzzytest-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.md"), "# Notes\nLe métiers du conseil, métiers, métiers.\n").unwrap();
        std::fs::write(root.join("b.md"), "# Other\nUn Métier.\nUn long paragraphe sans rapport pour allonger la section.\n").unwrap();
        std::fs::write(root.join("c.md"), "# Typo\nMetir\n").unwrap();
        let files = vec!["a.md".to_string(), "b.md".to_string(), "c.md".to_string()];
        let mut ix = FuzzyIndex::create().unwrap();
        ix.sync(&root, &files).unwrap();

        // Still typing "metier": exact (accent-folded) first, then the plural (prefix), then the typo.
        let (res, _) = ix.search(&root, "metier", &|| false).unwrap();
        let order: Vec<&str> = res.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(order, ["b.md", "a.md", "c.md"]);
        let h = &res[0].1[0];
        let (s, e) = h.ranges[0];
        assert_eq!(&h.text[s..e], "Métier");
        assert_eq!(h.line, 2);

        // Removing a file drops it from the index.
        ix.sync(&root, &files[..2]).unwrap();
        let (res, _) = ix.search(&root, "metier ", &|| false).unwrap();
        assert!(res.iter().all(|r| r.0 != "c.md"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
