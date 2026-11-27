//! アプリ内ブラウザの中身。OS のブラウザの部品（Windows は WebView2、macOS は WKWebView、
//! Linux は WebKitGTK）を、ウィンドウの中に子として置く。
//!
//! 画面の部品はウィンドウの上に重なるので、位置と見せるかどうかは `browser` が描くたびに決める。
//! メニューやダイアログが上に出るときは、隠す前に今のページの画像を撮り（[`snapshot`]）、
//! `browser` がその画像をページの代わりに描く。

use std::cell::RefCell;
use std::rc::Rc;
#[cfg(any(windows, target_os = "macos"))]
use std::sync::Arc;

#[cfg(any(windows, target_os = "macos"))]
use gpui_kit::RenderImage;
use gpui_kit::{Bounds, Pixels, Window};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{dpi, NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

use crate::browser::{Engine, Event, Setup, Shot, Zoom};

thread_local! {
    /// ログインの情報（Cookie）とキャッシュを置く場所。すべてのタブで 1 つを使う。
    static CONTEXT: RefCell<Option<WebContext>> = const { RefCell::new(None) };
}

/// Windows のキャッシュの上限（バイト）。ほかの OS は、起動のときに `browser::trim_caches` で整理する。
#[cfg(windows)]
const WINDOWS_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// ページを押したときと、ページの中に入力が移ったときに知らせる。
/// 知らせを受けたら、アプリの画面を押したときにキーの入力先をアプリへ戻す。
/// マウスがページに入ったとき・出たときも知らせる。出たときに、メニューの後ろに出す画像を撮っておく。
const FOCUS_SCRIPT: &str = r#"(() => {
  const post = (m) => { try { window.ipc.postMessage(m); } catch (_) {} };
  const tell = () => post("focus");
  window.addEventListener("mousedown", tell, true);
  window.addEventListener("focusin", tell, true);
  window.addEventListener("mouseover", (e) => { if (!e.relatedTarget) post("enter"); }, true);
  window.addEventListener("mouseout", (e) => { if (!e.relatedTarget) post("leave"); }, true);
})();"#;

/// ページを読み込まずに場所が変わったこと（`history.pushState` / `replaceState`、戻る・進む、`#` の移動）を、
/// アプリに知らせる。GitHub などは読み込みの知らせなしに場所を変えるので、これがないと URL の欄が古いまま残る。
/// 場所そのものは送らない（ページが好きな URL を書けてしまう）。アプリが OS の部品から読む。
const MOVE_SCRIPT: &str = r#"(() => {
  const tell = () => { try { window.ipc.postMessage("moved"); } catch (_) {} };
  for (const name of ["pushState", "replaceState"]) {
    const original = history[name];
    if (typeof original !== "function") continue;
    history[name] = function (...args) {
      const result = original.apply(this, args);
      tell();
      return result;
    };
  }
  window.addEventListener("popstate", tell);
  window.addEventListener("hashchange", tell);
})();"#;

/// ページの上の Ctrl+ホイールと Ctrl++ / Ctrl+- / Ctrl+0、ロックのキー（Ctrl+Shift+L / ⇧⌘L）を、アプリに知らせる。
/// 拡大率はアプリが変えて、場所の欄の右に出す（OS の部品に任せると、今の拡大率が分からない）。
/// 中の枠（iframe）からは、いちばん外のページを通して知らせる。
/// 地図など、自分で Ctrl+ホイールを使うページ（`preventDefault` したもの）はそのまま渡す。
const ZOOM_SCRIPT: &str = r#"(() => {
  const top = window.top === window;
  const mac = /Mac/.test(navigator.platform || "");
  const send = (step) => {
    try {
      if (top) window.ipc.postMessage("zoom:" + step);
      else window.top.postMessage({ malaktZoom: step }, "*");
    } catch (_) {}
  };
  const lock = () => {
    try {
      if (top) window.ipc.postMessage("lock");
      else window.top.postMessage({ malaktLock: true }, "*");
    } catch (_) {}
  };
  if (top) {
    window.addEventListener("message", (e) => {
      if (e.data && e.data.malaktLock === true) { lock(); return; }
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
  // 修飾キーは ctrl・alt・shift・cmd の順（shell_keys::page_keys も同じ順に並べている）
  window.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    if (e.shiftKey && e.code === "KeyL" && (mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey)) {
      e.preventDefault();
      lock();
      return;
    }
    let step = null;
    if (e.key === "+" || e.key === "=" || e.key === ";" || e.code === "NumpadAdd") step = "in";
    else if (e.key === "-" || e.code === "NumpadSubtract") step = "out";
    else if (e.key === "0" || e.code === "Numpad0") step = "reset";
    if (!step) return;
    e.preventDefault();
    send(step);
  }, true);
})();"#;

/// ページの中のリンクをホイールで押したら、新しいタブではなく小さなウィンドウで開くよう、アプリに知らせる。
/// 部品によって `auxclick` と `click` のどちらか、または両方が届くので、両方を受けて 1 回だけ知らせる。
/// ページが自分でホイールの押下を使うとき（`preventDefault` したもの）はそのまま渡す。
/// 中の枠（iframe）からは、いちばん外のページを通して知らせる。
const POPUP_SCRIPT: &str = r#"(() => {
  const top = window.top === window;
  const send = (url) => {
    try {
      if (top) window.ipc.postMessage("popup:" + url);
      else window.top.postMessage({ malaktPopup: url }, "*");
    } catch (_) {}
  };
  if (top) {
    window.addEventListener("message", (e) => {
      const url = e.data && e.data.malaktPopup;
      if (typeof url === "string") send(url);
    });
  }
  const link = (e) => {
    const path = e.composedPath ? e.composedPath() : [e.target];
    for (const el of path) {
      if (!el || !el.matches || !el.matches("a[href], area[href]")) continue;
      return typeof el.href === "string" ? el.href : null;
    }
    return null;
  };
  let last = 0;
  const middle = (e) => {
    if (e.button !== 1 || e.defaultPrevented) return;
    const url = link(e);
    if (!url || !/^https?:/i.test(url)) return;
    e.preventDefault();
    const now = Date.now();
    if (now - last < 400) return;
    last = now;
    send(url);
  };
  window.addEventListener("auxclick", middle);
  window.addEventListener("click", middle);
})();"#;

/// ページから届いた、小さなウィンドウで開くリンク。
fn popup_url(body: &str) -> Option<&str> {
    body.strip_prefix("popup:")
        .filter(|url| crate::browser::is_web(url))
}

/// ページの中で押したブラウザやアプリのキー（[`crate::shell_keys::page_keys`]）を、アプリに知らせる。
/// OS の部品にキーの入力先があると、キーはアプリに届かないため。
/// `__TAKE__` のキーはページより先に受けてページには渡さない。`__SOFT__` のキーは、ページが使わなかった
/// （`preventDefault` しなかった）ときだけ知らせる。中の枠（iframe）からは、いちばん外のページを通して知らせる。
const KEY_SCRIPT: &str = r#"(() => {
  const take = new Set(__TAKE__);
  const soft = new Set(__SOFT__);
  const top = window.top === window;
  const send = (key) => {
    try {
      if (top) window.ipc.postMessage("key:" + key);
      else window.top.postMessage({ malaktKey: key }, "*");
    } catch (_) {}
  };
  if (top) {
    window.addEventListener("message", (e) => {
      const key = e.data && e.data.malaktKey;
      if (typeof key === "string" && (take.has(key) || soft.has(key))) send(key);
    });
  }
  const named = {
    ArrowLeft: "left", ArrowRight: "right", Tab: "tab", PageUp: "pageup", PageDown: "pagedown",
    BracketLeft: "[", BracketRight: "]", Comma: ",", Slash: "/",
  };
  const name = (e) => {
    const code = e.code || "";
    if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
    if (/^Digit[0-9]$/.test(code)) return code.slice(5);
    if (/^F([1-9]|1[0-2])$/.test(e.key)) return e.key.toLowerCase();
    return named[code] || named[e.key] || null;
  };
  // 修飾キーは ctrl・alt・shift・cmd の順（shell_keys::page_keys も同じ順に並べている）
  window.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    const key = name(e);
    if (!key) return;
    const combo = (e.ctrlKey ? "ctrl-" : "") + (e.altKey ? "alt-" : "") +
      (e.shiftKey ? "shift-" : "") + (e.metaKey ? "cmd-" : "") + key;
    if (take.has(combo)) {
      e.preventDefault();
      e.stopImmediatePropagation();
      send(combo);
    } else if (soft.has(combo)) {
      // ページの処理が終わってから、ページが使ったかを見る
      setTimeout(() => { if (!e.defaultPrevented) send(combo); }, 0);
    }
  }, true);
})();"#;

/// [`KEY_SCRIPT`] に、知らせるキーを入れる。
fn key_script() -> String {
    let keys = crate::shell_keys::page_keys();
    let list = |keys: &[String]| serde_json::Value::from(keys.to_vec()).to_string();
    KEY_SCRIPT
        .replace("__TAKE__", &list(&keys.take))
        .replace("__SOFT__", &list(&keys.soft))
}

/// ページの中を探す。`window.find` は見つけた所を選んで、見える所まで動かす。
fn find_script(text: &str, backwards: bool, again: bool) -> String {
    let text = serde_json::Value::from(text).to_string();
    let restart = if again {
        ""
    } else {
        // 書き換えたときは、ページの頭から探し直す
        "const s = window.getSelection(); if (s) s.removeAllRanges();"
    };
    format!("(() => {{ {restart} return window.find({text}, false, {backwards}, true, false, false, false); }})()")
}

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

    fn snapshot(&self, done: Shot) -> Result<(), String> {
        snapshot(&self.view, done)
    }

    fn print(&self) -> Result<(), String> {
        self.view.print().map_err(text)
    }

    fn find(
        &self,
        query: &str,
        backwards: bool,
        again: bool,
        done: Box<dyn Fn(bool) + Send + 'static>,
    ) -> Result<(), String> {
        self.view
            .evaluate_script_with_callback(&find_script(query, backwards, again), move |result| {
                done(result.trim() == "true")
            })
            .map_err(text)
    }
}

/// RGBA の画像を、gpui が描く BGRA の画像にする。ページは透けないので、アルファは 255 にそろえる。
#[cfg(any(windows, target_os = "macos"))]
fn bgra(mut rgba: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in rgba.pixels_mut() {
        let [r, g, b, _] = pixel.0;
        pixel.0 = [b, g, r, 255];
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
}

/// 撮った画像（PNG・TIFF）を、画面を止めないように別のスレッドで読む。
#[cfg(any(windows, target_os = "macos"))]
fn decode(bytes: Vec<u8>, format: image::ImageFormat, done: Shot) {
    use std::sync::Mutex;

    // スレッドを作れなかったときに `done` を取り戻せるよう、外にも持っておく
    let slot = Arc::new(Mutex::new(Some((bytes, done))));
    let job = slot.clone();
    let spawned = std::thread::Builder::new()
        .name("browser-snapshot".into())
        .spawn(move || {
            let Some((bytes, done)) = job.lock().ok().and_then(|mut job| job.take()) else {
                return;
            };
            let image = image::load_from_memory_with_format(&bytes, format)
                .map(|image| bgra(image.to_rgba8()))
                .map_err(|e| format!("ページの画像を読めませんでした: {e}"));
            done(image);
        });
    if let Err(e) = spawned {
        if let Some((_, done)) = slot.lock().ok().and_then(|mut job| job.take()) {
            done(Err(format!(
                "ページの画像を読むスレッドを作れませんでした: {e}"
            )));
        }
    }
}

/// Windows: WebView2 の CapturePreview で、今見えている部分を PNG にする。
#[cfg(windows)]
fn snapshot(view: &WebView, done: Shot) -> Result<(), String> {
    use webview2_com::CapturePreviewCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
    use wry::WebViewExtWindows;

    fn read_all(stream: &IStream) -> windows::core::Result<Vec<u8>> {
        let mut stat = STATSTG::default();
        unsafe { stream.Stat(&mut stat, STATFLAG_NONAME)? };
        unsafe { stream.Seek(0, STREAM_SEEK_SET, None)? };
        let mut bytes = vec![0u8; stat.cbSize as usize];
        let mut read = 0u32;
        unsafe {
            stream
                .Read(
                    bytes.as_mut_ptr().cast(),
                    bytes.len() as u32,
                    Some(&mut read),
                )
                .ok()?
        };
        bytes.truncate(read as usize);
        Ok(bytes)
    }

    let stream: IStream = unsafe { CreateStreamOnHGlobal(Default::default(), true) }
        .map_err(|e| format!("ページの画像の置き場所を作れませんでした: {}", e.message()))?;
    let written = stream.clone();
    let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
        match result.and_then(|()| read_all(&written)) {
            Ok(png) => decode(png, image::ImageFormat::Png, done),
            Err(e) => done(Err(format!(
                "ページの画像を撮れませんでした: {}",
                e.message()
            ))),
        }
        Ok(())
    }));
    unsafe {
        view.webview().CapturePreview(
            COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
            &stream,
            &handler,
        )
    }
    .map_err(|e| format!("ページの画像を撮れませんでした: {}", e.message()))
}

/// macOS: WKWebView の takeSnapshot で、今見えている部分を TIFF にする。
#[cfg(target_os = "macos")]
fn snapshot(view: &WebView, done: Shot) -> Result<(), String> {
    use std::cell::Cell;

    use block2::RcBlock;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSError;
    use wry::WebViewExtMacOS;

    let done = Cell::new(Some(done));
    let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        // 完了は 1 回だけ届く
        let Some(done) = done.take() else {
            return;
        };
        let image = unsafe { image.as_ref() };
        let error = unsafe { error.as_ref() };
        let tiff = match (image, error) {
            (Some(image), _) => image
                .TIFFRepresentation()
                .map(|data| data.to_vec())
                .ok_or_else(|| "ページの画像を TIFF にできませんでした".to_owned()),
            (None, Some(error)) => Err(format!(
                "ページの画像を撮れませんでした: {}",
                error.localizedDescription()
            )),
            (None, None) => Err("ページの画像を撮れませんでした".to_owned()),
        };
        match tiff {
            Ok(tiff) => decode(tiff, image::ImageFormat::Tiff, done),
            Err(e) => done(Err(e)),
        }
    });
    unsafe {
        view.webview()
            .takeSnapshotWithConfiguration_completionHandler(None, &block)
    };
    Ok(())
}

/// Linux: WebKitGTK の snapshot で、今見えている部分を cairo の画像にする。
#[cfg(target_os = "linux")]
fn snapshot(view: &WebView, done: Shot) -> Result<(), String> {
    use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt};
    use wry::WebViewExtUnix;

    view.webview().snapshot(
        SnapshotRegion::Visible,
        SnapshotOptions::NONE,
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let image = result
                .map_err(|e| format!("ページの画像を撮れませんでした: {e}"))
                .and_then(linux::image);
            done(image);
        },
    );
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn snapshot(_view: &WebView, _done: Shot) -> Result<(), String> {
    Err("この OS では、ページの画像を撮れません".into())
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
            .with_initialization_script_for_main_only(MOVE_SCRIPT, true)
            .with_initialization_script_for_main_only(ZOOM_SCRIPT, false)
            .with_initialization_script_for_main_only(POPUP_SCRIPT, false)
            .with_initialization_script_for_main_only(key_script(), false)
            .with_ipc_handler({
                let send = send.clone();
                move |request| {
                    let body = request.body();
                    match body.as_str() {
                        "focus" => send(Event::Focused),
                        "enter" => send(Event::Pointer(true)),
                        "leave" => send(Event::Pointer(false)),
                        "lock" => send(Event::Lock),
                        "moved" => send(Event::Moved),
                        _ => {
                            if let Some(step) = Zoom::parse(body) {
                                send(Event::Zoom(step));
                            } else if let Some(url) = popup_url(body) {
                                send(Event::Popup(url.to_owned()));
                            } else if let Some(key) = body.strip_prefix("key:") {
                                send(Event::Key(key.to_owned()));
                            }
                        }
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
                    // 新しいウィンドウではなく、アプリの新しいタブで開く。ホイールで押したリンクは、
                    // POPUP_SCRIPT が小さなウィンドウで開く（止められなかった分は browser が捨てる）
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
            // ブラウザのキー（Ctrl+R、Ctrl+F など）は KEY_SCRIPT で受けて、アプリが動かす。
            // OS の部品にも任せると、ほかの OS と働きが変わり、同じキーで 2 回動くこともある
            builder.with_browser_accelerator_keys(false).with_additional_browser_args(format!(
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

/// 捨てたページの後片付けを、ウィンドウを閉じる前に済ませる。
/// Linux では、GTK がページの X のウィンドウを消す頼みを、親のウィンドウが消える前に X サーバーへ届ける
/// （親が先に消えると BadWindow になり、アプリが落ちる）。
pub fn settle() {
    #[cfg(target_os = "linux")]
    linux::settle();
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

    pub fn settle() {
        if !gtk::is_initialized_main_thread() {
            return;
        }
        pump();
        if let Some(display) = gtk::gdk::Display::default() {
            display.sync();
        }
        pump();
    }

    /// cairo の画像（ARGB32。リトルエンディアンでは B, G, R, A の順）を、gpui の画像にする。
    pub fn image(
        surface: gtk::cairo::Surface,
    ) -> Result<std::sync::Arc<gpui_kit::RenderImage>, String> {
        let surface = gtk::cairo::ImageSurface::try_from(surface)
            .map_err(|_| "ページの画像の形が違います".to_owned())?;
        surface.flush();
        let (width, height) = (surface.width().max(0), surface.height().max(0));
        let stride = surface.stride().max(0) as usize;
        let row = width as usize * 4;
        // gpui の画像も B, G, R, A の順なので、行の余りを除いてそのまま写す
        let mut bgra = Vec::with_capacity(row * height as usize);
        surface
            .with_data(|data| {
                for line in data.chunks(stride).take(height as usize) {
                    for &[b, g, r, _] in line[..row.min(line.len())].as_chunks::<4>().0 {
                        bgra.extend_from_slice(&[b, g, r, 255]);
                    }
                }
            })
            .map_err(|e| format!("ページの画像を読めませんでした: {e}"))?;
        let bgra = image::RgbaImage::from_raw(width as u32, height as u32, bgra)
            .ok_or_else(|| "ページの画像の大きさが合いません".to_owned())?;
        Ok(std::sync::Arc::new(gpui_kit::RenderImage::new(vec![
            image::Frame::new(bgra),
        ])))
    }
}

#[cfg(test)]
mod tests {
    use super::{popup_url, stays_inside};

    #[test]
    fn only_web_links_open_in_a_small_window() {
        assert_eq!(
            popup_url("popup:https://github.com/a"),
            Some("https://github.com/a")
        );
        assert_eq!(popup_url("popup:javascript:alert(1)"), None);
        assert_eq!(popup_url("popup:file:///etc/passwd"), None);
        assert_eq!(popup_url("zoom:in"), None);
    }

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
