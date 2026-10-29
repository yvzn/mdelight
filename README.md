<p align="center">
  <img src="native/icon.png" alt="MDelight icon" width="96" height="96">
</p>

<h1 align="center">MDelight</h1>

<p align="center">
  A native, lightweight Markdown viewer and editor for Windows, written in Rust.
</p>

---

MDelight opens a folder of Markdown files and lets you read and edit them side by side with a live preview. No Electron, no WebView, no runtime to install: it is a single Win32 executable with a Scintilla editor and a litehtml preview drawn with Direct2D/DirectWrite. It starts fast and stays small, using about 47 MB of private memory on the `samples` folder.

What you get:

- A code editor with Markdown highlighting, find & replace and Notepad++-style shortcuts
- A live preview with syntax-highlighted code blocks, tables, local images and scroll sync
- A sidebar with the folder's files, full-text search results and the document outline
- Quick file switching with Ctrl+P (or Ctrl+O), autosave, and reloading when files change on disk
- Light and dark themes
- Mermaid diagrams viewable in your browser, offline (mermaid.js is bundled)

## Installation

1. Go to the **Releases** page of this repository and download the latest `mdelight-native-<version>.zip`.
2. Unzip it anywhere you like, for example `%LOCALAPPDATA%\Programs\MDelight`.
3. Run `mdelight-native.exe`.

That's it: there is no installer and nothing else to install. To uninstall, delete the folder. Settings live in `%APPDATA%\MDelight\native-settings.json`, which you can delete too.

## Building from source

### Prerequisites

- **Windows 10 or 11** (x64)
- **Rust** with the MSVC toolchain, installed with [rustup](https://rustup.rs/) (`stable-x86_64-pc-windows-msvc`, the default on Windows)
- **Visual Studio C++ build tools**: Visual Studio or the standalone [Build Tools for Visual Studio](https://visualstudio.microsoft.com/visual-cpp-build-tools/), with the *Desktop development with C++* workload (MSVC compiler and a Windows SDK)

CMake is not needed: `build.rs` compiles the vendored C++ libraries (Scintilla, Lexilla, litehtml) itself.

The Rust project lives in the `native` folder, and all commands below run from there:

```powershell
cd native
```

### Build

```powershell
cargo build --release
```

This produces `target\release\mdelight-native.exe` (about 11 MB, 5 MB of which is the embedded mermaid.js).

The first build compiles all the C++ code and takes a few minutes; later builds are incremental. If MSVC fails with `C1056 cannot update the time date stamp`, a file was locked (often by an antivirus): run the build again.

### Run locally

```powershell
cargo run --release
```

`cargo run` (without `--release`) gives a debug build, which compiles faster but runs noticeably slower. To try the app on realistic content, open the `samples` folder at the root of the repository.

Run the tests with:

```powershell
cargo test
```

### Debug

- The app is a GUI program (`windows_subsystem = "windows"`), so it has no console and `println!` output is not visible. Use a debugger instead: build with `cargo build`, then open `target\debug\mdelight-native.exe` in Visual Studio (File → Open → Project/Solution, pick the exe) or in VS Code with the C/C++ extension's `cppvsdbg` launch type. Both step through Rust and the C++ code.
- On a crash (Rust panic or native exception), MDelight shows a message box and writes the details, with a symbolized stack, to `%APPDATA%\MDelight\crash.log`. Symbols come from the `.pdb` next to the exe; release builds keep line tables for this.
- The test hooks below let you render the preview to a PNG without clicking around, which helps when checking layout changes.

### Package

Bundle the release exe and its `.pdb` (so crash logs stay readable) into a zip for the Releases page:

```powershell
cargo build --release
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Compress-Archive -Force -DestinationPath "target\mdelight-native-$version.zip" -Path target\release\mdelight-native.exe, target\release\mdelight_native.pdb
```

## Project layout

`samples/` holds Markdown files for trying the app. The code is under `native/`:

| Path | What |
|---|---|
| `src/app.rs` | Main window: layout, painting, shortcuts, open/autosave/external edits, preview refresh, scroll sync |
| `src/panels.rs` | Sidebar TreeViews: files (lazy folders), search results, outline |
| `src/palette.rs`, `src/findbar.rs` | Ctrl+P (or Ctrl+O) / Ctrl+R / prompts; find & replace |
| `src/sci.rs` | Scintilla wrapper (theme, text, find, line commands) |
| `src/core.rs` | Workspace scan, watcher, file ops, fuzzy find, parallel content search |
| `src/markdown.rs` | comrak + syntect HTML, preview CSS, outline parser |
| `src/mermaid.rs` | Writes a Mermaid diagram as a browser page to %TEMP% (mermaid.js is embedded) |
| `cpp/preview.cpp` | litehtml container (Direct2D/DirectWrite/WIC) + preview window class |
| `cpp/editor_glue.cpp` | Lexilla markdown lexer, folder picker |
| `vendor/` | Scintilla 5.6.7, Lexilla 5.5.4 (lexlib + markdown lexer only), litehtml v0.10, mermaid.min.js 12.2.0 |

## Limitations

- **Mermaid**: diagrams show as their source code. To see one drawn, double-click it or use its hover **View** button (or right-click → View diagram): it opens in the default browser, rendered by the bundled mermaid.js, with no network access needed.
- **Preview selection** works by whole words: drag to select, Ctrl+C to copy. Code blocks have a hover **Copy** button; right-click also offers Copy code, Copy link, Copy paragraph and Copy all text.
- **Fullscreen lightbox**: double-clicking an image opens it in the default viewer instead.
- **Code blocks in the preview** wrap long lines instead of scrolling sideways. Wide tables scroll the whole page sideways.
- **Remote images** (`http(s)://`) are not loaded. Local images are (PNG/JPEG/GIF/BMP/TIFF through WIC; SVG is not supported).
- **Code highlighting**: syntect's bundled grammars. TypeScript uses the JavaScript grammar.
- **Editor**: no folding.

## Test hooks

`MDELIGHT_SNAPSHOT=<file.png>` renders the whole preview to a PNG about 1 second after start. `MDELIGHT_THEME=light|dark` and `MDELIGHT_MODE=code|split` set the theme and view mode for that run. `MDELIGHT_HOVER=x,y` hovers that point of the preview (client pixels) just before the snapshot.
