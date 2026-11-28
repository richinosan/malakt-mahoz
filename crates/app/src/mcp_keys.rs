//! 設定の「MCP」。キーなしの接続の扱いと、アクセスキーの発行・名前・できること・失効。
//!
//! キーは「＋」で「新規アクセスキー」の名前のまま発行し、その行でそのまま名前を書ける。
//! 発行したキーの文字列は、設定を閉じるまでその行の下に出す（あとからは出せない）。
//! 「ロック中も使える」をオンにするときは、ロックが有効ならパスキーで本人確認をする。

use gpui_kit::base::{Disableable, TestSupportExt};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable};
use gpui_kit::*;
use malakt_core::access::{self, AccessKey, KeyPatch, Keyless, NEW_KEY_NAME};
use malakt_core::{view, Id};

use crate::feel::{self, Lit};
use crate::fonts::MonoFont;
use crate::press::button;
use crate::render::{app_icon, col, px, row, Framed};
use crate::state::MainView;

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

    pub fn keyless(&self, cx: &App) -> Keyless {
        access::keyless(&self.db.read(cx).store).unwrap_or(Keyless::Off)
    }

    pub fn set_keyless(&mut self, keyless: Keyless, cx: &mut Context<Self>) {
        self.write(cx, |s| access::set_keyless(s, keyless));
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
        let Some(key) = self.access_keys(cx).into_iter().find(|k| k.id == id) else {
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
        let Some(key) = self.access_keys(cx).into_iter().find(|k| k.id == id) else {
            return;
        };
        if key.while_locked || !self.lock_enabled(cx) {
            self.set_key_while_locked(&id, !key.while_locked, cx);
            return;
        }
        self.pending_key_unlock = Some(id);
        self.with_passkey(
            "ロック中も使えるアクセスキーにする",
            window,
            cx,
            |this, cx| {
                if let Some(id) = this.pending_key_unlock.take() {
                    this.set_key_while_locked(&id, true, cx);
                }
            },
        );
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

    /// 発行したばかりのキーを、選んだ形でコピーする。
    fn copy_issued_key(&mut self, what: Copy, cx: &mut Context<Self>) {
        let Some(issued) = &self.issued_access_key else {
            return;
        };
        let secret = issued.secret.clone();
        let text = match what {
            Copy::Key => secret,
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
            let (write_id, lock_id) = (id.clone(), id.clone());
            let revoke_id = id.clone();
            let view_handle = cx.entity().downgrade();
            list = list.child(
                row()
                    .gap_2()
                    .child(name)
                    .child(masked)
                    .child(feel::gloss(
                        button(("access-key-write", ix))
                            // 押すたびに切り替える。オンのときは押したままの見た目
                            .icon(app_icon("eye"))
                            .ghost()
                            .xsmall()
                            .selected(key.read_only)
                            .tooltip("読み取りのみ")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_key_read_only(&write_id, cx)
                            })),
                    ))
                    .child(feel::gloss(
                        button(("access-key-lock", ix))
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
    Config,
}

impl Copy {
    fn tip(self) -> &'static str {
        match self {
            Copy::Key => "キーをコピー",
            Copy::Claude => "Claude Code に登録するコマンドをコピー",
            Copy::Config => "MCP の設定（JSON）をコピー",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Copy::Key => "アクセスキーをコピーしました。",
            Copy::Claude => "Claude Code に登録するコマンドをコピーしました。",
            Copy::Config => "MCP の設定をコピーしました。",
        }
    }
}
