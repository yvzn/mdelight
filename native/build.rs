//! Compiles the vendored C++ libraries (Scintilla, Lexilla's markdown lexer, litehtml + gumbo)
//! and our C++ glue, then embeds the icon and manifest.

use std::path::Path;

fn sources(dir: &str, ext: &str) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect();
    v.sort();
    v
}

fn cpp() -> cc::Build {
    let mut b = cc::Build::new();
    b.cpp(true)
        .std("c++17")
        .warnings(false)
        .define("NDEBUG", None)
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .define("NOMINMAX", None)
        .flag_if_supported("/EHsc")
        .flag_if_supported("/utf-8")
        .flag_if_supported("/permissive-");
    b
}

fn main() {
    // Scintilla + the one Lexilla lexer we use + editor glue.
    let mut sci = cpp();
    sci.include("vendor/scintilla/include")
        .include("vendor/scintilla/src")
        .include("vendor/lexilla/include")
        .include("vendor/lexilla/lexlib")
        .files(sources("vendor/scintilla/src", "cxx"))
        .files(sources("vendor/scintilla/win32", "cxx"))
        .files(sources("vendor/lexilla/lexlib", "cxx"))
        .file("vendor/lexilla/lexers/LexMarkdown.cxx")
        .file("cpp/editor_glue.cpp")
        .compile("scintilla");

    // gumbo (HTML parser used by litehtml).
    let mut gumbo = cc::Build::new();
    gumbo
        .warnings(false)
        .define("NDEBUG", None)
        .include("vendor/litehtml/src/gumbo/include")
        .include("vendor/litehtml/src/gumbo/include/gumbo")
        .include("vendor/litehtml/src/gumbo/visualc/include")
        .flag_if_supported("/utf-8")
        .files(sources("vendor/litehtml/src/gumbo", "c"))
        .compile("gumbo");

    // litehtml + our Direct2D/DirectWrite container and preview window.
    let mut lh = cpp();
    lh.include("vendor/litehtml/include")
        .include("vendor/litehtml/include/litehtml")
        .include("vendor/litehtml/src/gumbo/include")
        .files(sources("vendor/litehtml/src", "cpp"))
        .file("cpp/preview.cpp")
        .compile("litehtml");

    for lib in [
        "user32", "gdi32", "imm32", "ole32", "oleaut32", "uuid", "advapi32", "shell32", "comctl32", "msimg32", "d2d1",
        "dwrite", "windowscodecs", "uxtheme", "dwmapi",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }

    embed_resource::compile("app.rc", embed_resource::NONE).manifest_required().unwrap();

    println!("cargo:rerun-if-changed=cpp");
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    assert!(Path::new("icon.ico").exists(), "icon missing");
}
