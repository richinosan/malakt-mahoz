//! アプリ内ブラウザの中身。OS のブラウザの部品（Windows は WebView2、macOS は WKWebView、
//! Linux は WebKitGTK）を、ウィンドウの中に子として置く。
//!
//! 画面の部品はウィンドウの上に重なるので、位置と見せるかどうかは `browser` が描くたびに決める。

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{Bounds, Pixels, Window};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{dpi, NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

use crate::browser::{Engine, Event, Setup, Zoom};

thread_local! {
    /// ログインの情報（Cookie）とキャッシュを置く場所。すべてのタブで 1 つを使う。
    static CONTEXT: RefCell<Option<WebContext>> = const { RefCell::new(None) };
}

/// Windows のキャッシュの上限（バイト）。ほかの OS は、起動のときに `browser::trim_caches` で整理する。
#[cfg(windows)]
const WINDOWS_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// ページを押したときと、ページの中に入力が移ったときに知らせる。
/// 知らせを受けたら、アプリの画面を押したときにキーの入力先をアプリへ戻す。
const FOCUS_SCRIPT: &str = r#"(() => {
  const tell = () => { try { window.ipc.postMessage("focus"); } catch (_) {} };
  window.addEventListener("mousedown", tell, true);
  window.addEventListener("focusin", tell, true);
})();"#;

/// ページの上の Ctrl+ホイールと Ctrl++ / Ctrl+- / Ctrl+0 を、アプリに知らせる。
/// 拡大率はアプリが変えて、場所の欄の右に出す（OS の部品に任せると、今の拡大率が分からない）。
/// 中の枠（iframe）からは、いちばん外のページを通して知らせる。
/// 地図など、自分で Ctrl+ホイールを使うページ（`preventDefault` したもの）はそのまま渡す。
const ZOOM_SCRIPT: &str = r#"(() => {
  const top = window.top === window;
  const send = (step) => {
    try {
      if (top) window.ipc.postMessage("zoom:" + step);
      else window.top.postMessage({ malaktZoom: step }, "*");
    } catch (_) {}
  };
  if (top) {
    window.addEventListener("message", (e) => {
      const step = e.data && e.data.malaktZoom;
      if (step === "in" || step === "out" || step === "reset") send(step);
    });
  }
  let sum = 0, last = 0;
  window.addEventListener("wheel", (e) => {
    if (!e.ctrlKey || e.defaultPrevented) return;
    e.preventDefault();
    const now = Date.now();
    if (now - last > 400) sum = 0;
    last = now;
    sum += e.deltaMode === 0 ? e.deltaY : e.deltaY * 40;
    if (Math.abs(sum) < 40) return;
    send(sum < 0 ? "in" : "out");
    sum = 0;
  }, { passive: false });
  window.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    let step = null;
    if (e.key === "+" || e.key === "=" || e.key === ";" || e.code === "NumpadAdd") step = "in";
    else if (e.key === "-" || e.code === "NumpadSubtract") step = "out";
    else if (e.key === "0" || e.code === "Numpad0") step = "reset";
    if (!step) return;
    e.preventDefault();
    send(step);
  }, true);
})();"#;

/// ページの文字を読むスクリプト。AI に渡す。
const READ_SCRIPT: &str = r#"(() => JSON.stringify({
  url: location.href,
  title: document.title,
  selection: String(window.getSelection ? window.getSelection() : ""),
  text: document.body ? document.body.innerText : ""
}))()"#;

/// 子を置くウィンドウ。Linux（X11）では、gpui の xcb の番号を Xlib の番号として渡す。
struct Parent(RawWindowHandle);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // 番号はウィンドウが開いている間は変わらない。ブラウザはウィンドウより先に閉じる
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

fn parent(window: &Window) -> Result<Parent, String> {
    let raw = HasWindowHandle::window_handle(window)
        .map_err(|e| format!("ウィンドウの番号を読めませんでした: {e}"))?
        .as_raw();
    #[cfg(target_os = "linux")]
    let raw = match raw {
        RawWindowHandle::Xcb(handle) => RawWindowHandle::Xlib(
            raw_window_handle::XlibWindowHandle::new(u32::from(handle.window).into()),
        ),
        RawWindowHandle::Xlib(_) => raw,
        _ => return Err("Wayland のウィンドウには、ブラウザを置けません".into()),
    };
    Ok(Parent(raw))
}

fn rect(bounds: Bounds<Pixels>) -> Rect {
    Rect {
        position: dpi::LogicalPosition::new(
            f64::from(f32::from(bounds.origin.x)),
            f64::from(f32::from(bounds.origin.y)),
        )
        .into(),
        size: dpi::LogicalSize::new(
            f64::from(f32::from(bounds.size.width).max(1.)),
            f64::from(f32::from(bounds.size.height).max(1.)),
        )
        .into(),
    }
}

/// アプリの中で開くリンク。ほかのもの（mailto: など）は OS に渡す。
fn stays_inside(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http:", "https:", "about:", "data:", "blob:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// OS のブラウザの部品で作ったページ。
struct Wry {
    view: WebView,
}

fn text(e: wry::Error) -> String {
    e.to_string()
}

impl Engine for Wry {
    fn load(&self, url: &str) -> Result<(), String> {
        self.view.load_url(url).map_err(text)
    }

    fn back(&self) -> Result<(), String> {
        self.view.go_back().map_err(text)
    }

    fn forward(&self) -> Result<(), String> {
        self.view.go_forward().map_err(text)
    }

    fn reload(&self) -> Result<(), String> {
        self.view.reload().map_err(text)
    }

    fn stop(&self) -> Result<(), String> {
        self.view.evaluate_script("window.stop()").map_err(text)
    }

    fn place(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        self.view.set_bounds(rect(bounds)).map_err(text)
    }

    fn show(&self, visible: bool) -> Result<(), String> {
        self.view.set_visible(visible).map_err(text)
    }

    fn focus(&self) -> Result<(), String> {
        self.view.focus().map_err(text)
    }

    fn focus_parent(&self) -> Result<(), String> {
        self.view.focus_parent().map_err(text)
    }

    fn url(&self) -> Option<String> {
        self.view.url().ok()
    }

    fn history(&self) -> (bool, bool) {
        // 調べられなければ、押せないものとして出す
        (
            self.view.can_go_back().unwrap_or(false),
            self.view.can_go_forward().unwrap_or(false),
        )
    }

    fn read(&self, done: Box<dyn Fn(String) + Send + 'static>) -> Result<(), String> {
        self.view
            .evaluate_script_with_callback(READ_SCRIPT, done)
            .map_err(text)
    }

    fn clear_data(&self) -> Result<(), String> {
        self.view.clear_all_browsing_data().map_err(text)
    }

    fn zoom(&self, factor: f64) -> Result<(), String> {
        self.view.zoom(factor).map_err(text)
    }
}

/// ページを作って、ウィンドウの中に置く。最初は隠しておき、描いたときに見せる。
pub fn create(window: &mut Window, setup: Setup) -> Result<Rc<dyn Engine>, String> {
    #[cfg(target_os = "linux")]
    linux::init()?;
    let parent = parent(window)?;
    crate::browser::trim_once()?;
    let Setup {
        id,
        url,
        bounds,
        events,
    } = setup;
    let send = move |event: Event| {
        // アプリを閉じている途中なら、受け取る先がない
        let _ = events.try_send((id, event));
    };
    CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let context =
            context.get_or_insert_with(|| WebContext::new(Some(crate::browser::profile_dir())));
        let builder = WebViewBuilder::new_with_web_context(context)
            .with_url(url)
            .with_bounds(rect(bounds))
            .with_visible(false)
            .with_accept_first_mouse(true)
            .with_clipboard(true)
            // 拡大縮小は ZOOM_SCRIPT で受けて、アプリが変える
            .with_hotkeys_zoom(false)
            .with_initialization_script(FOCUS_SCRIPT)
            .with_initialization_script_for_main_only(ZOOM_SCRIPT, false)
            .with_ipc_handler({
                let send = send.clone();
                move |request| {
                    let body = request.body();
                    if body == "focus" {
                        send(Event::Focused);
                    } else if let Some(step) = Zoom::parse(body) {
                        send(Event::Zoom(step));
                    }
                }
            })
            .with_navigation_handler({
                let send = send.clone();
                move |url| {
                    if stays_inside(&url) {
                        true
                    } else {
                        send(Event::External(url));
                        false
                    }
                }
            })
            .with_new_window_req_handler({
                let send = send.clone();
                move |url, _| {
                    // 新しいウィンドウではなく、アプリの新しいタブで開く
                    send(Event::NewTab(url));
                    NewWindowResponse::Deny
                }
            })
            .with_on_page_load_handler({
                let send = send.clone();
                move |event, url| {
                    send(Event::Load {
                        done: matches!(event, PageLoadEvent::Finished),
                        url,
                    })
                }
            })
            .with_document_title_changed_handler(move |title| send(Event::Title(title)));
        #[cfg(windows)]
        let builder = {
            use wry::WebViewBuilderExtWindows;
            builder.with_additional_browser_args(format!(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disk-cache-size={WINDOWS_CACHE_BYTES}"
            ))
        };
        let view = builder
            .build_as_child(&parent)
            .map_err(|e| format!("ブラウザを開けませんでした: {e}"))?;
        Ok(Rc::new(Wry { view }) as Rc<dyn Engine>)
    })
}

/// OS のブラウザの部品の知らせを進める。Linux の GTK だけが要る。
pub fn pump() {
    #[cfg(target_os = "linux")]
    linux::pump();
}

#[cfg(target_os = "linux")]
mod linux {
    pub fn init() -> Result<(), String> {
        if gtk::is_initialized_main_thread() {
            return Ok(());
        }
        gtk::init().map_err(|e| format!("GTK を準備できませんでした: {e}"))
    }

    pub fn pump() {
        if !gtk::is_initialized_main_thread() {
            return;
        }
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stays_inside;

    #[test]
    fn only_web_pages_stay_inside() {
        assert!(stays_inside("https://github.com/"));
        assert!(stays_inside("HTTP://example.com"));
        assert!(stays_inside("about:blank"));
        assert!(!stays_inside("mailto:a@example.com"));
        assert!(!stays_inside("malakt-mahoz://task/abc"));
        assert!(!stays_inside("vscode://file/x"));
    }
}
