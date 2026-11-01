//! PATCH(rust-ui): nested submenus, shared by [`crate::dropdown_menu`], [`crate::context_menu`]
//! and [`crate::menubar`].
//!
//! A submenu is a wrapper (`*Sub`) around a trigger item (`*SubTrigger`), which sits in the
//! parent menu's focus collection like any other item, and a content list (`*SubContent`),
//! which has its own collection. The content re-provides the parent menu's context with the
//! submenu's collection and open state, so the menu's ordinary items work inside it unchanged,
//! and choosing one still closes the whole menu.
//!
//! Keyboard (WAI-ARIA APG menu pattern): ArrowRight, Enter or Space on the trigger opens the
//! submenu and focuses its first item; ArrowLeft or Escape inside it closes only the submenu and
//! returns focus to the trigger; ArrowUp, ArrowDown, Home and End move within it. Pointer: the
//! submenu opens after the pointer rests on the trigger for [`OPEN_DELAY`] and closes
//! [`CLOSE_DELAY`] after the pointer leaves both the trigger and the content, unless the
//! keyboard is inside. The grace period lets the pointer cross neighbouring items on its way
//! to the content (hover intent). Focus leaving the trigger and content for somewhere outside
//! the menu closes the whole menu. Everything is element events, so it also runs where
//! `document::eval` does not (dioxus-native).

use std::rc::Rc;
use std::time::Duration;

use dioxus::prelude::*;
use dioxus_core::Task;
use dioxus_sdk_time::sleep;

use crate::collection::{
    collection_item, use_deferred_collection_focus, use_item, CollectionItem, CollectionOptions,
    CollectionPlacement, CollectionState,
};
use crate::{use_animated_open, use_controlled, use_unique_id};

/// How long the pointer rests on a trigger before its submenu opens.
pub const OPEN_DELAY: Duration = Duration::from_millis(100);
/// How long a pointer-opened submenu stays open after the pointer leaves it and its trigger.
pub const CLOSE_DELAY: Duration = Duration::from_millis(300);

/// The props shared by every `*Sub` component.
#[derive(Props, Clone, PartialEq)]
pub struct SubmenuProps {
    /// The index of the submenu's trigger among the items of the menu that contains it. It
    /// shares the numbering with that menu's other items.
    pub index: ReadSignal<usize>,

    /// Whether the submenu is open. If not provided, the submenu is uncontrolled and starts
    /// with `default_open`.
    pub open: ReadSignal<Option<bool>>,

    /// Default open state if the submenu is not controlled.
    #[props(default)]
    pub default_open: bool,

    /// Callback when the open state changes.
    #[props(default)]
    pub on_open_change: Callback<bool>,

    /// Whether the submenu is disabled. Its trigger is skipped by the arrow keys and does not
    /// open the submenu.
    #[props(default)]
    pub disabled: ReadSignal<bool>,

    /// Additional attributes for the wrapper element.
    #[props(extends = GlobalAttributes)]
    pub attributes: Vec<Attribute>,

    /// The children: a `*SubTrigger` and a `*SubContent`.
    pub children: Element,
}

/// The props shared by every `*SubTrigger` component.
#[derive(Props, Clone, PartialEq)]
pub struct SubmenuTriggerProps {
    /// Additional attributes for the trigger element.
    #[props(extends = GlobalAttributes)]
    pub attributes: Vec<Attribute>,

    /// The children of the trigger, usually a label and a chevron.
    pub children: Element,
}

/// The props shared by every `*SubContent` component.
#[derive(Props, Clone, PartialEq)]
pub struct SubmenuContentProps {
    /// The id of the content element. If not provided, a unique id is generated.
    pub id: ReadSignal<Option<String>>,

    /// Additional attributes for the content element.
    #[props(extends = GlobalAttributes)]
    pub attributes: Vec<Attribute>,

    /// The children: the menu's ordinary items, and possibly more submenus.
    pub children: Element,
}

/// The state of one submenu, provided as context to its trigger and content.
#[derive(Clone, Copy)]
pub(crate) struct Submenu {
    /// Open, and the menu that holds the trigger is open too.
    pub(crate) open: Memo<bool>,
    set_open: Callback<bool>,
    /// The submenu's own items.
    pub(crate) focus: CollectionState,
    /// The collection the trigger is an item of.
    parent: CollectionState,
    index: ReadSignal<usize>,
    disabled: Memo<bool>,
    pub(crate) trigger_id: Signal<String>,
    pub(crate) content_id: Signal<String>,
    trigger_ref: Signal<Option<Rc<MountedData>>>,
    initial_focus: Signal<Option<CollectionPlacement>>,
    timer: Signal<Option<Task>>,
    inside: CopyValue<bool>,
    generation: CopyValue<u64>,
    close_tree: Callback<()>,
}

/// Set up a submenu. `parent` and `parent_open` describe the menu that holds the trigger;
/// `close_tree` closes the whole menu (focus left it).
pub(crate) fn use_submenu(
    props: &SubmenuProps,
    parent: CollectionState,
    parent_open: Memo<bool>,
    parent_disabled: impl Fn() -> bool + 'static,
    close_tree: impl FnMut(()) + 'static,
) -> Submenu {
    let (open_state, set_open) =
        use_controlled(props.open, props.default_open, props.on_open_change);
    let own_disabled = props.disabled;
    let disabled = use_memo(move || parent_disabled() || own_disabled());
    let open = use_memo(move || open_state() && parent_open() && !disabled());
    let focus =
        use_hook(|| CollectionState::new(parent.loop_signal(), CollectionOptions::default()));
    let index = props.index;
    let close_tree = use_callback(close_tree);

    let sub = Submenu {
        open,
        set_open,
        focus,
        parent,
        index,
        disabled,
        trigger_id: use_unique_id(),
        content_id: use_unique_id(),
        trigger_ref: use_signal(|| None),
        initial_focus: use_signal(|| None),
        timer: use_signal(|| None),
        inside: use_hook(|| CopyValue::new(false)),
        generation: use_hook(|| CopyValue::new(0)),
        close_tree,
    };

    // A closed parent closes the submenu, so it does not reappear when the parent reopens.
    use_effect(move || {
        if !parent_open() && *open_state.peek() {
            set_open.call(false);
        }
    });
    // Focus moving to another item of the parent menu closes the submenu.
    use_effect(move || {
        let focused = parent.focused_index();
        if focused.is_some_and(|focused| focused != index()) && *open_state.peek() {
            set_open.call(false);
        }
    });
    let mut sub_focus = focus;
    let mut initial_focus = sub.initial_focus;
    use_effect(move || {
        if !open() {
            sub_focus.clear_focus();
            initial_focus.set(None);
        }
    });

    sub
}

impl Submenu {
    fn cancel_timer(mut self) {
        if let Some(task) = self.timer.write().take() {
            task.cancel();
        }
    }

    fn open_now(mut self, placement: Option<CollectionPlacement>) {
        self.cancel_timer();
        if (self.disabled)() {
            return;
        }
        self.initial_focus.set(placement);
        if !(self.open)() {
            self.set_open.call(true);
        } else if let Some(placement) = placement {
            self.focus.try_focus_placement(placement);
        }
    }

    /// Close the submenu and put focus back on its trigger.
    fn close_to_trigger(mut self) {
        self.cancel_timer();
        self.focus.clear_focus();
        self.set_open.call(false);
        self.parent.set_focus(Some((self.index)()));
        if let Some(trigger) = (self.trigger_ref)() {
            spawn(async move {
                _ = trigger.set_focus(true).await;
            });
        }
    }

    fn schedule(mut self, open: bool) {
        self.cancel_timer();
        let task = spawn(async move {
            sleep(if open { OPEN_DELAY } else { CLOSE_DELAY }).await;
            self.timer.set(None);
            if open {
                self.open_now(None);
            } else if !self.focus.any_focused() {
                // Keyboard users inside the submenu keep it.
                self.set_open.call(false);
            }
        });
        self.timer.set(Some(task));
    }

    fn pointer_enter(self, event: &PointerEvent, on_trigger: bool) {
        if event.pointer_type() != "mouse" {
            return;
        }
        self.cancel_timer();
        if on_trigger && !(self.open)() && !(self.disabled)() {
            self.schedule(true);
        }
    }

    fn pointer_leave(self, event: &PointerEvent) {
        if event.pointer_type() != "mouse" {
            return;
        }
        self.cancel_timer();
        if (self.open)() && !self.focus.any_focused() {
            self.schedule(false);
        }
    }

    fn focus_in(mut self) {
        self.inside.set(true);
    }

    /// Focus left the trigger or the content. Whether it left both is known only after the
    /// matching `focusin` (if any), so the check waits for the next timer tick.
    fn focus_out(mut self) {
        self.inside.set(false);
        let generation = self.generation.cloned() + 1;
        self.generation.set(generation);
        spawn(async move {
            sleep(Duration::ZERO).await;
            if self.inside.cloned() || self.generation.cloned() != generation {
                return;
            }
            // Focus moved to another item of the parent menu: the effect in `use_submenu`
            // closes the submenu. Otherwise it left the menu.
            if self.parent.focused_index() == Some((self.index)()) {
                self.cancel_timer();
                self.parent.clear_focus();
                self.set_open.call(false);
                self.close_tree.call(());
            }
        });
    }
}

/// Attributes for the `*Sub` wrapper element, a plain box around the trigger and the content.
pub(crate) fn sub_attributes(sub: Submenu) -> Vec<Attribute> {
    dioxus_attributes::attributes!(div {
        "data-state": if (sub.open)() { "open" } else { "closed" },
        "data-disabled": (sub.disabled)(),
        onfocusin: move |_| sub.focus_in(),
        onfocusout: move |_| sub.focus_out(),
    })
}

/// Register the trigger as an item of the parent menu. Call once per render, in the
/// `*SubTrigger` component.
pub(crate) fn use_sub_trigger(sub: Submenu) -> CollectionItem {
    let disabled = sub.disabled;
    use_item(collection_item(sub.parent, sub.index).disabled(move || disabled()))
}

/// How the trigger describes itself: the role of its sibling items, and whether that role
/// may carry `aria-haspopup` and `aria-expanded` (a `menuitem` may, an `option` may not).
#[derive(Clone, Copy)]
pub(crate) struct TriggerRole {
    pub(crate) role: &'static str,
    pub(crate) popup: Option<&'static str>,
}

/// Attributes for the `*SubTrigger` element.
pub(crate) fn sub_trigger_attributes(
    sub: Submenu,
    item: CollectionItem,
    role: TriggerRole,
) -> Vec<Attribute> {
    let open = (sub.open)();
    let disabled = (sub.disabled)();
    let focused = item.focused();
    let mut item_mounted = item.onmounted();
    let mut trigger_ref = sub.trigger_ref;
    let mut parent = sub.parent;
    let index = sub.index;
    // `aria-controls` only while the content is in the DOM, so it never dangles.
    let controls = open.then(|| sub.content_id.cloned());
    let expanded = role.popup.map(|_| if open { "true" } else { "false" });

    dioxus_attributes::attributes!(div {
        id: sub.trigger_id,
        role: role.role,
        tabindex: if focused { "0" } else { "-1" },
        aria_haspopup: role.popup,
        aria_expanded: expanded,
        aria_controls: controls,
        aria_disabled: disabled,
        "data-state": if open { "open" } else { "closed" },
        "data-disabled": disabled,
        onmounted: move |event: MountedEvent| {
            trigger_ref.set(Some(event.data()));
            item_mounted(event);
        },
        // Keep focus where it is on press; the click below moves it to the trigger.
        onpointerdown: move |event: PointerEvent| event.prevent_default(),
        onclick: move |event: MouseEvent| {
            event.stop_propagation();
            if !(sub.disabled)() {
                parent.set_focus(Some(index()));
                sub.open_now(None);
            }
        },
        onpointerenter: move |event: PointerEvent| sub.pointer_enter(&event, true),
        onpointerleave: move |event: PointerEvent| sub.pointer_leave(&event),
        onkeydown: move |event: KeyboardEvent| {
            let key = event.key();
            if matches!(key, Key::ArrowRight | Key::Enter) || key == Key::Character(" ".into()) {
                event.prevent_default();
                event.stop_propagation();
                sub.open_now(Some(CollectionPlacement::First));
            }
        },
    })
}

/// Show the content while open (and while its exit animation runs), and move focus to its
/// first item when the keyboard opened it. Call once per render, in `*SubContent`.
pub(crate) fn use_sub_content(sub: Submenu) -> impl Fn() -> bool + Copy {
    let render = use_animated_open(sub.content_id, sub.open);
    use_deferred_collection_focus(sub.focus, sub.initial_focus, render);
    render
}

/// Attributes for the `*SubContent` element: `menu` in a menu, `group` in a listbox.
pub(crate) fn sub_content_attributes(sub: Submenu, role: &'static str) -> Vec<Attribute> {
    let mut focus = sub.focus;
    let orientation = (role == "menu").then_some("vertical");
    dioxus_attributes::attributes!(div {
        id: sub.content_id,
        role,
        aria_orientation: orientation,
        aria_labelledby: sub.trigger_id,
        "data-state": if (sub.open)() { "open" } else { "closed" },
        onpointerdown: move |event: PointerEvent| {
            // As in the dropdown content: keep focus (and the menu) until the click lands.
            event.prevent_default();
        },
        onpointerenter: move |event: PointerEvent| sub.pointer_enter(&event, false),
        onpointerleave: move |event: PointerEvent| sub.pointer_leave(&event),
        onkeydown: move |event: KeyboardEvent| {
            match event.key() {
                Key::ArrowDown => focus.focus_next(),
                Key::ArrowUp => focus.focus_prev(),
                Key::Home => focus.focus_first(),
                Key::End => focus.focus_last(),
                Key::ArrowLeft | Key::Escape => sub.close_to_trigger(),
                // ArrowRight on an ordinary item, Tab and the rest belong to the parent menu
                // (a menubar moves to its next menu).
                _ => return,
            }
            event.prevent_default();
            event.stop_propagation();
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_leave_room_to_cross_items() {
        // Hover intent: the close grace must be longer than the open delay, or a pointer
        // moving diagonally to the content would close it on the way.
        assert!(CLOSE_DELAY > OPEN_DELAY * 2);
    }
}
