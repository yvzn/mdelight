//! Smart paste: rich clipboard content (HTML from browsers, Word, Outlook, Google Docs...)
//! becomes Markdown, and a bare clipboard image is saved as a PNG next to the document.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_BITFIELDS, BI_RGB};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

use crate::win::wide;

/// What the clipboard holds, in the form smart paste cares about.
pub enum Clip {
    Markdown(String),
    Image(Vec<u8>),
    /// Nothing special: let the editor paste the plain text itself.
    Plain,
}

pub fn read(hwnd: HWND) -> Clip {
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return Clip::Plain;
        }
        let clip = read_open();
        CloseClipboard();
        clip
    }
}

unsafe fn read_open() -> Clip {
    let html_fmt = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
    if let Some(md) = data(html_fmt).and_then(|b| cf_html_to_markdown(&b)) {
        return Clip::Markdown(md);
    }
    // Images only when there is no text: Word and Excel put a picture of the text there too.
    if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) != 0 {
        return Clip::Plain;
    }
    let png_fmt = RegisterClipboardFormatW(wide("PNG").as_ptr());
    if let Some(png) = data(png_fmt).filter(|b| b.starts_with(b"\x89PNG")) {
        return Clip::Image(png);
    }
    match data(CF_DIB as u32).and_then(|b| dib_to_png(&b)) {
        Some(png) => Clip::Image(png),
        None => Clip::Plain,
    }
}

unsafe fn data(fmt: u32) -> Option<Vec<u8>> {
    if fmt == 0 || IsClipboardFormatAvailable(fmt) == 0 {
        return None;
    }
    let h = GetClipboardData(fmt);
    if h.is_null() {
        return None;
    }
    let p = GlobalLock(h) as *const u8;
    if p.is_null() {
        return None;
    }
    let v = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
    GlobalUnlock(h);
    Some(v)
}

/// A free `images/pasted-<stamp>.png` path next to `dir`, and its Markdown-relative form.
pub fn image_target(dir: &Path) -> (PathBuf, String) {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut t = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    let stamp = format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("pasted-{stamp}.png") } else { format!("pasted-{stamp}-{n}.png") };
        let p = dir.join("images").join(&name);
        if !p.exists() {
            return (p, format!("images/{name}"));
        }
        n += 1;
    }
}

// ---------------------------------------------------------------- HTML

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

macro_rules! re {
    ($pat:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        re(&R, $pat)
    }};
}

/// Pull the fragment and source URL out of a CF_HTML payload (UTF-8 with a `Key:value` header).
fn cf_html_parts(raw: &[u8]) -> Option<(String, Option<String>)> {
    let raw = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
    let s = String::from_utf8_lossy(raw);
    let header = |key: &str| -> Option<String> {
        s.lines().take(16).find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
    };
    let off = |key: &str| header(key).and_then(|v| v.parse::<usize>().ok());
    let source = header("SourceURL:").filter(|u| u.starts_with("http"));
    let frag = match (off("StartFragment:"), off("EndFragment:")) {
        (Some(a), Some(b)) if a < b && b <= raw.len() => String::from_utf8_lossy(&raw[a..b]).into_owned(),
        _ => {
            let a = s.find("<!--StartFragment-->")? + "<!--StartFragment-->".len();
            let b = s.find("<!--EndFragment-->")?;
            s.get(a..b)?.to_string()
        }
    };
    Some((frag, source))
}

pub fn cf_html_to_markdown(raw: &[u8]) -> Option<String> {
    let (frag, source) = cf_html_parts(raw)?;
    html_to_markdown(&frag, source.as_deref())
}

/// Convert an HTML fragment; None when it carries no structure worth converting (code
/// editors and terminals copy colored spans only), so the plain text is pasted instead.
pub fn html_to_markdown(frag: &str, base: Option<&str>) -> Option<String> {
    let mut h = clean(frag);
    if !re!(r"(?i)<(p|h[1-6]|ul|ol|li|a|strong|b|em|i|u|s|del|strike|table|img|pre|code|blockquote|hr)\b").is_match(&h) {
        return None;
    }
    h = word_lists(&h);
    h = styled_spans(&h);
    if let Some(b) = base {
        h = absolute_urls(&h, b);
    }
    let opts = htmd::options::Options {
        bullet_list_marker: htmd::options::BulletListMarker::Dash,
        ul_bullet_spacing: 1,
        ol_number_spacing: 1,
        hr_style: htmd::options::HrStyle::Dashes,
        br_style: htmd::options::BrStyle::Backslash,
        ..Default::default()
    };
    let conv = htmd::HtmlToMarkdown::builder().options(opts).skip_tags(vec!["script", "style", "head", "meta", "link", "title"]).build();
    let md = conv.convert(&h).ok()?;
    let md = md.replace('\u{a0}', " ");
    let md = re!(r"[ \t]+\n").replace_all(&md, "\n");
    let md = re!(r"\n{3,}").replace_all(&md, "\n\n");
    let md = md.trim_matches('\n').to_string();
    (!md.trim().is_empty()).then_some(md)
}

/// Drop what never converts: comments (and Word's conditional ones), style blocks, Office
/// namespaced tags, inline data: images, and the Google Docs bold wrapper around everything.
fn clean(h: &str) -> String {
    let h = re!(r"(?s)<!--.*?-->").replace_all(h, "");
    let h = re!(r"(?is)<(style|script|xml)\b.*?</(style|script|xml)>").replace_all(&h, "");
    let h = re!(r"(?i)<!\[(if [^\]]*|endif)\]>").replace_all(&h, "");
    let h = re!(r"(?i)</?[a-z]+:[a-z]+\b[^>]*>").replace_all(&h, "");
    let h = re!(r#"(?i)<img\b[^>]*\bsrc\s*=\s*["']?data:[^>]*>"#).replace_all(&h, "");
    let mut h = h.into_owned();
    if let Some(m) = re!(r#"(?i)<b\b[^>]*\bid\s*=\s*["']?docs-internal-guid[^>]*>"#).find(&h) {
        let (start, end) = (m.start(), m.end());
        if let Some(close) = h.rfind("</b>").filter(|&c| c > end) {
            h.replace_range(close..close + 4, "");
        }
        h.replace_range(start..end, "");
    }
    h
}

/// Google Docs and others style text-only spans instead of using tags: map bold, italic,
/// strikethrough and monospace fonts back to elements.
fn styled_spans(h: &str) -> String {
    re!(r#"(?is)<span\b[^>]*\bstyle\s*=\s*(?:"([^"]*)"|'([^']*)')[^>]*>([^<]*)</span>"#)
        .replace_all(h, |c: &regex::Captures| {
            let style = c.get(1).or(c.get(2)).map_or("", |m| m.as_str()).to_ascii_lowercase().replace(' ', "");
            let text = &c[3];
            if text.trim().is_empty() {
                return text.to_string();
            }
            let mut out = text.to_string();
            if re!(r"font-family:[^;]*(courier|consolas|mono|menlo)").is_match(&style) {
                out = format!("<code>{out}</code>");
            }
            if re!(r"text-decoration[^;]*line-through").is_match(&style) {
                out = format!("<del>{out}</del>");
            }
            if re!(r"font-style:italic").is_match(&style) {
                out = format!("<em>{out}</em>");
            }
            if re!(r"font-weight:(bold|[6-9]00)").is_match(&style) {
                out = format!("<strong>{out}</strong>");
            }
            out
        })
        .into_owned()
}

/// Word and Outlook write lists as `<p class=MsoListParagraph...>` with the bullet in a
/// `mso-list:Ignore` span and the depth in `mso-list:lN levelN`. Rebuild real nested lists.
fn word_lists(h: &str) -> String {
    let para = re!(r#"(?is)<p\b([^>]*class=["']?MsoListParagraph[^>]*)>(.*?)</p>"#);
    if !para.is_match(h) {
        return h.to_string();
    }
    let mut out = String::new();
    let mut last = 0;
    // Open lists, innermost last, as closing tags.
    let mut open: Vec<&str> = Vec::new();
    for c in para.captures_iter(h) {
        let m = c.get(0).unwrap();
        let between = &h[last..m.start()];
        if !between.trim().is_empty() {
            while let Some(t) = open.pop() {
                out.push_str(t);
            }
        }
        out.push_str(between);
        let level = re!(r"(?i)level(\d)").captures(&c[1]).and_then(|l| l[1].parse::<usize>().ok()).unwrap_or(1).max(1);
        let body = &c[2];
        let (bullet, text) = match re!(r#"(?is)<span\b[^>]*mso-list:\s*ignore[^>]*>(.*?)</span>\s*(?:</span>)?"#).captures(body) {
            Some(b) => (strip_tags(&b[1]), body.replacen(&b[0], "", 1)),
            None => (String::new(), body.to_string()),
        };
        let ordered = re!(r"^\s*[0-9a-zA-Z]{1,3}[.)]").is_match(&bullet);
        while open.len() > level {
            out.push_str(open.pop().unwrap());
        }
        // The previous item at this level stays open until a sibling comes, so sublists nest in it.
        if open.len() == level {
            out.push_str("</li>");
        }
        while open.len() < level {
            out.push_str(if ordered { "<ol>" } else { "<ul>" });
            open.push(if ordered { "</li></ol>" } else { "</li></ul>" });
        }
        out.push_str("<li>");
        out.push_str(&text);
        last = m.end();
    }
    while let Some(t) = open.pop() {
        out.push_str(t);
    }
    out.push_str(&h[last..]);
    out
}

fn strip_tags(s: &str) -> String {
    re!(r"<[^>]*>").replace_all(s, "").replace("&nbsp;", " ")
}

/// Make relative `href`/`src` absolute against the page they were copied from.
fn absolute_urls(h: &str, base: &str) -> String {
    re!(r#"(?i)\b(href|src)\s*=\s*(["'])([^"']*)(["'])"#)
        .replace_all(h, |c: &regex::Captures| format!("{}={}{}{}", &c[1], &c[2], join_url(base, &c[3]), &c[4]))
        .into_owned()
}

fn join_url(base: &str, url: &str) -> String {
    if url.is_empty() || url.starts_with('#') || re!(r"^[a-zA-Z][a-zA-Z0-9+.-]*:").is_match(url) {
        return url.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let origin_end = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if let Some(rest) = url.strip_prefix("//") {
        return format!("{}{rest}", &base[..scheme_end]);
    }
    if url.starts_with('/') {
        return format!("{}{url}", &base[..origin_end]);
    }
    let path = &base[..base.find(['?', '#']).unwrap_or(base.len())];
    let dir = &path[..path.rfind('/').filter(|&i| i >= origin_end).map_or(path.len(), |i| i + 1)];
    let dir = if dir.len() == origin_end { format!("{dir}/") } else { dir.to_string() };
    format!("{dir}{url}")
}

// ---------------------------------------------------------------- images

/// CF_DIB (BITMAPINFOHEADER + pixels, 24 or 32 bit) to PNG bytes.
fn dib_to_png(b: &[u8]) -> Option<Vec<u8>> {
    let hs = std::mem::size_of::<BITMAPINFOHEADER>();
    if b.len() < hs {
        return None;
    }
    let hdr: BITMAPINFOHEADER = unsafe { std::ptr::read_unaligned(b.as_ptr() as *const _) };
    let (w, h) = (hdr.biWidth as i64, hdr.biHeight as i64);
    let bpp = hdr.biBitCount as usize;
    if w <= 0 || h == 0 || !(bpp == 24 || bpp == 32) || !(hdr.biCompression == BI_RGB || hdr.biCompression == BI_BITFIELDS) {
        return None;
    }
    let (w, rows) = (w as usize, h.unsigned_abs() as usize);
    let masks = if hdr.biCompression == BI_BITFIELDS && hdr.biSize as usize == hs { 12 } else { 0 };
    let start = hdr.biSize as usize + masks + hdr.biClrUsed as usize * 4;
    let stride = (w * bpp / 8 + 3) & !3;
    let px = b.get(start..start + stride * rows)?;
    let has_alpha = bpp == 32 && px.chunks(stride).any(|r| r[..w * 4].chunks(4).any(|p| p[3] != 0));
    let mut rgba = Vec::with_capacity(w * rows * 4);
    for y in 0..rows {
        let row = if h > 0 { rows - 1 - y } else { y };
        let r = &px[row * stride..];
        for x in 0..w {
            let p = &r[x * bpp / 8..];
            rgba.extend_from_slice(&[p[2], p[1], p[0], if has_alpha { p[3] } else { 255 }]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, rows as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&rgba).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(h: &str) -> String {
        html_to_markdown(h, Some("https://example.com/docs/page.html?x=1")).unwrap_or_else(|| "<plain>".into())
    }

    #[test]
    fn converts_basic_structure() {
        let out = md("<h2>Title</h2><p>Some <b>bold</b> and <i>it</i> <a href=\"/a\">link</a>.</p><ul><li>one</li><li>two<ul><li>deep</li></ul></li></ul>");
        assert!(out.starts_with("## Title\n\nSome **bold** and *it* [link](https://example.com/a)."), "{out}");
        assert!(out.contains("- one\n- two\n  - deep"), "{out}");
    }

    #[test]
    fn styled_spans_only_is_plain() {
        assert_eq!(md("<div style=\"font-family:Consolas\"><div><span style=\"color:#f00\">let x = a * b;</span></div></div>"), "<plain>");
    }

    #[test]
    fn google_docs_wrapper_and_spans() {
        let out = md("<meta charset=\"utf-8\"><b style=\"font-weight:normal;\" id=\"docs-internal-guid-1\"><p dir=\"ltr\"><span style=\"font-weight:400\">plain </span><span style=\"font-weight:700\">bold</span><span style=\"font-style:italic\"> it</span></p></b>");
        assert_eq!(out, "plain **bold** *it*");
    }

    #[test]
    fn word_list_paragraphs() {
        let h = "<p class=MsoListParagraphCxSpFirst style='mso-list:l0 level1 lfo1'><![if !supportLists]><span style='mso-list:Ignore'>·<span>&nbsp;&nbsp;</span></span><![endif]>alpha<o:p></o:p></p>\
                 <p class=MsoListParagraphCxSpLast style='mso-list:l0 level2 lfo1'><![if !supportLists]><span style='mso-list:Ignore'>1.<span>&nbsp;</span></span><![endif]>beta<o:p></o:p></p>\
                 <p class=MsoNormal>after</p>";
        let out = md(h);
        assert!(out.starts_with("- alpha\n  1. beta"), "{out}");
        assert!(out.ends_with("after"), "{out}");
    }

    #[test]
    fn table_and_code() {
        let out = md("<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table><pre><code>x &lt; y</code></pre>");
        assert!(out.contains("| a | b |"), "{out}");
        assert!(out.contains("```\nx < y\n```"), "{out}");
    }

    #[test]
    fn cf_html_header() {
        let body = "<html><body><!--StartFragment--><p>Hi <em>there</em></p><!--EndFragment--></body></html>";
        let head = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\nSourceURL:https://x.org/a/b\r\n";
        let a = head.len() + body.find("<p>").unwrap();
        let b = head.len() + body.find("<!--EndFragment").unwrap();
        let head = head.replacen("StartFragment:0000000000", &format!("StartFragment:{a:010}"), 1).replacen("EndFragment:0000000000", &format!("EndFragment:{b:010}"), 1);
        assert_eq!(cf_html_to_markdown(format!("{head}{body}").as_bytes()).unwrap(), "Hi *there*");
    }

    #[test]
    fn joins_urls() {
        let b = "https://example.com/docs/page.html?x=1";
        assert_eq!(join_url(b, "img.png"), "https://example.com/docs/img.png");
        assert_eq!(join_url(b, "/r"), "https://example.com/r");
        assert_eq!(join_url(b, "//cdn.x/y"), "https://cdn.x/y");
        assert_eq!(join_url(b, "mailto:a@b"), "mailto:a@b");
        assert_eq!(join_url("https://example.com", "a"), "https://example.com/a");
    }

    #[test]
    fn dib_roundtrip() {
        let mut b = vec![0u8; 40];
        b[0] = 40;
        b[4] = 2; // width
        b[8] = 1; // height (bottom-up)
        b[12] = 1;
        b[14] = 24;
        b.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // red, green, padding
        let png = dib_to_png(&b).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }
}
