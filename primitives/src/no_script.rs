//! PATCH(rust-ui): fallbacks for renderers that cannot run JavaScript (dioxus-native).
//!
//! The primitives use `document::eval` for document-level listeners (Escape, outside clicks,
//! pointer tracking) and for focus traps. Where `eval` is unsupported (its `send` fails, as on
//! dioxus-native), these helpers do the same jobs with ordinary element events:
//!
//! - an overlay takes focus when it opens, and handles Escape on itself;
//! - focus leaving an element (a click elsewhere moves or clears focus) dismisses it, or, for
//!   a focus trap, brings focus back ([`FocusScope`]);
//! - a drag is followed by a window-sized layer under the pointer ([`PointerShield`]).
//!
//! Where `eval` works none of this is used, so web and desktop behave exactly as upstream.

use dioxus::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

thread_local! {
    static SUPPORTED: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Whether `document::eval` can run scripts in this renderer. Checked once per thread.
pub(crate) fn scripts_supported() -> bool {
    SUPPORTED.with(|cell| {
        if let Some(supported) = cell.get() {
            return supported;
        }
        let supported = document::eval("").send(()).is_ok();
        cell.set(Some(supported));
        supported
    })
}

/// Tracks whether focus is inside the element its handlers are attached to.
///
/// Attach [`FocusScope::focus_in`], [`FocusScope::focus_out`] and [`FocusScope::key_down`] to
/// the element's `onfocusin`, `onfocusout` and `onkeydown`. `on_leave` runs once focus has
/// moved somewhere outside the element (or nowhere), with `true` when a Tab key moved it.
#[derive(Clone, Copy)]
pub(crate) struct FocusScope {
    leaving: CopyValue<bool>,
    generation: CopyValue<u64>,
    tabbing: CopyValue<bool>,
    on_leave: Callback<bool>,
}

pub(crate) fn use_focus_scope(on_leave: impl FnMut(bool) + 'static) -> FocusScope {
    let on_leave = use_callback(on_leave);
    let leaving = use_hook(|| CopyValue::new(false));
    let generation = use_hook(|| CopyValue::new(0));
    let tabbing = use_hook(|| CopyValue::new(false));
    FocusScope {
        leaving,
        generation,
        tabbing,
        on_leave,
    }
}

impl FocusScope {
    pub(crate) fn focus_in(mut self) {
        self.leaving.set(false);
        self.tabbing.set(false);
    }

    /// Focus left an element inside. Whether it left the scope is known only after the
    /// matching `focusin` (if any) has run, so the check waits for the next task poll.
    pub(crate) fn focus_out(mut self) {
        if scripts_supported() {
            return;
        }
        self.leaving.set(true);
        let generation = self.generation.cloned() + 1;
        self.generation.set(generation);
        spawn(async move {
            if self.leaving.cloned() && self.generation.cloned() == generation {
                self.leaving.set(false);
                let by_keyboard = self.tabbing.cloned();
                self.tabbing.set(false);
                self.on_leave.call(by_keyboard);
            }
        });
    }

    pub(crate) fn key_down(mut self, event: &KeyboardEvent) {
        self.tabbing.set(event.key() == Key::Tab);
    }
}

/// Focus `element` when scripts are unsupported (the focus traps do it on the web).
pub(crate) fn focus_without_scripts(element: Option<Rc<MountedData>>) {
    if scripts_supported() {
        return;
    }
    if let Some(element) = element {
        spawn(async move {
            _ = element.set_focus(true).await;
        });
    }
}

/// Whether `event` is an Escape key the element itself should handle (scripts unsupported).
pub(crate) fn is_fallback_escape(event: &KeyboardEvent) -> bool {
    !scripts_supported() && event.key() == Key::Escape
}

/// A transparent window-sized layer that takes pointer moves and the release while a drag is
/// active, standing in for the document-level pointer listeners in `pointer.rs`. Render it
/// inside the dragged control while dragging; it renders nothing where scripts work.
#[component]
pub(crate) fn PointerShield(pointer_id: i32) -> Element {
    if scripts_supported() {
        return rsx! {};
    }
    rsx! {
        div {
            "data-pointer-shield": "true",
            style: "position: fixed; inset: 0; z-index: 2147483647; cursor: grabbing;",
            onpointermove: move |event| {
                crate::pointer::update_pointer(pointer_id, event.client_coordinates());
            },
            onpointerup: move |_| crate::pointer::remove_pointer(pointer_id),
        }
    }
}
