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

use crate::browser::{Engine, Event, PdfDone, Setup, Shot, Zoom};
use crate::downloads::Note;

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
  // ページの印刷のボタン（window.print）も、Ctrl+P と同じアプリの印刷のダイアログにする。
  // ダイアログの「プリンター」は、元の window.print を __malaktPrint で呼ぶ
  try {
    const print = window.print.bind(window);
    Object.defineProperty(window, "__malaktPrint", { value: print });
    window.print = () => send(__PRINT__);
  } catch (_) {}
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
        .replace(
            "__PRINT__",
            &serde_json::Value::from(crate::shell_keys::PRINT_KEY).to_string(),
        )
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
        // Windows の wry は window.print を呼ぶ。ページの window.print はアプリのダイアログを開くので、元のものを呼ぶ
        #[cfg(windows)]
        return self
            .view
            .evaluate_script("(window.__malaktPrint || window.print)()")
            .map_err(text);
        #[cfg(not(windows))]
        self.view.print().map_err(text)
    }

    fn save_pdf(
        &self,
        path: &std::path::Path,
        setup: &crate::print_setup::PrintSetup,
        pages: Option<&[u32]>,
        done: PdfDone,
    ) -> Result<(), String> {
        save_pdf(&self.view, path, setup, pages, done)
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

/// ダウンロードの進み具合を知らせる間隔。部品はもっと細かく知らせてくる。
#[cfg(any(windows, target_os = "linux"))]
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(150);

/// Windows: WebView2 のダウンロードの、受け取った大きさと全体の大きさを知らせる。
#[cfg(windows)]
fn watch_downloads(view: &WebView, send: impl Fn(Event) + Clone + 'static) -> Result<(), String> {
    use std::time::Instant;

    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_4;
    use webview2_com::{
        take_pwstr, BytesReceivedChangedEventHandler, DownloadStartingEventHandler,
    };
    use windows::core::{Interface, PWSTR};
    use wry::WebViewExtWindows;

    let webview: ICoreWebView2_4 = view
        .webview()
        .cast()
        .map_err(|e| format!("ダウンロードの進み具合を読めません: {}", e.message()))?;
    let handler = DownloadStartingEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        let operation = unsafe { args.DownloadOperation()? };
        let mut uri = PWSTR::null();
        unsafe { operation.Uri(&mut uri)? };
        let url = take_pwstr(uri);
        let send = send.clone();
        let mut last: Option<Instant> = None;
        let progress = BytesReceivedChangedEventHandler::create(Box::new(move |operation, _| {
            let Some(operation) = operation else {
                return Ok(());
            };
            if last.is_some_and(|at| at.elapsed() < PROGRESS_EVERY) {
                return Ok(());
            }
            last = Some(Instant::now());
            let (mut received, mut total) = (0i64, 0i64);
            unsafe {
                operation.BytesReceived(&mut received)?;
                operation.TotalBytesToReceive(&mut total)?;
            }
            send(Event::Download(Note::Progress {
                url: url.clone(),
                received: received.max(0) as u64,
                total: (total > 0).then_some(total as u64),
            }));
            Ok(())
        }));
        let mut token = 0i64;
        unsafe { operation.add_BytesReceivedChanged(&progress, &mut token)? };
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { webview.add_DownloadStarting(&handler, &mut token) }
        .map_err(|e| format!("ダウンロードの進み具合を読めません: {}", e.message()))
}

/// Windows: ページの右クリックのメニューの「印刷」を、Ctrl+P と同じアプリの印刷のダイアログにする。
/// WebView2 の印刷のプレビューから「Microsoft Print to PDF」で保存すると、ファイル名にページの題名が入らない。
#[cfg(windows)]
fn print_from_menu(view: &WebView, send: impl Fn(Event) + Clone + 'static) -> Result<(), String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Environment9, ICoreWebView2_11, ICoreWebView2_2,
        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
    };
    use webview2_com::{
        take_pwstr, ContextMenuRequestedEventHandler, CustomItemSelectedEventHandler,
    };
    use windows::core::{Interface, HSTRING, PWSTR};
    use wry::WebViewExtWindows;

    let failed = |e: windows::core::Error| {
        format!(
            "右クリックのメニューの印刷を変えられません: {}",
            e.message()
        )
    };
    let webview = view.webview();
    let environment: ICoreWebView2Environment9 = webview
        .cast::<ICoreWebView2_2>()
        .and_then(|w| unsafe { w.Environment() })
        .and_then(|e| e.cast())
        .map_err(failed)?;
    let handler = ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        let items = unsafe { args.MenuItems()? };
        let mut count = 0u32;
        unsafe { items.Count(&mut count)? };
        for index in 0..count {
            let item = unsafe { items.GetValueAtIndex(index)? };
            let mut name = PWSTR::null();
            unsafe { item.Name(&mut name)? };
            if take_pwstr(name) != "print" {
                continue;
            }
            let mut label = PWSTR::null();
            unsafe { item.Label(&mut label)? };
            let label = HSTRING::from(take_pwstr(label));
            let print = unsafe {
                environment.CreateContextMenuItem(
                    &label,
                    None,
                    COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
                )?
            };
            let send = send.clone();
            let selected = CustomItemSelectedEventHandler::create(Box::new(move |_, _| {
                send(Event::Key(crate::shell_keys::PRINT_KEY.to_owned()));
                Ok(())
            }));
            let mut token = 0i64;
            unsafe {
                print.add_CustomItemSelected(&selected, &mut token)?;
                items.RemoveValueAtIndex(index)?;
                items.InsertValueAtIndex(index, &print)?;
            }
            break;
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        webview
            .cast::<ICoreWebView2_11>()
            .and_then(|w| w.add_ContextMenuRequested(&handler, &mut token))
    }
    .map_err(failed)
}

/// macOS: 部品は進み具合を知らせないので、ダウンロードの一覧がファイルの大きさを見る。
#[cfg(target_os = "macos")]
fn watch_downloads(_: &WebView, _: impl Fn(Event) + Clone + 'static) -> Result<(), String> {
    Ok(())
}

/// Linux: WebKitGTK のダウンロードの、受け取った大きさと全体の大きさを知らせる。
/// ダウンロードはすべてのタブで 1 つの置き場所（WebContext）が受けるので、最初のページを作ったときに 1 回だけ見張る。
#[cfg(target_os = "linux")]
fn watch_downloads(view: &WebView, send: impl Fn(Event) + Clone + 'static) -> Result<(), String> {
    use std::cell::Cell;
    use std::time::Instant;

    use webkit2gtk::{DownloadExt, URIRequestExt, URIResponseExt, WebContextExt, WebViewExt};
    use wry::WebViewExtUnix;

    thread_local! {
        static WATCHING: Cell<bool> = const { Cell::new(false) };
    }
    if WATCHING.get() {
        return Ok(());
    }
    let context = view
        .webview()
        .context()
        .ok_or_else(|| "ダウンロードの進み具合を読めません".to_owned())?;
    WATCHING.set(true);
    context.connect_download_started(move |_, download| {
        let send = send.clone();
        let last: Cell<Option<Instant>> = Cell::new(None);
        download.connect_received_data(move |download, _| {
            if last.get().is_some_and(|at| at.elapsed() < PROGRESS_EVERY) {
                return;
            }
            last.set(Some(Instant::now()));
            let Some(url) = download.request().and_then(|r| r.uri()) else {
                return;
            };
            let total = download
                .response()
                .map(|r| r.content_length())
                .filter(|n| *n > 0);
            send(Event::Download(Note::Progress {
                url: url.to_string(),
                received: download.received_data_length(),
                total,
            }));
        });
    });
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn watch_downloads(_: &WebView, _: impl Fn(Event) + Clone + 'static) -> Result<(), String> {
    Ok(())
}

/// Windows: WebView2 の PrintToPdf で、印刷の設定どおりの PDF にする。
/// 長さはインチで渡す。フォントは WebView2 が埋め込む。
#[cfg(windows)]
fn save_pdf(
    view: &WebView,
    path: &std::path::Path,
    setup: &crate::print_setup::PrintSetup,
    pages: Option<&[u32]>,
    done: PdfDone,
) -> Result<(), String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Environment6, ICoreWebView2PrintSettings2, ICoreWebView2_7,
        COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE, COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT,
    };
    use webview2_com::PrintToPdfCompletedHandler;
    use windows::core::{Interface, HSTRING};
    use wry::WebViewExtWindows;

    let wrong = |e: windows::core::Error| format!("印刷の設定を渡せませんでした: {}", e.message());
    let webview: ICoreWebView2_7 = view
        .webview()
        .cast()
        .map_err(|e| format!("この WebView2 は PDF を作れません: {}", e.message()))?;
    let environment: ICoreWebView2Environment6 = view.environment().cast().map_err(|e| {
        format!(
            "この WebView2 は印刷の設定を受け取れません: {}",
            e.message()
        )
    })?;
    let inch = |mm: f32| f64::from(mm) / 25.4;
    let settings = unsafe { environment.CreatePrintSettings() }.map_err(wrong)?;
    let (width, height) = setup.paper.mm();
    let margins = setup.margins_mm();
    unsafe {
        settings
            .SetOrientation(if setup.landscape {
                COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE
            } else {
                COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT
            })
            .map_err(wrong)?;
        settings.SetPageWidth(inch(width)).map_err(wrong)?;
        settings.SetPageHeight(inch(height)).map_err(wrong)?;
        settings.SetMarginTop(inch(margins.top)).map_err(wrong)?;
        settings
            .SetMarginRight(inch(margins.right))
            .map_err(wrong)?;
        settings
            .SetMarginBottom(inch(margins.bottom))
            .map_err(wrong)?;
        settings.SetMarginLeft(inch(margins.left)).map_err(wrong)?;
        settings
            .SetScaleFactor(setup.scale_factor())
            .map_err(wrong)?;
        settings
            .SetShouldPrintBackgrounds(setup.backgrounds)
            .map_err(wrong)?;
        settings
            .SetShouldPrintHeaderAndFooter(setup.header_footer)
            .map_err(wrong)?;
    }
    let per_sheet = setup.per_sheet();
    if pages.is_some() || per_sheet > 1 {
        let more: ICoreWebView2PrintSettings2 = settings.cast().map_err(|e| {
            format!(
                "この WebView2 はページを選んだり並べたりできません: {}",
                e.message()
            )
        })?;
        unsafe {
            if let Some(pages) = pages {
                more.SetPageRanges(&HSTRING::from(crate::print_setup::ranges_text(pages)))
                    .map_err(wrong)?;
            }
            more.SetPagesPerSide(i32::from(per_sheet)).map_err(wrong)?;
        }
    }
    let handler = PrintToPdfCompletedHandler::create(Box::new(move |result, saved| {
        done(match result {
            Ok(()) if saved => Ok(()),
            Ok(()) => Err("WebView2 が PDF を書き込めませんでした".to_owned()),
            Err(e) => Err(e.message().to_string()),
        });
        Ok(())
    }));
    let path = HSTRING::from(path.as_os_str());
    unsafe { webview.PrintToPdf(&path, &settings, &handler) }.map_err(|e| e.message().to_string())
}

/// macOS: WKWebView の印刷（NSPrintOperation）を、パネルを出さずにファイルへ保存する。
/// 長さはポイント（1/72 インチ）で渡す。WebKit はフォントを埋め込む。
/// 1 枚に何ページも並べることはできない（[`crate::print_setup::N_UP`]）ので、`pages` は来ない。
/// ページは保存したあとで選ぶ。
#[cfg(target_os = "macos")]
fn save_pdf(
    view: &WebView,
    path: &std::path::Path,
    setup: &crate::print_setup::PrintSetup,
    pages: Option<&[u32]>,
    done: PdfDone,
) -> Result<(), String> {
    use std::cell::Cell;
    use std::ffi::c_void;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::{
        NSPaperOrientation, NSPrintHeaderAndFooter, NSPrintInfo, NSPrintJobSavingURL,
        NSPrintOperation, NSPrintSaveJob, NSPrintScalingFactor, NSPrintingPaginationMode,
    };
    use objc2_foundation::{MainThreadMarker, NSNumber, NSSize, NSString, NSURL};
    use wry::WebViewExtMacOS;

    struct Waiting {
        done: Cell<Option<PdfDone>>,
    }
    define_class!(
        /// 印刷が終わったことを受け取る。
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "MalaktPrintFinished"]
        #[ivars = Waiting]
        struct PrintFinished;

        impl PrintFinished {
            #[unsafe(method(printOperationDidRun:success:contextInfo:))]
            fn did_run(&self, _operation: &NSPrintOperation, success: Bool, _context: *mut c_void) {
                if let Some(done) = self.ivars().done.take() {
                    done(if success.as_bool() {
                        Ok(())
                    } else {
                        Err("macOS が PDF を書き込めませんでした".to_owned())
                    });
                }
                WAITING.with(|waiting| {
                    waiting
                        .borrow_mut()
                        .retain(|d| !std::ptr::eq::<PrintFinished>(&**d, self))
                });
            }
        }
    );
    thread_local! {
        /// 印刷している途中の受け取り口。終わるまで持っておく。
        static WAITING: RefCell<Vec<Retained<PrintFinished>>> = const { RefCell::new(Vec::new()) };
    }

    if pages.is_some() {
        return Err("macOS の部品は、ページを選んで PDF にできません".to_owned());
    }
    let mtm = MainThreadMarker::new().ok_or("PDF は画面の処理の中でしか作れません")?;
    let webview = view.webview();
    let window = webview
        .window()
        .ok_or("ページがウィンドウに入っていないので、PDF にできません")?;
    // 背景を印刷する設定は macOS 13.3 から
    let preferences = unsafe { webview.configuration().preferences() };
    if preferences.respondsToSelector(sel!(setShouldPrintBackgrounds:)) {
        unsafe { preferences.setShouldPrintBackgrounds(setup.backgrounds) };
    } else if setup.backgrounds {
        return Err(
            "この macOS では背景のグラフィックを印刷できません（macOS 13.3 以降で使えます）"
                .to_owned(),
        );
    }
    let points = |mm: f32| f64::from(mm) * 72. / 25.4;
    let (width, height) = setup.paper_mm();
    let margins = setup.margins_mm();
    let info = NSPrintInfo::new();
    info.setOrientation(if setup.landscape {
        NSPaperOrientation::Landscape
    } else {
        NSPaperOrientation::Portrait
    });
    info.setPaperSize(NSSize::new(points(width), points(height)));
    info.setTopMargin(points(margins.top));
    info.setRightMargin(points(margins.right));
    info.setBottomMargin(points(margins.bottom));
    info.setLeftMargin(points(margins.left));
    info.setHorizontalPagination(NSPrintingPaginationMode::Fit);
    info.setVerticalPagination(NSPrintingPaginationMode::Automatic);
    unsafe {
        info.setJobDisposition(NSPrintSaveJob);
        let target = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let settings = info.dictionary();
        settings.insert(NSPrintJobSavingURL, AsRef::<AnyObject>::as_ref(&*target));
        settings.insert(
            NSPrintScalingFactor,
            AsRef::<AnyObject>::as_ref(&*NSNumber::new_f64(setup.scale_factor())),
        );
        settings.insert(
            NSPrintHeaderAndFooter,
            AsRef::<AnyObject>::as_ref(&*NSNumber::new_bool(setup.header_footer)),
        );
    }
    let operation = unsafe { webview.printOperationWithPrintInfo(&info) };
    operation.setShowsPrintPanel(false);
    operation.setShowsProgressPanel(false);
    // 印刷するビューに大きさがないと、白紙の PDF になる
    if let Some(printing) = operation.view() {
        printing.setFrame(webview.frame());
    }
    let finished = PrintFinished::alloc(mtm).set_ivars(Waiting {
        done: Cell::new(Some(done)),
    });
    let finished: Retained<PrintFinished> = unsafe { msg_send![super(finished), init] };
    WAITING.with(|waiting| waiting.borrow_mut().push(finished.clone()));
    unsafe {
        operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
            &window,
            Some(AsRef::<AnyObject>::as_ref(&*finished)),
            Some(sel!(printOperationDidRun:success:contextInfo:)),
            std::ptr::null_mut(),
        )
    };
    Ok(())
}

/// Linux: WebKitGTK の印刷を、ダイアログを出さずに「ファイルへ印刷」（PDF）に送る。
/// 余白は mm、倍率は % で渡す。cairo がフォントを埋め込む。
#[cfg(target_os = "linux")]
fn save_pdf(
    view: &WebView,
    path: &std::path::Path,
    setup: &crate::print_setup::PrintSetup,
    pages: Option<&[u32]>,
    done: PdfDone,
) -> Result<(), String> {
    use webkit2gtk::{PrintOperation, PrintOperationExt, SettingsExt, WebViewExt};
    use wry::WebViewExtUnix;

    thread_local! {
        /// 印刷している途中の PDF。終わるまで持っておく。
        static PRINTING: RefCell<Vec<PrintOperation>> = const { RefCell::new(Vec::new()) };
    }
    let uri = gtk::glib::filename_to_uri(path, None)
        .map_err(|e| format!("{} を URL にできません: {e}", path.display()))?;
    let settings = gtk::PrintSettings::new();
    settings.set_printer("Print to File");
    settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));
    settings.set(gtk::PRINT_SETTINGS_OUTPUT_URI, Some(&uri));
    let orientation = if setup.landscape {
        gtk::PageOrientation::Landscape
    } else {
        gtk::PageOrientation::Portrait
    };
    let paper = gtk::PaperSize::new(Some(setup.paper.pwg()));
    settings.set_orientation(orientation);
    settings.set_paper_size(&paper);
    settings.set_scale(setup.scale_factor() * 100.);
    settings.set_number_up(i32::from(setup.per_sheet()));
    if let Some(pages) = pages {
        // GTK のページは 0 から数える
        let ranges: Vec<gtk::PageRange> = pages
            .iter()
            .map(|p| {
                let p = *p as i32 - 1;
                gtk::PageRange::new(p, p)
            })
            .collect();
        settings.set_print_pages(gtk::PrintPages::Ranges);
        settings.set_page_ranges(&ranges);
    }
    let page_setup = gtk::PageSetup::new();
    page_setup.set_paper_size(&paper);
    page_setup.set_orientation(orientation);
    let margins = setup.margins_mm();
    page_setup.set_top_margin(f64::from(margins.top), gtk::Unit::Mm);
    page_setup.set_right_margin(f64::from(margins.right), gtk::Unit::Mm);
    page_setup.set_bottom_margin(f64::from(margins.bottom), gtk::Unit::Mm);
    page_setup.set_left_margin(f64::from(margins.left), gtk::Unit::Mm);
    let webview = view.webview();
    if let Some(preferences) = WebViewExt::settings(&webview) {
        preferences.set_print_backgrounds(setup.backgrounds);
    } else if setup.backgrounds {
        return Err(
            "WebKitGTK の設定を読めないので、背景のグラフィックを印刷できません".to_owned(),
        );
    }
    let operation = PrintOperation::new(&webview);
    operation.set_print_settings(&settings);
    operation.set_page_setup(&page_setup);
    let done = Rc::new(RefCell::new(Some(done)));
    let failed = done.clone();
    operation.connect_failed(move |_, error| {
        if let Some(done) = failed.borrow_mut().take() {
            done(Err(error.to_string()));
        }
    });
    // 失敗したときも、そのあとに finished が届く
    operation.connect_finished(move |operation| {
        if let Some(done) = done.borrow_mut().take() {
            done(Ok(()));
        }
        PRINTING.with(|printing| printing.borrow_mut().retain(|p| p != operation));
    });
    PRINTING.with(|printing| printing.borrow_mut().push(operation.clone()));
    operation.print();
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn save_pdf(
    _: &WebView,
    _: &std::path::Path,
    _: &crate::print_setup::PrintSetup,
    _: Option<&[u32]>,
    _: PdfDone,
) -> Result<(), String> {
    Err("この OS では、ページを PDF にできません".into())
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
            .with_download_started_handler({
                let send = send.clone();
                move |url, path| {
                    *path = crate::downloads::download_target(path);
                    // ダウンロードのフォルダがなければ作る。作れなければ、部品がダウンロードの失敗を知らせる
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    send(Event::Download(Note::Started {
                        url,
                        path: path.clone(),
                        ask: crate::download_rules::current().ask,
                    }));
                    true
                }
            })
            .with_download_completed_handler({
                let send = send.clone();
                move |url, path, ok| send(Event::Download(Note::Finished { url, path, ok }))
            })
            .with_document_title_changed_handler({
                let send = send.clone();
                move |title| send(Event::Title(title))
            });
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
        // 進み具合は、部品の知らせから読む（macOS は、ダウンロード中のファイルの大きさを見る）
        #[cfg(windows)]
        print_from_menu(&view, send.clone())?;
        watch_downloads(&view, send)?;
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
    use super::{key_script, popup_url, stays_inside};

    /// ページのスクリプトに、埋めていない印（__TAKE__ など）が残らない。印刷のボタンは印刷のキーとして知らせる
    #[test]
    fn the_key_script_has_every_value_filled_in() {
        let script = key_script();
        for mark in ["__TAKE__", "__SOFT__", "__PRINT__"] {
            assert!(!script.contains(mark), "{mark}");
        }
        let print = format!(
            "window.print = () => send(\"{}\")",
            crate::shell_keys::PRINT_KEY
        );
        assert!(script.contains(&print));
    }

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
