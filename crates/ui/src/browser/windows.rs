//! Windows WebView2 host through Wry. The WebView is a child HWND owned by
//! the GPUI window; browser callbacks only enqueue state events.
use super::model::{PageState, Presentation, allowed_navigation};
use gpui::{Bounds, Pixels, Window};
use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use std::{cell::RefCell, rc::Rc};
use tokio::sync::mpsc::Sender;
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

#[derive(Clone, Default)]
pub(super) struct BrowserData;

/// A `'static`, `Copy` parent-window handle. WebView2 creation must run from a
/// spawned GPUI task rather than from the update that requested navigation,
/// because it synchronously pumps the thread's message loop (see
/// `NativePage::create`). Capturing the raw handle lets that task create the
/// child WebView without borrowing [`gpui::Window`].
#[derive(Clone, Copy)]
pub(super) struct ParentWindow(Win32WindowHandle);

impl ParentWindow {
    pub(super) fn from_window(window: &Window) -> Result<Self, String> {
        match window
            .window_handle()
            .map_err(|error: HandleError| error.to_string())?
            .as_raw()
        {
            RawWindowHandle::Win32(handle) => Ok(Self(handle)),
            _ => Err("This window cannot host a WebView2 browser.".into()),
        }
    }
}

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(self.0)) })
    }
}

pub(super) enum NativeEvent {
    Changed,
    NewTab(String),
}

#[derive(Default)]
struct HostState {
    url: Option<String>,
    title: String,
    loading: bool,
}

pub(super) struct NativePage(Rc<RefCell<Host>>);

pub(super) struct Host {
    web: WebView,
    state: Rc<RefCell<HostState>>,
    bounds: Option<Bounds<Pixels>>,
    presentation: Presentation,
    tx: Sender<NativeEvent>,
}

impl NativePage {
    /// Create the page and its WebView2 controller.
    ///
    /// WebView2 controller creation synchronously pumps this thread's message
    /// loop while it waits for its COM callback (`webview2_com::wait_with_pump`).
    /// If that pump runs while GPUI's `App` cell is borrowed, a queued GPUI task
    /// re-enters `App::borrow_mut` and panics, which the platform window
    /// procedure turns into `process::abort`. Callers must therefore invoke this
    /// from a spawned task whose first poll happens outside any app borrow.
    pub fn create(parent: ParentWindow, tx: Sender<NativeEvent>) -> Result<Self, String> {
        let state = Rc::new(RefCell::new(HostState::default()));
        let load_state = state.clone();
        let load_tx = tx.clone();
        let title_state = state.clone();
        let title_tx = tx.clone();
        let new_tab = tx.clone();

        let web = WebViewBuilder::new()
            .with_visible(false)
            .with_focused(false)
            .with_incognito(true)
            .with_clipboard(true)
            .with_navigation_handler(|url| allowed_navigation(&url))
            .with_new_window_req_handler(move |url, _features| {
                if allowed_navigation(&url) {
                    let _ = new_tab.try_send(NativeEvent::NewTab(url));
                }
                NewWindowResponse::Deny
            })
            .with_document_title_changed_handler(move |title| {
                title_state.borrow_mut().title = title;
                let _ = title_tx.try_send(NativeEvent::Changed);
            })
            .with_on_page_load_handler(move |event, url| {
                let mut state = load_state.borrow_mut();
                state.url = Some(url);
                state.loading = matches!(event, PageLoadEvent::Started);
                let _ = load_tx.try_send(NativeEvent::Changed);
            })
            .build_as_child(&parent)
            .map_err(|error| {
                format!(
                    "Could not create the WebView2 browser. Install the Microsoft Edge WebView2 Runtime: {error}"
                )
            })?;

        Ok(Self(Rc::new(RefCell::new(Host {
            web,
            state,
            bounds: None,
            presentation: Presentation::Hidden,
            tx,
        }))))
    }

    pub fn handle(&self) -> Rc<RefCell<Host>> {
        self.0.clone()
    }

    pub fn focus_chrome(&self) {
        let _ = self.0.borrow().web.focus_parent();
    }

    pub fn present(&mut self, presentation: Presentation) {
        self.0.borrow_mut().present(presentation);
    }

    pub fn load(&self, url: &str) -> Result<(), String> {
        {
            let host = self.0.borrow();
            let mut state = host.state.borrow_mut();
            state.url = Some(url.to_owned());
            state.title.clear();
            state.loading = true;
        }
        self.0
            .borrow()
            .web
            .load_url(url)
            .map_err(|error| error.to_string())
    }

    pub fn reload(&self) {
        let host = self.0.borrow();
        host.state.borrow_mut().loading = true;
        let _ = host.web.reload();
        let _ = host.tx.try_send(NativeEvent::Changed);
    }

    pub fn history(&self, forward: bool) {
        let host = self.0.borrow();
        let result = if forward {
            host.web.go_forward()
        } else {
            host.web.go_back()
        };
        if result.is_ok() {
            let _ = host.tx.try_send(NativeEvent::Changed);
        }
    }

    pub fn state(&self) -> PageState {
        let host = self.0.borrow();
        // Don't hold the state borrow across WebView2 calls: a callback could
        // re-enter while a query pumps.
        let (title, loading, fallback_url) = {
            let state = host.state.borrow();
            (state.title.clone(), state.loading, state.url.clone())
        };
        PageState {
            // WebView2 can change the URL without a page-load callback (for
            // example history.pushState). Prefer its live URL so same-document
            // navigation is reflected in the browser chrome.
            url: host.web.url().ok().or(fallback_url),
            title,
            loading,
            can_back: host.web.can_go_back().unwrap_or(false),
            can_forward: host.web.can_go_forward().unwrap_or(false),
            error: None,
        }
    }
}

impl Host {
    pub(super) fn sync(&mut self, bounds: Bounds<Pixels>, scale: f32, dragging: bool) {
        if self.bounds != Some(bounds) {
            let _ = self.web.set_bounds(Rect {
                position: wry::dpi::PhysicalPosition::new(
                    (f32::from(bounds.origin.x) * scale) as i32,
                    (f32::from(bounds.origin.y) * scale) as i32,
                )
                .into(),
                size: wry::dpi::PhysicalSize::new(
                    (f32::from(bounds.size.width) * scale).max(0.) as u32,
                    (f32::from(bounds.size.height) * scale).max(0.) as u32,
                )
                .into(),
            });
            self.bounds = Some(bounds);
        }
        let visible = self.presentation == Presentation::Live
            && !dragging
            && f32::from(bounds.size.width) > 0.
            && f32::from(bounds.size.height) > 0.;
        let _ = self.web.set_visible(visible);
    }

    fn present(&mut self, presentation: Presentation) {
        self.presentation = presentation;
        let visible = presentation == Presentation::Live
            && self.bounds.is_some_and(|bounds| {
                f32::from(bounds.size.width) > 0. && f32::from(bounds.size.height) > 0.
            });
        let _ = self.web.set_visible(visible);
    }
}

#[cfg(feature = "browser-fixture")]
impl NativePage {
    pub fn fixture_eval(&self, script: &str) {
        let _ = self.0.borrow().web.evaluate_script(script);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.web.set_visible(false);
    }
}
