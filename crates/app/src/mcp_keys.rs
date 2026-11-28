//! 設定の「アクセスキー」と「MCP の接続」。
//!
//! キーとキーなしの接続の決まりは、一覧に名前・できること・使える時間・今使えるかを文字で並べ、
//! 押すと詳細を開いて変える（Esc か「←」で一覧に戻る）。詳細では、できること（読み書き /
//! 読み取りのみ）、ロック中も使うか、開始と期限（タスクと同じカレンダー）、使える時間
//! （いつでも / 毎日 / 平日 / 曜日ごと、時刻はメニュー）を選ぶ。
//!
//! キーは「アクセスキーを発行」で「新規アクセスキー」の名前のまま発行し、詳細でそのまま名前を書ける。
//! 発行したキーの文字列は、設定を閉じるまで詳細に出す（あとからは出せない）。
//! 「ロック中も使う」をオンにするときは、ロックが有効ならパスキーで本人確認をする。
//!
//! 「HTTP で開く」は、ポートを自動（空いているポートを選んで覚える）か固定で選ぶ。
//! 開けなかった理由は、その欄の中にだけ出す。

use gpui_kit::base::{Disableable, TestSupportExt};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::access::{
    self, AccessKey, Expiry, HoursMode, KeyPatch, Window as Hours, NEW_KEY_NAME, STANDARD_HOURS,
    WEEKDAYS,
};
use malakt_core::{view, Id};

use crate::date_pick::{pill, DateTarget};
use crate::feel::{self, Lit};
use crate::fonts::MonoFont;
use crate::mcp_http::{self, PortMode};
use crate::press::button;
use crate::render::{app_icon, col, px, row, Framed};
use crate::state::{CustomizeSection, LimitField, MainView};

/// 一覧の 1 行の高さ。
const ENTRY_ROW_HEIGHT: Pixels = px(52.);
/// 詳細の左の見出しの幅。
const LABEL_WIDTH: Pixels = px(96.);

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

    /// キーなしの接続の決まりを足す（読み書き・いつでも）。足したら、その詳細を開く。
    pub fn add_keyless_rule(&mut self, cx: &mut Context<Self>) {
        if let Some(rule) = self.write(cx, |s| s.add_keyless_rule(false)) {
            self.open_access_entry(&rule.id, cx);
        }
        cx.notify();
    }

    pub fn remove_keyless_rule(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.write(cx, |s| s.remove_keyless_rule(id)).is_some() {
            self.forget_access_entry(id);
        }
        cx.notify();
    }

    /// キーか、キーなしの接続の決まり。
    pub(crate) fn access_entry(&self, id: &str, cx: &App) -> Option<AccessKey> {
        self.db.read(cx).store.access_key(id).ok()
    }

    fn is_keyless_rule(&self, id: &str, cx: &App) -> bool {
        self.keyless_rules(cx).iter().any(|r| r.id == id)
    }

    /// キー（決まり）の詳細を開く。
    pub fn open_access_entry(&mut self, id: &str, cx: &mut Context<Self>) {
        // 詳細を見出しの下から出す
        let section = if self.is_keyless_rule(id, cx) {
            CustomizeSection::McpConnection
        } else {
            CustomizeSection::AccessKeys
        };
        self.scroll_to_section(section, cx);
        self.key_limits_open = Some(id.to_owned());
        self.key_hours_per_day = None;
        self.key_limit_editing = None;
        cx.notify();
    }

    /// 詳細を閉じて一覧に戻る。開いていたら `true`。
    pub fn close_access_entry(&mut self) -> bool {
        self.key_hours_per_day = None;
        if self.key_limits_open.take().is_none() {
            return false;
        }
        if self.date_pick.as_ref().is_some_and(|p| {
            matches!(
                p.target,
                DateTarget::KeyStarts(_) | DateTarget::KeyExpires(_)
            )
        }) {
            self.date_pick = None;
        }
        true
    }

    /// 消したキー（決まり）の詳細を開いていたら閉じる。
    fn forget_access_entry(&mut self, id: &str) {
        if self.key_limits_open.as_deref() == Some(id) {
            self.close_access_entry();
        }
        if self.key_naming.as_deref() == Some(id) {
            self.key_naming = None;
        }
    }

    /// 保存してある MCP の HTTP の設定。
    pub fn mcp_http(&self, cx: &App) -> mcp_http::Config {
        mcp_http::config(&self.db.read(cx).store).unwrap_or_default()
    }

    pub fn set_mcp_http(&mut self, on: bool, cx: &mut Context<Self>) {
        let next = mcp_http::Config {
            on,
            ..self.mcp_http(cx)
        };
        self.change_mcp_http(next, cx);
    }

    /// ポートを自動にするか、固定にするか。固定にするときは、今のポートのまま。
    pub fn set_mcp_port_mode(&mut self, mode: PortMode, cx: &mut Context<Self>) {
        let now = self.mcp_http(cx);
        let next = mcp_http::Config {
            mode,
            port: mcp_http::running_port().unwrap_or(now.port),
            ..now
        };
        self.change_mcp_http(next, cx);
    }

    /// HTTP を開く・閉じるか、ポートを変える。開けなければ理由を欄に出し、設定も前のまま。
    fn change_mcp_http(&mut self, next: mcp_http::Config, cx: &mut Context<Self>) -> bool {
        let Some(db) = self.db.read(cx).store.file_path() else {
            self.mcp_http_error =
                Some("データベースの場所が分からないので、MCP を HTTP で開けません。".into());
            cx.notify();
            return false;
        };
        let before = self.mcp_http(cx);
        let port = match mcp_http::apply(&db, &next) {
            Ok(port) => port,
            Err(error) => {
                self.mcp_http_error = Some(error.to_string().into());
                cx.notify();
                return false;
            }
        };
        // 自動で選んだポートは覚えて、次も使う
        let saved = mcp_http::Config {
            port: port.unwrap_or(next.port),
            ..next
        };
        if self.write(cx, |s| mcp_http::save(s, &saved)).is_none() {
            // 保存できなければ、前の状態に戻す（保存できなかったことは画面に出ている）
            if let Err(error) = mcp_http::apply(&db, &before) {
                self.mcp_http_error = Some(error.to_string().into());
            }
            cx.notify();
            return false;
        }
        self.mcp_http_error = None;
        cx.notify();
        true
    }

    /// MCP の URL をコピーする。
    pub fn copy_mcp_url(&mut self, cx: &mut Context<Self>) {
        let port = mcp_http::running_port().unwrap_or(self.mcp_http(cx).port);
        cx.write_to_clipboard(ClipboardItem::new_string(mcp_http::url(port)));
        self.show_notice("MCP の URL をコピーしました。".into());
        cx.notify();
    }

    /// 「新規アクセスキー」の名前で発行して、その詳細を開き、そのまま名前を書けるようにする。
    pub fn issue_access_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(issued) = self.write(cx, |s| s.issue_access_key(NEW_KEY_NAME, false, false)) {
            let id = issued.key.id.clone();
            self.issued_access_key = Some(issued);
            self.open_access_entry(&id, cx);
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

    /// 読み取りのみにするか、読み書きにするか。
    pub fn set_key_read_only(&mut self, id: &str, read_only: bool, cx: &mut Context<Self>) {
        self.update_key(
            id,
            KeyPatch {
                read_only: Some(read_only),
                ..Default::default()
            },
            cx,
        );
    }

    /// 「ロック中も使う」を切り替える。オンにするときは、ロックが有効ならパスキーで確かめる。
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
        self.update_key(
            id,
            KeyPatch {
                while_locked: Some(on),
                ..Default::default()
            },
            cx,
        );
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
            self.forget_access_entry(id);
            if let Some(name) = name {
                self.show_notice(format!("アクセスキー「{name}」を失効させました。").into());
            }
        }
        cx.notify();
    }

    pub(crate) fn update_key(&mut self, id: &str, patch: KeyPatch, cx: &mut Context<Self>) -> bool {
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

    /// 使える時間の決め方（いつでも・毎日・平日・曜日ごと）。
    fn hours_mode_of(&self, key: &AccessKey) -> HoursMode {
        if self.key_hours_per_day.as_deref() == Some(key.id.as_str()) && !key.windows.is_empty() {
            HoursMode::PerDay
        } else {
            HoursMode::of(&key.windows)
        }
    }

    /// 使える時間の決め方を変える。今の時刻は引き継ぐ。
    pub fn set_key_hours_mode(&mut self, id: &str, mode: HoursMode, cx: &mut Context<Self>) {
        let windows = self.key_windows(id, cx);
        self.key_hours_per_day = (mode == HoursMode::PerDay).then(|| id.to_owned());
        let next = mode.apply(&windows);
        if next != windows {
            self.set_key_windows(id, next, cx);
        }
        cx.notify();
    }

    /// 時刻を変える。`day` が `None` なら毎日・平日の時刻、`Some` ならその曜日の `ix` 番目の時刻。
    pub fn set_key_hours_time(
        &mut self,
        id: &str,
        day: Option<u8>,
        ix: usize,
        from: bool,
        minutes: u32,
        cx: &mut Context<Self>,
    ) {
        let set = |range: &mut (u32, u32)| {
            if from {
                range.0 = minutes;
            } else {
                range.1 = minutes;
            }
        };
        let windows = self.key_windows(id, cx);
        let next = match day {
            None => windows
                .into_iter()
                .map(|mut w| {
                    let mut range = (w.from, w.to);
                    set(&mut range);
                    (w.from, w.to) = range;
                    w
                })
                .collect(),
            Some(day) => {
                let mut days = access::day_hours(&windows);
                let Some(range) = days.get_mut(day as usize).and_then(|d| d.get_mut(ix)) else {
                    return;
                };
                set(range);
                access::windows_of(&days)
            }
        };
        self.set_key_windows(id, next, cx);
    }

    /// 曜日ごとのときに、その曜日を使う・使わない。最後の 1 つは外さない
    /// （どの曜日も使わないなら「いつでも」と区別できない）。
    pub fn toggle_key_day(&mut self, id: &str, day: u8, cx: &mut Context<Self>) {
        let windows = self.key_windows(id, cx);
        let mut days = access::day_hours(&windows);
        let used = days.iter().filter(|d| !d.is_empty()).count();
        let Some(hours) = days.get(day as usize) else {
            return;
        };
        if hours.is_empty() {
            // ほかの曜日と同じ時刻で足す
            let range = days
                .iter()
                .find_map(|d| d.first().copied())
                .unwrap_or(STANDARD_HOURS);
            days[day as usize].push(range);
        } else if used > 1 {
            days[day as usize].clear();
        } else {
            return;
        }
        self.key_hours_per_day = Some(id.to_owned());
        self.set_key_windows(id, access::windows_of(&days), cx);
    }

    /// 名前・ポートを、その場の入力欄で書き直す。
    pub fn edit_key_limit(
        &mut self,
        id: Id,
        field: LimitField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match field {
            LimitField::Port => self.mcp_http(cx).port.to_string(),
        };
        self.inputs.key_limit.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.key_limit_editing = Some((id, field));
        cx.notify();
    }

    /// 書いたポートを保存する。Enter か、ほかの場所を押したとき。
    /// 読めない書き方なら、何も変えずに理由を欄に出して、入力欄はそのまま。
    pub fn save_key_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, field)) = self.key_limit_editing.clone() else {
            return;
        };
        let text = self.inputs.key_limit.read(cx).value().trim().to_owned();
        match field {
            LimitField::Port => {
                let port = match text.parse::<u16>() {
                    Ok(port) if port > 0 => port,
                    _ => {
                        self.mcp_http_error =
                            Some(format!("ポート番号は 1〜65535 の数字です: {text}").into());
                        cx.notify();
                        return;
                    }
                };
                let next = mcp_http::Config {
                    port,
                    ..self.mcp_http(cx)
                };
                if !self.change_mcp_http(next, cx) {
                    return;
                }
            }
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
            Copy::ClaudeHttp => mcp_http::claude_add_command(
                mcp_http::running_port().unwrap_or(self.mcp_http(cx).port),
                &secret,
            ),
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

    /// 設定の「アクセスキー」。キーの一覧と発行のボタン。キーを開いていれば、その詳細。
    pub(crate) fn access_keys_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let keys = self.access_keys(cx);
        if let Some(key) = self
            .key_limits_open
            .as_ref()
            .and_then(|id| keys.iter().find(|k| &k.id == id))
        {
            return self.entry_detail(key.clone(), false, cx);
        }
        let theme = cx.theme().clone();
        let mut list = col().gap_2();
        if keys.is_empty() {
            list = list.child(empty_line("access-key-none", "アクセスキーなし", &theme));
        }
        for (ix, key) in keys.iter().enumerate() {
            list = list.child(self.entry_row(("access-key", ix), key, false, cx));
        }
        col()
            .gap_3()
            .child(list)
            .child(
                row().child(feel::gloss(
                    button("access-key-add")
                        .icon(app_icon("plus"))
                        .label("アクセスキーを発行")
                        .framed(cx)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.issue_access_key(window, cx)),
                        ),
                )),
            )
            .into_any_element()
    }

    /// 設定の「キーなしの接続」。決まりの一覧と足すボタン。決まりを開いていれば、その詳細。
    pub(crate) fn keyless_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let rules = self.keyless_rules(cx);
        if let Some(rule) = self
            .key_limits_open
            .as_ref()
            .and_then(|id| rules.iter().find(|r| &r.id == id))
        {
            return self.entry_detail(rule.clone(), true, cx);
        }
        let theme = cx.theme().clone();
        let mut list = col().gap_2();
        if rules.is_empty() {
            list = list.child(empty_line(
                "keyless-none",
                "キーなしではつなげません（アクセスキーが必要）",
                &theme,
            ));
        }
        for (ix, rule) in rules.iter().enumerate() {
            list = list.child(self.entry_row(("keyless", ix), rule, true, cx));
        }
        col()
            .gap_3()
            .child(list)
            .child(
                row().child(feel::gloss(
                    button("keyless-add")
                        .icon(app_icon("plus"))
                        .label("キーなしで使える時間を追加")
                        .framed(cx)
                        .on_click(cx.listener(|this, _, _, cx| this.add_keyless_rule(cx))),
                )),
            )
            .into_any_element()
    }

    /// 一覧の 1 行。名前（決まりなら使える時間）、できることとロック中、今使えるか。押すと詳細。
    fn entry_row(
        &self,
        id: impl Into<ElementId>,
        key: &AccessKey,
        rule: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let open = key.id.clone();
        let (title, icon) = if rule {
            (key.when_label(), "plug-zap")
        } else {
            (key.name.clone(), "key-round")
        };
        let mut facts = vec![ability_label(key).to_owned(), lock_label(key).to_owned()];
        if !rule {
            facts.push(key.when_label());
        }
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_3()
            .min_h(ENTRY_ROW_HEIGHT)
            .px_3()
            .py_2()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .lit(theme.muted, cx)
            .child(
                app_icon(icon)
                    .size_4()
                    .flex_none()
                    .text_color(theme.muted_foreground),
            )
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().truncate().child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(facts.join("・")),
                    )
                    .when(!rule, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(format!("{}・{}", key.masked(), used_label(key))),
                        )
                    }),
            )
            .child(status_badge(key, &theme))
            .child(
                app_icon("chevron-right")
                    .size_4()
                    .flex_none()
                    .text_color(theme.muted_foreground),
            )
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.open_access_entry(&open, cx)),
            )
            .test_support()
            .into_any_element()
    }

    /// キー（決まり）の詳細。上に一覧へ戻るボタン、下に失効（外す）のボタン。
    fn entry_detail(&self, key: AccessKey, rule: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = key.id.clone();
        let mut lines: Vec<(&'static str, AnyElement)> = Vec::new();
        if !rule {
            lines.push(("名前", self.key_name_control(&key, cx)));
            lines.push((
                "キー",
                col()
                    .gap_0p5()
                    .child(
                        div()
                            .id("access-key-masked")
                            .text_sm()
                            .mono_font(cx)
                            .child(key.masked())
                            .test_support(),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "発行 {}・{}",
                                view::format_datetime(key.created_at),
                                used_label(&key)
                            )),
                    )
                    .into_any_element(),
            ));
        }
        lines.push((
            "今の状態",
            div()
                .id("access-entry-status")
                .text_sm()
                .map(|d| match key.usable_at(access::now_local()) {
                    Ok(()) => d.child("使えます"),
                    Err(denied) => d
                        .text_color(theme.danger)
                        .child(format!("使えません（{}）", denied.short())),
                })
                .test_support()
                .into_any_element(),
        ));
        // できること
        let (write_id, read_id) = (id.clone(), id.clone());
        lines.push((
            "できること",
            row()
                .gap_1p5()
                .flex_wrap()
                .child(
                    pill("access-entry-write", "読み書き", !key.read_only, false, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_key_read_only(&write_id, false, cx)
                        }))
                        .test_support(),
                )
                .child(
                    pill(
                        "access-entry-read",
                        "読み取りのみ",
                        key.read_only,
                        false,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_key_read_only(&read_id, true, cx)
                    }))
                    .test_support(),
                )
                .into_any_element(),
        ));
        // ロック中
        let lock_id = id.clone();
        lines.push((
            "ロック中",
            row()
                .gap_2()
                .child(
                    Switch::new("access-entry-lock")
                        .checked(key.while_locked)
                        .disabled(self.verifying(cx))
                        .on_click(cx.listener(move |this, _: &bool, window, cx| {
                            this.toggle_key_while_locked(lock_id.clone(), window, cx)
                        })),
                )
                .child(div().text_sm().child(lock_label(&key)))
                .into_any_element(),
        ));
        // 開始
        let starts_text = match &key.starts {
            Some(at) => div().text_sm().child(access::moment_label(at)),
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if rule {
                    "足したときから"
                } else {
                    "発行したときから"
                }),
        };
        let clear_id = id.clone();
        lines.push((
            "開始",
            row()
                .gap_1()
                .flex_wrap()
                .child(self.date_trigger(
                    "access-entry-starts",
                    DateTarget::KeyStarts(id.clone()),
                    starts_text,
                    cx,
                ))
                .when(key.starts.is_some(), |d| {
                    d.child(feel::gloss(
                        button("access-entry-starts-clear")
                            .label("外す")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.clear_key_start(&clear_id, cx)
                            })),
                    ))
                })
                .into_any_element(),
        ));
        // 期限。今からの長さでも選べる
        let expires_text = match &key.expires {
            Some(at) => div().text_sm().child(access::moment_label(at)),
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("なし"),
        };
        let view_handle = cx.entity().downgrade();
        let expiry_id = id.clone();
        lines.push((
            "期限",
            row()
                .gap_1()
                .flex_wrap()
                .child(self.date_trigger(
                    "access-entry-expires",
                    DateTarget::KeyExpires(id.clone()),
                    expires_text,
                    cx,
                ))
                .child(
                    button("access-entry-expiry-menu")
                        .label("今から")
                        .icon(app_icon("chevron-down"))
                        .ghost()
                        .small()
                        .pop_menu(move |mut menu, _, _| {
                            for preset in std::iter::once(None).chain(Expiry::ALL.map(Some)) {
                                let (view, id) = (view_handle.clone(), expiry_id.clone());
                                let name = preset.map_or("期限なし", Expiry::label);
                                menu = menu.item(PopupMenuItem::new(name).on_click(
                                    move |_, _, cx| {
                                        // 設定を閉じたあとなら、反映する先がない
                                        let _ = view.update(cx, |this, cx| {
                                            this.set_key_expiry_in(&id, preset, cx)
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                )
                .into_any_element(),
        ));
        lines.push(("使える時間", self.hours_editor(&key, cx).into_any_element()));

        let back_label = if rule {
            "キーなしの接続"
        } else {
            "アクセスキー"
        };
        let header = row().child(feel::gloss(
            button("access-entry-back")
                .icon(app_icon("chevron-left"))
                .label(back_label)
                .ghost()
                .small()
                .tooltip("一覧に戻る")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_access_entry();
                    cx.notify();
                })),
        ));
        let mut body = col();
        for (ix, (label, content)) in lines.into_iter().enumerate() {
            body = body.child(detail_line(label, content, ix == 0, &theme));
        }
        let remove_id = id.clone();
        let footer = if rule {
            row().child(feel::gloss(
                button("keyless-remove")
                    .icon(app_icon("trash"))
                    .label("この決まりを外す")
                    .danger()
                    .small()
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.remove_keyless_rule(&remove_id, cx)),
                    ),
            ))
        } else {
            let view_handle = cx.entity().downgrade();
            row().child(
                button("access-key-revoke")
                    .icon(app_icon("trash"))
                    .label("失効させる")
                    .danger()
                    .small()
                    .pop_menu(move |menu, _, _| {
                        let (view, id) = (view_handle.clone(), remove_id.clone());
                        menu.item(
                            PopupMenuItem::new("このキーを失効させる（元に戻せません）").on_click(
                                move |_, _, cx| {
                                    // 設定を閉じたあとなら、反映する先がない
                                    let _ =
                                        view.update(cx, |this, cx| this.revoke_access_key(&id, cx));
                                },
                            ),
                        )
                    }),
            )
        };
        let issued = self
            .issued_access_key
            .as_ref()
            .filter(|i| i.key.id == key.id)
            .map(|i| self.issued_key_box(&i.secret, cx));
        col()
            .id(if rule {
                "keyless-detail"
            } else {
                "access-key-detail"
            })
            .gap_2()
            .child(header)
            .children(issued)
            .child(
                body.px_3()
                    .rounded(feel::md())
                    .border_1()
                    .border_color(theme.border),
            )
            .child(footer)
            .test_support()
            .into_any_element()
    }

    /// 詳細の名前。押すとその場で書ける。
    fn key_name_control(&self, key: &AccessKey, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.key_naming.as_ref() == Some(&key.id) {
            // Enter かほかの場所を押すと保存、Esc で取り消し
            return div()
                .flex_1()
                .min_w_0()
                .on_mouse_down_out(
                    cx.listener(|this, _, window, cx| this.save_access_key_name(window, cx)),
                )
                .child(
                    Input::new(&self.inputs.key_name)
                        .id("access-key-input")
                        .aria_label("アクセスキーの名前")
                        .small()
                        .bg(theme.background)
                        .border_color(theme.ring),
                )
                .into_any_element();
        }
        let id = key.id.clone();
        row()
            .id("access-key-name")
            .gap_1p5()
            .px_2()
            .h(px(26.))
            .rounded(feel::md())
            .cursor_pointer()
            .lit(theme.muted, cx)
            .child(div().text_sm().truncate().child(key.name.clone()))
            .child(
                app_icon("pencil")
                    .size_3p5()
                    .text_color(theme.muted_foreground),
            )
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("名前を変更").build(window, cx)
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.rename_access_key(id.clone(), window, cx)
            }))
            .test_support()
            .into_any_element()
    }

    /// 使える時間。いつでも・毎日・平日・曜日ごとと、その時刻。
    fn hours_editor(&self, key: &AccessKey, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mode = self.hours_mode_of(key);
        let mut modes = row().gap_1p5().flex_wrap();
        for (ix, m) in HoursMode::ALL.into_iter().enumerate() {
            let id = key.id.clone();
            modes = modes.child(
                pill(("access-entry-hours", ix), m.label(), m == mode, false, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_key_hours_mode(&id, m, cx)
                    }))
                    .test_support(),
            );
        }
        let editor = match mode {
            HoursMode::Always => None,
            HoursMode::Everyday | HoursMode::Weekdays => key.windows.first().map(|w| {
                self.range_line(&key.id, None, 0, (w.from, w.to), cx)
                    .into_any_element()
            }),
            HoursMode::PerDay => {
                let days = access::day_hours(&key.windows);
                let used = days.iter().filter(|d| !d.is_empty()).count();
                let mut table = col().gap_1();
                for (d, hours) in days.iter().enumerate() {
                    let id = key.id.clone();
                    let on = !hours.is_empty();
                    let mut ranges = col().gap_1();
                    if hours.is_empty() {
                        ranges = ranges.child(
                            div()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("使えない"),
                        );
                    }
                    for (rx, range) in hours.iter().enumerate() {
                        ranges =
                            ranges.child(self.range_line(&key.id, Some(d as u8), rx, *range, cx));
                    }
                    table = table.child(
                        row()
                            .gap_3()
                            .items_start()
                            .child(
                                row()
                                    .gap_2()
                                    .w(px(64.))
                                    .flex_none()
                                    .h(px(28.))
                                    .child(
                                        Switch::new(("access-entry-day", d))
                                            .checked(on)
                                            // 最後の 1 つは外せない
                                            .disabled(on && used == 1)
                                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                                this.toggle_key_day(&id, d as u8, cx)
                                            })),
                                    )
                                    .child(div().text_sm().child(WEEKDAYS[d])),
                            )
                            .child(ranges),
                    );
                }
                Some(table.into_any_element())
            }
        };
        col().gap_2().child(modes).children(editor)
    }

    /// 「9:00 〜 18:00」の 1 行。時刻はメニューで選ぶ。日をまたぐときは「翌日」と出す。
    fn range_line(
        &self,
        key: &str,
        day: Option<u8>,
        ix: usize,
        (from, to): (u32, u32),
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let slot = day.map_or(0, |d| 1 + d as usize * 8 + ix);
        row()
            .gap_1p5()
            .child(self.time_choice(("access-entry-from", slot), key, day, ix, true, from, cx))
            .child(div().text_sm().child("〜"))
            .when(to <= from, |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("翌日"),
                )
            })
            .child(self.time_choice(("access-entry-to", slot), key, day, ix, false, to, cx))
    }

    /// 時刻のボタン。押すと 30 分ごとの時刻のメニューが開く。
    #[allow(clippy::too_many_arguments)]
    fn time_choice(
        &self,
        id: impl Into<ElementId>,
        key: &str,
        day: Option<u8>,
        ix: usize,
        from: bool,
        minutes: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let key = key.to_owned();
        button(id)
            .label(access::clock(minutes))
            .icon(app_icon("chevron-down"))
            .outline()
            .small()
            .accessibility_label(if from {
                "始まりの時刻"
            } else {
                "終わりの時刻"
            })
            .pop_menu(move |mut menu, _, _| {
                menu = menu.max_h(px(320.));
                for choice in time_choices(minutes, from) {
                    let (view, key) = (view.clone(), key.clone());
                    menu = menu.item(
                        PopupMenuItem::new(access::clock(choice))
                            .checked(choice == minutes)
                            .on_click(move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = view.update(cx, |this, cx| {
                                    this.set_key_hours_time(&key, day, ix, from, choice, cx)
                                });
                            }),
                    );
                }
                menu
            })
    }

    /// 設定の「HTTP で開く」。スイッチ、今の状態と URL、ポートの決め方、開けなかった理由。
    pub(crate) fn mcp_http_editor(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let config = self.mcp_http(cx);
        let running = mcp_http::running_port();
        let head = row()
            .gap_4()
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().child("HTTP で開く"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("この PC（127.0.0.1）からだけつなげます"),
                    ),
            )
            .child(crate::render::on_off(
                config.on,
                Switch::new("mcp-http-switch")
                    .checked(config.on)
                    .on_click(cx.listener(|this, on: &bool, _, cx| this.set_mcp_http(*on, cx))),
                cx,
            ));
        let status = match running {
            Some(port) => row()
                .gap_2()
                .flex_wrap()
                .child(div().size_2().rounded_full().bg(theme.success))
                .child(div().text_sm().child("受付中"))
                .child(
                    div()
                        .id("mcp-http-url")
                        .text_sm()
                        .mono_font(cx)
                        .child(mcp_http::url(port))
                        .test_support(),
                )
                .child(feel::gloss(
                    button("mcp-http-copy")
                        .icon(app_icon("copy"))
                        .label("URL をコピー")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.copy_mcp_url(cx))),
                )),
            None => row()
                .gap_2()
                .child(div().size_2().rounded_full().bg(theme.muted_foreground))
                .child(
                    div()
                        .id("mcp-http-state")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("停止中")
                        .test_support(),
                ),
        };
        let mut modes = row().gap_1p5();
        for (ix, mode) in [PortMode::Auto, PortMode::Fixed].into_iter().enumerate() {
            modes =
                modes.child(
                    pill(
                        ("mcp-http-port-mode", ix),
                        mode.label(),
                        config.mode == mode,
                        false,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_mcp_port_mode(mode, cx)
                    }))
                    .test_support(),
                );
        }
        let port_value = match config.mode {
            PortMode::Auto => div()
                .id("mcp-http-port")
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(match running {
                    Some(port) => {
                        format!("今は {port}（使われていれば空いているポート）")
                    }
                    None => format!("{}（使われていれば空いているポート）", config.port),
                })
                .test_support()
                .into_any_element(),
            PortMode::Fixed => self.limit_value(
                "",
                LimitField::Port,
                "mcp-http-port",
                config.port.to_string(),
                false,
                cx,
            ),
        };
        let port_line = row()
            .gap_3()
            .flex_wrap()
            .child(
                div()
                    .w(LABEL_WIDTH)
                    .flex_none()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("ポート"),
            )
            .child(modes)
            .child(port_value);
        col()
            .gap_2()
            .child(head)
            .when(config.on || running.is_some(), |d| d.child(status))
            .child(port_line)
            .when_some(self.mcp_http_error.clone(), |d, error| {
                d.child(
                    row()
                        .id("mcp-http-error")
                        .gap_1p5()
                        .items_start()
                        .child(
                            app_icon("circle-alert")
                                .size_4()
                                .flex_none()
                                .mt_0p5()
                                .text_color(theme.danger),
                        )
                        .child(div().text_sm().text_color(theme.danger).child(error))
                        .test_support(),
                )
            })
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
        row()
            .id(id)
            .gap_1p5()
            .px_2()
            .h(px(26.))
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .text_sm()
            .when(placeholder, |d| d.text_color(theme.muted_foreground))
            .lit(theme.muted, cx)
            .child(text)
            .child(
                app_icon("pencil")
                    .size_3p5()
                    .text_color(theme.muted_foreground),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.edit_key_limit(key_id.clone(), field, window, cx)
            }))
            .test_support()
            .into_any_element()
    }

    /// 発行したばかりのキーの文字列と、コピーのボタン。
    fn issued_key_box(&self, secret: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let copy = |id: &'static str, icon: &str, what: Copy, cx: &mut Context<Self>| {
            feel::gloss(
                button(id)
                    .icon(app_icon(icon))
                    .label(what.label())
                    .outline()
                    .small()
                    .tooltip(what.tip())
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_issued_key(what, cx))),
            )
        };
        col()
            .gap_2()
            .p_3()
            .rounded(feel::md())
            .bg(theme.secondary)
            .child(
                row()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child("発行したキー（今だけ表示されます）"),
                    )
                    .child(feel::gloss(
                        button("access-key-hide")
                            .icon(app_icon("x"))
                            .label("隠す")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.issued_access_key = None;
                                cx.notify();
                            })),
                    )),
            )
            .child(
                div()
                    .id("access-key-secret")
                    .text_sm()
                    .mono_font(cx)
                    .overflow_x_hidden()
                    .child(secret.to_owned())
                    .test_support(),
            )
            .child(
                row()
                    .gap_1p5()
                    .flex_wrap()
                    .child(copy("access-key-copy", "copy", Copy::Key, cx))
                    .child(copy("access-key-copy-claude", "terminal", Copy::Claude, cx))
                    .when(mcp_http::running_port().is_some(), |d| {
                        d.child(copy("access-key-copy-http", "globe", Copy::ClaudeHttp, cx))
                    })
                    .child(copy("access-key-copy-config", "braces", Copy::Config, cx)),
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
    /// ボタンの名前。
    fn label(self) -> &'static str {
        match self {
            Copy::Key => "キーをコピー",
            Copy::Claude => "Claude Code のコマンド",
            Copy::ClaudeHttp => "Claude Code（HTTP）のコマンド",
            Copy::Config => "MCP の設定（JSON）",
        }
    }

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

/// できること。
fn ability_label(key: &AccessKey) -> &'static str {
    if key.read_only {
        "読み取りのみ"
    } else {
        "読み書き"
    }
}

/// ロック中に使えるか。
fn lock_label(key: &AccessKey) -> &'static str {
    if key.while_locked {
        "ロック中も使える"
    } else {
        "ロック中は使えない"
    }
}

/// 最後に使った日時。
fn used_label(key: &AccessKey) -> String {
    match key.used_at {
        Some(at) => format!("最後に使った {}", view::format_datetime(at)),
        None => "未使用".into(),
    }
}

/// 一覧の右の、今使えるか。
fn status_badge(key: &AccessKey, theme: &gpui_kit::component::Theme) -> Div {
    let (text, color) = match key.usable_at(access::now_local()) {
        Ok(()) => ("使える".into(), theme.foreground),
        Err(denied) => (SharedString::from(denied.short()), theme.danger),
    };
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.5))
        .text_xs()
        .text_color(color)
        .child(text)
}

/// 一覧が空のときの 1 行。
fn empty_line(
    id: &'static str,
    text: &'static str,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(text)
        .test_support()
}

/// 詳細の 1 行。左に見出し、右に中身。
fn detail_line(
    label: &'static str,
    content: AnyElement,
    first: bool,
    theme: &gpui_kit::component::Theme,
) -> Div {
    row()
        .items_start()
        .gap_3()
        .py_2p5()
        .when(!first, |d| {
            d.border_t_1().border_color(theme.border.opacity(0.6))
        })
        .child(
            div()
                .w(LABEL_WIDTH)
                .flex_none()
                .pt_1()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// 時刻のメニューに出す時刻。30 分ごと（始まりは 0:00〜23:30、終わりは 0:30〜24:00）。
/// 今の時刻が 30 分ごとでなければ、それも入れる。
fn time_choices(current: u32, from: bool) -> Vec<u32> {
    let mut times: Vec<u32> = if from {
        (0..48).map(|i| i * 30).collect()
    } else {
        (1..=48).map(|i| i * 30).collect()
    };
    if !times.contains(&current) {
        times.push(current);
        times.sort_unstable();
    }
    times
}

#[cfg(test)]
mod tests {
    use super::time_choices;

    #[test]
    fn time_menu_choices() {
        let from = time_choices(9 * 60, true);
        assert_eq!(from.len(), 48);
        assert_eq!(from.first(), Some(&0));
        assert_eq!(from.last(), Some(&(23 * 60 + 30)));
        let to = time_choices(18 * 60, false);
        assert_eq!(to.first(), Some(&30));
        assert_eq!(to.last(), Some(&(24 * 60)));
        // 30 分ごとでない時刻も、選んだまま出す
        let odd = time_choices(9 * 60 + 5, true);
        assert_eq!(odd.len(), 49);
        assert!(odd.windows(2).all(|w| w[0] < w[1]));
        assert!(odd.contains(&(9 * 60 + 5)));
    }
}
