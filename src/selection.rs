//! Capture before activation using simulated Copy with clipboard restoration.
use anyhow::Result;

#[derive(Default)]
pub struct Capture {
    pub text: Option<String>,
    pub error: String,
    pub writing_source: String,
}
impl Capture {
    pub fn update(&mut self, result: Result<String>) {
        match result {
            Ok(text) => {
                self.text = Some(text);
                self.error.clear();
            }
            Err(error) => {
                self.text = None;
                self.error = format!("{error:#}");
            }
        }
    }
    pub fn snapshot_writing(&mut self) {
        self.writing_source = self.text.clone().unwrap_or_default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_new_activation_updates_capture_without_changing_inflight_source() {
        let mut selection = Capture::default();
        selection.update(Ok("A".into()));
        selection.snapshot_writing();
        selection.update(Ok("B".into()));
        assert_eq!(selection.writing_source, "A");
        assert_eq!(selection.text.as_deref(), Some("B"));
        selection.snapshot_writing();
        assert_eq!(selection.writing_source, "B");
    }
    #[test]
    fn failed_capture_never_leaves_old_text_ready_to_send() {
        let mut selection = Capture::default();
        selection.update(Ok("A".into()));
        selection.update(Err(anyhow::anyhow!("No selection")));
        assert!(selection.text.is_none());
        assert_eq!(selection.error, "No selection");
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, Result, bail, ensure};
    use objc2_app_kit::NSWorkspace;
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr,
    };

    type Ref = *const c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> Ref;
        fn AXUIElementGetTypeID() -> usize;
        fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
        fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: Ref);
        fn CFEqual(left: Ref, right: Ref) -> bool;
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFStringCreateWithCString(allocator: Ref, text: *const c_char, encoding: u32) -> Ref;
    }
    struct Owned(Ref);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }
    fn string(value: &CStr) -> Option<Owned> {
        let string =
            Owned(unsafe { CFStringCreateWithCString(ptr::null(), value.as_ptr(), 0x08000100) });
        (!string.0.is_null()).then_some(string)
    }
    fn attribute(element: Ref, name: &CStr) -> Option<Owned> {
        unsafe { AXUIElementSetMessagingTimeout(element, 0.1) };
        let name = string(name)?;
        let mut raw = ptr::null();
        let code = unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut raw) };
        let value = Owned(raw);
        (code == 0 && !value.0.is_null()).then_some(value)
    }
    fn is_protected_field(app: Ref) -> bool {
        // Accessibility is used only for this safety check, never to extract text.
        let Some(focused) = attribute(app, c"AXFocusedUIElement") else {
            return false;
        };
        if unsafe { CFGetTypeID(focused.0) != AXUIElementGetTypeID() } {
            return false;
        }
        let Some(subrole) = attribute(focused.0, c"AXSubrole") else {
            return false;
        };
        let Some(secure) = string(c"AXSecureTextField") else {
            return false;
        };
        unsafe { CFEqual(subrole.0, secure.0) }
    }

    pub enum PendingCapture {
        Failed(Option<anyhow::Error>),
        Copy {
            capture: crate::clipboard_capture::CopyCapture,
            name: String,
        },
    }
    impl PendingCapture {
        pub fn failed(error: anyhow::Error) -> Self {
            Self::Failed(Some(error))
        }
        pub fn poll(&mut self) -> Option<Result<String>> {
            match self {
                Self::Failed(error) => error.take().map(Err),
                Self::Copy { capture, name } => capture.poll().map(|result| {
                    result.with_context(|| {
                        format!("Could not capture selected text from {name} using Copy")
                    })
                }),
            }
        }
    }

    pub fn is_trusted() -> bool {
        // This is the live TCC status of this running process, not a saved preference.
        unsafe { AXIsProcessTrusted() }
    }
    pub fn begin() -> Result<PendingCapture> {
        if !is_trusted() {
            bail!(
                "Grant Wiesel Accessibility access in System Settings → Privacy & Security → Accessibility, then select text and trigger the hotkey again."
            );
        }
        let source = NSWorkspace::sharedWorkspace().frontmostApplication()
            .context("macOS reports no frontmost application. Focus the source app and retry the hotkey.")?;
        let pid = source.processIdentifier();
        ensure!(
            pid > 0 && pid as u32 != std::process::id(),
            "Wiesel is the frontmost app. Select text in another app and press the hotkey without opening Wiesel first."
        );
        let name = source
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("process {pid}"));
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        ensure!(
            !app.0.is_null(),
            "Cannot create an Accessibility connection to {name}"
        );
        ensure!(
            !is_protected_field(app.0),
            "Protected fields cannot be captured"
        );
        ensure!(
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .is_some_and(|current| current.processIdentifier() == pid),
            "The active app changed during capture. Select text and retry the hotkey."
        );
        Ok(PendingCapture::Copy {
            capture: crate::clipboard_capture::CopyCapture::new(pid),
            name,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn failed_capture_reports_error_once() {
            let mut capture = PendingCapture::failed(anyhow::anyhow!("Capture failed"));
            assert_eq!(
                capture.poll().unwrap().unwrap_err().to_string(),
                "Capture failed"
            );
            assert!(capture.poll().is_none());
        }
    }
}
pub fn accessibility_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
pub use macos::PendingCapture;

#[cfg(target_os = "macos")]
pub fn begin() -> PendingCapture {
    macos::begin().unwrap_or_else(PendingCapture::failed)
}
