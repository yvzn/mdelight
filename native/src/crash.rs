//! Crash capture: Rust panics and native exceptions are written, with a symbolized stack,
//! to %APPDATA%\MDelight\crash.log, and shown in a message box before the process ends.

use std::fmt::Write as _;

use windows_sys::Win32::System::Diagnostics::Debug::*;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

use crate::win::wide;

fn log_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("MDelight").join("crash.log"))
}

fn report(text: &str) {
    // The message box pumps messages; a repaint can hit the same bug again. Report once.
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let mut path_note = String::new();
    if let Some(p) = log_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, text);
        path_note = format!("\n\nDetails saved to {}", p.display());
    }
    let first = text.lines().take(3).collect::<Vec<_>>().join("\n");
    let body = wide(&format!("MDelight crashed.\n\n{first}{path_note}"));
    let title = wide("MDelight");
    unsafe { MessageBoxW(std::ptr::null_mut(), body.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

/// Symbolized native stack of the current thread (needs the .pdb next to the exe).
fn native_stack(skip: u32) -> String {
    let mut out = String::new();
    unsafe {
        let proc = GetCurrentProcess();
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
        SymInitializeW(proc, std::ptr::null(), 1);
        let mut frames = [std::ptr::null_mut(); 48];
        let n = RtlCaptureStackBackTrace(skip, frames.len() as u32, frames.as_mut_ptr(), std::ptr::null_mut());
        // SYMBOL_INFOW followed by room for the name.
        let mut buf = vec![0u64; (std::mem::size_of::<SYMBOL_INFOW>() + 512 * 2) / 8 + 1];
        for &f in &frames[..n as usize] {
            let addr = f as u64;
            let sym = buf.as_mut_ptr() as *mut SYMBOL_INFOW;
            (*sym).SizeOfStruct = std::mem::size_of::<SYMBOL_INFOW>() as u32;
            (*sym).MaxNameLen = 512;
            let mut disp = 0u64;
            let name = if SymFromAddrW(proc, addr, &mut disp, sym) != 0 {
                let p = (*sym).Name.as_ptr();
                String::from_utf16_lossy(std::slice::from_raw_parts(p, (*sym).NameLen as usize))
            } else {
                "?".into()
            };
            let mut line: IMAGEHLP_LINEW64 = std::mem::zeroed();
            line.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINEW64>() as u32;
            let mut ldisp = 0u32;
            let loc = if SymGetLineFromAddrW64(proc, addr, &mut ldisp, &mut line) != 0 {
                let mut len = 0;
                while *line.FileName.add(len) != 0 {
                    len += 1;
                }
                format!("  {}:{}", String::from_utf16_lossy(std::slice::from_raw_parts(line.FileName, len)), line.LineNumber)
            } else {
                String::new()
            };
            let _ = writeln!(out, "  {addr:#x} {name}{loc}");
        }
    }
    out
}

unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
    let rec = &*(*info).ExceptionRecord;
    let mut text = format!("Native exception {:#010x} at {:p}\n\nStack:\n", rec.ExceptionCode as u32, rec.ExceptionAddress);
    text.push_str(&native_stack(0));
    report(&text);
    EXCEPTION_CONTINUE_SEARCH
}

pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        report(&format!("Panic: {info}\n\nBacktrace:\n{bt}"));
    }));
    unsafe { SetUnhandledExceptionFilter(Some(on_exception)) };
}
