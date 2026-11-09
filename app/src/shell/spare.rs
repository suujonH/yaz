//! The spare window: a hidden window created in advance, whose WebView and page are already
//! loaded. Hovering an entry that opens a window renders that window in it without showing it
//! ([`prepare`]); opening it then only has to show it ([`show`]). Once shown, the spare window is an
//! ordinary window and a new spare window is created in the background.

use super::state::AppShared;
use super::window::{
    ShowWhenReady, WindowArgs, WindowKey, WindowSpec, activate, place_window, register_window,
    window_config,
};
use crate::state::{AccountScope, AppCommand, send_command};
use dioxus::desktop::{DesktopContext, WeakDesktopContext, use_window, window};
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tokio::sync::watch;

/// Whether the window is shown. False while it is rendered in advance in the spare window: pages
/// hold back effects the user should only see when the window is open (marking as read, opening
/// the conversation).
#[derive(Clone, Copy)]
struct Presented(Signal<bool>);

/// Always true outside the spare window.
pub fn use_presented() -> ReadSignal<bool> {
    let provided = use_hook(try_consume_context::<Presented>);
    let always = use_signal(|| true);
    provided.map_or(always.into(), |p| p.0.into())
}

/// State of a page shared between the window that opens it and the spare window.
struct PageState {
    presented: watch::Sender<bool>,
    mounted: Cell<bool>,
    show_on_mount: Cell<bool>,
}

/// A page rendered in the spare window. Compared by `id`: each prepare is a new page.
#[derive(Clone)]
struct SparePage {
    id: u64,
    root: RootFn,
    provide: Rc<dyn Fn()>,
    state: Rc<PageState>,
}

impl PartialEq for SparePage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

/// Root component of a page. Never changes within one page host (the host is keyed by page id).
#[derive(Clone, Copy)]
struct RootFn(fn() -> Element);

impl PartialEq for RootFn {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// The page currently rendered and how it was asked for, to tell whether opening can use it.
struct Prepared {
    key: String,
    args: Box<dyn Any>,
    /// The `watch::Sender<T>` of the window parameters.
    args_tx: Box<dyn Any>,
    state: Rc<PageState>,
}

#[derive(Default)]
enum Spare {
    #[default]
    None,
    Creating,
    Ready {
        window: WeakDesktopContext,
        pages: watch::Sender<Option<SparePage>>,
        prepared: Option<Prepared>,
    },
}

thread_local! {
    static SPARE: RefCell<Spare> = RefCell::default();
    static NEXT_PAGE: Cell<u64> = const { Cell::new(0) };
}

/// Channel through which the spare window receives the page to render.
#[derive(Clone)]
struct SpareChannel(watch::Sender<Option<SparePage>>);

/// Creates the spare window when there is none. Called from the main window.
pub fn create_spare() {
    if !SPARE.with_borrow(|s| matches!(s, Spare::None)) {
        return;
    }
    let shared = consume_context::<AppShared>();
    let pages = watch::Sender::new(None);
    let dom = VirtualDom::new(SpareRoot)
        .with_root_context(shared.clone())
        .with_root_context(shared.theme.clone())
        .with_root_context(SpareChannel(pages.clone()));
    let mut spec = WindowSpec::new("spare", "YaZ", (800.0, 600.0));
    spec.centered = false;
    spec.remember_bounds = false;
    let pending = window().new_window(dom, window_config(&spec, &shared));
    SPARE.set(Spare::Creating);
    // Ready once its page has loaded ([`SpareRoot`])
    spawn_forever(async move {
        if pending.try_resolve().await.is_err() {
            SPARE.with_borrow_mut(|s| {
                if matches!(s, Spare::Creating) {
                    *s = Spare::None;
                }
            });
        }
    });
}

/// Closes the spare window (logout, exit).
pub fn close_spare() {
    if let Spare::Ready { window, .. } = SPARE.take()
        && let Some(window) = window.upgrade()
    {
        window.close();
    }
}

/// Renders a page in the spare window for the window `spec` without showing it. Does nothing when
/// the same page is already rendered or the spare window is not ready.
pub fn prepare<T: Clone + PartialEq + 'static>(
    spec: &WindowSpec,
    root: fn() -> Element,
    args: &T,
    account: Option<i64>,
) {
    let Some(window) = ready_window() else {
        // Closed by a logout, or its creation failed: create it again for the next hover
        create_spare();
        return;
    };
    let same = SPARE.with_borrow(|s| match s {
        Spare::Ready {
            prepared: Some(p), ..
        } => is_same(p, spec, args),
        _ => false,
    });
    if !same {
        render(&window, spec, root, args, account);
    }
}

/// Shows the window `spec` using the spare window: the page rendered by [`prepare`] when it is the
/// same, otherwise it is rendered now and shown once mounted. Returns false when there is no spare
/// window; the caller then creates the window as usual.
pub fn show<T: Clone + PartialEq + 'static>(
    spec: &WindowSpec,
    root: fn() -> Element,
    args: &T,
    account: Option<i64>,
) -> bool {
    let Some(window) = ready_window() else {
        return false;
    };
    let reuse = SPARE.with_borrow(|s| match s {
        Spare::Ready {
            prepared: Some(p), ..
        } => is_same(p, spec, args),
        _ => false,
    });
    if !reuse {
        render(&window, spec, root, args, account);
    }
    let Spare::Ready {
        prepared: Some(prepared),
        ..
    } = SPARE.take()
    else {
        return false;
    };
    register_window(spec.key.clone(), Rc::downgrade(&window), prepared.args_tx);
    prepared.state.presented.send_replace(true);
    if prepared.state.mounted.get() {
        activate(&window);
    } else {
        prepared.state.show_on_mount.set(true);
    }
    send_command(AppCommand::RefillSpareWindow);
    true
}

fn ready_window() -> Option<DesktopContext> {
    SPARE.with_borrow_mut(|s| {
        let Spare::Ready { window, .. } = s else {
            return None;
        };
        let live = window.upgrade();
        if live.is_none() {
            *s = Spare::None;
        }
        live
    })
}

fn is_same<T: PartialEq + 'static>(prepared: &Prepared, spec: &WindowSpec, args: &T) -> bool {
    prepared.key == spec.key && prepared.args.downcast_ref::<T>() == Some(args)
}

/// Moves the spare window into place and replaces its page.
fn render<T: Clone + PartialEq + 'static>(
    window: &DesktopContext,
    spec: &WindowSpec,
    root: fn() -> Element,
    args: &T,
    account: Option<i64>,
) {
    place_window(window, spec, &consume_context::<AppShared>());
    let id = NEXT_PAGE.get() + 1;
    NEXT_PAGE.set(id);
    let args_tx = watch::Sender::new(args.clone());
    let state = Rc::new(PageState {
        presented: watch::Sender::new(false),
        mounted: Cell::new(false),
        show_on_mount: Cell::new(false),
    });
    let key = WindowKey {
        bounds_key: spec.bounds_key.clone(),
        remember: spec.remember_bounds,
    };
    let provide = {
        let args_tx = args_tx.clone();
        Rc::new(move || {
            provide_context(key.clone());
            provide_context(ShowWhenReady(false));
            provide_context(WindowArgs(args_tx.clone()));
            if let Some(account) = account {
                provide_context(AccountScope(account));
            }
        })
    };
    let page = SparePage {
        id,
        root: RootFn(root),
        provide,
        state: state.clone(),
    };
    SPARE.with_borrow_mut(|s| {
        if let Spare::Ready {
            pages, prepared, ..
        } = s
        {
            pages.send_replace(Some(page));
            *prepared = Some(Prepared {
                key: spec.key.clone(),
                args: Box::new(args.clone()),
                args_tx: Box::new(args_tx),
                state,
            });
        }
    });
}

#[component]
fn SpareRoot() -> Element {
    let channel = use_context::<SpareChannel>();
    let window = use_window();
    // The first render runs once the page has loaded: pages can be rendered from now on
    use_hook(|| {
        SPARE.with_borrow_mut(|s| {
            if matches!(s, Spare::Creating) {
                *s = Spare::Ready {
                    window: Rc::downgrade(&window),
                    pages: channel.0.clone(),
                    prepared: None,
                };
            }
        });
    });
    let mut page = use_signal(|| channel.0.borrow().clone());
    use_future(move || {
        let mut rx = channel.0.subscribe();
        async move {
            // A page sent before subscribing is picked up here
            page.set(rx.borrow_and_update().clone());
            while rx.changed().await.is_ok() {
                let next = rx.borrow_and_update().clone();
                page.set(next);
            }
        }
    });
    rsx! {
        for page in page() {
            SparePageHost { key: "{page.id}", page }
        }
    }
}

#[component]
fn SparePageHost(page: SparePage) -> Element {
    use_hook(|| (page.provide)());
    let mut presented = use_signal(|| *page.state.presented.borrow());
    use_context_provider(|| Presented(presented));
    use_future({
        let state = page.state.clone();
        move || {
            let mut rx = state.presented.subscribe();
            async move {
                presented.set(*rx.borrow_and_update());
                while rx.changed().await.is_ok() {
                    let next = *rx.borrow_and_update();
                    presented.set(next);
                }
            }
        }
    });
    let window = use_window();
    use_effect({
        let state = page.state.clone();
        move || {
            state.mounted.set(true);
            if state.show_on_mount.get() {
                activate(&window);
            }
        }
    });
    rsx! {
        PageRoot { root: page.root }
    }
}

#[component]
fn PageRoot(root: RootFn) -> Element {
    (root.0)()
}
