# MDelight, native build

Same app as the Tauri build, with no WebView: a Win32 window, a Scintilla editor, and a litehtml preview drawn with Direct2D/DirectWrite. One process, about 47 MB private memory on the samples folder.

```
cargo build --release      # → target/release/mdelight-native.exe (~6 MB, no runtime needed)
cargo test
```

Needs the Rust MSVC toolchain and Visual Studio C++ build tools. `build.rs` compiles the vendored C++ itself, so CMake isn't needed. MSVC sometimes fails with `C1056 cannot update the time date stamp` (file locked, likely by antivirus). If that happens, run the build again.

## Layout

| Path | What |
|---|---|
| `src/app.rs` | Main window: layout, painting, shortcuts, open/autosave/external edits, preview refresh, scroll sync |
| `src/panels.rs` | Sidebar TreeViews: files (lazy folders), search results, outline |
| `src/palette.rs`, `src/findbar.rs` | Ctrl+P (or Ctrl+O) / Ctrl+R / prompts; find & replace |
| `src/sci.rs` | Scintilla wrapper (theme, text, find, line commands) |
| `src/core.rs` | Workspace scan, watcher, file ops, fuzzy find, parallel content search |
| `src/markdown.rs` | comrak + syntect HTML, preview CSS, outline parser |
| `cpp/preview.cpp` | litehtml container (Direct2D/DirectWrite/WIC) + preview window class |
| `cpp/editor_glue.cpp` | Lexilla markdown lexer, folder picker |
| `vendor/` | Scintilla 5.6.7, Lexilla 5.5.4 (lexlib + markdown lexer only), litehtml v0.10 |

## Differences from the webview build

- **Mermaid**: diagrams show as their source code (syntax-highlighted).
- **Preview selection** works by whole words: drag to select, Ctrl+C to copy. Code blocks have a hover **Copy** button, as before; right-click also offers Copy code, Copy link, Copy paragraph and Copy all text.
- **Fullscreen lightbox**: double-clicking an image opens it in the default viewer instead.
- **Code blocks in the preview** wrap long lines instead of scrolling sideways. Wide tables scroll the whole page sideways.
- **Remote images** (`http(s)://`) are not loaded. Local images are (PNG/JPEG/GIF/BMP/TIFF through WIC; SVG is not supported).
- **Code highlighting**: syntect's bundled grammars. TypeScript uses the JavaScript grammar.
- **Editor**: no folding.

## Test hooks

`MDELIGHT_SNAPSHOT=<file.png>` renders the whole preview to a PNG about 1 second after start. `MDELIGHT_THEME=light|dark` and `MDELIGHT_MODE=code|split` set the theme and view mode for that run. `MDELIGHT_HOVER=x,y` hovers that point of the preview (client pixels) just before the snapshot.
