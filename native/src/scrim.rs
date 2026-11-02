//! The dimmed backdrop behind modal overlays: a layered black window over the client area,
//! slipped just under the overlay in z-order. A click on it dismisses the overlay.

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, BLACK_BRUSH, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::win::*;

/// Scrim opacity (0–255); the help sheet bakes the same value into its own pixels.
pub fn alpha(dark: bool) -> u8 {
    if dark { 0x8c } else { 0x66 }
}

pub fn register(inst: HINSTANCE) {
    let cls = wide("MDelightScrim");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(scrim_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: inst,
        hIcon: std::ptr::null_mut(),
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) } as HBRUSH,
        lpszMenuName: std::ptr::null(),
        lpszClassName: cls.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    unsafe { RegisterClassExW(&wc) };
}

/// A hidden scrim; `on_click` runs when the user clicks it.
pub fn create(parent: HWND, on_click: fn()) -> HWND {
    unsafe {
        let cls = wide("MDelightScrim");
        let h = CreateWindowExW(WS_EX_LAYERED, cls.as_ptr(), std::ptr::null(), WS_CHILD, 0, 0, 10, 10, parent, std::ptr::null_mut(), GetModuleHandleW(std::ptr::null()), std::ptr::null());
        SetWindowLongPtrW(h, GWLP_USERDATA, on_click as usize as isize);
        h
    }
}

/// Cover the parent's client area, just below `above`, at the theme's opacity.
pub fn place(h: HWND, above: HWND, dark: bool) {
    unsafe {
        SetLayeredWindowAttributes(h, 0, alpha(dark), LWA_ALPHA);
        let r = client_rect(GetParent(h));
        SetWindowPos(h, above, 0, 0, r.right, r.bottom, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

unsafe extern "system" fn scrim_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            let f = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if f != 0 {
                let on_click: fn() = std::mem::transmute(f as usize);
                on_click();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
