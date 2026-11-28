//! 設定の「同期」。同期するアカウント（CalDAV のサーバーと、macOS のリマインダー）の一覧、
//! 追加・外す・今すぐ同期と、サーバーのカレンダーをワークスペースにつなぐメニュー。
//!
//! 「アカウントを追加」で設定の上にダイアログを重ね、プロバイダを選んでから、
//! サーバー・ユーザー名・パスワード・説明を入れて足す。パスワードは OS のキーチェーンに入れる。
//! サーバーのカレンダーごとに、つなぐワークスペースを選ぶ（新しく作る・今あるもの・つながない）。

use std::collections::HashMap;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::caldav::{Account, Calendar};
use malakt_core::{view, Id, Workspace};

use crate::ai_view::icon_button;
use crate::caldav::{CalDav, Syncer};
use crate::db::{self, Db};
use crate::feel;
use crate::press::button;
use crate::render::{app_icon, col, px, row};

/// アカウントの行の高さ。担当者やアクセスキーの一覧とそろえる。
const ROW_HEIGHT: Pixels = px(32.);
/// ワークスペースを選ぶボタンの幅。
const PICK_WIDTH: Pixels = px(220.);

/// 項目の名前の幅。追加のダイアログで、入力欄の左をそろえる。
const LABEL_WIDTH: Pixels = px(96.);

/// アカウントを足す先の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// macOS の「リマインダー」
    Reminders,
    Fastmail,
    Nextcloud,
    /// ほかの CalDAV のサーバー
    Other,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::Reminders,
        Provider::Fastmail,
        Provider::Nextcloud,
        Provider::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Reminders => "リマインダー",
            Provider::Fastmail => "Fastmail",
            Provider::Nextcloud => "Nextcloud",
            Provider::Other => "その他の CalDAV アカウント",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Provider::Reminders => "list-todo",
            Provider::Fastmail => "mail",
            Provider::Nextcloud => "cloud",
            Provider::Other => "server",
        }
    }

    /// 一覧の行の id。
    fn id(self) -> &'static str {
        match self {
            Provider::Reminders => "caldav-pick-reminders",
            Provider::Fastmail => "caldav-pick-fastmail",
            Provider::Nextcloud => "caldav-pick-nextcloud",
            Provider::Other => "caldav-pick-other",
        }
    }

    /// サーバーが決まっているプロバイダの URL。サーバーの欄を出さない。
    fn server(self) -> Option<&'static str> {
        match self {
            Provider::Fastmail => Some("https://caldav.fastmail.com/"),
            _ => None,
        }
    }

    fn server_hint(self) -> &'static str {
        match self {
            Provider::Nextcloud => "cloud.example.com",
            _ => "caldav.example.com",
        }
    }

    fn user_hint(self) -> &'static str {
        match self {
            Provider::Fastmail => "メールアドレス",
            _ => "必須",
        }
    }

    fn password_hint(self) -> &'static str {
        match self {
            Provider::Fastmail => "アプリパスワード",
            _ => "必須",
        }
    }
}

/// 追加のダイアログの、今の段階。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// プロバイダを選ぶ
    Pick,
    /// 選んだプロバイダのアカウントを書く
    Form(Provider),
}

/// 追加のダイアログを開く・閉じる。設定の上に重ねるのは、設定を出しているビュー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddDialog {
    Open,
    Close,
}

/// サーバーの欄に書いたものを URL にする。`https://` などがなければ `https://` を付ける。
pub fn server_url(input: &str) -> String {
    let input = input.trim();
    if input.is_empty() || input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    }
}

/// カレンダーをどこにつなぐか。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    None,
    New,
    Workspace(Id),
}

pub struct CalDavSettings {
    db: Entity<Db>,
    syncer: Entity<Syncer>,
    url: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    /// 説明（アカウントに付ける名前）
    name: Entity<InputState>,
    /// 追加のダイアログの段階。閉じていれば `None`。
    step: Option<Step>,
    /// 最後に出した段階。閉じたダイアログが消えるまでの間も、同じ中身を描く。
    shown: Step,
    /// 追加のダイアログを開くたび・閉じるたびに進める。古い接続の結果は捨てる。
    add_gen: u64,
    /// サーバーを調べている。
    connecting: bool,
    /// 追加のダイアログで失敗したこと。
    add_error: Option<SharedString>,
    /// 一覧・つなぐメニューで失敗したこと。
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AddDialog> for CalDavSettings {}

impl CalDavSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let db = db::shared(cx);
        let syncer = crate::caldav::shared(cx);
        let url = cx.new(|cx| InputState::new(window, cx));
        let user = cx.new(|cx| InputState::new(window, cx));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("省略可"));
        let mut subs = vec![
            cx.observe(&db, |_, _, cx| cx.notify()),
            cx.observe(&syncer, |_, _, cx| cx.notify()),
        ];
        for input in [&url, &user, &password, &name] {
            subs.push(
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit(window, cx);
                    }
                }),
            );
        }
        CalDavSettings {
            db,
            syncer,
            url,
            user,
            password,
            name,
            step: None,
            shown: Step::Pick,
            add_gen: 0,
            connecting: false,
            add_error: None,
            error: None,
            _subscriptions: subs,
        }
    }

    fn accounts(&self, cx: &App) -> Vec<(Account, Vec<Calendar>)> {
        let store = &self.db.read(cx).store;
        let read = store.caldav_accounts().and_then(|accounts| {
            accounts
                .into_iter()
                .map(|a| {
                    let cals = store.caldav_calendars(&a.id)?;
                    Ok((a, cals))
                })
                .collect::<malakt_core::Result<Vec<_>>>()
        });
        match read {
            Ok(list) => list,
            Err(error) => {
                // 描くたびに呼ぶので、ここでは画面のエラーに書かず、空の一覧を出す。
                // 書き換えのときに、同じエラーが画面に出る
                eprintln!("CalDAV のアカウントを読めませんでした: {error}");
                Vec::new()
            }
        }
    }

    /// 追加のダイアログを出している。
    pub fn is_adding(&self) -> bool {
        self.step.is_some()
    }

    /// 追加のダイアログの見出し。プロバイダを選んだら、その名前。
    pub fn add_title(&self) -> &'static str {
        match self.step.unwrap_or(self.shown) {
            Step::Form(p) => p.name(),
            Step::Pick => "アカウントを追加",
        }
    }

    /// 追加のダイアログを開き、プロバイダを選ぶところから始める。
    pub fn start_adding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_gen += 1;
        self.step = Some(Step::Pick);
        self.shown = Step::Pick;
        self.connecting = false;
        self.add_error = None;
        for input in [&self.url, &self.user, &self.password, &self.name] {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        cx.emit(AddDialog::Open);
        cx.notify();
    }

    /// 追加のダイアログを閉じたことを受ける。調べている途中の結果は捨てる。
    pub fn end_adding(&mut self, cx: &mut Context<Self>) {
        if self.step.take().is_some() {
            self.add_gen += 1;
            self.connecting = false;
            self.add_error = None;
            cx.notify();
        }
    }

    fn close_adding(&mut self, cx: &mut Context<Self>) {
        self.end_adding(cx);
        cx.emit(AddDialog::Close);
    }

    /// プロバイダを選ぶ。リマインダーはすぐ同期を始め、ほかはアカウントの欄を出す。
    fn choose(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if provider == Provider::Reminders {
            match crate::reminders::set_enabled(true, cx) {
                Ok(()) => self.close_adding(cx),
                Err(e) => {
                    self.add_error =
                        Some(format!("リマインダーとの同期を始められませんでした。{e}").into());
                    cx.notify();
                }
            }
            return;
        }
        self.step = Some(Step::Form(provider));
        self.add_error = None;
        let first = if provider.server().is_some() {
            &self.user
        } else {
            &self.url
        };
        for (input, hint) in [
            (&self.url, provider.server_hint()),
            (&self.user, provider.user_hint()),
            (&self.password, provider.password_hint()),
        ] {
            input.update(cx, |s, cx| s.set_placeholder(hint, window, cx));
        }
        first.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// 入れたサーバー・ユーザー名・パスワードでサーバーを調べ、アカウントを足す。
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Step::Form(provider)) = self.step else {
            return;
        };
        if self.connecting {
            return;
        }
        let url = match provider.server() {
            Some(server) => server.to_owned(),
            None => server_url(&self.url.read(cx).value()),
        };
        let user = self.user.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        let name = self.name.read(cx).value().trim().to_owned();
        let missing: Vec<&str> = [
            ("サーバー", &url),
            ("ユーザー名", &user),
            ("パスワード", &password),
        ]
        .into_iter()
        .filter(|(_, v)| v.is_empty())
        .map(|(name, _)| name)
        .collect();
        if !missing.is_empty() {
            self.add_error = Some(format!("{}を入れてください", missing.join("・")).into());
            cx.notify();
            return;
        }
        let Some(caldav) = cx.try_global::<CalDav>() else {
            self.add_error = Some("この環境ではサーバーに接続できません".into());
            cx.notify();
            return;
        };
        let (discover, secrets) = (caldav.discover.clone(), caldav.secrets.clone());
        self.connecting = true;
        self.add_error = None;
        cx.notify();
        let gen = self.add_gen;
        let (u, n, p) = (url.clone(), user.clone(), password.clone());
        let task = cx
            .background_executor()
            .spawn(async move { discover(&u, &n, &p) });
        cx.spawn_in(window, async move |this, cx| {
            let found = task.await;
            // 設定を閉じたあとなら、反映する先がない
            let _ = this.update_in(cx, |this, _, cx| {
                // 待っている間にダイアログを閉じたら、アカウントは足さない
                if this.add_gen != gen {
                    return;
                }
                this.connecting = false;
                let found = match found {
                    Ok(found) => found,
                    Err(e) => {
                        this.add_error = Some(e.into());
                        cx.notify();
                        return;
                    }
                };
                let duplicate = this
                    .accounts(cx)
                    .iter()
                    .any(|(a, _)| a.url == found.base && a.username == user);
                if duplicate {
                    this.add_error = Some("このアカウントは、もう追加しています".into());
                    cx.notify();
                    return;
                }
                let added = this.db.update(cx, |db, cx| {
                    let added = db.store.add_caldav_account(
                        &found.base,
                        &user,
                        &name,
                        &found.addresses,
                        &found.calendars,
                    );
                    cx.notify();
                    added
                });
                let account = match added {
                    Ok(a) => a,
                    Err(e) => {
                        this.add_error =
                            Some(format!("アカウントを保存できませんでした。{e}").into());
                        cx.notify();
                        return;
                    }
                };
                // パスワードを入れられなければ、足したアカウントも戻す
                if let Err(e) = secrets.set(&account.id, &password) {
                    let undone = this
                        .db
                        .update(cx, |db, _| db.store.remove_caldav_account(&account.id));
                    this.add_error = Some(match undone {
                        Ok(()) => e.into(),
                        Err(more) => {
                            format!("{e}（追加したアカウントも外せませんでした。{more}）").into()
                        }
                    });
                    cx.notify();
                    return;
                }
                this.error = found
                    .calendars
                    .is_empty()
                    .then(|| "このアカウントには、タスクを入れられるカレンダーがありません".into());
                this.close_adding(cx);
            });
        })
        .detach();
    }

    /// リマインダーとの同期をやめる。このアプリが「リマインダー」に作ったものは消える。
    fn stop_reminders(&mut self, cx: &mut Context<Self>) {
        self.error = crate::reminders::set_enabled(false, cx)
            .err()
            .map(|e| format!("リマインダーとの同期をやめられませんでした。{e}").into());
        cx.notify();
    }

    /// カレンダーをワークスペースにつなぐ・外す。つないだらすぐ同期する。
    fn link(
        &mut self,
        account_id: &str,
        calendar: &Calendar,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        let result = self.db.update(cx, |db, cx| {
            let store = &mut db.store;
            let result = match &target {
                Target::None => store.link_caldav_calendar(&calendar.id, None),
                Target::Workspace(w) => store.link_caldav_calendar(&calendar.id, Some(w)),
                Target::New => store
                    .create_workspace(None, &calendar.name)
                    .and_then(|w| store.link_caldav_calendar(&calendar.id, Some(&w.id))),
            };
            cx.notify();
            result
        });
        match result {
            Ok(()) => {
                self.error = None;
                if target != Target::None {
                    let id = account_id.to_owned();
                    self.syncer.update(cx, |s, cx| s.request(&id, true, cx));
                }
            }
            Err(e) => {
                self.error = Some(format!("「{}」をつなげませんでした。{e}", calendar.name).into())
            }
        }
        cx.notify();
    }

    /// サーバーのカレンダーの一覧を読み直してから同期する。
    fn sync_now(&mut self, account: &Account, cx: &mut Context<Self>) {
        let Some(caldav) = cx.try_global::<CalDav>() else {
            return;
        };
        let (discover, secrets) = (caldav.discover.clone(), caldav.secrets.clone());
        let account = account.clone();
        let task = cx.background_executor().spawn({
            let account = account.clone();
            async move {
                let password = secrets
                    .get(&account.id)?
                    .ok_or("パスワードが保存されていません。アカウントを追加し直してください")?;
                discover(&account.url, &account.username, &password)
            }
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            // 設定を閉じたあとなら、反映する先がない
            let _ = this.update(cx, |this, cx| {
                match found {
                    Ok(found) => {
                        let saved = this.db.update(cx, |db, cx| {
                            cx.notify();
                            db.store.refresh_caldav_calendars(
                                &account.id,
                                &found.addresses,
                                &found.calendars,
                            )
                        });
                        this.error = saved
                            .err()
                            .map(|e| format!("カレンダーの一覧を保存できませんでした。{e}").into());
                    }
                    Err(e) => {
                        this.error = Some(format!("カレンダーの一覧を読めませんでした。{e}").into())
                    }
                }
                let id = account.id.clone();
                this.syncer.update(cx, |s, cx| s.request(&id, true, cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// アカウントを外す。タスクはこのアプリにもサーバーにも残る。
    fn remove(&mut self, account_id: &str, cx: &mut Context<Self>) {
        let removed = self.db.update(cx, |db, cx| {
            cx.notify();
            db.store.remove_caldav_account(account_id)
        });
        if let Err(e) = removed {
            self.error = Some(format!("アカウントを外せませんでした。{e}").into());
            cx.notify();
            return;
        }
        let id = account_id.to_owned();
        self.syncer.update(cx, |s, cx| s.forget(&id, cx));
        if let Some(caldav) = cx.try_global::<CalDav>() {
            if let Err(e) = caldav.secrets.delete(account_id) {
                self.error = Some(e.into());
            }
        }
        cx.notify();
    }

    fn workspace_names(&self, cx: &App) -> Vec<(Workspace, String)> {
        let workspaces = self.db.read(cx).store.workspaces().unwrap_or_default();
        let by_id: HashMap<&str, &Workspace> =
            workspaces.iter().map(|w| (w.id.as_str(), w)).collect();
        let path = |w: &Workspace| {
            let mut names = vec![w.name.as_str()];
            let mut at = w.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
            while let Some(p) = at {
                names.push(p.name.as_str());
                at = p.parent_id.as_deref().and_then(|q| by_id.get(q).copied());
            }
            names.reverse();
            names.join(" / ")
        };
        let mut out: Vec<(Workspace, String)> =
            workspaces.iter().map(|w| (w.clone(), path(w))).collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    fn calendar_row(
        &self,
        account: &Account,
        cal: &Calendar,
        ix: usize,
        names: &[(Workspace, String)],
        linked: &HashMap<Id, String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let current = cal
            .workspace_id
            .as_deref()
            .and_then(|w| names.iter().find(|(ws, _)| ws.id == w))
            .map(|(_, n)| n.clone());
        let label: SharedString = current
            .clone()
            .unwrap_or_else(|| "つながない".into())
            .into();
        let view = cx.entity().downgrade();
        let choices: Vec<(Target, String, bool, bool)> = {
            let mut list = vec![
                (
                    Target::None,
                    "つながない".to_owned(),
                    cal.workspace_id.is_none(),
                    true,
                ),
                (Target::New, "新しいワークスペース".to_owned(), false, true),
            ];
            for (w, name) in names {
                let mine = cal.workspace_id.as_deref() == Some(w.id.as_str());
                // ほかのカレンダーとつないだワークスペースは選べない
                let free = mine || !linked.contains_key(&w.id);
                list.push((Target::Workspace(w.id.clone()), name.clone(), mine, free));
            }
            list
        };
        let (account_id, calendar) = (account.id.clone(), cal.clone());
        row()
            .h(ROW_HEIGHT)
            .gap_2()
            .pl_6()
            .child(
                app_icon(if cal.read_only { "lock" } else { "calendar" })
                    .size_4()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .id(("caldav-calendar", ix))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(cal.name.clone())
                    .when(cal.read_only, |d| {
                        d.tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("読み取りのみ")
                                .build(window, cx)
                        })
                    })
                    .test_support(),
            )
            .child(
                button(("caldav-link", ix))
                    .label(label)
                    .icon(app_icon("chevron-down"))
                    .outline()
                    .small()
                    .w(PICK_WIDTH)
                    .pop_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for (i, (target, name, checked, enabled)) in
                            choices.iter().cloned().enumerate()
                        {
                            if i == 2 {
                                menu = menu.separator();
                            }
                            let (view, account_id, calendar) =
                                (view.clone(), account_id.clone(), calendar.clone());
                            menu = menu.item(
                                PopupMenuItem::new(name)
                                    .checked(checked)
                                    .disabled(!enabled)
                                    .on_click(move |_, _, cx| {
                                        // 設定を閉じたあとなら、反映する先がない
                                        let _ = view.update(cx, |this, cx| {
                                            this.link(&account_id, &calendar, target.clone(), cx)
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }

    fn account_header(&self, account: &Account, ix: usize, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let state = self.syncer.read(cx).state(&account.id);
        let synced = self
            .db
            .read(cx)
            .store
            .caldav_calendars(&account.id)
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c.synced_at)
            .max();
        let status: AnyElement = if state.syncing {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("同期中…")
                .into_any_element()
        } else if let Some(error) = state.error.clone() {
            div()
                .id(("caldav-error", ix))
                .child(app_icon("cloud-alert").size_4().text_color(theme.danger))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
                .test_support()
                .into_any_element()
        } else if let Some(at) = synced {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("{} に同期", view::format_datetime(at)))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let (sync_account, remove_id) = (account.clone(), account.id.clone());
        let view = cx.entity().downgrade();
        row()
            .h(ROW_HEIGHT)
            .gap_2()
            .child(
                app_icon("server")
                    .size_4()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .id(("caldav-account", ix))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(account.label())
                    .test_support(),
            )
            .child(status)
            .child(feel::gloss(
                icon_button("caldav-sync", "refresh-cw", "今すぐ同期")
                    .xsmall()
                    .disabled(state.syncing)
                    .on_click(cx.listener(move |this, _, _, cx| this.sync_now(&sync_account, cx))),
            ))
            .child(
                button(("caldav-remove", ix))
                    .icon(app_icon("trash"))
                    .ghost()
                    .xsmall()
                    .tooltip("アカウントを外す")
                    .pop_menu(move |menu, _, _| {
                        let (view, id) = (view.clone(), remove_id.clone());
                        menu.item(PopupMenuItem::new("アカウントを外す").on_click(
                            move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = view.update(cx, |this, cx| this.remove(&id, cx));
                            },
                        ))
                    }),
            )
    }

    /// macOS のリマインダーと同期しているときの行。ごみ箱のメニューで同期をやめる。
    fn reminders_row(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let view = cx.entity().downgrade();
        row()
            .h(ROW_HEIGHT)
            .gap_2()
            .child(
                app_icon(Provider::Reminders.icon())
                    .size_4()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .id("reminders-account")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(Provider::Reminders.name())
                    .test_support(),
            )
            .child(
                button("reminders-remove")
                    .icon(app_icon("trash"))
                    .ghost()
                    .xsmall()
                    .tooltip("同期をやめる")
                    .pop_menu(move |menu, _, _| {
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::new("同期をやめる").on_click(move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = view.update(cx, |this, cx| this.stop_reminders(cx));
                            }),
                        )
                    }),
            )
    }

    /// 追加のダイアログで選べるプロバイダ。リマインダーは、この OS にあって、まだ同期して
    /// いないときだけ。
    fn providers(&self, cx: &App) -> Vec<Provider> {
        let reminders = crate::reminders::is_app(cx) && !crate::reminders::enabled(cx);
        Provider::ALL
            .into_iter()
            .filter(|p| *p != Provider::Reminders || reminders)
            .collect()
    }

    /// 設定の上に重ねる、アカウントを足すダイアログの中身。
    pub fn add_dialog(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        if let Some(step) = self.step {
            self.shown = step;
        }
        let body = match self.shown {
            Step::Form(provider) => self.add_form(provider, cx),
            Step::Pick => {
                let mut list = col().id("caldav-providers").gap_1();
                for provider in self.providers(cx) {
                    list = list.child(feel::gloss(
                        button(provider.id())
                            .ghost()
                            .w_full()
                            .h(px(40.))
                            .child(
                                row()
                                    .w_full()
                                    .gap_3()
                                    .child(
                                        app_icon(provider.icon())
                                            .size_4()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(div().flex_1().child(provider.name()))
                                    .child(
                                        app_icon("chevron-right")
                                            .size_4()
                                            .text_color(theme.muted_foreground),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose(provider, window, cx)
                            })),
                    ));
                }
                col().child(list)
            }
        };
        body.gap_3().children(self.add_error.clone().map(|e| {
            div()
                .id("caldav-add-error")
                .text_sm()
                .text_color(theme.danger)
                .child(e)
                .test_support()
        }))
    }

    /// 選んだプロバイダのアカウントの欄。項目の名前を左に、入力欄を右に並べる。
    fn add_form(&self, provider: Provider, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let line = |label: &'static str, input: &Entity<InputState>, id: &'static str| {
            row()
                .gap_3()
                .child(
                    div()
                        .w(LABEL_WIDTH)
                        .flex_none()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(input).id(id).aria_label(label)),
                )
        };
        col()
            .key_context("CalDavForm")
            .on_action(cx.listener(|this, _: &crate::CancelKeyEdit, _, cx| this.close_adding(cx)))
            .child(
                col()
                    .gap_2()
                    .when(provider.server().is_none(), |d| {
                        d.child(line("サーバー", &self.url, "caldav-url"))
                    })
                    .child(line("ユーザー名", &self.user, "caldav-user"))
                    .child(line("パスワード", &self.password, "caldav-password"))
                    .child(line("説明", &self.name, "caldav-name")),
            )
            .child(
                row()
                    .gap_2()
                    .justify_end()
                    .child(feel::gloss(
                        button("caldav-cancel")
                            .label("キャンセル")
                            .ghost()
                            .disabled(self.connecting)
                            .on_click(cx.listener(|this, _, _, cx| this.close_adding(cx))),
                    ))
                    .child(feel::gloss(
                        button("caldav-connect")
                            .label("追加")
                            .primary()
                            .loading(self.connecting)
                            .disabled(self.connecting)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    )),
            )
    }
}

impl Render for CalDavSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let accounts = self.accounts(cx);
        let names = self.workspace_names(cx);
        let linked: HashMap<Id, String> = accounts
            .iter()
            .flat_map(|(_, cals)| cals.iter())
            .filter_map(|c| Some((c.workspace_id.clone()?, c.name.clone())))
            .collect();
        let mut list = col().w_full().gap_1();
        if crate::reminders::is_app(cx) && crate::reminders::enabled(cx) {
            list = list.child(self.reminders_row(cx));
        }
        let mut cal_ix = 0;
        for (ix, (account, cals)) in accounts.iter().enumerate() {
            list = list.child(self.account_header(account, ix, cx));
            for cal in cals {
                list = list.child(self.calendar_row(account, cal, cal_ix, &names, &linked, cx));
                cal_ix += 1;
            }
        }
        list.child(
            row().child(feel::gloss(
                button("caldav-add")
                    .label("アカウントを追加")
                    .icon(app_icon("plus"))
                    .outline()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.start_adding(window, cx))),
            )),
        )
        .children(self.error.clone().map(|e| {
            div()
                .id("caldav-form-error")
                .text_xs()
                .text_color(theme.danger)
                .child(e)
                .test_support()
        }))
    }
}
