//! 設定の「アクセスキー」と「MCP の接続」。アクセスキーの発行・名前・できること・失効と、
//! キーなしの接続の決まり、MCP を HTTP で開くかどうか。
//!
//! キーなしの接続の決まりは、キーと同じボタン（読み取りのみ・ロック中も・期限と時間帯）で決める。
//!
//! キーは「＋」で「新規アクセスキー」の名前のまま発行し、その行でそのまま名前を書ける。
//! 発行したキーの文字列は、設定を閉じるまでその行の下に出す（あとからは出せない）。
//! 「ロック中も使える」をオンにするときは、ロックが有効ならパスキーで本人確認をする。
//! 時計のボタンで、そのキーの開始・期限・使える時間帯（曜日と時刻）をその場で変える。

use gpui_kit::base::{Disableable, TestSupportExt};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::access::{
    self, AccessKey, Expiry, KeyPatch, Window as Hours, NEW_KEY_NAME, WEEKDAYS,
};
use malakt_core::{view, Id};

use crate::feel::{self, Lit};
use crate::fonts::MonoFont;
use crate::mcp_http;
use crate::press::button;
use crate::render::{app_icon, col, px, row, Framed};
use crate::state::{LimitField, MainView};

/// キーと決まりの行のボタンの id。
struct EntryIds {
    off: &'static str,
    write: &'static str,
    lock: &'static str,
    limits: &'static str,
}

const KEY_IDS: EntryIds = EntryIds {
    off: "access-key-off",
    write: "access-key-write",
    lock: "access-key-lock",
    limits: "access-key-limits",
};

const RULE_IDS: EntryIds = EntryIds {
    off: "keyless-off",
    write: "keyless-write",
    lock: "keyless-lock",
    limits: "keyless-limits",
};

/// キーの行の高さ。担当者の一覧とそろえる。
const KEY_ROW_HEIGHT: Pixels = px(32.);

impl MainView {
    pub fn access_keys(&self, cx: &App) -> Vec<AccessKey> {
        match self.db.read(cx).store.access_keys() {
            Ok(keys) => keys,
            Err(error) => {
                // 描くたびに呼ぶので、ここでは画面のエラーに書かず、空の一覧を出す。
                // 書き換えのときに、同じエラーが画面に出る
                eprintln!("アクセスキーを読めませんでした: {error}");
                Vec::new()
            }
        }
    }

    /// キーなしの接続の決まり。
    pub fn keyless_rules(&self, cx: &App) -> Vec<AccessKey> {
        match self.db.read(cx).store.keyless_rules() {
            Ok(rules) => rules,
            Err(error) => {
                // 描くたびに呼ぶので、ここでは画面のエラーに書かない。書き換えのときに、同じエラーが出る
                eprintln!("キーなしの接続の決まりを読めませんでした: {error}");
                Vec::new()
            }
        }
    }

    /// キーなしの接続の決まりを足す（読み書き・いつでも）。足したら、期限と時間帯の欄を開く。
    pub fn add_keyless_rule(&mut self, cx: &mut Context<Self>) {
        if let Some(rule) = self.write(cx, |s| s.add_keyless_rule(false)) {
            self.key_limits_open = Some(rule.id);
            self.key_limit_editing = None;
        }
        cx.notify();
    }

    pub fn remove_keyless_rule(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.write(cx, |s| s.remove_keyless_rule(id)).is_some()
            && self.key_limits_open.as_deref() == Some(id)
        {
            self.key_limits_open = None;
            self.key_limit_editing = None;
        }
        cx.notify();
    }

    /// キーか、キーなしの接続の決まり。
    fn access_entry(&self, id: &str, cx: &App) -> Option<AccessKey> {
        self.db.read(cx).store.access_key(id).ok()
    }

    fn is_keyless_rule(&self, id: &str, cx: &App) -> bool {
        self.keyless_rules(cx).iter().any(|r| r.id == id)
    }

    /// MCP を HTTP で開くかと、そのポート。
    pub fn mcp_http(&self, cx: &App) -> (bool, u16) {
        let store = &self.db.read(cx).store;
        (
            mcp_http::enabled(store).unwrap_or(false),
            mcp_http::port(store).unwrap_or(mcp_http::DEFAULT_PORT),
        )
    }

    pub fn set_mcp_http(&mut self, on: bool, cx: &mut Context<Self>) {
        let (_, port) = self.mcp_http(cx);
        self.open_mcp_http(on, port, cx);
    }

    /// HTTP を開く・閉じるか、ポートを変える。開けなければエラーを出し、設定も前のまま。
    fn open_mcp_http(&mut self, on: bool, port: u16, cx: &mut Context<Self>) -> bool {
        let Some(db) = self.db.read(cx).store.file_path() else {
            self.error =
                Some("データベースの場所が分からないので、MCP を HTTP で開けません。".into());
            cx.notify();
            return false;
        };
        let (was_on, was_port) = self.mcp_http(cx);
        if let Err(error) = mcp_http::apply(&db, on, port) {
            self.error = Some(error.into());
            cx.notify();
            return false;
        }
        if self.write(cx, |s| mcp_http::save(s, on, port)).is_none() {
            // 保存できなければ、開く前に戻す。戻せなければ、その理由も出す
            if let Err(error) = mcp_http::apply(&db, was_on, was_port) {
                self.error = Some(error.into());
            }
            cx.notify();
            return false;
        }
        cx.notify();
        true
    }

    /// MCP の URL をコピーする。
    pub fn copy_mcp_url(&mut self, cx: &mut Context<Self>) {
        let (_, port) = self.mcp_http(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(mcp_http::url(port)));
        self.show_notice("MCP の URL をコピーしました。".into());
        cx.notify();
    }

    /// 「新規アクセスキー」の名前で発行して、その行でそのまま名前を書けるようにする。
    pub fn issue_access_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(issued) = self.write(cx, |s| s.issue_access_key(NEW_KEY_NAME, false, false)) {
            let id = issued.key.id.clone();
            self.issued_access_key = Some(issued);
            self.rename_access_key(id, window, cx);
        }
    }

    pub fn rename_access_key(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.access_keys(cx).into_iter().find(|k| k.id == id) else {
            return;
        };
        self.inputs.key_name.update(cx, |s, cx| {
            s.set_value(key.name, window, cx);
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.key_naming = Some(id);
        cx.notify();
    }

    /// 書いたキーの名前を保存する。Enter か、ほかの場所を押したとき。
    pub fn save_access_key_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.key_naming.clone() else {
            return;
        };
        let name = self.inputs.key_name.read(cx).value().trim().to_owned();
        let current = self.access_keys(cx).into_iter().find(|k| k.id == id);
        // 空にしたときと変えていないときは、今の名前のままにする
        if current.is_some_and(|k| !name.is_empty() && k.name != name) {
            let patch = KeyPatch {
                name: Some(name.clone()),
                ..Default::default()
            };
            if self
                .write(cx, |s| s.update_access_key(&id, &patch))
                .is_none()
            {
                return;
            }
            if let Some(issued) = self.issued_access_key.as_mut().filter(|i| i.key.id == id) {
                issued.key.name = name;
            }
        }
        self.key_naming = None;
        self.leave_input(&self.inputs.key_name.clone(), window, cx);
        cx.notify();
    }

    pub fn toggle_key_read_only(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(key) = self.access_entry(id, cx) else {
            return;
        };
        let patch = KeyPatch {
            read_only: Some(!key.read_only),
            ..Default::default()
        };
        self.write(cx, |s| s.update_access_key(id, &patch));
        cx.notify();
    }

    /// 「ロック中も使える」を切り替える。オンにするときは、ロックが有効ならパスキーで確かめる。
    pub fn toggle_key_while_locked(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.access_entry(&id, cx) else {
            return;
        };
        let what = if self.is_keyless_rule(&id, cx) {
            "ロック中もキーなしで使えるようにする"
        } else {
            "ロック中も使えるアクセスキーにする"
        };
        if key.while_locked || !self.lock_enabled(cx) {
            self.set_key_while_locked(&id, !key.while_locked, cx);
            return;
        }
        self.pending_key_unlock = Some(id);
        self.with_passkey(what, window, cx, |this, cx| {
            if let Some(id) = this.pending_key_unlock.take() {
                this.set_key_while_locked(&id, true, cx);
            }
        });
    }

    fn set_key_while_locked(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        let patch = KeyPatch {
            while_locked: Some(on),
            ..Default::default()
        };
        self.write(cx, |s| s.update_access_key(id, &patch));
        cx.notify();
    }

    pub fn revoke_access_key(&mut self, id: &str, cx: &mut Context<Self>) {
        let name = self
            .access_keys(cx)
            .into_iter()
            .find(|k| k.id == id)
            .map(|k| k.name);
        if self.write(cx, |s| s.revoke_access_key(id)).is_some() {
            if self
                .issued_access_key
                .as_ref()
                .is_some_and(|i| i.key.id == id)
            {
                self.issued_access_key = None;
            }
            if self.key_naming.as_deref() == Some(id) {
                self.key_naming = None;
            }
            if let Some(name) = name {
                self.show_notice(format!("アクセスキー「{name}」を失効させました。").into());
            }
        }
        cx.notify();
    }

    /// キーの期限と時間帯の欄を開く・閉じる。
    pub fn toggle_key_limits(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.key_limits_open.as_deref() == Some(id) {
            self.key_limits_open = None;
        } else {
            self.key_limits_open = Some(id.to_owned());
        }
        self.key_limit_editing = None;
        cx.notify();
    }

    fn update_key(&mut self, id: &str, patch: KeyPatch, cx: &mut Context<Self>) -> bool {
        let saved = self
            .write(cx, |s| s.update_access_key(id, &patch))
            .is_some();
        cx.notify();
        saved
    }

    /// 期限を、今からの長さで決める。`None` は期限なし。
    pub fn set_key_expiry_in(&mut self, id: &str, preset: Option<Expiry>, cx: &mut Context<Self>) {
        let expires = preset.map(|p| p.until(access::now_local()));
        self.update_key(
            id,
            KeyPatch {
                expires: Some(expires),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn clear_key_start(&mut self, id: &str, cx: &mut Context<Self>) {
        self.update_key(
            id,
            KeyPatch {
                starts: Some(None),
                ..Default::default()
            },
            cx,
        );
    }

    fn set_key_windows(&mut self, id: &str, windows: Vec<Hours>, cx: &mut Context<Self>) {
        self.update_key(
            id,
            KeyPatch {
                windows: Some(windows),
                ..Default::default()
            },
            cx,
        );
    }

    fn key_windows(&self, id: &str, cx: &App) -> Vec<Hours> {
        self.access_entry(id, cx)
            .map(|k| k.windows)
            .unwrap_or_default()
    }

    /// 時間帯を足す。最初は毎日 9:00〜18:00。
    pub fn add_key_window(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut windows = self.key_windows(id, cx);
        windows.push(Hours::standard());
        self.set_key_windows(id, windows, cx);
    }

    pub fn remove_key_window(&mut self, id: &str, ix: usize, cx: &mut Context<Self>) {
        let mut windows = self.key_windows(id, cx);
        if ix < windows.len() {
            windows.remove(ix);
            self.set_key_windows(id, windows, cx);
        }
    }

    /// 時間帯の曜日を付ける・外す。最後の 1 つは外さない（曜日のない時間帯は作れない）。
    pub fn toggle_key_window_day(&mut self, id: &str, ix: usize, day: u8, cx: &mut Context<Self>) {
        let mut windows = self.key_windows(id, cx);
        let Some(window) = windows.get_mut(ix) else {
            return;
        };
        if let Some(at) = window.days.iter().position(|d| *d == day) {
            if window.days.len() == 1 {
                return;
            }
            window.days.remove(at);
        } else {
            window.days.push(day);
        }
        self.set_key_windows(id, windows, cx);
    }

    /// 開始・期限・時間帯の時刻を、その場の入力欄で書き直す。
    pub fn edit_key_limit(
        &mut self,
        id: Id,
        field: LimitField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = if field == LimitField::Port {
            self.mcp_http(cx).1.to_string()
        } else {
            let Some(key) = self.access_entry(&id, cx) else {
                return;
            };
            match field {
                // 見えている書き方のまま直せるようにする
                LimitField::Starts => key
                    .starts
                    .as_deref()
                    .map(access::moment_label)
                    .unwrap_or_default(),
                LimitField::Expires => key
                    .expires
                    .as_deref()
                    .map(access::moment_label)
                    .unwrap_or_default(),
                LimitField::From(ix) => key
                    .windows
                    .get(ix)
                    .map(|w| access::clock(w.from))
                    .unwrap_or_default(),
                LimitField::To(ix) => key
                    .windows
                    .get(ix)
                    .map(|w| access::clock(w.to))
                    .unwrap_or_default(),
                LimitField::Port => unreachable!("ポートは上で読んだ"),
            }
        };
        self.inputs.key_limit.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.key_limit_editing = Some((id, field));
        cx.notify();
    }

    /// 書いた開始・期限・時刻を保存する。Enter か、ほかの場所を押したとき。
    /// 読めない書き方なら、何も変えずにエラーを出して、入力欄はそのまま。
    pub fn save_key_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, field)) = self.key_limit_editing.clone() else {
            return;
        };
        let text = self.inputs.key_limit.read(cx).value().trim().to_owned();
        if field == LimitField::Port {
            let port = match text.parse::<u16>() {
                Ok(port) if port > 0 => port,
                _ => {
                    self.error = Some(format!("ポート番号として読めません: {text}").into());
                    cx.notify();
                    return;
                }
            };
            let (on, _) = self.mcp_http(cx);
            if !self.open_mcp_http(on, port, cx) {
                return;
            }
            self.key_limit_editing = None;
            self.leave_input(&self.inputs.key_limit.clone(), window, cx);
            cx.notify();
            return;
        }
        let patch = match field {
            LimitField::Starts | LimitField::Expires => {
                let value = if text.is_empty() {
                    None
                } else {
                    match malakt_core::when::parse(&text, &view::now(), None) {
                        Some(value) => Some(value),
                        None => {
                            self.error = Some(format!("日時として読めません: {text}").into());
                            cx.notify();
                            return;
                        }
                    }
                };
                if field == LimitField::Starts {
                    KeyPatch {
                        starts: Some(value),
                        ..Default::default()
                    }
                } else {
                    KeyPatch {
                        expires: Some(value),
                        ..Default::default()
                    }
                }
            }
            LimitField::From(ix) | LimitField::To(ix) => {
                let Some(minutes) = access::parse_clock(&text) else {
                    self.error = Some(format!("時刻として読めません: {text}").into());
                    cx.notify();
                    return;
                };
                let mut windows = self.key_windows(&id, cx);
                let Some(w) = windows.get_mut(ix) else {
                    self.key_limit_editing = None;
                    cx.notify();
                    return;
                };
                if matches!(field, LimitField::From(_)) {
                    w.from = minutes.min(24 * 60 - 1);
                } else {
                    w.to = minutes;
                }
                KeyPatch {
                    windows: Some(windows),
                    ..Default::default()
                }
            }
            LimitField::Port => unreachable!("ポートは上で保存した"),
        };
        if !self.update_key(&id, patch, cx) {
            return;
        }
        self.key_limit_editing = None;
        self.leave_input(&self.inputs.key_limit.clone(), window, cx);
        cx.notify();
    }

    /// 発行したばかりのキーを、選んだ形でコピーする。
    fn copy_issued_key(&mut self, what: Copy, cx: &mut Context<Self>) {
        let Some(issued) = &self.issued_access_key else {
            return;
        };
        let secret = issued.secret.clone();
        let text = match what {
            Copy::Key => secret,
            Copy::ClaudeHttp => mcp_http::claude_add_command(self.mcp_http(cx).1, &secret),
            Copy::Claude | Copy::Config => {
                let exe = match std::env::current_exe() {
                    Ok(exe) => exe,
                    Err(error) => {
                        self.error = Some(
                            format!("アプリの実行ファイルの場所が分かりません: {error}").into(),
                        );
                        cx.notify();
                        return;
                    }
                };
                if what == Copy::Claude {
                    crate::mcp::claude_add_command(&exe, &secret)
                } else {
                    // 標準でない場所のデータベースを使っているときは、その場所も渡す
                    let db = std::env::var_os(crate::mcp::ENV_DB).map(std::path::PathBuf::from);
                    let config = crate::mcp::config_json(&exe, db.as_deref(), Some(&secret));
                    match serde_json::to_string_pretty(&config) {
                        Ok(text) => text,
                        Err(error) => {
                            self.error =
                                Some(format!("MCP の設定を作れませんでした: {error}").into());
                            cx.notify();
                            return;
                        }
                    }
                }
            }
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_notice(what.done().into());
        cx.notify();
    }

    /// 設定の「アクセスキー」。キーの一覧と、発行のボタン。
    pub(crate) fn access_keys_editor(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col().gap_1();
        for (ix, key) in self.access_keys(cx).into_iter().enumerate() {
            let id = key.id.clone();
            let name = if self.key_naming.as_ref() == Some(&key.id) {
                // その場で名前を書く。Enter かほかの場所を押すと保存、Esc で取り消し
                div()
                    .flex_1()
                    .min_w_0()
                    .on_mouse_down_out(
                        cx.listener(|this, _, window, cx| this.save_access_key_name(window, cx)),
                    )
                    .child(
                        Input::new(&self.inputs.key_name)
                            .id("access-key-input")
                            .aria_label("アクセスキーの名前")
                            .text_base()
                            .bg(theme.background)
                            .border_color(theme.ring)
                            .px_2(),
                    )
                    .into_any_element()
            } else {
                let id = id.clone();
                div()
                    .id(("access-key", ix))
                    .flex_1()
                    .min_w_0()
                    .h(KEY_ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .px_2()
                    .rounded(feel::md())
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .cursor_pointer()
                    .lit(theme.muted, cx)
                    .child(div().truncate().child(key.name.clone()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.rename_access_key(id.clone(), window, cx)
                    }))
                    .test_support()
                    .into_any_element()
            };
            let used: SharedString = match key.used_at {
                Some(at) => format!(
                    "発行 {}・最後に使った日時 {}",
                    view::format_datetime(key.created_at),
                    view::format_datetime(at)
                )
                .into(),
                None => format!("発行 {}・未使用", view::format_datetime(key.created_at)).into(),
            };
            let masked = row()
                .id(("access-key-masked", ix))
                .gap_1p5()
                .flex_none()
                .child(
                    app_icon("key-round")
                        .size_4()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .mono_font(cx)
                        .child(key.masked()),
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(used.clone()).build(window, cx)
                })
                .test_support();
            let (denied, toggles) = self.entry_toggles(&key, &KEY_IDS, ix, cx);
            let revoke_id = id.clone();
            let view_handle = cx.entity().downgrade();
            list = list.child(
                row()
                    .gap_2()
                    .child(name)
                    .children(denied)
                    .child(masked)
                    .child(toggles)
                    .child(
                        button(("access-key-revoke", ix))
                            .icon(app_icon("trash"))
                            .ghost()
                            .xsmall()
                            .tooltip("失効させる")
                            .pop_menu(move |menu, _, _| {
                                let (view, id) = (view_handle.clone(), revoke_id.clone());
                                menu.item(PopupMenuItem::new("失効させる").on_click(
                                    move |_, _, cx| {
                                        // 設定を閉じたあとなら、反映する先がない
                                        let _ = view
                                            .update(cx, |this, cx| this.revoke_access_key(&id, cx));
                                    },
                                ))
                            }),
                    ),
            );
            if let Some(issued) = self
                .issued_access_key
                .as_ref()
                .filter(|i| i.key.id == key.id)
            {
                list = list.child(self.issued_key_box(&issued.secret, cx));
            }
            if self.key_limits_open.as_ref() == Some(&key.id) {
                list = list.child(self.key_limits(&key, false, cx));
            }
        }
        col().gap_3().child(list).child(
            row().child(feel::gloss(
                button("access-key-add")
                    .icon(app_icon("plus"))
                    .framed(cx)
                    .tooltip("アクセスキーを発行")
                    .on_click(cx.listener(|this, _, window, cx| this.issue_access_key(window, cx))),
            )),
        )
    }

    /// キーと決まりの行の右側。今使えないときの印と、読み取りのみ・ロック中も・期限と時間帯のボタン。
    fn entry_toggles(
        &self,
        key: &AccessKey,
        ids: &EntryIds,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, Div) {
        let theme = cx.theme().clone();
        let (write_id, lock_id, limits_id) = (key.id.clone(), key.id.clone(), key.id.clone());
        // 今は期限・時間帯の外なら、印を出す（ホバーで理由）
        let denied = key.usable_at(access::now_local()).err().map(|d| {
            let why = SharedString::from(d.to_string());
            div()
                .id((ids.off, ix))
                .child(app_icon("calendar-x").size_4().text_color(theme.danger))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(why.clone()).build(window, cx)
                })
                .test_support()
                .into_any_element()
        });
        let toggles = row()
            .gap_2()
            .child(feel::gloss(
                button((ids.write, ix))
                    // 押すたびに切り替える。オンのときは押したままの見た目
                    .icon(app_icon("eye"))
                    .ghost()
                    .xsmall()
                    .selected(key.read_only)
                    .tooltip("読み取りのみ")
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_key_read_only(&write_id, cx)),
                    ),
            ))
            .child(feel::gloss(
                button((ids.lock, ix))
                    .icon(app_icon("lock-open"))
                    .ghost()
                    .xsmall()
                    .selected(key.while_locked)
                    .disabled(self.verifying(cx))
                    .tooltip("ロック中も使える")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_key_while_locked(lock_id.clone(), window, cx)
                    })),
            ))
            .child(feel::gloss(
                button((ids.limits, ix))
                    .icon(app_icon("calendar-clock"))
                    .ghost()
                    .xsmall()
                    .selected(key.limited())
                    .tooltip(limits_tip(key))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_key_limits(&limits_id, cx)),
                    ),
            ));
        (denied, toggles)
    }

    /// 設定の「キーなしの接続」。決まりの一覧と、足すボタン。
    pub(crate) fn keyless_editor(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col().gap_1();
        let rules = self.keyless_rules(cx);
        if rules.is_empty() {
            list = list.child(
                div()
                    .id("keyless-none")
                    .h(KEY_ROW_HEIGHT)
                    .px_2()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("アクセスキーが必要")
                    .test_support(),
            );
        }
        for (ix, rule) in rules.into_iter().enumerate() {
            let (open_id, remove_id) = (rule.id.clone(), rule.id.clone());
            let (denied, toggles) = self.entry_toggles(&rule, &RULE_IDS, ix, cx);
            let label = div()
                .id(("keyless", ix))
                .flex_1()
                .min_w_0()
                .h(KEY_ROW_HEIGHT)
                .flex()
                .items_center()
                .px_2()
                .rounded(feel::md())
                .cursor_pointer()
                .lit(theme.muted, cx)
                .child(div().truncate().child(rule.when_label()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_key_limits(&open_id, cx)
                }))
                .test_support();
            list = list.child(
                row()
                    .gap_2()
                    .child(label)
                    .children(denied)
                    .child(toggles)
                    .child(feel::gloss(
                        button(("keyless-remove", ix))
                            .icon(app_icon("trash"))
                            .ghost()
                            .xsmall()
                            .tooltip("外す")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_keyless_rule(&remove_id, cx)
                            })),
                    )),
            );
            if self.key_limits_open.as_ref() == Some(&rule.id) {
                list = list.child(self.key_limits(&rule, true, cx));
            }
        }
        col().gap_3().child(list).child(
            row().child(feel::gloss(
                button("keyless-add")
                    .icon(app_icon("plus"))
                    .framed(cx)
                    .tooltip("キーなしの接続を追加")
                    .on_click(cx.listener(|this, _, _, cx| this.add_keyless_rule(cx))),
            )),
        )
    }

    /// 「MCP の接続」の HTTP のポート。押すと、その場で書き直す。
    pub(crate) fn mcp_port_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let (_, port) = self.mcp_http(cx);
        self.limit_value(
            "",
            LimitField::Port,
            "mcp-http-port",
            port.to_string(),
            false,
            cx,
        )
    }

    /// 「MCP の接続」の URL と、コピーのボタン。
    pub(crate) fn mcp_url_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (_, port) = self.mcp_http(cx);
        row()
            .gap_1()
            .child(
                div()
                    .id("mcp-http-url")
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .mono_font(cx)
                    .child(mcp_http::url(port))
                    .test_support(),
            )
            .child(feel::gloss(
                button("mcp-http-copy")
                    .icon(app_icon("copy"))
                    .ghost()
                    .xsmall()
                    .tooltip("URL をコピー")
                    .on_click(cx.listener(|this, _, _, cx| this.copy_mcp_url(cx))),
            ))
            .into_any_element()
    }

    /// その場で書き直す値。書いている最中なら入力欄、そうでなければ押せる文字。
    fn limit_value(
        &self,
        owner: &str,
        field: LimitField,
        id: impl Into<ElementId>,
        text: String,
        placeholder: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let editing = self
            .key_limit_editing
            .as_ref()
            .is_some_and(|(k, f)| k == owner && *f == field);
        if editing {
            return div()
                .w(px(150.))
                .on_mouse_down_out(
                    cx.listener(|this, _, window, cx| this.save_key_limit(window, cx)),
                )
                .child(
                    Input::new(&self.inputs.key_limit)
                        .id("access-key-limit-input")
                        .aria_label(field.label())
                        .small()
                        .bg(theme.background)
                        .border_color(theme.ring),
                )
                .into_any_element();
        }
        let key_id: Id = owner.to_owned();
        div()
            .id(id)
            .px_2()
            .h(px(26.))
            .flex()
            .items_center()
            .rounded(feel::md())
            .cursor_pointer()
            .text_sm()
            .when(placeholder, |d| d.text_color(theme.muted_foreground))
            .lit(theme.muted, cx)
            .child(text)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.edit_key_limit(key_id.clone(), field, window, cx)
            }))
            .test_support()
            .into_any_element()
    }

    /// キーの開始・期限・時間帯。キーの行の下に出す。
    fn key_limits(&self, key: &AccessKey, rule: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let label = |text: &'static str| {
            div()
                .w(px(64.))
                .flex_none()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let shown = |value: &Option<String>, none: &str| match value {
            Some(v) => (access::moment_label(v), false),
            None => (none.to_owned(), true),
        };
        // 開始
        let (starts, empty) = shown(
            &key.starts,
            if rule {
                "足したときから"
            } else {
                "発行したときから"
            },
        );
        let start_id = key.id.clone();
        let start_row = row()
            .gap_2()
            .child(label("開始"))
            .child(self.limit_value(
                &key.id,
                LimitField::Starts,
                "access-key-starts",
                starts,
                empty,
                cx,
            ))
            .when(key.starts.is_some(), |d| {
                d.child(feel::gloss(
                    button("access-key-starts-clear")
                        .icon(app_icon("x"))
                        .ghost()
                        .xsmall()
                        .tooltip("開始を外す")
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.clear_key_start(&start_id, cx)),
                        ),
                ))
            });
        // 期限。今からの長さでも選べる
        let (expires, empty) = shown(&key.expires, "なし");
        let view_handle = cx.entity().downgrade();
        let expiry_id = key.id.clone();
        let expiry_row = row()
            .gap_2()
            .child(label("期限"))
            .child(self.limit_value(
                &key.id,
                LimitField::Expires,
                "access-key-expires",
                expires,
                empty,
                cx,
            ))
            .child(
                button("access-key-expiry-menu")
                    .icon(app_icon("hourglass"))
                    .ghost()
                    .xsmall()
                    .tooltip("今から")
                    .pop_menu(move |mut menu, _, _| {
                        for preset in std::iter::once(None).chain(Expiry::ALL.map(Some)) {
                            let (view, id) = (view_handle.clone(), expiry_id.clone());
                            let name = preset.map_or("なし", Expiry::label);
                            menu = menu.item(PopupMenuItem::new(name).on_click(move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = view
                                    .update(cx, |this, cx| this.set_key_expiry_in(&id, preset, cx));
                            }));
                        }
                        menu
                    }),
            );
        // 時間帯
        let mut hours = col().gap_1();
        for (wx, w) in key.windows.iter().enumerate() {
            let mut days = row().gap_0p5();
            for (d, name) in WEEKDAYS.iter().enumerate() {
                let day_id = key.id.clone();
                days = days.child(feel::gloss(
                    button(("access-key-day", wx * 7 + d))
                        .label(*name)
                        .ghost()
                        .xsmall()
                        .selected(w.days.contains(&(d as u8)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_key_window_day(&day_id, wx, d as u8, cx)
                        })),
                ));
            }
            let remove_id = key.id.clone();
            hours = hours.child(
                row().gap_2().flex_wrap().child(days).child(
                    row()
                        .gap_1()
                        .child(self.limit_value(
                            &key.id,
                            LimitField::From(wx),
                            ("access-key-from", wx),
                            access::clock(w.from),
                            false,
                            cx,
                        ))
                        .child(div().text_sm().child("〜"))
                        .child(self.limit_value(
                            &key.id,
                            LimitField::To(wx),
                            ("access-key-to", wx),
                            access::clock(w.to),
                            false,
                            cx,
                        ))
                        .child(feel::gloss(
                            button(("access-key-window-remove", wx))
                                .icon(app_icon("x"))
                                .ghost()
                                .xsmall()
                                .tooltip("時間帯を外す")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_key_window(&remove_id, wx, cx)
                                })),
                        )),
                ),
            );
        }
        let add_id = key.id.clone();
        let hours_row = row()
            .gap_2()
            .items_start()
            .child(label("時間帯").pt_1())
            .child(
                col()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .when(key.windows.is_empty(), |d| {
                        d.child(
                            div()
                                .px_2()
                                .pt_1()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("いつでも"),
                        )
                    })
                    .child(hours)
                    .child(
                        row().child(feel::gloss(
                            button("access-key-window-add")
                                .icon(app_icon("plus"))
                                .ghost()
                                .xsmall()
                                .tooltip("時間帯を追加")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_key_window(&add_id, cx)
                                })),
                        )),
                    ),
            );
        col()
            .gap_1()
            .ml_2()
            .p_2()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .child(start_row)
            .child(expiry_row)
            .child(hours_row)
    }

    /// 発行したばかりのキーの文字列と、コピーのボタン。
    fn issued_key_box(&self, secret: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let copy = |id: &'static str, icon: &str, what: Copy, cx: &mut Context<Self>| {
            feel::gloss(
                button(id)
                    .icon(app_icon(icon))
                    .ghost()
                    .xsmall()
                    .tooltip(what.tip())
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_issued_key(what, cx))),
            )
        };
        col()
            .gap_1()
            .ml_2()
            .p_2()
            .rounded(feel::md())
            .bg(theme.secondary)
            .child(
                row()
                    .gap_1()
                    .child(
                        div()
                            .id("access-key-secret")
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .mono_font(cx)
                            .overflow_x_hidden()
                            .child(secret.to_owned())
                            .test_support(),
                    )
                    .child(copy("access-key-copy", "copy", Copy::Key, cx))
                    .child(copy("access-key-copy-claude", "terminal", Copy::Claude, cx))
                    .when(self.mcp_http(cx).0, |d| {
                        d.child(copy("access-key-copy-http", "globe", Copy::ClaudeHttp, cx))
                    })
                    .child(copy("access-key-copy-config", "braces", Copy::Config, cx))
                    .child(feel::gloss(
                        button("access-key-hide")
                            .icon(app_icon("x"))
                            .ghost()
                            .xsmall()
                            .tooltip("閉じる")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.issued_access_key = None;
                                cx.notify();
                            })),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("キーは今だけ表示されます"),
            )
    }
}

/// 発行したばかりのキーを、どの形でコピーするか。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copy {
    Key,
    Claude,
    ClaudeHttp,
    Config,
}

impl Copy {
    fn tip(self) -> &'static str {
        match self {
            Copy::Key => "キーをコピー",
            Copy::Claude => "Claude Code に登録するコマンドをコピー",
            Copy::ClaudeHttp => "Claude Code に HTTP で登録するコマンドをコピー",
            Copy::Config => "MCP の設定（JSON）をコピー",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Copy::Key => "アクセスキーをコピーしました。",
            Copy::Claude => "Claude Code に登録するコマンドをコピーしました。",
            Copy::ClaudeHttp => "Claude Code に HTTP で登録するコマンドをコピーしました。",
            Copy::Config => "MCP の設定をコピーしました。",
        }
    }
}

/// 時計のボタンのツールチップ。絞っていれば、その中身。
fn limits_tip(key: &AccessKey) -> SharedString {
    if !key.limited() {
        return "期限と時間帯".into();
    }
    key.when_label().into()
}
