//! Syntax colours for fenced code blocks in the editor. The Markdown lexer styles a whole block
//! as one style, so syntect (the preview's grammars and themes) paints over it through a single
//! INDIC_TEXTFORE indicator whose per-run value is the colour. Results are cached per block, so
//! an edit only re-highlights the block it touched.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use syntect::easy::HighlightLines;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::markdown;
use crate::sci::Editor;

/// Blocks bigger than this stay one colour: highlighting them would stall typing.
const MAX_BLOCK: usize = 256 * 1024;

/// Coloured runs of one block: (byte offset from the block start, length, 0xBBGGRR).
type Runs = Vec<(u32, u32, u32)>;

#[derive(Default)]
pub struct CodeColours {
    cache: HashMap<u64, Runs>,
}

impl CodeColours {
    /// Recolour every fenced block, highlighting only those not seen before.
    pub fn paint(&mut self, ed: &Editor, dark: bool) {
        let text = ed.text();
        let mut seen: HashMap<u64, Runs> = HashMap::new();
        ed.clear_code_colours();
        for (start, end, info) in fenced_blocks(&text) {
            if end - start > MAX_BLOCK {
                continue;
            }
            let Some((syntax, ss)) = markdown::find_syntax(info) else { continue };
            let code = &text[start..end];
            let mut h = DefaultHasher::new();
            (syntax.name.as_str(), code, dark).hash(&mut h);
            let key = h.finish();
            let runs = seen.entry(key).or_insert_with(|| self.cache.remove(&key).unwrap_or_else(|| highlight(syntax, ss, code, dark)));
            ed.colour_runs(start, runs);
        }
        // Keep only what the document still holds.
        self.cache = seen;
    }
}

fn highlight(syntax: &SyntaxReference, ss: &SyntaxSet, code: &str, dark: bool) -> Runs {
    let mut h = HighlightLines::new(syntax, markdown::theme(dark));
    let mut runs: Runs = Vec::new();
    let mut off = 0u32;
    for line in LinesWithEndings::from(code) {
        let Ok(regions) = h.highlight_line(line, ss) else { break };
        for (style, t) in regions {
            let c = style.foreground;
            let bgr = (c.b as u32) << 16 | (c.g as u32) << 8 | c.r as u32;
            let len = t.len() as u32;
            match runs.last_mut() {
                Some((o, l, col)) if *col == bgr && *o + *l == off => *l += len,
                _ => runs.push((off, len, bgr)),
            }
            off += len;
        }
    }
    runs
}

/// Content byte ranges of the fenced code blocks, with each fence's info string. An unclosed
/// fence runs to the end of the document, as in CommonMark.
pub fn fenced_blocks(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    // (fence char, fence length, content start, info string)
    let mut open: Option<(u8, usize, usize, &str)> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        let body = line.trim();
        let run = |c: u8| body.bytes().take_while(|&b| b == c).count();
        let first = body.bytes().next();
        match open {
            None => {
                let Some(c @ (b'`' | b'~')) = first else { continue };
                let n = run(c);
                let info = body[n..].trim();
                // A backtick fence's info string can't hold backticks (that's inline code).
                if n >= 3 && !(c == b'`' && info.contains('`')) {
                    open = Some((c, n, pos, info));
                }
            }
            Some((c, n, start, info)) => {
                if first == Some(c) && run(c) >= n && run(c) == body.len() {
                    out.push((start, line_start, info));
                    open = None;
                }
            }
        }
    }
    if let Some((_, _, start, info)) = open {
        out.push((start, text.len(), info));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{fenced_blocks, highlight};
    use crate::markdown::find_syntax;

    #[test]
    fn language_aliases() {
        for (info, name) in [("csharp", "C#"), ("cs", "C#"), ("c#", "C#"), ("powershell", "PowerShell"), ("pwsh", "PowerShell"), ("ps1", "PowerShell")] {
            assert_eq!(find_syntax(info).map(|s| s.0.name.as_str()), Some(name), "{info}");
        }
    }

    #[test]
    fn powershell_grammar() {
        let src = include_str!("syntaxes/PowerShell.sublime-syntax");
        syntect::parsing::SyntaxDefinition::load_from_str(src, true, None).unwrap();
        let code = "<# block #>
function Get-X { param([string]$Name = 'a') # c
  if ($Name -eq \"x $($env:PATH) `n\") { Write-Host -Object 0x1F } }
$s = @\"
hi $Name
\"@
";
        for dark in [false, true] {
            let (syntax, ss) = find_syntax("powershell").unwrap();
            let runs = highlight(syntax, ss, code, dark);
            let colours: std::collections::HashSet<_> = runs.iter().map(|r| r.2).collect();
            assert!(colours.len() >= 4, "{colours:?}");
            assert_eq!(runs.iter().map(|r| r.1 as usize).sum::<usize>(), code.len());
        }
    }

    #[test]
    fn finds_fences() {
        let t = "a\n```rust\nfn x() {}\n```\n~~~~ py\nx = 1\n~~~\n~~~~\n``` js\nopen";
        let b: Vec<_> = fenced_blocks(t).into_iter().map(|(s, e, i)| (&t[s..e], i)).collect();
        assert_eq!(b, [("fn x() {}\n", "rust"), ("x = 1\n~~~\n", "py"), ("open", "js")]);
        assert!(fenced_blocks("``x`` and ```a`b```\n").is_empty());
    }

    #[test]
    fn crlf_fences() {
        let t = "```c\r\nint x;\r\n```\r\n";
        let b: Vec<_> = fenced_blocks(t).into_iter().map(|(s, e, i)| (&t[s..e], i)).collect();
        assert_eq!(b, [("int x;\r\n", "c")]);
    }
}

