//! Simulated Copy capture. Polled on the UI thread before Wiesel activates.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use objc2::{
    rc::Retained,
    runtime::{NSObjectProtocol, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardAccessBehavior, NSPasteboardItem, NSPasteboardTypeString,
    NSPasteboardWriting, NSWorkspace,
};
use objc2_foundation::NSArray;

const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
const COPY_TIMEOUT: Duration = Duration::from_millis(1200);
const SETTLE_TIME: Duration = Duration::from_millis(80);

trait Pasteboard {
    type Saved;
    fn change_count(&self) -> isize;
    fn snapshot(&self) -> Result<Self::Saved>;
    fn text(&self) -> Result<String>;
    fn restore(&self, saved: &Self::Saved, expected: isize) -> Result<()>;
}
trait CopyHost {
    fn is_active(&self) -> bool;
    fn keys_released(&self) -> bool;
    fn post_copy(&self) -> Result<()>;
}

enum Phase {
    Release(Instant),
    Copy {
        sent: Instant,
        changed: Option<(isize, Instant)>,
    },
    Done,
}
struct CopySession<B: Pasteboard, H: CopyHost> {
    board: B,
    host: H,
    saved: Option<B::Saved>,
    baseline: isize,
    phase: Phase,
}
impl<B: Pasteboard, H: CopyHost> CopySession<B, H> {
    fn new(board: B, host: H, now: Instant) -> Self {
        Self {
            board,
            host,
            saved: None,
            baseline: 0,
            phase: Phase::Release(now),
        }
    }
    fn poll(&mut self, now: Instant) -> Option<Result<String>> {
        let result = self.advance(now);
        match result {
            Ok(None) => None,
            Ok(Some(text)) => {
                self.phase = Phase::Done;
                Some(Ok(text))
            }
            Err(error) => {
                self.phase = Phase::Done;
                Some(Err(error))
            }
        }
    }
    fn advance(&mut self, now: Instant) -> Result<Option<String>> {
        if matches!(self.phase, Phase::Done) {
            return Ok(None);
        }
        // If focus changes we cannot attribute a clipboard write to our Copy.
        // In that case preserve the current clipboard rather than overwrite it.
        ensure!(
            self.host.is_active(),
            "The source app changed during Copy capture. Retry from the source app; the current clipboard was left untouched."
        );
        match &mut self.phase {
            Phase::Release(started) => {
                ensure!(
                    now.duration_since(*started) < RELEASE_TIMEOUT,
                    "Release the shortcut keys and try again. No Copy was sent."
                );
                if !self.host.keys_released() {
                    return Ok(None);
                }
                self.baseline = self.board.change_count();
                let saved = self
                    .board
                    .snapshot()
                    .context("Could not safely save the clipboard; Copy was not sent")?;
                ensure!(
                    self.board.change_count() == self.baseline,
                    "The clipboard changed while saving it. Retry; Copy was not sent."
                );
                ensure!(
                    self.host.is_active() && self.host.keys_released(),
                    "The source or keyboard state changed while saving the clipboard. Retry; Copy was not sent."
                );
                self.host.post_copy()?;
                self.saved = Some(saved);
                self.phase = Phase::Copy {
                    sent: Instant::now(),
                    changed: None,
                };
            }
            Phase::Copy { sent, changed } => {
                let count = self.board.change_count();
                if count != self.baseline {
                    match changed {
                        Some((observed, since))
                            if *observed == count && now.duration_since(*since) >= SETTLE_TIME =>
                        {
                            // Read only after a fresh, stable write. Never use old clipboard text.
                            let text = self.board.text();
                            let saved =
                                self.saved.as_ref().context("Missing clipboard snapshot")?;
                            self.board.restore(saved, count).context("Could not restore the previous clipboard; captured text was discarded")?;
                            self.saved = None;
                            return text.map(Some);
                        }
                        Some((observed, _)) if *observed == count => {}
                        Some(_) => bail!(
                            "The clipboard changed again during Copy. Capture was cancelled and the newer clipboard was left untouched."
                        ),
                        None => *changed = Some((count, now)),
                    }
                }
                if now.duration_since(*sent) >= COPY_TIMEOUT {
                    // Restore an observed Copy even if it never settled, but never
                    // overwrite a later version that we have not observed.
                    if let Some((observed, _)) = changed {
                        let saved = self.saved.as_ref().context("Missing clipboard snapshot")?;
                        self.board
                            .restore(saved, *observed)
                            .context("Copy timed out and clipboard restoration failed")?;
                        self.saved = None;
                    }
                    bail!(
                        "Copy did not produce a stable text selection. Select text and retry; no previous clipboard text was captured."
                    );
                }
            }
            Phase::Done => {}
        }
        Ok(None)
    }
}
impl<B: Pasteboard, H: CopyHost> Drop for CopySession<B, H> {
    fn drop(&mut self) {
        // Best-effort cleanup if a capture is cancelled after observing Copy.
        // Normal completion reports restoration failures instead of hiding them.
        if let Phase::Copy {
            changed: Some((count, _)),
            ..
        } = self.phase
            && let Some(saved) = &self.saved
            && self.host.is_active()
            && let Err(error) = self.board.restore(saved, count)
        {
            eprintln!("Wiesel cancelled capture cleanup: {error:#}");
        }
    }
}

struct NativePasteboard(Retained<NSPasteboard>);
impl Pasteboard for NativePasteboard {
    type Saved = Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>;
    fn change_count(&self) -> isize {
        self.0.changeCount()
    }
    fn snapshot(&self) -> Result<Self::Saved> {
        // This privacy setting is only present on newer macOS versions. A denied
        // read must not be mistaken for an empty clipboard and later erase it.
        if self.0.respondsToSelector(sel!(accessBehavior)) {
            ensure!(
                self.0.accessBehavior() != NSPasteboardAccessBehavior::AlwaysDeny,
                "Allow Wiesel to paste from other apps in macOS settings before using Copy capture"
            );
        }
        let mut copies = Vec::new();
        let mut size = 0usize;
        if let Some(items) = self.0.pasteboardItems() {
            ensure!(
                items.len() <= 128,
                "Clipboard has too many items to safely preserve"
            );
            for item in items.iter() {
                let copy = NSPasteboardItem::new();
                let types = item.types();
                ensure!(
                    types.len() <= 512 && !types.is_empty(),
                    "Clipboard item has an unsupported number of formats"
                );
                for kind in types.iter() {
                    // Materialize every promised representation before sending Copy.
                    let data = item
                        .dataForType(&kind)
                        .context("A clipboard format could not be saved")?;
                    size = size
                        .checked_add(data.len())
                        .context("Clipboard size overflow")?;
                    ensure!(
                        size <= 64 * 1024 * 1024,
                        "Clipboard is larger than the 64 MiB preservation limit"
                    );
                    ensure!(
                        copy.setData_forType(&data, &kind),
                        "A clipboard format could not be preserved"
                    );
                }
                copies.push(ProtocolObject::from_retained(copy));
            }
        } else {
            ensure!(
                self.0.types().is_none_or(|types| types.is_empty()),
                "Clipboard items could not be read; refusing to replace them"
            );
        }
        Ok(NSArray::from_retained_slice(&copies))
    }
    fn text(&self) -> Result<String> {
        let text = self
            .0
            .stringForType(unsafe { NSPasteboardTypeString })
            .context("Copy produced no plain text; the previous clipboard was restored")?
            .to_string();
        ensure!(
            text.len() <= 4_000_000,
            "Copied selection is too large; the previous clipboard was restored"
        );
        ensure!(
            !text.trim().is_empty(),
            "Copied selection is empty; the previous clipboard was restored"
        );
        Ok(text)
    }
    fn restore(&self, saved: &Self::Saved, expected: isize) -> Result<()> {
        ensure!(
            self.change_count() == expected,
            "The clipboard changed again. Newer clipboard content was preserved instead of overwritten."
        );
        let cleared = self.0.clearContents();
        // NSPasteboard has no atomic compare-and-swap. Check again immediately
        // before writing, but a concurrent writer can still race the OS calls.
        ensure!(
            self.change_count() == cleared,
            "Another app changed the clipboard during restoration"
        );
        ensure!(
            saved.is_empty() || self.0.writeObjects(saved),
            "macOS rejected clipboard restoration"
        );
        Ok(())
    }
}

struct NativeHost {
    pid: i32,
}
type Ref = *const std::ffi::c_void;
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> Ref;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventPostToPid(pid: i32, event: Ref);
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGPreflightPostEventAccess() -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
}
struct Event(Ref);
impl Drop for Event {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}
impl CopyHost for NativeHost {
    fn is_active(&self) -> bool {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|app| app.processIdentifier() == self.pid)
    }
    fn keys_released(&self) -> bool {
        // HID state ignores our private synthetic event source. Caps Lock is harmless.
        const MODIFIERS: u64 = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
        unsafe { CGEventSourceFlagsState(1) & MODIFIERS == 0 && !CGEventSourceKeyState(1, 8) }
    }
    fn post_copy(&self) -> Result<()> {
        ensure!(
            unsafe { CGPreflightPostEventAccess() },
            "macOS denied simulated Copy. Recheck Wiesel's Accessibility permission and relaunch."
        );
        let source = Event(unsafe { CGEventSourceCreate(-1) });
        ensure!(
            !source.0.is_null(),
            "Could not create a private keyboard event source"
        );
        let mut events = Vec::new();
        // Allocate every event first so allocation failure cannot leave Command down.
        for (key, down, flags) in [
            (55, true, 1 << 20),
            (8, true, 1 << 20),
            (8, false, 1 << 20),
            (55, false, 0),
        ] {
            let event = Event(unsafe { CGEventCreateKeyboardEvent(source.0, key, down) });
            ensure!(!event.0.is_null(), "Could not create Copy keyboard events");
            unsafe { CGEventSetFlags(event.0, flags) };
            events.push(event);
        }
        ensure!(
            self.is_active() && self.keys_released(),
            "Source app or keyboard state changed; Copy was not sent"
        );
        for event in events {
            unsafe { CGEventPostToPid(self.pid, event.0) };
        }
        Ok(())
    }
}

pub struct CopyCapture(CopySession<NativePasteboard, NativeHost>);
impl CopyCapture {
    pub fn new(pid: i32) -> Self {
        Self(CopySession::new(
            NativePasteboard(NSPasteboard::generalPasteboard()),
            NativeHost { pid },
            Instant::now(),
        ))
    }
    pub fn poll(&mut self) -> Option<Result<String>> {
        self.0.poll(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    type Items = Vec<Vec<(String, Vec<u8>)>>;
    struct FakeBoard {
        version: Cell<isize>,
        items: RefCell<Items>,
        text: RefCell<Option<String>>,
        fail_snapshot: Cell<bool>,
        race_on_read: Cell<bool>,
        restores: Cell<usize>,
    }
    impl Pasteboard for Rc<FakeBoard> {
        type Saved = Items;
        fn change_count(&self) -> isize {
            self.version.get()
        }
        fn snapshot(&self) -> Result<Items> {
            ensure!(!self.fail_snapshot.get(), "unreadable format");
            Ok(self.items.borrow().clone())
        }
        fn text(&self) -> Result<String> {
            if self.race_on_read.get() {
                self.version.set(self.version.get() + 1);
            }
            let text = self.text.borrow().clone().context("no text")?;
            ensure!(!text.trim().is_empty(), "empty text");
            Ok(text)
        }
        fn restore(&self, saved: &Items, expected: isize) -> Result<()> {
            ensure!(self.change_count() == expected, "newer clipboard");
            *self.items.borrow_mut() = saved.clone();
            self.restores.set(self.restores.get() + 1);
            Ok(())
        }
    }
    struct FakeHost {
        active: Cell<bool>,
        released: Cell<bool>,
        copies: Cell<usize>,
    }
    impl CopyHost for Rc<FakeHost> {
        fn is_active(&self) -> bool {
            self.active.get()
        }
        fn keys_released(&self) -> bool {
            self.released.get()
        }
        fn post_copy(&self) -> Result<()> {
            self.copies.set(self.copies.get() + 1);
            Ok(())
        }
    }
    fn fixture(items: Items) -> (Rc<FakeBoard>, Rc<FakeHost>) {
        (
            Rc::new(FakeBoard {
                version: Cell::new(10),
                items: RefCell::new(items),
                text: RefCell::new(Some("old clipboard text".into())),
                fail_snapshot: Cell::new(false),
                race_on_read: Cell::new(false),
                restores: Cell::new(0),
            }),
            Rc::new(FakeHost {
                active: Cell::new(true),
                released: Cell::new(true),
                copies: Cell::new(0),
            }),
        )
    }
    #[test]
    fn native_snapshot_restores_multiple_items_and_binary_formats() {
        use objc2_foundation::{NSData, NSString};
        // Use a private pasteboard; unit tests never read or modify the user's clipboard.
        let board = NativePasteboard(NSPasteboard::pasteboardWithUniqueName());
        let text = NSPasteboardItem::new();
        assert!(
            text.setString_forType(&NSString::from_str("old text"), unsafe {
                NSPasteboardTypeString
            })
        );
        let rtf_type = NSString::from_str("public.rtf");
        let rtf = b"{\\rtf1 old text}";
        assert!(text.setData_forType(&NSData::with_bytes(rtf), &rtf_type));
        let binary = NSPasteboardItem::new();
        let binary_type = NSString::from_str("com.wiesel.test.binary");
        assert!(binary.setData_forType(&NSData::with_bytes(&[0, 1, 255]), &binary_type));
        let items = NSArray::from_retained_slice(&[
            ProtocolObject::from_retained(text),
            ProtocolObject::from_retained(binary),
        ]);
        board.0.clearContents();
        assert!(board.0.writeObjects(&items));
        let saved = board.snapshot().unwrap();
        board.0.clearContents();
        assert!(
            board
                .0
                .setString_forType(&NSString::from_str("new selection"), unsafe {
                    NSPasteboardTypeString
                })
        );
        assert_eq!(board.text().unwrap(), "new selection");
        board.restore(&saved, board.change_count()).unwrap();
        let restored = board.0.pasteboardItems().unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(
            restored
                .objectAtIndex(0)
                .stringForType(unsafe { NSPasteboardTypeString })
                .unwrap()
                .to_string(),
            "old text"
        );
        assert_eq!(
            restored
                .objectAtIndex(0)
                .dataForType(&rtf_type)
                .unwrap()
                .to_vec(),
            rtf
        );
        assert_eq!(
            restored
                .objectAtIndex(1)
                .dataForType(&binary_type)
                .unwrap()
                .to_vec(),
            [0, 1, 255]
        );
        board.0.clearContents();
    }
    #[test]
    fn native_empty_clipboard_is_restored() {
        use objc2_foundation::NSString;
        let board = NativePasteboard(NSPasteboard::pasteboardWithUniqueName());
        board.0.clearContents();
        let saved = board.snapshot().unwrap();
        assert!(
            board
                .0
                .setString_forType(&NSString::from_str("selection"), unsafe {
                    NSPasteboardTypeString
                })
        );
        board.restore(&saved, board.change_count()).unwrap();
        assert!(board.0.types().is_none_or(|types| types.is_empty()));
    }
    #[test]
    fn a_second_clipboard_write_cancels_without_overwriting_it() {
        let (board, host) = fixture(vec![]);
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        board.version.set(11);
        session.poll(now + Duration::from_millis(40));
        board.version.set(12);
        assert!(
            session
                .poll(now + Duration::from_millis(140))
                .unwrap()
                .is_err()
        );
        assert_eq!(board.restores.get(), 0);
    }
    #[test]
    fn release_timeout_sends_no_copy() {
        let (board, host) = fixture(vec![]);
        host.released.set(false);
        let now = Instant::now();
        let mut session = CopySession::new(board, host.clone(), now);
        assert!(session.poll(now + RELEASE_TIMEOUT).unwrap().is_err());
        assert_eq!(host.copies.get(), 0);
    }
    #[test]
    fn waits_for_hotkey_modifiers_before_copy() {
        let (board, host) = fixture(vec![]);
        host.released.set(false);
        let now = Instant::now();
        let mut session = CopySession::new(board, host.clone(), now);
        assert!(session.poll(now).is_none());
        assert_eq!(host.copies.get(), 0);
        host.released.set(true);
        assert!(session.poll(now).is_none());
        assert_eq!(host.copies.get(), 1);
    }
    #[test]
    fn timeout_never_captures_stale_clipboard_or_rewrites_it() {
        let (board, host) = fixture(vec![]);
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        assert!(session.poll(now + Duration::from_secs(2)).unwrap().is_err());
        assert_eq!(board.restores.get(), 0);
    }
    #[test]
    fn fresh_copy_restores_every_item_and_format() {
        let original = vec![
            vec![
                ("public.utf8-plain-text".into(), b"old".to_vec()),
                ("public.rtf".into(), b"{rtf}".to_vec()),
            ],
            vec![("public.png".into(), vec![0, 1, 255])],
        ];
        let (board, host) = fixture(original.clone());
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        board.version.set(11);
        *board.items.borrow_mut() = vec![];
        *board.text.borrow_mut() = Some("日本😀".into());
        session.poll(now + Duration::from_millis(40));
        assert_eq!(
            session
                .poll(now + Duration::from_millis(140))
                .unwrap()
                .unwrap(),
            "日本😀"
        );
        assert_eq!(*board.items.borrow(), original);
    }
    #[test]
    fn empty_original_clipboard_is_restored_to_empty() {
        let (board, host) = fixture(vec![]);
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        board.version.set(11);
        *board.items.borrow_mut() = vec![vec![("text".into(), b"copy".to_vec())]];
        session.poll(now + Duration::from_millis(40));
        session
            .poll(now + Duration::from_millis(140))
            .unwrap()
            .unwrap();
        assert!(board.items.borrow().is_empty());
    }
    #[test]
    fn non_text_copy_still_restores_clipboard() {
        let (board, host) = fixture(vec![]);
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        board.version.set(11);
        *board.text.borrow_mut() = None;
        session.poll(now + Duration::from_millis(40));
        assert!(
            session
                .poll(now + Duration::from_millis(140))
                .unwrap()
                .is_err()
        );
        assert_eq!(board.restores.get(), 1);
    }
    #[test]
    fn newer_clipboard_is_not_overwritten_and_capture_is_discarded() {
        let (board, host) = fixture(vec![]);
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        board.version.set(11);
        board.race_on_read.set(true);
        session.poll(now + Duration::from_millis(40));
        assert!(
            session
                .poll(now + Duration::from_millis(140))
                .unwrap()
                .is_err()
        );
        assert_eq!(board.restores.get(), 0);
    }
    #[test]
    fn unreadable_snapshot_prevents_copy() {
        let (board, host) = fixture(vec![]);
        board.fail_snapshot.set(true);
        let now = Instant::now();
        let mut session = CopySession::new(board, host.clone(), now);
        assert!(session.poll(now).unwrap().is_err());
        assert_eq!(host.copies.get(), 0);
    }
    #[test]
    fn focus_change_prevents_copy() {
        let (board, host) = fixture(vec![]);
        host.active.set(false);
        let now = Instant::now();
        let mut session = CopySession::new(board, host.clone(), now);
        assert!(session.poll(now).unwrap().is_err());
        assert_eq!(host.copies.get(), 0);
    }
    #[test]
    fn cancelled_observed_copy_restores_clipboard() {
        let (board, host) = fixture(vec![]);
        let now = Instant::now();
        let mut session = CopySession::new(board.clone(), host, now);
        session.poll(now);
        board.version.set(11);
        session.poll(now + Duration::from_millis(40));
        drop(session);
        assert_eq!(board.restores.get(), 1);
    }
}
