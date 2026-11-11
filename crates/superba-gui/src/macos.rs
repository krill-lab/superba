//! AppKit lifecycle hooks missing from winit's application delegate.
//!
//! Keep winit's delegate (and all its event-loop methods), adding only the
//! reopen callback. Both a Dock/Finder reopen and second-process IPC enter the
//! same request queue, including repainting a hidden window.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{ClassType, MainThreadMarker, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSObject, NSProcessInfo, NSString};

use crate::instance::Request;

type ReopenTarget = (Arc<Mutex<Vec<Request>>>, egui::Context);
thread_local! {
    static REOPEN: RefCell<Option<ReopenTarget>> = const { RefCell::new(None) };
}

/// winit constructs About/Hide/Quit from NSProcessInfo, not the window title.
pub(crate) fn set_application_name() {
    NSProcessInfo::processInfo().setProcessName(&NSString::from_str(crate::APP_TITLE));
}

extern "C-unwind" fn reopen(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut NSApplication,
    _has_visible_windows: Bool,
) -> Bool {
    REOPEN.with_borrow(|target| {
        if let Some((queue, ctx)) = target {
            queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(Request::Show);
            ctx.request_repaint();
        }
    });
    // Superba handles visibility itself; do not ask AppKit to open documents.
    Bool::NO
}

pub(crate) fn install_reopen(queue: Arc<Mutex<Vec<Request>>>, ctx: egui::Context) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        return;
    };
    let object: &AnyObject = (*delegate).as_ref();
    let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    // Use objc2 to generate the method encoding (BOOL differs between Mac
    // architectures). The implementation uses no ivars or concrete receiver.
    static METHODS: OnceLock<&'static AnyClass> = OnceLock::new();
    let methods = METHODS.get_or_init(|| {
        let mut builder = ClassBuilder::new(c"SuperbaReopenMethods", NSObject::class())
            .expect("unique Superba lifecycle class");
        // SAFETY: the selector and function signature match NSApplicationDelegate.
        unsafe {
            builder.add_method(
                selector,
                reopen
                    as extern "C-unwind" fn(*mut AnyObject, Sel, *mut NSApplication, Bool) -> Bool,
            );
        }
        builder.register()
    });
    let method = methods.instance_method(selector).expect("reopen method");
    // SAFETY: only adds this optional delegate method, with its exact AppKit
    // encoding. It accesses no receiver storage, and the delegate remains
    // owned by winit. AppKit invokes it on the main thread.
    let added = unsafe {
        objc2::ffi::class_addMethod(
            (object.class() as *const AnyClass).cast_mut(),
            selector,
            method.implementation(),
            objc2::ffi::method_getTypeEncoding(method),
        )
    };
    if !added.as_bool()
        && object
            .class()
            .instance_method(selector)
            .is_none_or(|existing| {
                !std::ptr::fn_addr_eq(existing.implementation(), method.implementation())
            })
    {
        tracing::warn!("application delegate already handles reopen");
        return;
    }
    REOPEN.with_borrow_mut(|target| *target = Some((queue, ctx)));
    // AppKit caches which optional delegate selectors are implemented.
    app.setDelegate(None);
    app.setDelegate(Some(&delegate));
}

// ---------------------------------------------------------------------------
// superba:// links (feature 140)
// ---------------------------------------------------------------------------

/// Links Launch Services delivered (an Apple event, not argv), until the app
/// takes them ([`take_links`]).
static LINKS: Mutex<Vec<Request>> = Mutex::new(Vec::new());
/// The window's context, to wake it when a link arrives.
static LINK_WAKE: OnceLock<egui::Context> = OnceLock::new();

/// `kInternetEventClass` and `kAEGetURL` (`'GURL'`).
const GURL: u32 = u32::from_be_bytes(*b"GURL");
/// `keyDirectObject` (`'----'`).
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

extern "C-unwind" fn handle_get_url(
    _this: *mut AnyObject,
    _selector: Sel,
    event: *mut AnyObject,
    _reply: *mut AnyObject,
) {
    // SAFETY: AppKit passes a valid NSAppleEventDescriptor (or nil) for the
    // duration of the call; both messages exist on it and return objects
    // (or nil) that are only read here.
    let text = unsafe {
        let Some(event) = event.as_ref() else {
            return;
        };
        let param: *mut AnyObject =
            objc2::msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        let Some(param) = param.as_ref() else {
            return;
        };
        let value: *mut NSString = objc2::msg_send![param, stringValue];
        match value.as_ref() {
            Some(s) => s.to_string(),
            None => return,
        }
    };
    let link = superba_core::ImportLink::parse(&text);
    // Never log the link: it holds the key.
    LINKS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Request::link(link));
    if let Some(ctx) = LINK_WAKE.get() {
        ctx.request_repaint();
    }
}

/// Handle `GetURL` Apple events (a `superba://` link opened anywhere) from
/// now on. Called before the event loop runs and again once the window
/// exists: AppKit installs its own `GetURL` handler while it finishes
/// launching, and the event of a start by a link comes after that.
pub(crate) fn install_url_handler() {
    static HANDLER: OnceLock<usize> = OnceLock::new();
    let handler = *HANDLER.get_or_init(|| {
        let mut builder = ClassBuilder::new(c"SuperbaUrlEvents", NSObject::class())
            .expect("unique Superba URL event class");
        // SAFETY: the selector and signature match NSAppleEventManager's
        // handler contract (`-handleEvent:withReplyEvent:`, void).
        unsafe {
            builder.add_method(
                sel!(handleGetURLEvent:withReplyEvent:),
                handle_get_url
                    as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
        }
        let class = builder.register();
        // SAFETY: plain allocation of an NSObject subclass without ivars. It
        // is never released: the handler lives for the whole process (the
        // event manager does not retain it).
        let handler: *mut AnyObject = unsafe { objc2::msg_send![class, new] };
        handler as usize
    });
    let Some(manager_class) = AnyClass::get(c"NSAppleEventManager") else {
        return;
    };
    // SAFETY: `handler` is the live object created above; the manager is
    // AppKit's shared instance; the selector exists on the handler and the
    // event class / id are FourCharCodes (u32), as the method takes them.
    unsafe {
        let manager: *mut AnyObject = objc2::msg_send![manager_class, sharedAppleEventManager];
        if let (Some(manager), Some(handler)) =
            (manager.as_ref(), (handler as *mut AnyObject).as_ref())
        {
            let () = objc2::msg_send![
                manager,
                setEventHandler: handler,
                andSelector: sel!(handleGetURLEvent:withReplyEvent:),
                forEventClass: GURL,
                andEventID: GURL
            ];
        }
    }
}

/// Wake `ctx` when a link arrives.
pub(crate) fn wake_on_links(ctx: egui::Context) {
    let _ = LINK_WAKE.set(ctx);
}

/// The links delivered so far.
pub(crate) fn take_links() -> Vec<Request> {
    std::mem::take(&mut *LINKS.lock().unwrap_or_else(|p| p.into_inner()))
}
