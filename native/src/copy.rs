//! Copy as rich text: HTML fragments (from the editor's Markdown or the preview's selection)
//! go on the clipboard as CF_HTML with inline styles, next to a plain-text version.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

use crate::win::wide;

const MONO: &str = "Consolas,'Courier New',monospace";
const BORDER: &str = "#d1d9e0";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

macro_rules! re {
    ($pat:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        re(&R, $pat)
    }};
}

/// Put `fragment` (HTML) and `text` on the clipboard.
pub fn set_rich(hwnd: HWND, fragment: &str, text: &str) {
    let html = cf_html(&inline_styles(fragment));
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return;
        }
        EmptyClipboard();
        let fmt = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
        let mut html = html.into_bytes();
        html.push(0);
        put(fmt, &html);
        put(CF_UNICODETEXT as u32, std::slice::from_raw_parts(text.as_ptr() as *const u8, text.len() * 2));
        CloseClipboard();
    }
}

unsafe fn put(fmt: u32, bytes: &[u8]) {
    let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
    if mem.is_null() {
        return;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), GlobalLock(mem) as *mut u8, bytes.len());
    GlobalUnlock(mem);
    if SetClipboardData(fmt, mem).is_null() {
        GlobalFree(mem);
    }
}

/// Wrap a fragment in the CF_HTML envelope: a header of byte offsets, then the document.
fn cf_html(fragment: &str) -> String {
    const HEAD: &str = "Version:0.9\r\nStartHTML:{0000000000}\r\nEndHTML:{1111111111}\r\nStartFragment:{2222222222}\r\nEndFragment:{3333333333}\r\n";
    let pre = "<html><head><meta charset=\"utf-8\"></head><body><!--StartFragment-->";
    let post = "<!--EndFragment--></body></html>";
    let start_html = HEAD.len() - 4 * 2; // each placeholder is 12 chars wide, the number 10
    let start_frag = start_html + pre.len();
    let end_frag = start_frag + fragment.len();
    let end_html = end_frag + post.len();
    let head = HEAD
        .replace("{0000000000}", &format!("{start_html:010}"))
        .replace("{1111111111}", &format!("{end_html:010}"))
        .replace("{2222222222}", &format!("{start_frag:010}"))
        .replace("{3333333333}", &format!("{end_frag:010}"));
    format!("{head}{pre}{fragment}{post}")
}

/// Mail clients and Word ignore stylesheets: give the elements the preview styles inline.
pub fn inline_styles(html: &str) -> String {
    let style_of = |tag: &str| -> Option<String> {
        Some(match tag {
            "pre" => format!("background:#f6f8fa;padding:12px 16px;border-radius:6px;font-family:{MONO};font-size:13px;line-height:1.45;white-space:pre-wrap"),
            "code" => format!("background:#eff1f3;padding:1px 4px;border-radius:4px;font-family:{MONO};font-size:0.9em"),
            "blockquote" => format!("margin:0 0 1em;padding:0 1em;color:#59636e;border-left:4px solid {BORDER}"),
            "table" => "border-collapse:collapse".into(),
            "th" => format!("border:1px solid {BORDER};padding:5px 12px;font-weight:600"),
            "td" => format!("border:1px solid {BORDER};padding:5px 12px"),
            "hr" => format!("border:0;border-top:2px solid {BORDER}"),
            "img" => "max-width:100%".into(),
            _ => return None,
        })
    };
    let out = re!(r"<(pre|code|blockquote|table|th|td|hr|img)\b([^>]*)>").replace_all(html, |c: &regex::Captures| {
        let attrs = &c[2];
        match style_of(&c[1]) {
            Some(s) if !attrs.contains("style=") => format!("<{}{} style=\"{s}\">", &c[1], attrs.trim_end_matches('/').trim_end()),
            _ => c[0].to_string(),
        }
    });
    // Code inside a block keeps only the font: the <pre> already draws the box.
    re!(r#"(<pre\b[^>]*>)<code\b[^>]*>"#).replace_all(&out, format!("$1<code style=\"font-family:{MONO}\">")).into_owned()
}

/// Markdown → HTML for other apps: light colors, no preview-only attributes, local images
/// as file:// URLs.
pub fn markdown_html(md: &str, doc_dir: &Path) -> String {
    let html = crate::markdown::render(md, false);
    let html = re!(r#" (data-sourcepos|class)="[^"]*""#).replace_all(&html, "");
    re!(r#"(<img\b[^>]*\bsrc=")([^"]*)""#)
        .replace_all(&html, |c: &regex::Captures| format!("{}{}\"", &c[1], file_url(&c[2], doc_dir)))
        .into_owned()
}

fn file_url(src: &str, dir: &Path) -> String {
    if src.is_empty() || re!(r"^([a-zA-Z][a-zA-Z0-9+.-]*:|//)").is_match(src) {
        return src.to_string();
    }
    let p = dir.join(src.replace('/', "\\"));
    format!("file:///{}", p.to_string_lossy().replace('\\', "/").replace(' ', "%20"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_html_offsets_point_at_fragment() {
        let s = cf_html("<p>é</p>");
        let num = |k: &str| s.lines().find_map(|l| l.strip_prefix(k)).unwrap().parse::<usize>().unwrap();
        let b = s.as_bytes();
        assert_eq!(&b[num("StartFragment:")..num("EndFragment:")], "<p>é</p>".as_bytes());
        assert!(s[num("StartHTML:")..].starts_with("<html>"));
        assert_eq!(num("EndHTML:"), b.len());
    }

    #[test]
    fn styles_inline_without_doubling() {
        let out = inline_styles("<pre><code class=\"language-rs\"><span style=\"color:#f00\">x</span></code></pre><p><code>y</code></p><img src=\"a.png\" />");
        assert!(out.starts_with("<pre style=\"background:#f6f8fa"), "{out}");
        assert!(out.contains("<code style=\"font-family:Consolas,'Courier New',monospace\"><span style=\"color:#f00\">"), "{out}");
        assert!(out.contains("<p><code style=\"background:#eff1f3"), "{out}");
        assert!(out.contains("<img src=\"a.png\" style=\"max-width:100%\">"), "{out}");
    }

    #[test]
    fn markdown_export() {
        let out = markdown_html("# T\n\n![x](img/a.png)\n\n- [x] done\n", Path::new("C:\\my docs"));
        assert!(!out.contains("data-sourcepos") && !out.contains("class="), "{out}");
        assert!(out.contains("src=\"file:///C:/my%20docs/img/a.png\""), "{out}");
    }
}
