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
