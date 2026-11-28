//! 設定の「連携と同期」。同期するアカウント（CalDAV のサーバーと、macOS のリマインダー）の一覧、
//! 追加・外す・今すぐ同期と、サーバーのカレンダーをワークスペースにつなぐメニュー。
//! ほかのツール（Notion・Redmine・Backlog・ClickUp・Slack）からの取り込みも、ここから選ぶ。
//!
//! 「連携を追加」で設定の上にダイアログを重ね、プロバイダを選んでから、
//! サーバー・ユーザー名・パスワード・説明を入れ、同期するカレンダーにチェックを付けて足す
//! （最初は全部オン）。チェックしたカレンダーは、アカウントの名前のワークスペースの下に、
//! カレンダーごとのサブワークスペースを作ってつなぐ。パスワードは OS のキーチェーンに入れる。
//! 足したあとも、一覧のチェックボックスでつなぐ・接続しないを切り替え、メニューで
//! つなぐワークスペースを選び直せる。

use std::collections::HashMap;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_caldav::http::Discovery;
use malakt_core::caldav::{Account, Calendar};
use malakt_core::intake::{Batch, FileOptions, Source};
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
    // ここから下は取り込み
    Notion,
    Redmine,
    Backlog,
    ClickUp,
    Slack,
}

impl Provider {
    pub const ALL: [Provider; 9] = [
        Provider::Reminders,
        Provider::Fastmail,
        Provider::Nextcloud,
        Provider::Other,
        Provider::Notion,
        Provider::Redmine,
        Provider::Backlog,
        Provider::ClickUp,
        Provider::Slack,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Reminders => "リマインダー",
            Provider::Fastmail => "Fastmail",
            Provider::Nextcloud => "Nextcloud",
            Provider::Other => "その他の CalDAV アカウント",
            Provider::Notion => "Notion",
            Provider::Redmine => "Redmine",
            Provider::Backlog => "Backlog",
            Provider::ClickUp => "ClickUp",
            Provider::Slack => "Slack",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Provider::Reminders => "list-todo",
            Provider::Fastmail => "mail",
            Provider::Nextcloud => "cloud",
            Provider::Other => "server",
            Provider::Notion => "book",
            Provider::Redmine => "bug",
            Provider::Backlog => "layers",
            Provider::ClickUp => "check-check",
            Provider::Slack => "message-square",
        }
    }

    /// 一覧の行の id。
    fn id(self) -> &'static str {
        match self {
            Provider::Reminders => "caldav-pick-reminders",
            Provider::Fastmail => "caldav-pick-fastmail",
            Provider::Nextcloud => "caldav-pick-nextcloud",
            Provider::Other => "caldav-pick-other",
            Provider::Notion => "intake-pick-notion",
            Provider::Redmine => "intake-pick-redmine",
            Provider::Backlog => "intake-pick-backlog",
            Provider::ClickUp => "intake-pick-clickup",
            Provider::Slack => "intake-pick-slack",
        }
    }

    /// 取り込みのプロバイダ（同期ではない）。
    pub fn imports(self) -> bool {
        self.source().is_some() || self == Provider::Slack
    }

    /// ファイルから取り込むときの形式。
    fn source(self) -> Option<Source> {
        match self {
            Provider::Notion => Some(Source::Notion),
            Provider::Redmine => Some(Source::Redmine),
            Provider::Backlog => Some(Source::Backlog),
            Provider::ClickUp => Some(Source::ClickUp),
            _ => None,
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
            Provider::Redmine => "redmine.example.com（省略可）",
            Provider::Backlog => "example.backlog.jp（省略可）",
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
    /// 見つかったカレンダーから、同期するものを選ぶ
    Calendars(Provider),
}

/// 調べ終わって、カレンダーを選んでいるアカウント。
struct Pending {
    user: String,
    password: String,
    name: String,
    found: Discovery,
    /// カレンダーごとの、同期するか（`found.calendars` と同じ順）
    on: Vec<bool>,
}

/// 追加のダイアログを開く・閉じる。設定の上に重ねるのは、設定を出しているビュー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddDialog {
    Open,
    Close,
    /// 取り込みが終わった。知らせる文
    Imported(SharedString),
}

/// Slack から何を取り込むか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlackKind {
    Reminders,
    List,
}

impl SlackKind {
    fn label(self) -> &'static str {
        match self {
            SlackKind::Reminders => "リマインダー",
            SlackKind::List => "Slack リスト",
        }
    }
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
    /// アカウントのワークスペースの下に、新しく作る
    New,
    /// チェックを付け直した。アカウントのワークスペースの下に、同じ名前でどこにも
    /// つないでいないワークスペースがあればそれに、なければ新しく作ってつなぐ
    Again,
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
    /// 取り込む Slack リストの URL
    list: Entity<InputState>,
    /// 取り込むファイル
    file: Option<std::path::PathBuf>,
    /// 取り込む先。`None` は新しいワークスペース
    import_to: Option<Id>,
    slack_kind: SlackKind,
    /// 追加のダイアログの段階。閉じていれば `None`。
    step: Option<Step>,
    /// 最後に出した段階。閉じたダイアログが消えるまでの間も、同じ中身を描く。
    shown: Step,
    /// 調べ終わったアカウント。カレンダーを選んで「追加」を押すまで、まだ足さない。
    pending: Option<Pending>,
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
        let list = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://example.slack.com/lists/…"));
        let mut subs = vec![
            cx.observe(&db, |_, _, cx| cx.notify()),
            cx.observe(&syncer, |_, _, cx| cx.notify()),
        ];
        for input in [&url, &user, &password, &name, &list] {
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
            list,
            file: None,
            import_to: None,
            slack_kind: SlackKind::Reminders,
            step: None,
            shown: Step::Pick,
            pending: None,
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
            Step::Form(p) | Step::Calendars(p) => p.name(),
            Step::Pick => "連携を追加",
        }
    }

    /// 追加のダイアログを開き、プロバイダを選ぶところから始める。
    pub fn start_adding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_gen += 1;
        self.step = Some(Step::Pick);
        self.shown = Step::Pick;
        self.pending = None;
        self.connecting = false;
        self.add_error = None;
        for input in [
            &self.url,
            &self.user,
            &self.password,
            &self.name,
            &self.list,
        ] {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.file = None;
        self.import_to = None;
        self.slack_kind = SlackKind::Reminders;
        cx.emit(AddDialog::Open);
        cx.notify();
    }

    /// 追加のダイアログを閉じたことを受ける。調べている途中の結果は捨てる。
    pub fn end_adding(&mut self, cx: &mut Context<Self>) {
        if self.step.take().is_some() {
            self.add_gen += 1;
            self.pending = None;
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
        let first = if provider == Provider::Slack {
            &self.list
        } else if provider.server().is_some() {
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

    /// 入れたサーバー・ユーザー名・パスワードでサーバーを調べ、見つかったカレンダーを並べる。
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Step::Form(provider)) = self.step else {
            return;
        };
        if self.connecting {
            return;
        }
        if provider.imports() {
            self.start_import(provider, window, cx);
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
        let discover = caldav.discover.clone();
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
                this.pending = Some(Pending {
                    on: vec![true; found.calendars.len()],
                    user,
                    password,
                    name,
                    found,
                });
                this.step = Some(Step::Calendars(provider));
                cx.notify();
            });
        })
        .detach();
    }

    /// 取り込むファイルを選ぶダイアログを出す。
    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("取り込む".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            // 設定を閉じたあとなら、反映する先がない
            let _ = this.update(cx, |this, cx| this.choose_file(path, cx));
        })
        .detach();
    }

    /// 取り込むファイルを決める。
    pub fn choose_file(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.file = Some(path);
        self.add_error = None;
        cx.notify();
    }

    /// 読んで、取り込む。読むのは裏で、入れるのは画面のスレッドで。
    fn start_import(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        enum Pull {
            File(std::path::PathBuf, FileOptions),
            Reminders,
            List(String),
        }
        let pull = if let Some(source) = provider.source() {
            let Some(path) = self.file.clone() else {
                self.add_error = Some("ファイルを選んでください".into());
                cx.notify();
                return;
            };
            let base = self.url.read(cx).value().trim().to_owned();
            Pull::File(
                path,
                FileOptions {
                    source: Some(source),
                    base_url: (source.needs_base_url() && !base.is_empty()).then_some(base),
                },
            )
        } else {
            match self.slack_kind {
                SlackKind::Reminders => Pull::Reminders,
                SlackKind::List => {
                    let list = self.list.read(cx).value().trim().to_owned();
                    if list.is_empty() {
                        self.add_error = Some("Slack リストの URL を入れてください".into());
                        cx.notify();
                        return;
                    }
                    Pull::List(list)
                }
            }
        };
        self.connecting = true;
        self.add_error = None;
        cx.notify();
        let gen = self.add_gen;
        let task = cx.background_executor().spawn(async move {
            match pull {
                Pull::File(path, options) => {
                    malakt_core::intake::read_file(&path, &options).map_err(|e| e.to_string())
                }
                Pull::Reminders => malakt_ai::slack::import_reminders(),
                Pull::List(list) => malakt_ai::slack::import_list(&list),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let batch = task.await;
            // 設定を閉じたあとなら、反映する先がない
            let _ = this.update_in(cx, |this, _, cx| {
                // 待っている間にダイアログを閉じたら、取り込まない
                if this.add_gen != gen {
                    return;
                }
                this.connecting = false;
                match batch.and_then(|batch| this.import_batch(provider, &batch, cx)) {
                    Ok(done) => {
                        this.close_adding(cx);
                        cx.emit(AddDialog::Imported(done.into()));
                    }
                    Err(e) => {
                        this.add_error = Some(e.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 読んだ中身を入れる。新しいワークスペースに入れるときは、取り込めなければ作ったものも消す。
    /// 全体を、元に戻す 1 つの段にする。
    fn import_batch(
        &mut self,
        provider: Provider,
        batch: &Batch,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let target = self.import_to.clone();
        self.db.update(cx, |db, cx| {
            let store = &mut db.store;
            let result = store.undo_mark().and_then(|()| match &target {
                Some(w) => store.import_batch(Some(w), batch),
                None => {
                    let w = store.create_workspace(None, provider.name())?;
                    match store.import_batch(Some(&w.id), batch) {
                        Ok(report) => Ok(report),
                        Err(e) => match store.delete_workspace(&w.id) {
                            Ok(()) => Err(e),
                            Err(more) => Err(malakt_core::Error::Invalid(format!(
                                "{e}（作ったワークスペースも消せませんでした。{more}）"
                            ))),
                        },
                    }
                }
            });
            let marked = store.undo_mark();
            cx.notify();
            let report = result
                .and_then(|r| marked.map(|()| r))
                .map_err(|e| e.to_string())?;
            Ok(report.summary(batch.source))
        })
    }

    /// 選んだカレンダーで、アカウントを足す。チェックしたカレンダーは、アカウントの名前の
    /// ワークスペースの下に、カレンダーごとのサブワークスペースを作ってつなぎ、すぐ同期する。
    fn finish(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let Some(caldav) = cx.try_global::<CalDav>() else {
            self.add_error = Some("この環境ではサーバーに接続できません".into());
            self.pending = Some(pending);
            cx.notify();
            return;
        };
        let secrets = caldav.secrets.clone();
        let Pending {
            user,
            password,
            name,
            found,
            on,
        } = &pending;
        let added = self.db.update(cx, |db, cx| {
            let added = db.store.add_caldav_account(
                &found.base,
                user,
                name,
                &found.addresses,
                &found.calendars,
            );
            cx.notify();
            added
        });
        let account = match added {
            Ok(a) => a,
            Err(e) => {
                self.add_error = Some(format!("アカウントを保存できませんでした。{e}").into());
                self.pending = Some(pending);
                cx.notify();
                return;
            }
        };
        // パスワードを入れられなければ、足したアカウントも戻す
        if let Err(e) = secrets.set(&account.id, password) {
            let undone = self
                .db
                .update(cx, |db, _| db.store.remove_caldav_account(&account.id));
            self.add_error = Some(match undone {
                Ok(()) => e.into(),
                Err(more) => format!("{e}（追加したアカウントも外せませんでした。{more}）").into(),
            });
            self.pending = Some(pending);
            cx.notify();
            return;
        }
        let chosen: Vec<&str> = found
            .calendars
            .iter()
            .zip(on)
            .filter(|(_, on)| **on)
            .map(|(c, _)| c.href.as_str())
            .collect();
        let linked = self.db.update(cx, |db, cx| {
            let store = &mut db.store;
            let linked = (|| -> malakt_core::Result<usize> {
                let cals: Vec<Calendar> = store
                    .caldav_calendars(&account.id)?
                    .into_iter()
                    .filter(|c| chosen.contains(&c.href.as_str()))
                    .collect();
                if cals.is_empty() {
                    return Ok(0);
                }
                let parent = store.create_workspace(None, &account.label())?;
                for c in &cals {
                    let w = store.create_workspace(Some(&parent.id), &c.name)?;
                    store.link_caldav_calendar(&c.id, Some(&w.id))?;
                }
                Ok(cals.len())
            })();
            cx.notify();
            linked
        });
        self.error = match linked {
            Ok(0) if found.calendars.is_empty() => {
                Some("このアカウントには、タスクを入れられるカレンダーがありません".into())
            }
            Ok(0) => None,
            Ok(_) => {
                let id = account.id.clone();
                self.syncer.update(cx, |s, cx| s.request(&id, true, cx));
                None
            }
            Err(e) => Some(format!("カレンダーをワークスペースにつなげませんでした。{e}").into()),
        };
        self.close_adding(cx);
    }

    /// 追加のダイアログで、カレンダーのチェックを切り替える。
    fn toggle_pending(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(on) = self.pending.as_mut().and_then(|p| p.on.get_mut(ix)) {
            *on = !*on;
            cx.notify();
        }
    }

    /// リマインダーとの同期をやめる。このアプリが「リマインダー」に作ったものは消える。
    fn stop_reminders(&mut self, cx: &mut Context<Self>) {
        self.error = crate::reminders::set_enabled(false, cx)
            .err()
            .map(|e| format!("リマインダーとの同期をやめられませんでした。{e}").into());
        cx.notify();
    }

    /// カレンダーをワークスペースにつなぐ・外す。つないだらすぐ同期する。
    /// 新しいワークスペースは、アカウントのワークスペース（ほかのカレンダーをつないだ
    /// ワークスペースの親、なければアカウントの名前のいちばん上のワークスペース。それも
    /// なければ作る）の下に作る。
    fn link(
        &mut self,
        account: &Account,
        calendar: &Calendar,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        let account_id = account.id.as_str();
        let result = self.db.update(cx, |db, cx| {
            let store = &mut db.store;
            let result = match &target {
                Target::None => store.link_caldav_calendar(&calendar.id, None),
                Target::Workspace(w) => store.link_caldav_calendar(&calendar.id, Some(w)),
                Target::New | Target::Again => (|| {
                    let mut parent = None;
                    for c in store.caldav_calendars(account_id)? {
                        if let Some(w) = c.workspace_id.as_deref() {
                            if let Some(p) = store.workspace(w)?.parent_id {
                                parent = Some(p);
                                break;
                            }
                        }
                    }
                    // ほかにつないだカレンダーがなければ、アカウントの名前のワークスペースを使う
                    let label = account.label();
                    if parent.is_none() {
                        parent = store
                            .workspaces()?
                            .into_iter()
                            .find(|w| w.parent_id.is_none() && w.name == label)
                            .map(|w| w.id);
                    }
                    let parent = match parent {
                        Some(p) => p,
                        None => store.create_workspace(None, &label)?.id,
                    };
                    let mut reuse = None;
                    if target == Target::Again {
                        let mut linked = std::collections::HashSet::new();
                        for a in store.caldav_accounts()? {
                            for c in store.caldav_calendars(&a.id)? {
                                linked.extend(c.workspace_id);
                            }
                        }
                        reuse = store
                            .workspaces()?
                            .into_iter()
                            .find(|w| {
                                w.parent_id.as_deref() == Some(parent.as_str())
                                    && w.name == calendar.name
                                    && !linked.contains(&w.id)
                            })
                            .map(|w| w.id);
                    }
                    let w = match reuse {
                        Some(w) => w,
                        None => store.create_workspace(Some(&parent), &calendar.name)?.id,
                    };
                    store.link_caldav_calendar(&calendar.id, Some(&w))
                })(),
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
        let view = cx.entity().downgrade();
        let choices: Vec<(Target, String, bool, bool)> = {
            let mut list = vec![
                (
                    Target::None,
                    "接続しない".to_owned(),
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
        let on = cal.workspace_id.is_some();
        let (toggle_account, toggle_cal) = (account.clone(), cal.clone());
        let (account, calendar) = (account.clone(), cal.clone());
        row()
            .h(ROW_HEIGHT)
            .gap_2()
            .pl_6()
            .child(
                Checkbox::new(("caldav-on", ix))
                    .checked(on)
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        let target = if on { Target::None } else { Target::Again };
                        this.link(&toggle_account, &toggle_cal, target, cx)
                    })),
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
            .when(cal.read_only, |d| {
                d.child(app_icon("lock").size_4().text_color(theme.muted_foreground))
            })
            // つないでいるときだけ、つなぐワークスペースを選び直すメニューを出す
            .when_some(current, |d, current| {
                d.child(
                    button(("caldav-link", ix))
                        .label(current)
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
                                let (view, account, calendar) =
                                    (view.clone(), account.clone(), calendar.clone());
                                menu = menu.item(
                                    PopupMenuItem::new(name)
                                        .checked(checked)
                                        .disabled(!enabled)
                                        .on_click(move |_, _, cx| {
                                            // 設定を閉じたあとなら、反映する先がない
                                            let _ = view.update(cx, |this, cx| {
                                                this.link(&account, &calendar, target.clone(), cx)
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                )
            })
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
            Step::Form(provider) if provider.imports() => self.import_form(provider, cx),
            Step::Form(provider) => self.add_form(provider, cx),
            Step::Calendars(_) => self.add_calendars(cx),
            Step::Pick => {
                let mut list = col().id("caldav-providers").gap_1();
                let mut was_sync = false;
                for provider in self.providers(cx) {
                    // 同期のプロバイダと、取り込みのプロバイダの間に線を引く
                    if provider.imports() && was_sync {
                        list = list.child(div().my_1().h(px(1.)).bg(theme.border));
                    }
                    was_sync = !provider.imports();
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

    /// 取り込みのプロバイダの欄。ファイル（Slack は種類とリスト）、サーバー、取り込む先。
    fn import_form(&self, provider: Provider, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let label = |text: &'static str| {
            div()
                .w(LABEL_WIDTH)
                .flex_none()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let line = |name: &'static str, field: AnyElement| {
            row()
                .gap_3()
                .child(label(name))
                .child(div().flex_1().min_w_0().child(field))
        };
        let view = cx.entity().downgrade();
        let mut fields = col().gap_2();
        if provider.source().is_some() {
            let chosen: SharedString = self
                .file
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ファイルを選ぶ".into())
                .into();
            fields = fields.child(line(
                "ファイル",
                feel::gloss(
                    button("intake-file")
                        .label(chosen)
                        .icon(app_icon("folder"))
                        .outline()
                        .small()
                        .w_full()
                        .on_click(cx.listener(|this, _, window, cx| this.pick_file(window, cx))),
                )
                .into_any_element(),
            ));
            if provider.source().is_some_and(Source::needs_base_url) {
                fields = fields.child(line(
                    "サーバー",
                    Input::new(&self.url)
                        .id("intake-server")
                        .aria_label("サーバー")
                        .into_any_element(),
                ));
            }
        } else {
            let kind = self.slack_kind;
            let v = view.clone();
            fields = fields.child(line(
                "種類",
                button("intake-slack-kind")
                    .label(kind.label())
                    .icon(app_icon("chevron-down"))
                    .outline()
                    .small()
                    .w_full()
                    .pop_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for k in [SlackKind::Reminders, SlackKind::List] {
                            let v = v.clone();
                            menu = menu.item(
                                PopupMenuItem::new(k.label()).checked(k == kind).on_click(
                                    move |_, _, cx| {
                                        // 設定を閉じたあとなら、反映する先がない
                                        let _ = v.update(cx, |this, cx| {
                                            this.slack_kind = k;
                                            this.add_error = None;
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    })
                    .into_any_element(),
            ));
            if kind == SlackKind::List {
                fields = fields.child(line(
                    "リスト",
                    Input::new(&self.list)
                        .id("intake-list")
                        .aria_label("リスト")
                        .into_any_element(),
                ));
            }
        }
        let names = self.workspace_names(cx);
        let current: SharedString = self
            .import_to
            .as_deref()
            .and_then(|w| names.iter().find(|(ws, _)| ws.id == w))
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| "新しいワークスペース".into())
            .into();
        let picked = self.import_to.clone();
        fields = fields.child(line(
            "取り込む先",
            button("intake-target")
                .label(current)
                .icon(app_icon("chevron-down"))
                .outline()
                .small()
                .w_full()
                .pop_menu(move |menu, _, _| {
                    let mut menu = menu;
                    let choices = std::iter::once((None, "新しいワークスペース".to_owned()))
                        .chain(names.iter().map(|(w, n)| (Some(w.id.clone()), n.clone())));
                    for (i, (target, name)) in choices.enumerate() {
                        if i == 1 {
                            menu = menu.separator();
                        }
                        let v = view.clone();
                        let checked = target == picked;
                        menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                            move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = v.update(cx, |this, cx| {
                                    this.import_to = target.clone();
                                    cx.notify();
                                });
                            },
                        ));
                    }
                    menu
                })
                .into_any_element(),
        ));
        col()
            .key_context("CalDavForm")
            .on_action(cx.listener(|this, _: &crate::CancelKeyEdit, _, cx| this.close_adding(cx)))
            .child(fields)
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
                        button("intake-run")
                            .label("取り込む")
                            .primary()
                            .loading(self.connecting)
                            .disabled(self.connecting)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    )),
            )
    }

    /// 見つかったカレンダーの一覧。チェックしたものを同期する（最初は全部オン）。
    fn add_calendars(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col().id("caldav-new-calendars").gap_1();
        if let Some(p) = &self.pending {
            for (ix, (cal, on)) in p.found.calendars.iter().zip(&p.on).enumerate() {
                list = list.child(
                    row()
                        .h(ROW_HEIGHT)
                        .gap_2()
                        .child(
                            Checkbox::new(("caldav-new-on", ix))
                                .checked(*on)
                                .label(cal.name.clone())
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.toggle_pending(ix, cx)
                                })),
                        )
                        .when(cal.read_only, |d| {
                            d.child(app_icon("lock").size_4().text_color(theme.muted_foreground))
                        }),
                );
            }
            if p.found.calendars.is_empty() {
                list = list.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("タスクを入れられるカレンダーがありません"),
                );
            }
        }
        col()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("同期するカレンダー"),
            )
            .child(
                div()
                    .id("caldav-new-calendars-scroll")
                    .max_h(px(360.))
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .child(
                row()
                    .gap_2()
                    .justify_end()
                    .child(feel::gloss(
                        button("caldav-cancel")
                            .label("キャンセル")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.close_adding(cx))),
                    ))
                    .child(feel::gloss(
                        button("caldav-finish")
                            .label("追加")
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                    )),
            )
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
                            .label("次へ")
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
                    .label("連携を追加")
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
