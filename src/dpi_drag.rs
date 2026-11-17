//! Windows only. When a window crosses to a screen with another display scale,
//! Windows sends WM_DPICHANGED with the rectangle the window should take. winit
//! ignores that rectangle's size and, during a mouse drag, shifts and nudges the
//! window on its own. That fights the system's move loop: the window jumps about and
//! can end up tiny or off screen. Moving it by code is unaffected.
//!
//! The guard wraps each window's procedure and makes the one move winit performs
//! while handling WM_DPICHANGED land exactly on the rectangle Windows asked for,
//! which is what the move loop expects.

use std::cell::Cell;
use std::sync::Mutex;

type Hwnd = isize;
type WindowProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct WindowPos {
    hwnd: Hwnd,
    insert_after: Hwnd,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    flags: u32,
}

const WM_WINDOWPOSCHANGING: u32 = 0x0046;
const WM_NCDESTROY: u32 = 0x0082;
const WM_DPICHANGED: u32 = 0x02E0;
const GWLP_WNDPROC: i32 = -4;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
/// The class winit registers for its windows. Other windows of the thread (input
/// method helpers, winit's hidden message window) are left alone.
const WINIT_CLASS: &str = "Window Class";

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumThreadWindows(thread: u32, each: unsafe extern "system" fn(Hwnd, isize) -> i32, data: isize) -> i32;
    fn GetClassNameW(hwnd: Hwnd, name: *mut u16, capacity: i32) -> i32;
    fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
    fn CallWindowProcW(previous: isize, hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> isize;
    fn DefWindowProcW(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
}

/// Guarded windows with the procedure each had before.
static PREVIOUS: Mutex<Vec<(Hwnd, isize)>> = Mutex::new(Vec::new());

thread_local! {
    /// The window handling WM_DPICHANGED right now, and the rectangle Windows suggested.
    static SUGGESTED: Cell<Option<(Hwnd, Rect)>> = const { Cell::new(None) };
}

fn previous() -> std::sync::MutexGuard<'static, Vec<(Hwnd, isize)>> {
    PREVIOUS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Guards every winit window of the calling thread. Cheap and repeatable: call it each
/// frame from the UI thread, so windows opened later are covered as well.
pub fn guard_windows() {
    // SAFETY: `guard` matches the callback signature and only uses the handle it is given.
    unsafe { EnumThreadWindows(GetCurrentThreadId(), guard, 0) };
}

unsafe extern "system" fn guard(hwnd: Hwnd, _: isize) -> i32 {
    let mut class = [0u16; 64];
    // SAFETY: the buffer is valid for the capacity passed.
    let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) }.max(0) as usize;
    let mut known = previous();
    if String::from_utf16_lossy(&class[..length]) == WINIT_CLASS && !known.iter().any(|(guarded, _)| *guarded == hwnd) {
        let ours: WindowProc = window_proc;
        // SAFETY: `hwnd` is a live window of this thread; the old procedure is kept and always called.
        let old = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, ours as usize as isize) };
        if old != 0 {
            known.push((hwnd, old));
        }
    }
    1 // keep enumerating
}

unsafe extern "system" fn window_proc(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> isize {
    // The lock is released before calling on: window procedures re-enter.
    let old = previous().iter().find(|(guarded, _)| *guarded == hwnd).map(|(_, old)| *old);
    let Some(old) = old else {
        // SAFETY: plain forwarding of the message we were given.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_DPICHANGED => {
            // SAFETY: for this message `lparam` points to the suggested RECT.
            SUGGESTED.set(Some((hwnd, unsafe { *(lparam as *const Rect) })));
            // SAFETY: forwarding to the procedure that was installed before ours.
            let result = unsafe { CallWindowProcW(old, hwnd, message, wparam, lparam) };
            SUGGESTED.set(None);
            return result;
        }
        WM_WINDOWPOSCHANGING => {
            if let Some((target, rect)) = SUGGESTED.get()
                && target == hwnd
            {
                // SAFETY: for this message `lparam` points to a WINDOWPOS the handler may edit.
                let pos = unsafe { &mut *(lparam as *mut WindowPos) };
                (pos.x, pos.y, pos.cx, pos.cy) = (rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top);
                pos.flags &= !(SWP_NOSIZE | SWP_NOMOVE);
            }
        }
        WM_NCDESTROY => previous().retain(|(guarded, _)| *guarded != hwnd),
        _ => {}
    }
    // SAFETY: forwarding to the procedure that was installed before ours.
    unsafe { CallWindowProcW(old, hwnd, message, wparam, lparam) }
}
