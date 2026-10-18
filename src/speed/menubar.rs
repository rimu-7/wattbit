//! The menu bar item, built directly on AppKit.
//!
//! `wattbit speed on` runs a small agent in the user's login session, and this
//! module turns it into a status item next to the other menu bar extras. It
//! talks to AppKit through the Objective-C runtime with hand-written
//! declarations, the same way the rest of wattbit talks to IOKit.
//!
//! The item shows the live rates as its title (`↓ 1.23 MB/s ↑ 340 KB/s`).
//! Clicking it opens a small menu with today's and the month's totals and a
//! way to turn the meter off. When a newer wattbit release is known, the
//! menu opens with an update warning, the exact command to run in
//! Terminal, and an item that copies that command to the clipboard. macOS remembers where a status item was
//! dragged while it has an autosave name, so once the user moves wattbit next
//! to the other icons it stays there.
//!
//! Everything here is optional: without a logged-in Aqua session (for
//! example over SSH) [`Ui::create`] reports why the menu bar is unavailable
//! and the agent keeps recording usage headlessly.

use std::ffi::c_void;

use core_foundation::base::{CFType, TCFType};
use core_foundation::string::CFString;

use super::store::RETENTION_DAYS;
use super::Display;
use crate::update::{Version, INSTALL_COMMAND};

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    /// `NULL` outside a logged-in GUI session.
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const std::ffi::c_char) -> Id;
    fn sel_registerName(name: *const std::ffi::c_char) -> Sel;
    fn objc_msgSend();
    fn objc_retain(object: Id) -> Id;
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
    fn objc_allocateClassPair(superclass: Id, name: *const std::ffi::c_char, extra: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    fn class_addMethod(
        class: Id,
        selector: Sel,
        implementation: *const c_void,
        types: *const std::ffi::c_char,
    ) -> i8;
}

/// Calls `objc_msgSend` through the exact signature of the selected method.
///
/// SAFETY: the caller must pass the true signature of the method named by
/// `$selector` on `$receiver`; the Objective-C runtime does not check it.
macro_rules! msg_send {
    ($signature:ty, $receiver:expr, $selector:expr $(, $argument:expr)*) => {{
        // SAFETY: transmuting the runtime's entry point to the caller's
        // signature and calling it is the documented way to use
        // `objc_msgSend`; the caller guarantees the signature and the
        // receiver/selector pair match.
        let function: $signature = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: as above.
        unsafe { function($receiver, $selector $(, $argument)*) }
    }};
}

/// SAFETY: `name` must be a valid class name.
unsafe fn class(name: &std::ffi::CStr) -> Id {
    // SAFETY: passed through from the caller.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// SAFETY: `object` must be a valid Objective-C object (or `NULL`, which
/// `objc_retain` tolerates).
unsafe fn retain(object: Id) -> Id {
    // SAFETY: passed through from the caller.
    unsafe { objc_retain(object) }
}

/// Registers (or looks up) the selector named `name`.
fn selector(name: &std::ffi::CStr) -> Sel {
    // SAFETY: sel_registerName accepts any NUL-terminated string.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// A `NSString` for `text`; toll-free bridged from a `CFString`.
fn nsstring(text: &str) -> CFString {
    CFString::new(text)
}

/// Calls `f` with an `NSString` for `text` that stays alive for the whole
/// call. AppKit retains or copies the string as needed before it is
/// released again, so the pointer must not outlive `f`.
fn with_nsstring<T>(text: &str, f: impl FnOnce(Id) -> T) -> T {
    let string = nsstring(text);
    f(string.as_CFTypeRef() as Id)
}

/// SAFETY: `receiver` must be a live object that responds to `selector`.
unsafe fn send_id(receiver: Id, selector: Sel) -> Id {
    msg_send!(unsafe extern "C" fn(Id, Sel) -> Id, receiver, selector)
}

/// SAFETY: `receiver` must be a live object that responds to `selector`.
unsafe fn send_void(receiver: Id, selector: Sel) {
    msg_send!(unsafe extern "C" fn(Id, Sel), receiver, selector)
}

/// SAFETY: `receiver` must be a live object that responds to `selector`
/// with one object argument.
unsafe fn send_void_of_id(receiver: Id, selector: Sel, argument: Id) {
    msg_send!(
        unsafe extern "C" fn(Id, Sel, Id),
        receiver,
        selector,
        argument
    )
}

/// SAFETY: `receiver` must be a live object that responds to `selector`
/// with one `BOOL` argument.
unsafe fn send_void_of_bool(receiver: Id, selector: Sel, argument: bool) {
    msg_send!(
        unsafe extern "C" fn(Id, Sel, i8),
        receiver,
        selector,
        i8::from(argument)
    )
}

/// SAFETY: `receiver` must be a live object that responds to `selector`,
/// which must return `NSInteger` and take no arguments.
unsafe fn send_isize(receiver: Id, selector: Sel) -> isize {
    msg_send!(unsafe extern "C" fn(Id, Sel) -> isize, receiver, selector)
}

/// SAFETY: `receiver` must be a live object that responds to `selector`
/// with two object arguments, returning `BOOL`.
unsafe fn send_bool_of_two(receiver: Id, selector: Sel, first: Id, second: Id) -> bool {
    msg_send!(
        unsafe extern "C" fn(Id, Sel, Id, Id) -> i8,
        receiver,
        selector,
        first,
        second
    ) != 0
}

/// SAFETY: `receiver` must be a live object that responds to `selector`
/// with one `double` argument.
unsafe fn send_id_of_f64(receiver: Id, selector: Sel, argument: f64) -> Id {
    msg_send!(
        unsafe extern "C" fn(Id, Sel, f64) -> Id,
        receiver,
        selector,
        argument
    )
}

/// An `NSRect` as AppKit lays it out (points, origin bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct NSRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// SAFETY: `receiver` must be a live object that responds to `selector`,
/// which must return `BOOL` and take no arguments.
unsafe fn send_bool(receiver: Id, selector: Sel) -> bool {
    msg_send!(unsafe extern "C" fn(Id, Sel) -> i8, receiver, selector) != 0
}

/// SAFETY: `receiver` must be a live object that responds to `selector`,
/// which must return an `NSRect` and take no arguments.
unsafe fn send_rect(receiver: Id, selector: Sel) -> NSRect {
    msg_send!(unsafe extern "C" fn(Id, Sel) -> NSRect, receiver, selector)
}

/// SAFETY: `receiver` must be a live object that responds to `selector`
/// with one `NSInteger` argument, returning `BOOL`.
unsafe fn send_bool_of_isize(receiver: Id, selector: Sel, argument: isize) -> bool {
    msg_send!(
        unsafe extern "C" fn(Id, Sel, isize) -> i8,
        receiver,
        selector,
        argument
    ) != 0
}

/// SAFETY: `receiver` must be a live object that responds to `selector`
/// with three arguments as declared.
unsafe fn send_id_of_three(receiver: Id, selector: Sel, first: Id, second: Sel, third: Id) -> Id {
    msg_send!(
        unsafe extern "C" fn(Id, Sel, Id, Sel, Id) -> Id,
        receiver,
        selector,
        first,
        second,
        third
    )
}

/// Whether the calling process runs inside a logged-in GUI session.
pub fn gui_session() -> bool {
    // SAFETY: CGSessionCopyCurrentDictionary has no preconditions; the
    // result is a +1 dictionary or NULL, released by dropping `CFType`.
    let dictionary = unsafe { CGSessionCopyCurrentDictionary() };
    if dictionary.is_null() {
        return false;
    }
    // SAFETY: non-null result follows the Create rule.
    drop(unsafe { CFType::wrap_under_create_rule(dictionary) });
    true
}

/// The action target for menu items. The selector sets the shared stop flag.
extern "C" fn wattbit_turn_off(_this: Id, _command: Sel, _sender: Id) {
    super::request_stop();
}

/// The *Copy Update Command* item: puts [`INSTALL_COMMAND`] on the
/// clipboard, ready to paste into Terminal.
extern "C" fn wattbit_copy_update_command(_this: Id, _command: Sel, _sender: Id) {
    copy_to_clipboard(INSTALL_COMMAND);
}

/// Replaces the general pasteboard's contents with `text`.
fn copy_to_clipboard(text: &str) -> bool {
    // SAFETY: NSPasteboard's class methods and instance methods are called
    // with their declared signatures; the strings live for the call.
    unsafe {
        let pasteboard = send_id(class(c"NSPasteboard"), selector(c"generalPasteboard"));
        if pasteboard.is_null() {
            return false;
        }
        send_isize(pasteboard, selector(c"clearContents"));
        with_nsstring(text, |text| {
            // NSPasteboardTypeString's value.
            with_nsstring("public.utf8-plain-text", |kind| {
                send_bool_of_two(pasteboard, selector(c"setString:forType:"), text, kind)
            })
        })
    }
}

/// Creates the target object for the menu's action items, if the runtime
/// cooperates.
fn menu_target() -> Option<Id> {
    // SAFETY: `class` is called with valid class names, and the added
    // method's implementation matches its `v@:@` (void, self, _cmd, id)
    // encoding.
    unsafe {
        let existing = class(c"WattbitSpeedMenuTarget");
        let target_class = if existing.is_null() {
            let superclass = class(c"NSObject");
            if superclass.is_null() {
                return None;
            }
            let created = objc_allocateClassPair(superclass, c"WattbitSpeedMenuTarget".as_ptr(), 0);
            if created.is_null() {
                return None;
            }
            let methods: [(Sel, extern "C" fn(Id, Sel, Id)); 2] = [
                (selector(c"wattbitTurnOff:"), wattbit_turn_off),
                (
                    selector(c"wattbitCopyUpdateCommand:"),
                    wattbit_copy_update_command,
                ),
            ];
            for (name, implementation) in methods {
                if class_addMethod(
                    created,
                    name,
                    implementation as *const c_void,
                    c"v@:@".as_ptr(),
                ) == 0
                {
                    return None;
                }
            }
            objc_registerClassPair(created);
            created
        } else {
            existing
        };
        let target = send_id(send_id(target_class, selector(c"alloc")), selector(c"init"));
        (!target.is_null()).then_some(target)
    }
}

/// The live menu bar item. Drop or exit to remove it.
#[derive(Debug)]
pub struct Ui {
    item: Id,
    button: Id,
    download_item: Id,
    upload_item: Id,
    today_item: Id,
    window_item: Id,
    /// The update section, hidden until a newer release is known.
    update_items: [Id; 5],
    update_title_item: Id,
    shown_update: Option<Version>,
    run_loop: Id,
    last_title: String,
    /// Kept alive for the process lifetime.
    _menu: Id,
    _target: Id,
}

impl Ui {
    /// Asks AppKit for a status item. Fails when there is no GUI session.
    pub fn create() -> Result<Self, String> {
        if !gui_session() {
            return Err("no GUI session (the menu bar needs a logged-in user)".into());
        }
        // SAFETY: every call below uses an AppKit class or instance with a
        // selector it implements. The autorelease pool wraps the setup so
        // temporary objects do not accumulate.
        let pool = unsafe { objc_autoreleasePoolPush() };
        let created = Self::create_inner().inspect_err(|_| {
            // SAFETY: `pool` came from objc_autoreleasePoolPush.
            unsafe { objc_autoreleasePoolPop(pool) };
        })?;
        // SAFETY: `pool` came from objc_autoreleasePoolPush.
        unsafe { objc_autoreleasePoolPop(pool) };
        Ok(created)
    }

    fn create_inner() -> Result<Self, String> {
        // SAFETY: all receivers are live AppKit objects; selectors are
        // declared by the corresponding headers.
        unsafe {
            let app = send_id(class(c"NSApplication"), selector(c"sharedApplication"));
            if app.is_null() {
                return Err("NSApplication is unavailable".into());
            }
            if !send_bool_of_isize(app, selector(c"setActivationPolicy:"), 1) {
                // 1 is NSApplicationActivationPolicyAccessory: no Dock icon.
                return Err("cannot become a menu bar accessory".into());
            }
            send_void(app, selector(c"finishLaunching"));

            let status_bar = send_id(class(c"NSStatusBar"), selector(c"systemStatusBar"));
            if status_bar.is_null() {
                return Err("NSStatusBar is unavailable".into());
            }
            // -1.0 is NSVariableStatusItemLength. AppKit hands out
            // autoreleased objects; retain the ones the `Ui` keeps, because
            // the pool pushed by `create` is popped before they are used.
            let item = retain(send_id_of_f64(
                status_bar,
                selector(c"statusItemWithLength:"),
                -1.0,
            ));
            if item.is_null() {
                return Err("cannot create a status item".into());
            }
            // Persist the item's position and visibility across restarts,
            // so a user-dragged place next to the system icons sticks.
            with_nsstring("com.wattbit.speed", |name| {
                send_void_of_id(item, selector(c"setAutosaveName:"), name)
            });

            let button = retain(send_id(item, selector(c"button")));
            if button.is_null() {
                return Err("the status item has no button".into());
            }
            with_nsstring("wattbit speed", |title| {
                send_void_of_id(button, selector(c"setTitle:"), title)
            });

            let menu = send_id(
                send_id(class(c"NSMenu"), selector(c"alloc")),
                selector(c"init"),
            );
            if menu.is_null() {
                return Err("cannot create the status menu".into());
            }
            let add = |title: &str, action: Sel| {
                with_nsstring(title, |title| {
                    with_nsstring("", |empty| {
                        retain(send_id_of_three(
                            menu,
                            selector(c"addItemWithTitle:action:keyEquivalent:"),
                            title,
                            action,
                            empty,
                        ))
                    })
                })
            };
            let separator = || {
                let item = retain(send_id(class(c"NSMenuItem"), selector(c"separatorItem")));
                send_void_of_id(menu, selector(c"addItem:"), item);
                item
            };

            let update_title_item = add("Update available", selector(c"wattbitCopyUpdateCommand:"));
            let update_items = [
                update_title_item,
                add("Run this in Terminal to update:", std::ptr::null_mut()),
                add(
                    &format!("    {INSTALL_COMMAND}"),
                    selector(c"wattbitCopyUpdateCommand:"),
                ),
                add(
                    "Copy Update Command",
                    selector(c"wattbitCopyUpdateCommand:"),
                ),
                separator(),
            ];
            for item in update_items {
                send_void_of_bool(item, selector(c"setHidden:"), true);
            }

            let download_item = add("Download: –", std::ptr::null_mut());
            let upload_item = add("Upload: –", std::ptr::null_mut());
            separator();
            let today_item = add("Today: –", std::ptr::null_mut());
            let window_item = add(
                &format!("Last {RETENTION_DAYS} days: –"),
                std::ptr::null_mut(),
            );
            separator();
            let turn_off = add("Turn off speed meter", selector(c"wattbitTurnOff:"));
            let target = menu_target().ok_or("cannot register the menu action")?;
            send_void_of_id(turn_off, selector(c"setTarget:"), target);
            send_void_of_id(update_items[0], selector(c"setTarget:"), target);
            send_void_of_id(update_items[2], selector(c"setTarget:"), target);
            send_void_of_id(update_items[3], selector(c"setTarget:"), target);
            send_void_of_id(item, selector(c"setMenu:"), menu);

            let run_loop = retain(send_id(class(c"NSRunLoop"), selector(c"currentRunLoop")));
            if run_loop.is_null() {
                return Err("the main run loop is unavailable".into());
            }

            Ok(Self {
                item,
                button,
                download_item,
                upload_item,
                today_item,
                window_item,
                update_items,
                update_title_item,
                shown_update: None,
                run_loop,
                last_title: String::new(),
                _menu: menu,
                _target: target,
            })
        }
    }

    /// Where AppKit placed the item, for logs and troubleshooting.
    pub fn placement(&self) -> String {
        // SAFETY: all receivers are live AppKit objects created above.
        unsafe {
            let visible = send_bool(self.item, selector(c"isVisible"));
            let window = send_id(self.button, selector(c"window"));
            if window.is_null() {
                return format!("visible={visible}, not placed yet");
            }
            let frame = send_rect(window, selector(c"frame"));
            format!(
                "visible={visible}, at ({:.0}, {:.0}), {:.0}x{:.0} points",
                frame.x, frame.y, frame.width, frame.height
            )
        }
    }

    /// Paints the latest [`Display`] values.
    pub fn refresh(&mut self, display: &Display) {
        let title = super::format::rates(display.download_bps, display.upload_bps);
        // SAFETY: all receivers are live AppKit objects created above.
        unsafe {
            if title != self.last_title {
                with_nsstring(&title, |title| {
                    send_void_of_id(self.button, selector(c"setTitle:"), title)
                });
                self.last_title = title;
            }
            let set = |item: Id, text: String| {
                with_nsstring(&text, |text| {
                    send_void_of_id(item, selector(c"setTitle:"), text)
                });
            };
            set(
                self.download_item,
                format!("Download: {}", super::format::rate(display.download_bps)),
            );
            set(
                self.upload_item,
                format!("Upload: {}", super::format::rate(display.upload_bps)),
            );
            set(
                self.today_item,
                format!(
                    "Today: ↓ {} ↑ {}",
                    super::format::bytes(display.today_download_bytes),
                    super::format::bytes(display.today_upload_bytes)
                ),
            );
            set(
                self.window_item,
                format!(
                    "Last {RETENTION_DAYS} days: ↓ {} ↑ {}",
                    super::format::bytes(display.window_download_bytes),
                    super::format::bytes(display.window_upload_bytes)
                ),
            );
            if display.update != self.shown_update {
                if let Some(latest) = display.update {
                    set(self.update_title_item, update_title(latest));
                }
                for item in self.update_items {
                    send_void_of_bool(item, selector(c"setHidden:"), display.update.is_none());
                }
                self.shown_update = display.update;
            }
        }
    }

    /// Pumps the main run loop for up to `seconds`, keeping the status item
    /// responsive while the agent samples in the background.
    pub fn pump(&self, seconds: f64) {
        // SAFETY: `run_loop` is the main thread's run loop and
        // `initWithTimeIntervalSinceNow:` returns an owned NSDate, released
        // after use; the run loop retains it while running.
        unsafe {
            let date = send_id_of_f64(
                send_id(class(c"NSDate"), selector(c"alloc")),
                selector(c"initWithTimeIntervalSinceNow:"),
                seconds,
            );
            send_void_of_id(self.run_loop, selector(c"runUntilDate:"), date);
            send_void(date, selector(c"release"));
        }
    }
}

/// The first line of the update section.
fn update_title(latest: Version) -> String {
    format!(
        "⚠️ Update available: wattbit {latest} (you have {})",
        Version::current()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_titles_name_both_versions() {
        let latest = Version {
            major: 9,
            minor: 8,
            patch: 7,
        };
        assert_eq!(
            update_title(latest),
            format!(
                "⚠️ Update available: wattbit 9.8.7 (you have {})",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    /// Touches the real clipboard (and restores it), so it only runs on
    /// request: `cargo test --lib copies_to_the_clipboard -- --ignored`.
    #[test]
    #[ignore = "uses the real clipboard"]
    fn copies_to_the_clipboard() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let saved = Command::new("/usr/bin/pbpaste").output().unwrap().stdout;
        assert!(copy_to_clipboard(INSTALL_COMMAND));
        let pasted = Command::new("/usr/bin/pbpaste").output().unwrap().stdout;
        let mut restore = Command::new("/usr/bin/pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        restore.stdin.take().unwrap().write_all(&saved).unwrap();
        restore.wait().unwrap();
        assert_eq!(String::from_utf8(pasted).unwrap(), INSTALL_COMMAND);
    }

    #[test]
    fn session_probe_never_panics() {
        let _ = gui_session();
    }

    #[test]
    fn strings_are_utf8() {
        let text = nsstring("↓ 1.23 MB/s ↑ 340 KB/s");
        assert!(!text.as_CFTypeRef().is_null());
    }
}
