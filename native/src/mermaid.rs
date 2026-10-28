//! Mermaid diagrams, on demand: litehtml can't run mermaid.js, so a diagram opens as a small
//! HTML page in the default browser. mermaid.js is embedded and written once to %TEMP%.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const JS: &[u8] = include_bytes!("../vendor/mermaid/mermaid.min.js");
const JS_NAME: &str = "mermaid-12.2.0.min.js";

/// Write the page for `code` and return its path.
pub fn write_page(code: &str, dark: bool) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("mdelight-mermaid");
    std::fs::create_dir_all(&dir)?;
    let js = dir.join(JS_NAME);
    if std::fs::metadata(&js).map(|m| m.len()).ok() != Some(JS.len() as u64) {
        std::fs::write(&js, JS)?;
    }
    prune(&dir);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (code, dark).hash(&mut h);
    let page = dir.join(format!("diagram-{:016x}.html", h.finish()));
    std::fs::write(&page, page_html(code, dark))?;
    Ok(page)
}

/// Pages from earlier days are no longer open anywhere that matters.
fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let cutoff = SystemTime::now() - Duration::from_secs(24 * 3600);
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff);
        if old && e.path().extension().is_some_and(|x| x == "html") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn page_html(code: &str, dark: bool) -> String {
    let (bg, fg) = if dark { ("#1e1f22", "#d4d6da") } else { ("#ffffff", "#1f2328") };
    let theme = if dark { "dark" } else { "default" };
    let code = code.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Mermaid diagram</title>
<style>
html, body {{ margin: 0; background: {bg}; color: {fg}; font-family: "Segoe UI", sans-serif; }}
body {{ display: flex; justify-content: center; padding: 32px; box-sizing: border-box; min-height: 100vh; }}
.mermaid svg {{ max-width: 100%; height: auto; }}
pre.error {{ white-space: pre-wrap; color: #d1242f; }}
</style></head>
<body><pre class="mermaid">{code}</pre>
<script src="{JS_NAME}"></script>
<script>
mermaid.initialize({{ startOnLoad: false, securityLevel: "strict", theme: "{theme}" }});
mermaid.run().catch(e => {{
  const el = document.querySelector(".mermaid");
  el.className = "error";
  el.textContent = String(e && e.message || e);
}});
</script></body></html>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_escapes_source_and_picks_theme() {
        let html = page_html("graph TD\n  A-->B<br/>&", true);
        assert!(html.contains("A--&gt;B&lt;br/&gt;&amp;"), "{html}");
        assert!(html.contains(r#"theme: "dark""#));
        assert!(html.contains(JS_NAME));
    }

    #[test]
    fn page_sits_next_to_mermaid_js() {
        let page = write_page("graph TD\n  A-->B\n", false).unwrap();
        let js = page.with_file_name(JS_NAME);
        assert_eq!(std::fs::metadata(js).unwrap().len(), JS.len() as u64);
        println!("{}", page.display());
    }
}
