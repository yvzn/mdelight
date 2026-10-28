//! Markdown → HTML for the preview (comrak + syntect), the preview stylesheet, and the outline parser.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{self, Write};
use std::sync::OnceLock;

use comrak::adapters::SyntaxHighlighterAdapter;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::theme::Palette;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme(dark: bool) -> &'static Theme {
    static T: OnceLock<ThemeSet> = OnceLock::new();
    let set = T.get_or_init(ThemeSet::load_defaults);
    &set.themes[if dark { "base16-ocean.dark" } else { "InspiredGitHub" }]
}

fn escape(out: &mut dyn Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '&' => out.write_str("&amp;")?,
            '<' => out.write_str("&lt;")?,
            '>' => out.write_str("&gt;")?,
            '"' => out.write_str("&quot;")?,
            _ => out.write_char(c)?,
        }
    }
    Ok(())
}

fn write_tag(out: &mut dyn Write, tag: &str, attrs: HashMap<&'static str, Cow<'_, str>>) -> fmt::Result {
    write!(out, "<{tag}")?;
    let mut attrs: Vec<_> = attrs.into_iter().collect();
    attrs.sort_by_key(|a| a.0);
    for (k, v) in attrs {
        write!(out, " {k}=\"")?;
        escape(out, &v)?;
        out.write_char('"')?;
    }
    out.write_char('>')
}

/// Inline-styled syntax highlighting (litehtml handles inline styles fine, and the preview is
/// re-rendered on theme changes anyway). Keeps comrak's `data-sourcepos` on `<pre>` for scroll sync.
struct Highlighter {
    dark: bool,
}

impl SyntaxHighlighterAdapter for Highlighter {
    fn write_highlighted(&self, out: &mut dyn Write, lang: Option<&str>, code: &str) -> fmt::Result {
        let lang = lang.unwrap_or("").split([',', ' ']).next().unwrap_or("");
        // syntect's bundled grammars have no TypeScript; JavaScript colors it well enough.
        let lang = match lang.to_lowercase().as_str() {
            "ts" | "tsx" | "typescript" | "jsx" | "mjs" => "js",
            "sh" | "shell" | "zsh" => "bash",
            _ => lang,
        };
        let ss = syntaxes();
        let syntax = (!lang.is_empty()).then(|| ss.find_syntax_by_token(lang)).flatten();
        let Some(syntax) = syntax else { return escape(out, code) };
        let mut h = HighlightLines::new(syntax, theme(self.dark));
        for line in LinesWithEndings::from(code) {
            let Ok(regions) = h.highlight_line(line, ss) else { return escape(out, line) };
            for (style, text) in regions {
                let c = style.foreground;
                write!(out, "<span style=\"color:#{:02x}{:02x}{:02x}", c.r, c.g, c.b)?;
                if style.font_style.contains(FontStyle::BOLD) {
                    out.write_str(";font-weight:bold")?;
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    out.write_str(";font-style:italic")?;
                }
                out.write_str("\">")?;
                escape(out, text)?;
                out.write_str("</span>")?;
            }
        }
        Ok(())
    }

    fn write_pre_tag(&self, out: &mut dyn Write, attrs: HashMap<&'static str, Cow<'_, str>>) -> fmt::Result {
        write_tag(out, "pre", attrs)
    }

    fn write_code_tag(&self, out: &mut dyn Write, attrs: HashMap<&'static str, Cow<'_, str>>) -> fmt::Result {
        write_tag(out, "code", attrs)
    }
}

pub fn render(text: &str, dark: bool) -> String {
    let mut opts = comrak::Options::default();
    opts.extension.strikethrough = true;
    opts.extension.table = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.footnotes = true;
    opts.extension.header_id_prefix = Some(String::new());
    // Raw HTML stays escaped, like the webview build.
    opts.render.r#unsafe = false;
    opts.render.sourcepos = true;
    opts.render.tasklist_classes = true;
    let hl = Highlighter { dark };
    let mut plugins = comrak::options::Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&hl);
    let html = comrak::markdown_to_html_with_plugins(text, &opts, &plugins);
    // litehtml has no form controls: draw task checkboxes as glyphs (U+FE0E: text, not emoji).
    static CHECKBOX: OnceLock<regex::Regex> = OnceLock::new();
    let re = CHECKBOX.get_or_init(|| regex::Regex::new(r#"<input type="checkbox"[^>]*>"#).unwrap());
    let html = re.replace_all(&html, |c: &regex::Captures| {
        if c[0].contains("checked") {
            "<span class=\"task done\">\u{2611}\u{FE0E}</span>"
        } else {
            "<span class=\"task\">\u{2610}\u{FE0E}</span>"
        }
    });
    // A paragraph holding only an image (optionally linked) is a figure: wide and centered.
    static FIGURE: OnceLock<regex::Regex> = OnceLock::new();
    let fig = FIGURE.get_or_init(|| regex::Regex::new(r#"<p( data-sourcepos="[^"]*")?>((?:<a [^>]*>)?<img [^>]*/?>(?:</a>)?)</p>"#).unwrap());
    let html = fig.replace_all(&html, "<p class=\"figure\"$1>$2</p>");
    mark_wide_code(&html)
}

/// Monospace characters that fit the 472px measure at 13px Cascadia Mono, after padding.
const MEASURE_CHARS: usize = 57;

/// Tag `<pre>` blocks with a line longer than the measure as `wide`. litehtml has no
/// min-width for shrink-to-fit blocks, so the stylesheet needs to know which ones grow.
fn mark_wide_code(html: &str) -> String {
    static PRE: OnceLock<regex::Regex> = OnceLock::new();
    static TAG: OnceLock<regex::Regex> = OnceLock::new();
    static ENTITY: OnceLock<regex::Regex> = OnceLock::new();
    let pre = PRE.get_or_init(|| regex::Regex::new(r"(?s)<pre([^>]*)>(.*?)</pre>").unwrap());
    let tag = TAG.get_or_init(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let entity = ENTITY.get_or_init(|| regex::Regex::new(r"&[#a-zA-Z0-9]+;").unwrap());
    pre.replace_all(html, |c: &regex::Captures| {
        let text = tag.replace_all(&c[2], "");
        let text = entity.replace_all(&text, "x");
        let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
        let class = if longest > MEASURE_CHARS { " class=\"wide\"" } else { "" };
        format!("<pre{class}{}>{}</pre>", &c[1], &c[2])
    })
    .into_owned()
}

/// Stylesheet for the preview, after the webview build's `.markdown-body` rules.
pub fn css(p: &Palette) -> String {
    let c = |v: u32| format!("#{v:06x}");
    format!(
        r#"
html, body {{ background: {bg}; color: {fg}; }}
body {{ margin: 0; padding: 24px 24px 0; font-family: "Segoe UI Variable Text", "Segoe UI", sans-serif; font-size: 15px; line-height: 1.6; }}
/* Reading measure: 31.5em of 15px (~68 chars a line, max ~75) for prose, centered. Code, tables
   and figures may grow to the full width, start at the measure, and stay centered. */
.page > * {{ max-width: 472px; margin-left: auto; margin-right: auto; }}
.page > pre {{ display: table; box-sizing: border-box; width: 504px; max-width: 100%; }}
.page > pre.wide {{ width: auto; }}
.page > table {{ max-width: 100%; }}
.page > p.figure {{ max-width: 100%; text-align: center; }}
p, ul, ol, blockquote, table, pre, dl {{ margin-top: 0; margin-bottom: 1em; }}
h1, h2, h3, h4, h5, h6 {{ margin: 1.4em 0 0.6em; font-weight: 600; line-height: 1.25; }}
.page > :first-child {{ margin-top: 0; }}
h1 {{ font-size: 2em; padding-bottom: 0.3em; border-bottom: 1px solid {border}; }}
h2 {{ font-size: 1.5em; padding-bottom: 0.3em; border-bottom: 1px solid {border}; }}
h3 {{ font-size: 1.25em; }}
h4 {{ font-size: 1em; }}
h5 {{ font-size: 0.875em; }}
h6 {{ font-size: 0.85em; color: {dim}; }}
a {{ color: {accent}; text-decoration: none; cursor: pointer; }}
a:hover {{ text-decoration: underline; }}
strong {{ font-weight: 600; }}
code {{ font-family: "Cascadia Mono", Consolas, monospace; font-size: 0.88em; background: {code_bg}; padding: 0.15em 0.35em; border-radius: 4px; }}
pre {{ background: {code_bg}; padding: 12px 16px; border-radius: 6px; line-height: 1.45; white-space: pre-wrap; }}
pre code {{ background: transparent; padding: 0; font-size: 13px; }}
blockquote {{ margin-left: 0; margin-right: 0; padding: 0 1em; color: {dim}; border-left: 4px solid {border_strong}; }}
ul, ol {{ padding-left: 2em; }}
li + li {{ margin-top: 0.25em; }}
li > p {{ margin-bottom: 0.5em; }}
table {{ border-collapse: collapse; }}
th, td {{ border: 1px solid {border_strong}; padding: 5px 12px; }}
th {{ font-weight: 600; white-space: nowrap; }}
tr:nth-child(2n) {{ background: {side}; }}
img {{ max-width: 100%; }}
hr {{ border: 0; height: 2px; background: {border_strong}; margin: 1.5em 0; }}
li.task-list-item {{ list-style-type: none; }}
.task {{ color: {dim}; font-family: "Segoe UI Symbol", "Segoe UI"; margin-left: -1.3em; padding-right: 0.3em; }}
.task.done {{ color: {accent}; }}
del {{ text-decoration: line-through; }}
.footnotes {{ font-size: 0.9em; color: {dim}; border-top: 1px solid {border}; padding-top: 0.5em; }}
sup {{ font-size: 0.75em; vertical-align: super; line-height: 0; }}
"#,
        bg = c(p.bg),
        fg = c(p.fg),
        dim = c(p.fg_dim),
        border = c(p.border),
        border_strong = c(p.border_strong),
        accent = c(p.accent),
        code_bg = c(p.code_bg),
        side = c(p.side),
    )
}

/// Wrap the rendered body so the stylesheet can center a reading column.
pub fn page(html: &str) -> String {
    format!("<html><body><div class=\"page\">{html}</div></body></html>")
}

// ---------------------------------------------------------------- outline

pub struct Heading {
    pub level: u8,
    pub text: String,
    /// 1-based source line.
    pub line: usize,
}

/// Strip inline markdown so headings read as plain text.
fn plain(s: &str) -> String {
    static LINK: OnceLock<regex::Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| regex::Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").unwrap());
    link.replace_all(s, "$1").replace(['*', '_', '`', '~'], "").trim().to_string()
}

/// ATX (`# x`) and setext (`x\n===`) headings, skipping fenced code and YAML front matter.
pub fn headings(doc: &str) -> Vec<Heading> {
    static ATX: OnceLock<regex::Regex> = OnceLock::new();
    static FENCE: OnceLock<regex::Regex> = OnceLock::new();
    static FENCE_CLOSE: OnceLock<regex::Regex> = OnceLock::new();
    static SETEXT: OnceLock<regex::Regex> = OnceLock::new();
    static NOT_PARA: OnceLock<regex::Regex> = OnceLock::new();
    let atx = ATX.get_or_init(|| regex::Regex::new(r"^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$").unwrap());
    let fence_re = FENCE.get_or_init(|| regex::Regex::new(r"^ {0,3}(`{3,}|~{3,})").unwrap());
    let fence_close = FENCE_CLOSE.get_or_init(|| regex::Regex::new(r"^ {0,3}[`~]+\s*$").unwrap());
    let setext = SETEXT.get_or_init(|| regex::Regex::new(r"^ {0,3}(=+|-+)[ \t]*$").unwrap());
    let not_para = NOT_PARA.get_or_init(|| regex::Regex::new(r"^\s*([-*+]\s|\d+[.)]\s|#|>|\|)").unwrap());

    let lines: Vec<&str> = doc.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut out: Vec<Heading> = Vec::new();
    let mut i = 0;
    if lines.first() == Some(&"---") {
        i = lines.iter().skip(1).position(|l| *l == "---" || *l == "...").map(|p| p + 2).unwrap_or(0);
    }
    let mut fence: Option<String> = None;
    while i < lines.len() {
        let l = lines[i];
        let f = fence_re.captures(l).map(|c| c[1].to_string());
        if let Some(open) = &fence {
            if let Some(f) = &f {
                if f.as_bytes()[0] == open.as_bytes()[0] && f.len() >= open.len() && fence_close.is_match(l) {
                    fence = None;
                }
            }
            i += 1;
            continue;
        }
        if f.is_some() {
            fence = f;
            i += 1;
            continue;
        }
        if let Some(c) = atx.captures(l) {
            out.push(Heading { level: c[1].len() as u8, text: plain(c.get(2).map_or("", |m| m.as_str())), line: i + 1 });
            i += 1;
            continue;
        }
        if i > 0 && setext.is_match(l) {
            let prev = lines[i - 1];
            if !prev.trim().is_empty() && !not_para.is_match(prev) && !out.iter().any(|h| h.line == i) {
                let level = if l.trim().starts_with('=') { 1 } else { 2 };
                out.push(Heading { level, text: plain(prev), line: i });
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_sourcepos_and_escapes_html() {
        let html = render("# Hi\n\n<script>x</script>\n\n```rust\nfn x() {}\n```\n\n- [x] done\n- [ ] todo\n", false);
        assert!(html.contains("data-sourcepos=\"1:1"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("<pre data-sourcepos=\"5:1-7:3\""), "{html}");
        assert!(html.contains("<span style=\"color:#"), "{html}");
        assert!(html.contains("\u{2611}") && html.contains("\u{2610}"), "{html}");
    }

    #[test]
    fn outline_skips_code_and_front_matter() {
        let doc = "---\ntitle: x\n---\n# One\n\n```\n# not\n```\nTwo\n---\n## *Three*\n";
        let hs = headings(doc);
        let got: Vec<_> = hs.iter().map(|h| (h.level, h.text.as_str(), h.line)).collect();
        assert_eq!(got, vec![(1, "One", 4), (2, "Two", 9), (2, "Three", 11)]);
    }

    #[test]
    fn long_code_and_lone_images_are_wide() {
        let html = render(&format!("```\nshort\n```\n\n```\n{}\n```\n\n![x](a.png)\n", "y".repeat(80)), false);
        assert_eq!(html.matches("<pre class=\"wide\"").count(), 1, "{html}");
        assert!(html.contains("<p class=\"figure\""), "{html}");
    }

    #[test]
    fn outline_of_sample_intro() {
        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../samples/guide/intro.md")).unwrap();
        let got: Vec<_> = headings(&doc).iter().map(|h| (h.level, h.text.clone())).collect();
        assert_eq!(got, vec![(1, "Intro".into()), (2, "Sequence".into()), (2, "Code".into())]);
    }
}
