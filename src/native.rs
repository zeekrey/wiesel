//! AppKit window chrome that GPUI does not expose: hiding one window while the app keeps
//! running, and centering it on the active screen. Test windows have no native handle,
//! so both are no-ops in-process.
use gpui::Window;
use objc2::{msg_send, runtime::AnyObject};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Remove a window from screen without closing it, like the cmd+tab switcher does.
pub fn order_out(window: &Window) {
    with_ns_window(window, |ns_window| unsafe {
        let _: () = msg_send![ns_window, orderOut: std::ptr::null::<AnyObject>()];
    });
}

/// Center a window on its screen, slightly above the middle (AppKit's convention).
pub fn center(window: &Window) {
    with_ns_window(window, |ns_window| unsafe {
        let _: () = msg_send![ns_window, center];
    });
}

fn with_ns_window(window: &Window, f: impl FnOnce(&AnyObject)) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: GPUI hands out its live content NSView; `window` returns its owning NSWindow
    // (or nil), which outlives this synchronous call on the main thread.
    let ns_window: *mut AnyObject = unsafe { msg_send![view, window] };
    if let Some(ns_window) = unsafe { ns_window.as_ref() } {
        f(ns_window);
    }
}
