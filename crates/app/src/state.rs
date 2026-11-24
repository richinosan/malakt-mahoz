//! 1 枚のウィンドウの状態と操作。描画は `render` にある。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::WindowExt;
use gpui_kit::*;
use malakt_core::bulk::{self, BulkOp, ListMode};
use malakt_core::notice::Notice;
use malakt_core::ticket::IssuedTicket;
use malakt_core::view::{self, ListQuery};
use malakt_core::{
    archive, lock, mention, Anchor, Attachment, Change, Comment, DueRule, Id, NewComment, Person,
    Scope, Status, Store, Task as Todo, TaskDefaults, TaskDraft, TaskFilter, Workspace,
};

use crate::db::{self, Db};

/// 左のメニューで選んでいる場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nav {
    Home,
    /// すべてのタスク
    All,
    /// どのワークスペースにも入れていないタスク
    Unfiled,
    Workspace(Id),
}

/// 画面の上に重ねて出すダイアログ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    None,
    /// ワークスペースの作成（`rename` があれば名前の変更）
    WorkspaceName {
        parent: Option<Id>,
        rename: Option<Id>,
    },
    DeleteWorkspace(Id),
    DeleteTasks(Vec<Id>),
    /// 移動先のワークスペースを選ぶ
    MoveTasks(Vec<Id>),
    People,
    History,
    /// 状態の変更（コメントを添えられる）
    Status(Id, Status),
    Lock,
    /// ワークスペースの基準日と、タスク追加時の既定値
    WorkspaceSettings(Id),
    /// ワークスペースのアイコンと色を選ぶ
    WorkspaceIcon(Id),
    /// タスクをほかのワークスペースにも表示する（表示先を選ぶ）
    LinkTask(Id),
    /// 選んだタスクの期限をまとめて変える
    BulkDue,
    /// 選んだタスクの担当者をまとめて変える
    BulkAssignees,
    /// 選んだタスクのカテゴリをまとめて変える
    BulkCategories,
    /// 項目を入れてタスクを作る
    NewTask,
    /// 通知の一覧
    Notices,
    /// 完了チケットの発行
    Ticket(Id),
}

/// 詳細パネルで編集中の項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editing {
    Title,
    Description,
}

/// コメント欄でいま書いているもの。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CommentMode {
    /// 新しいコメント
    #[default]
    New,
    /// スレッドへの返信（ルートのコメント）
    Reply(Id),
    /// 書いたコメントの書き直し
    Edit(Id),
}

/// 詳細パネルに出すタスクと、その周りのデータ。
pub struct Detail {
    pub task: Todo,
    pub comments: Vec<Comment>,
    pub attachments: Vec<Attachment>,
    pub changes: Vec<Change>,
    pub subtasks: Vec<Todo>,
    pub parent: Option<Todo>,
}

/// ワークスペースのツリーの 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub index: usize,
    pub depth: usize,
    pub has_children: bool,
}

pub struct Inputs {
    pub quick_add: Entity<InputState>,
    pub search: Entity<InputState>,
    pub name: Entity<InputState>,
    pub passphrase: Entity<InputState>,
    pub new_passphrase: Entity<InputState>,
    pub title: Entity<InputState>,
    pub subtask: Entity<InputState>,
    pub start: Entity<InputState>,
    pub due: Entity<InputState>,
    pub category: Entity<InputState>,
    pub description: Entity<TextareaState>,
    pub comment: Entity<TextareaState>,
    pub status_comment: Entity<TextareaState>,
    pub ws_date: Entity<InputState>,
    pub def_days: Entity<InputState>,
    pub def_date: Entity<InputState>,
    pub def_time: Entity<InputState>,
    pub def_categories: Entity<InputState>,
    pub def_description: Entity<TextareaState>,
    pub bulk_due: Entity<InputState>,
    pub bulk_categories: Entity<InputState>,
    pub nt_title: Entity<InputState>,
    pub nt_start: Entity<InputState>,
    pub nt_due: Entity<InputState>,
    pub nt_categories: Entity<InputState>,
    pub nt_description: Entity<TextareaState>,
}

pub struct MainView {
    pub db: Entity<Db>,
    pub focus: FocusHandle,
    pub nav: Nav,
    pub collapsed: HashSet<Id>,
    pub include_descendants: bool,
    pub workspaces: Vec<Workspace>,
    pub tree: Vec<TreeRow>,
    pub open_counts: HashMap<Id, u32>,
    pub people: Vec<Person>,
    pub self_id: Option<Id>,
    pub categories: Vec<String>,
    pub tasks: Rc<Vec<Todo>>,
    pub visible: Rc<Vec<usize>>,
    pub counts: (usize, usize),
    pub due: Rc<Vec<Todo>>,
    pub due_mine: bool,
    pub query: ListQuery,
    pub filters_open: bool,
    pub selected: HashSet<Id>,
    /// Shift を押しながらの範囲選択の起点（最後に押した行）
    pub select_anchor: Option<Id>,
    /// まとめて変えるダイアログの入力（担当者の変え方、選んだ担当者、カテゴリを足すか外すか）
    pub bulk_mode: ListMode,
    pub bulk_people: Vec<Id>,
    pub bulk_category_add: bool,
    /// 新しいタスクのダイアログで編集中の下書き（入力欄にないもの: 優先度・担当者）
    pub new_task: TaskDraft,
    /// 一覧の最後の追加行に入れている状態・優先度・担当・期限
    pub quick: TaskDraft,
    /// `quick` を作ったときの行き先。行き先が変わったら作り直す
    quick_nav: Option<Nav>,
    /// 追加行の値を手で変えた。変えるまでは、既定値の変更に合わせて作り直す
    pub quick_touched: bool,
    pub panel: Option<Id>,
    pub detail: Option<Detail>,
    pub editing: Option<Editing>,
    pub comment_mode: CommentMode,
    /// 次のコメントを付ける、説明の選択範囲。
    pub pending_anchor: Option<Anchor>,
    /// 開いて見せている、解決済みのスレッド。
    pub expanded_threads: HashSet<Id>,
    /// 説明の表示。文字を選んでレビューコメントを付けられる。
    pub description_view: Entity<TextViewState>,
    description_shown: String,
    pub modal: Modal,
    /// 完了チケットをだれの分として発行するか（`None` は自分）と、発行したばかりのチケット。
    /// チケットの文字列は発行したときしか分からないので、ダイアログを閉じるまで持つ
    pub ticket_person: Option<Id>,
    pub issued_ticket: Option<IssuedTicket>,
    /// 通知（新しい順）と、未読の数
    pub notices: Vec<Notice>,
    pub unread_notices: u32,
    /// 設定のダイアログで編集中の既定値（入力欄にないもの: 担当者・期限の決め方・優先度・表示先）
    pub defaults_draft: TaskDefaults,
    pub error: Option<SharedString>,
    pub notice: Option<SharedString>,
    pub sidebar_open: bool,
    /// 広いウィンドウで、左のメニューを閉じている
    pub sidebar_hidden: bool,
    /// 出入りの動きを付ける。テストでは止めて、すぐ最後の位置に出す
    pub motion: bool,
    pub today: String,
    pub inputs: Inputs,
    _subscriptions: Vec<Subscription>,
}

impl MainView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let db = db::shared(cx);
        let input = |window: &mut Window, cx: &mut Context<Self>, placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let area = |window: &mut Window, cx: &mut Context<Self>, placeholder: &'static str| {
            cx.new(|cx| TextareaState::new(window, cx).placeholder(placeholder))
        };
        let inputs = Inputs {
            quick_add: input(window, cx, "タスクを追加…"),
            search: input(window, cx, "キーワード"),
            name: input(window, cx, "名前"),
            passphrase: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("パスフレーズ")
            }),
            new_passphrase: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("新しいパスフレーズ")
            }),
            title: input(window, cx, "タイトル"),
            subtask: input(window, cx, "サブタスクを追加…"),
            start: input(window, cx, "YYYY-MM-DD"),
            due: input(window, cx, "YYYY-MM-DD"),
            category: input(window, cx, "カテゴリを追加して Enter"),
            description: area(window, cx, "説明"),
            comment: area(window, cx, "コメントを書く"),
            status_comment: area(window, cx, "コメントを書く"),
            ws_date: input(window, cx, "YYYY-MM-DD"),
            def_days: input(window, cx, "例: -7（7 日前）"),
            def_date: input(window, cx, "YYYY-MM-DD"),
            def_time: input(window, cx, "HH:MM（空なら日付だけ）"),
            def_categories: input(window, cx, "「、」で区切って入力"),
            def_description: area(window, cx, "説明の雛形"),
            bulk_due: input(window, cx, "YYYY-MM-DD（時刻は続けて HH:MM）"),
            bulk_categories: input(window, cx, "「、」で区切って入力"),
            nt_title: input(window, cx, "タイトル"),
            nt_start: input(window, cx, "YYYY-MM-DD（時刻は続けて HH:MM）"),
            nt_due: input(window, cx, "YYYY-MM-DD（時刻は続けて HH:MM）"),
            nt_categories: input(window, cx, "「、」で区切って入力"),
            nt_description: area(window, cx, "説明"),
        };

        let mut subs = vec![
            cx.observe(&db, |this, _, cx| this.reload(cx)),
            // 読み直すたびに、まだ出していない通知を OS に出す
            cx.observe_in(&db, window, |this, _, window, cx| {
                this.deliver_notices(window, cx)
            }),
        ];
        macro_rules! on_enter {
            ($field:ident, $method:ident) => {
                subs.push(cx.subscribe_in(
                    &inputs.$field,
                    window,
                    |this, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            this.$method(window, cx);
                        }
                    },
                ));
            };
        }
        on_enter!(quick_add, submit_quick_add);
        on_enter!(name, submit_name);
        on_enter!(passphrase, submit_unlock);
        on_enter!(title, save_title);
        // タイトルは、ほかの場所を押したときにも保存する
        subs.push(cx.subscribe_in(
            &inputs.title,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) && this.editing == Some(Editing::Title) {
                    this.save_title(window, cx);
                }
            },
        ));
        on_enter!(subtask, submit_subtask);
        on_enter!(start, save_start);
        on_enter!(due, save_due);
        on_enter!(category, submit_category);
        subs.push(cx.subscribe_in(
            &inputs.search,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query.q = input.read(cx).value().to_string();
                    this.refilter();
                    cx.notify();
                }
            },
        ));
        // 複数行の欄は Ctrl / ⌘ + Enter で送る
        macro_rules! on_secondary_enter {
            ($field:ident, $method:ident) => {
                subs.push(cx.subscribe_in(
                    &inputs.$field,
                    window,
                    |this, _, event: &InputEvent, window, cx| {
                        if matches!(
                            event,
                            InputEvent::PressEnter {
                                secondary: true,
                                ..
                            }
                        ) {
                            this.$method(window, cx);
                        }
                    },
                ));
            };
        }
        on_secondary_enter!(description, save_description);
        on_secondary_enter!(comment, submit_comment);
        // `@` の候補を、打つたびに出し直す
        subs.push(cx.subscribe_in(
            &inputs.comment,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        on_secondary_enter!(status_comment, submit_status);

        let mut this = Self {
            db,
            focus: cx.focus_handle(),
            nav: Nav::Home,
            collapsed: HashSet::new(),
            include_descendants: true,
            workspaces: Vec::new(),
            tree: Vec::new(),
            open_counts: HashMap::new(),
            people: Vec::new(),
            self_id: None,
            categories: Vec::new(),
            tasks: Rc::new(Vec::new()),
            visible: Rc::new(Vec::new()),
            counts: (0, 0),
            due: Rc::new(Vec::new()),
            due_mine: false,
            query: ListQuery::default(),
            filters_open: false,
            selected: HashSet::new(),
            select_anchor: None,
            bulk_mode: ListMode::Add,
            bulk_people: Vec::new(),
            bulk_category_add: true,
            new_task: TaskDraft::default(),
            quick: TaskDraft::default(),
            quick_nav: None,
            quick_touched: false,
            panel: None,
            detail: None,
            editing: None,
            comment_mode: CommentMode::New,
            pending_anchor: None,
            expanded_threads: HashSet::new(),
            description_view: cx.new(|cx| TextViewState::markdown("", cx).selectable(true)),
            description_shown: String::new(),
            modal: Modal::None,
            ticket_person: None,
            issued_ticket: None,
            notices: Vec::new(),
            unread_notices: 0,
            defaults_draft: TaskDefaults::default(),
            error: None,
            notice: None,
            sidebar_open: false,
            sidebar_hidden: false,
            motion: true,
            today: view::today(),
            inputs,
            _subscriptions: subs,
        };
        this.reload(cx);
        // ウィンドウの準備が終わってから、たまっている通知を出す
        // 「システムに合わせる」のとき、OS の外観が変わったら塗り直す
        cx.observe_window_appearance(window, |_, window, cx| {
            crate::theme::system_changed(window, cx)
        })
        .detach();
        cx.defer_in(window, |_, window, cx| {
            crate::theme::system_changed(window, cx)
        });
        cx.defer_in(window, |this, window, cx| this.deliver_notices(window, cx));
        this
    }

    pub fn unlocked(&self, cx: &App) -> bool {
        self.db.read(cx).unlocked
    }

    // ---- 読み込み ----

    /// データベースから読み直す。どのウィンドウで変えても、全部のウィンドウで呼ばれる。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.today = view::today();
        if !self.quick_touched || self.quick_nav.as_ref() != Some(&self.nav) {
            self.reset_quick(cx);
        }
        let result = self.load(cx);
        if let Err(error) = result {
            self.error = Some(error.to_string().into());
        }
        let description = self
            .detail
            .as_ref()
            .map_or("", |d| d.task.description.as_str());
        if self.description_shown != description {
            self.description_shown = description.to_owned();
            self.description_view
                .update(cx, |view, cx| view.set_text(description, cx));
        }
        self.refilter();
        cx.notify();
    }

    fn load(&mut self, cx: &App) -> malakt_core::Result<()> {
        let store = &self.db.read(cx).store;
        self.workspaces = store.workspaces()?;
        if let Nav::Workspace(id) = &self.nav {
            if !self.workspaces.iter().any(|w| &w.id == id) {
                self.nav = Nav::Home;
            }
        }
        self.tree = build_tree(&self.workspaces, &self.collapsed);
        self.open_counts = store.open_task_counts()?;
        self.people = store.people()?;
        self.self_id = self.people.iter().find(|p| p.is_self).map(|p| p.id.clone());
        self.categories = store.categories()?;
        self.notices = store.notices(NOTICES_SHOWN)?;
        self.unread_notices = store.unread_notice_count()?;
        let filter = TaskFilter {
            include_descendants: self.include_descendants,
            ..Default::default()
        };
        self.tasks = Rc::new(match &self.nav {
            Nav::Home => Vec::new(),
            Nav::All => store.tasks(Scope::All, &filter)?,
            Nav::Unfiled => store.tasks(Scope::Unfiled, &filter)?,
            Nav::Workspace(id) => store.tasks(id, &filter)?,
        });
        let until = view::add_days(&self.today, view::HOME_DUE_DAYS);
        let mine = if self.due_mine {
            self.self_id.as_deref()
        } else {
            None
        };
        self.due = Rc::new(store.due_tasks(&until, mine)?);
        self.detail = match &self.panel {
            Some(id) => match store.task(id) {
                Ok(task) => Some(Detail {
                    comments: store.comments(id)?,
                    attachments: store.attachments(id)?,
                    changes: store.changes(id)?,
                    subtasks: store.subtasks(id)?,
                    parent: task.parent_id.as_deref().and_then(|p| store.task(p).ok()),
                    task,
                }),
                // ほかのウィンドウで消された
                Err(_) => None,
            },
            None => None,
        };
        if self.detail.is_none() {
            self.panel = None;
            self.editing = None;
        }
        Ok(())
    }

    /// 絞り込みと並べ替えだけをやり直す（データベースは読まない）。
    pub fn refilter(&mut self) {
        self.counts = view::counts(&self.tasks);
        self.visible = Rc::new(view::apply(
            &self.tasks,
            &self.query,
            self.self_id.as_deref(),
        ));
        // 見えなくなったタスクは、選択から外す（見えていないものをまとめて変えないように）
        let shown: HashSet<&str> = self
            .visible
            .iter()
            .map(|&i| self.tasks[i].id.as_str())
            .collect();
        self.selected.retain(|id| shown.contains(id.as_str()));
    }

    /// データベースを書き換える。失敗したら画面に出す。
    pub fn write<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Store) -> malakt_core::Result<R>,
    ) -> Option<R> {
        let result = self.db.update(cx, |db, cx| {
            let result = f(&mut db.store);
            cx.notify();
            result
        });
        match result {
            Ok(value) => {
                self.error = None;
                Some(value)
            }
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                None
            }
        }
    }

    /// ワークスペースのアイコンと色を決める。`None` は既定に戻す。
    pub fn set_workspace_icon(
        &mut self,
        id: &str,
        icon: Option<&str>,
        color: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.write(cx, |s| s.set_workspace_icon(id, icon, color));
    }

    // ---- 移動 ----

    pub fn go(&mut self, nav: Nav, cx: &mut Context<Self>) {
        // 狭いウィンドウでは、行き先を選んだらメニューを閉じて中身を見せる
        self.sidebar_open = false;
        if self.nav != nav {
            self.nav = nav;
            self.selected.clear();
            self.panel = None;
            self.editing = None;
            self.notice = None;
        }
        self.reload(cx);
    }

    pub fn toggle_collapsed(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_owned());
        }
        self.tree = build_tree(&self.workspaces, &self.collapsed);
        cx.notify();
    }

    /// 今の場所で新しく作るタスクの入れ先。
    pub fn current_workspace(&self) -> Option<&str> {
        match &self.nav {
            Nav::Workspace(id) => Some(id),
            _ => None,
        }
    }

    pub fn workspace_name(&self, id: Option<&str>) -> SharedString {
        match id.and_then(|id| self.workspaces.iter().find(|w| w.id == id)) {
            Some(w) => w.name.clone().into(),
            None => "未所属".into(),
        }
    }

    pub fn person_name(&self, id: &str) -> &str {
        self.people
            .iter()
            .find(|p| p.id == id)
            .map_or("（不明）", |p| p.name.as_str())
    }

    pub fn assignee_label(&self, ids: &[Id]) -> SharedString {
        if ids.is_empty() {
            return "—".into();
        }
        ids.iter()
            .map(|id| self.person_name(id))
            .collect::<Vec<_>>()
            .join("、")
            .into()
    }

    // ---- タスク ----

    fn take(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) -> String {
        let value = input.read(cx).value().to_string();
        input.update(cx, |state, cx| state.set_value("", window, cx));
        value
    }

    fn take_area(
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        let value = input.read(cx).value().to_string();
        input.update(cx, |state, cx| state.set_value("", window, cx));
        value
    }

    /// 追加行の状態・優先度・担当・期限を、今のワークスペースの既定値に戻す。
    pub fn reset_quick(&mut self, cx: &mut Context<Self>) {
        let workspace = self.current_workspace().map(str::to_owned);
        // 既定値を読めないときは、何も入っていない状態から始める（追加のときに同じ失敗が画面に出る）
        self.quick = self
            .db
            .read(cx)
            .store
            .draft_for(workspace.as_deref(), "")
            .unwrap_or_default();
        self.quick_nav = Some(self.nav.clone());
        self.quick_touched = false;
    }

    pub fn submit_quick_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.inputs.quick_add.read(cx).value().to_string();
        if title.trim().is_empty() {
            return;
        }
        let workspace = self.current_workspace().map(str::to_owned);
        let mut draft = self.quick.clone();
        draft.title = title.trim().to_owned();
        if self
            .write(cx, |s| s.create_task_shown(workspace.as_deref(), &draft))
            .is_some()
        {
            Self::take(&self.inputs.quick_add.clone(), window, cx);
            self.reset_quick(cx);
            // 追加したものが見えるように、未完了のタブに戻す
            self.query.tab = view::Tab::Open;
            self.refilter();
            cx.notify();
        }
    }

    pub fn open_panel(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.as_deref() != Some(id) {
            self.comment_mode = CommentMode::New;
            self.pending_anchor = None;
            self.expanded_threads.clear();
        }
        self.panel = Some(id.to_owned());
        self.editing = None;
        self.reload(cx);
        if let Some(detail) = &self.detail {
            let start = detail.task.start.clone().unwrap_or_default();
            let due = detail.task.due.clone().unwrap_or_default();
            self.inputs
                .start
                .update(cx, |s, cx| s.set_value(start, window, cx));
            self.inputs
                .due
                .update(cx, |s, cx| s.set_value(due, window, cx));
        }
    }

    pub fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.panel = None;
        self.detail = None;
        self.editing = None;
        self.comment_mode = CommentMode::New;
        self.pending_anchor = None;
        cx.notify();
    }

    /// 詳細パネルのタスクを書き換える。
    pub fn edit_task(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TaskDraft)) {
        let Some(detail) = &self.detail else { return };
        let id = detail.task.id.clone();
        let mut draft = TaskDraft::from(&detail.task);
        f(&mut draft);
        self.write(cx, |s| s.update_task(&id, &draft));
    }

    /// 一覧の行のタスクを書き換える。
    pub fn edit_task_by_id(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TaskDraft),
    ) {
        self.write(cx, |s| {
            let mut draft = TaskDraft::from(&s.task(id)?);
            f(&mut draft);
            s.update_task(id, &draft)
        });
    }

    pub fn start_editing(&mut self, what: Editing, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        match what {
            Editing::Title => {
                let title = detail.task.title.clone();
                self.inputs.title.update(cx, |s, cx| {
                    s.set_value(title, window, cx);
                    s.focus(window, cx);
                });
            }
            Editing::Description => {
                let description = detail.task.description.clone();
                self.inputs.description.update(cx, |s, cx| {
                    s.set_value(description, window, cx);
                    s.focus(window, cx);
                });
            }
        }
        self.editing = Some(what);
        cx.notify();
    }

    pub fn save_title(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let title = self.inputs.title.read(cx).value().to_string();
        self.edit_task(cx, |d| d.title = title);
        if self.error.is_none() {
            self.editing = None;
        }
    }

    pub fn save_description(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let description = self.inputs.description.read(cx).value().to_string();
        self.edit_task(cx, |d| d.description = description);
        if self.error.is_none() {
            self.editing = None;
        }
    }

    pub fn save_start(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let value = self.inputs.start.read(cx).value().to_string();
        self.edit_task(cx, |d| d.start = Some(value));
    }

    pub fn save_due(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let value = self.inputs.due.read(cx).value().to_string();
        self.edit_task(cx, |d| d.due = Some(value));
    }

    /// 期限を今日から `days` 日後にする。`None` なら期限をなくす。
    pub fn set_due_in(&mut self, days: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let value = days.map(|d| view::add_days(&self.today, d));
        let text = value.clone().unwrap_or_default();
        self.inputs
            .due
            .update(cx, |s, cx| s.set_value(text, window, cx));
        self.edit_task(cx, |d| d.due = value);
    }

    pub fn clear_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs
            .start
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.edit_task(cx, |d| d.start = None);
    }

    pub fn toggle_assignee(&mut self, person_id: &str, cx: &mut Context<Self>) {
        self.edit_task(cx, |d| {
            if let Some(i) = d.assignee_ids.iter().position(|a| a == person_id) {
                d.assignee_ids.remove(i);
            } else {
                d.assignee_ids.push(person_id.to_owned());
            }
        });
    }

    /// 担当者の役を付ける・外す。最後の 1 つは外せない。
    pub fn toggle_role(
        &mut self,
        person_id: &str,
        role: malakt_core::Role,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &self.detail else { return };
        let Some(current) = detail.task.roles_of(person_id) else {
            return;
        };
        let next = if current.contains(role) {
            current.without(role)
        } else {
            current.with(role)
        };
        if next.is_empty() {
            self.error = Some("役は 1 つ以上必要です。別の役を付けてから外してください。".into());
            cx.notify();
            return;
        }
        let id = detail.task.id.clone();
        self.write(cx, |s| s.set_assignee_roles(&id, person_id, next));
    }

    /// 担当者の「自分の分」を完了にする・取り消す。
    pub fn toggle_assignee_done(&mut self, person_id: &str, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        let id = detail.task.id.clone();
        let done = !detail.task.is_done_by(person_id);
        self.write(cx, |s| s.set_assignee_done(&id, person_id, done));
    }

    pub fn submit_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = Self::take(&self.inputs.category.clone(), window, cx);
        self.edit_task(cx, |d| d.categories.push(value));
    }

    pub fn remove_category(&mut self, category: &str, cx: &mut Context<Self>) {
        self.edit_task(cx, |d| d.categories.retain(|c| c != category));
    }

    pub fn submit_subtask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        let title = self.inputs.subtask.read(cx).value().to_string();
        if title.trim().is_empty() {
            return;
        }
        let workspace = detail.task.workspace_id.clone();
        let draft = TaskDraft {
            title,
            parent_id: Some(detail.task.id.clone()),
            ..Default::default()
        };
        if self
            .write(cx, |s| s.create_task_shown(workspace.as_deref(), &draft))
            .is_some()
        {
            Self::take(&self.inputs.subtask.clone(), window, cx);
        }
    }

    pub fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_comment_as(false, window, cx);
    }

    /// コメント欄の内容を送る。返信・書き直しの途中ならそれを、
    /// 選択範囲があるか `review` ならレビューコメントを書く。
    pub fn submit_comment_as(&mut self, review: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.panel.clone() else { return };
        let body = self.inputs.comment.read(cx).value().to_string();
        if body.trim().is_empty() {
            return;
        }
        let author = self.self_id.clone();
        let mode = self.comment_mode.clone();
        let anchor = self.pending_anchor.clone();
        let done = self.write(cx, |s| match mode {
            CommentMode::Edit(comment_id) => s.update_comment(&comment_id, &body),
            CommentMode::Reply(root) => s.add_comment_with(
                &id,
                author.as_deref(),
                &NewComment {
                    body,
                    reply_to: Some(root),
                    ..Default::default()
                },
            ),
            CommentMode::New => s.add_comment_with(
                &id,
                author.as_deref(),
                &NewComment {
                    body,
                    review,
                    anchor,
                    ..Default::default()
                },
            ),
        });
        if done.is_some() {
            Self::take_area(&self.inputs.comment.clone(), window, cx);
            self.comment_mode = CommentMode::New;
            self.pending_anchor = None;
        }
    }

    /// 説明で選んでいる文字列に、次のコメントを付ける。
    pub fn quote_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        let source = detail.task.description.as_str();
        let view = self.description_view.read(cx);
        let range = view
            .selected_source_range()
            .filter(|r| r.start < r.end && r.end <= source.len())
            .filter(|r| source.is_char_boundary(r.start) && source.is_char_boundary(r.end));
        let Some(range) = range else {
            self.error = Some("説明の文章を選択してください。".into());
            cx.notify();
            return;
        };
        self.set_pending_anchor(anchor_at(source, range), window, cx);
    }

    pub fn set_pending_anchor(
        &mut self,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_anchor = Some(anchor);
        self.comment_mode = CommentMode::New;
        self.error = None;
        self.inputs.comment.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    pub fn start_reply(&mut self, root: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_mode = CommentMode::Reply(root.to_owned());
        self.pending_anchor = None;
        self.inputs.comment.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    pub fn start_edit_comment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(body) = self
            .detail
            .as_ref()
            .and_then(|d| d.comments.iter().find(|c| c.id == id))
            .map(|c| c.body.clone())
        else {
            return;
        };
        self.comment_mode = CommentMode::Edit(id.to_owned());
        self.pending_anchor = None;
        self.inputs.comment.update(cx, |s, cx| {
            s.set_value(body, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// 返信・書き直し・選択範囲の指定をやめる。
    pub fn cancel_comment_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.comment_mode, CommentMode::Edit(_)) {
            Self::take_area(&self.inputs.comment.clone(), window, cx);
        }
        self.comment_mode = CommentMode::New;
        self.pending_anchor = None;
        cx.notify();
    }

    pub fn set_thread_resolved(&mut self, id: &str, resolved: bool, cx: &mut Context<Self>) {
        self.expanded_threads.remove(id);
        self.write(cx, |s| s.set_comment_resolved(id, resolved));
    }

    pub fn toggle_thread(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded_threads.remove(id) {
            self.expanded_threads.insert(id.to_owned());
        }
        cx.notify();
    }

    /// 書きかけの `@` に合う人。
    pub fn mention_candidates(&self, cx: &App) -> Vec<Person> {
        let text = self.inputs.comment.read(cx).value();
        match mention::pending(&text) {
            Some(query) => mention::candidates(query, &self.people)
                .into_iter()
                .take(8)
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn complete_mention(
        &mut self,
        person: &Person,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.inputs.comment.read(cx).value().to_string();
        let next = mention::complete(&text, person);
        self.inputs.comment.update(cx, |s, cx| {
            s.set_value(next, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// 状態を変える前に、コメントを添えるダイアログを出す。
    pub fn ask_status(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.status)
            .or_else(|| {
                self.detail
                    .as_ref()
                    .filter(|d| d.task.id == id)
                    .map(|d| d.task.status)
            })
            .unwrap_or(Status::NeedsAction);
        // よく使う変更を最初から選んでおく（未完了なら完了、完了なら未着手）
        let next = if current.is_open() {
            Status::Completed
        } else {
            Status::NeedsAction
        };
        self.inputs
            .status_comment
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.modal = Modal::Status(id.to_owned(), next);
        cx.notify();
    }

    pub fn submit_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::Status(id, status) = self.modal.clone() else {
            return;
        };
        let body = self.inputs.status_comment.read(cx).value().to_string();
        let author = self.self_id.clone();
        let done = self.write(cx, |s| {
            if body.trim().is_empty() {
                s.set_task_status(&id, status)?;
            } else {
                // 状態つきのコメント。完了なら完了報告として残る
                let new = NewComment {
                    body,
                    status: Some(status),
                    ..Default::default()
                };
                s.add_comment_with(&id, author.as_deref(), &new)?;
            }
            Ok(())
        });
        if done.is_some() {
            Self::take_area(&self.inputs.status_comment.clone(), window, cx);
            self.modal = Modal::None;
            self.notice = Some(format!("状態を「{}」にしました。", status.label()).into());
        }
    }

    pub fn confirm_delete_tasks(&mut self, cx: &mut Context<Self>) {
        let Modal::DeleteTasks(ids) = self.modal.clone() else {
            return;
        };
        let done = self.write(cx, |s| {
            for id in &ids {
                s.delete_task(id)?;
            }
            Ok(())
        });
        if done.is_some() {
            self.modal = Modal::None;
            self.selected.clear();
            self.notice = Some(format!("{} 件のタスクを削除しました。", ids.len()).into());
        }
    }

    /// 選んだタスクを、いま見ているワークスペースから外す（タスクは消さない）。
    /// ほかに出ている場所がないタスクは、そのまま残す。
    pub fn unlink_selected_here(&mut self, cx: &mut Context<Self>) {
        let Modal::DeleteTasks(ids) = self.modal.clone() else {
            return;
        };
        let Some(here) = self.current_workspace().map(str::to_owned) else {
            return;
        };
        let done = self.write(cx, |s| {
            let mut removed = 0;
            for id in &ids {
                if s.task(id)?.places().count() > 1 {
                    s.unlink_task(id, &here)?;
                    removed += 1;
                }
            }
            Ok(removed)
        });
        if let Some(removed) = done {
            self.modal = Modal::None;
            self.selected.clear();
            let kept = ids.len() - removed;
            self.notice = Some(if kept == 0 {
                format!("{removed} 件をこのワークスペースから外しました。").into()
            } else {
                format!(
                    "{removed} 件をこのワークスペースから外しました。同期先のない {kept} 件はそのままです。"
                )
                .into()
            });
        }
    }

    /// 開いているタスクを、ほかのワークスペースにも表示する。
    pub fn link_task_to(&mut self, workspace: &str, cx: &mut Context<Self>) {
        let Modal::LinkTask(id) = self.modal.clone() else {
            return;
        };
        if self.write(cx, |s| s.link_task(&id, workspace)).is_some() {
            self.modal = Modal::None;
            let name = self.workspace_name(Some(workspace));
            self.notice = Some(format!("「{name}」と同期しました。").into());
        }
    }

    pub fn unlink_task_from(&mut self, id: &str, workspace: &str, cx: &mut Context<Self>) {
        self.write(cx, |s| s.unlink_task(id, workspace));
    }

    pub fn move_tasks_to(&mut self, workspace: Option<Id>, cx: &mut Context<Self>) {
        let Modal::MoveTasks(ids) = self.modal.clone() else {
            return;
        };
        let done = self.write(cx, |s| {
            for id in &ids {
                s.move_task(id, workspace.as_deref())?;
            }
            Ok(())
        });
        if done.is_some() {
            self.modal = Modal::None;
            self.selected.clear();
            let name = self.workspace_name(workspace.as_deref());
            self.notice = Some(format!("{} 件を「{name}」に移しました。", ids.len()).into());
        }
    }

    // ---- 選択と一括操作 ----

    pub fn toggle_selected(&mut self, id: &str, cx: &mut Context<Self>) {
        self.click_select(id, false, cx);
    }

    /// 行のチェックボックスを押した。Shift つきなら、前に押した行からの範囲をまとめて変える。
    pub fn click_select(&mut self, id: &str, shift: bool, cx: &mut Context<Self>) {
        let visible: Vec<&Id> = self.visible.iter().map(|&i| &self.tasks[i].id).collect();
        if shift {
            bulk::toggle_range(
                &mut self.selected,
                &visible,
                self.select_anchor.as_deref(),
                id,
            );
        } else {
            bulk::toggle(&mut self.selected, id);
        }
        self.select_anchor = Some(id.to_owned());
        cx.notify();
    }

    pub fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        let visible: Vec<&Id> = self.visible.iter().map(|&i| &self.tasks[i].id).collect();
        bulk::toggle_all(&mut self.selected, &visible);
        cx.notify();
    }

    pub fn selected_ids(&self) -> Vec<Id> {
        self.visible
            .iter()
            .map(|&i| &self.tasks[i].id)
            .filter(|id| self.selected.contains(*id))
            .cloned()
            .collect()
    }

    /// 選んだタスクすべてに同じ変更を当てる。
    pub fn bulk_apply(&mut self, op: &BulkOp, cx: &mut Context<Self>) -> bool {
        let ids = self.selected_ids();
        match self.write(cx, |s| s.bulk_apply(&ids, op)) {
            Some(changed) => {
                self.notice = Some(if changed == ids.len() {
                    format!("{changed} 件のタスクを変更しました。").into()
                } else {
                    format!(
                        "{changed} 件のタスクを変更しました（{} 件は、もともと同じでした）。",
                        ids.len() - changed
                    )
                    .into()
                });
                true
            }
            None => false,
        }
    }

    /// まとめて変えるダイアログを開く。入力は毎回空から始める。
    pub fn open_bulk(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        Self::take(&self.inputs.bulk_due.clone(), window, cx);
        Self::take(&self.inputs.bulk_categories.clone(), window, cx);
        self.bulk_mode = ListMode::Add;
        self.bulk_people.clear();
        self.bulk_category_add = true;
        self.error = None;
        self.modal = modal;
        cx.notify();
    }

    /// 期限をまとめて決める。`clear` なら期限をなくす。
    pub fn submit_bulk_due(&mut self, clear: bool, cx: &mut Context<Self>) {
        let due = if clear {
            None
        } else {
            let value = self.inputs.bulk_due.read(cx).value().trim().to_owned();
            if value.is_empty() {
                self.error = Some("期限を入力してください。".into());
                cx.notify();
                return;
            }
            Some(value)
        };
        if self.bulk_apply(&BulkOp::Due(due), cx) {
            self.modal = Modal::None;
        }
    }

    pub fn toggle_bulk_person(&mut self, person: &str, cx: &mut Context<Self>) {
        toggle(&mut self.bulk_people, person);
        cx.notify();
    }

    pub fn submit_bulk_assignees(&mut self, cx: &mut Context<Self>) {
        if self.bulk_people.is_empty() && self.bulk_mode != ListMode::Set {
            self.error = Some("担当者を選んでください。".into());
            cx.notify();
            return;
        }
        let op = BulkOp::Assignees {
            mode: self.bulk_mode,
            ids: self.bulk_people.clone(),
        };
        if self.bulk_apply(&op, cx) {
            self.modal = Modal::None;
        }
    }

    pub fn submit_bulk_categories(&mut self, cx: &mut Context<Self>) {
        let values = split_list(self.inputs.bulk_categories.read(cx).value().as_ref());
        if values.is_empty() {
            self.error = Some("カテゴリを入力してください。".into());
            cx.notify();
            return;
        }
        let op = BulkOp::Categories {
            add: self.bulk_category_add,
            values,
        };
        if self.bulk_apply(&op, cx) {
            self.modal = Modal::None;
        }
    }

    // ---- 新しいタスク（項目を入れて作る） ----

    /// 新しいタスクのダイアログを開く。いまのワークスペースの既定値と、
    /// 上の入力欄に書きかけたタイトルを入れておく。
    pub fn open_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.current_workspace().map(str::to_owned);
        let title = Self::take(&self.inputs.quick_add.clone(), window, cx);
        let draft = match self
            .db
            .read(cx)
            .store
            .draft_for(workspace.as_deref(), title.trim())
        {
            Ok(draft) => draft,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |s, cx| s.set_value(value, window, cx));
        };
        let inputs = &self.inputs;
        set(&inputs.nt_title.clone(), draft.title.clone(), window, cx);
        set(
            &inputs.nt_start.clone(),
            draft.start.clone().unwrap_or_default(),
            window,
            cx,
        );
        set(
            &inputs.nt_due.clone(),
            draft.due.clone().unwrap_or_default(),
            window,
            cx,
        );
        set(
            &inputs.nt_categories.clone(),
            draft.categories.join("、"),
            window,
            cx,
        );
        let description = draft.description.clone();
        self.inputs
            .nt_description
            .update(cx, |s, cx| s.set_value(description, window, cx));
        self.inputs.nt_title.update(cx, |s, cx| s.focus(window, cx));
        self.new_task = draft;
        self.error = None;
        self.modal = Modal::NewTask;
        cx.notify();
    }

    pub fn toggle_new_task_assignee(&mut self, person: &str, cx: &mut Context<Self>) {
        toggle(&mut self.new_task.assignee_ids, person);
        cx.notify();
    }

    pub fn set_new_task_priority(&mut self, priority: u8, cx: &mut Context<Self>) {
        self.new_task.priority = priority;
        cx.notify();
    }

    pub fn submit_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Modal::NewTask {
            return;
        }
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let some = |value: String| (!value.is_empty()).then_some(value);
        let mut draft = self.new_task.clone();
        draft.title = read(&self.inputs.nt_title);
        draft.start = some(read(&self.inputs.nt_start));
        draft.due = some(read(&self.inputs.nt_due));
        draft.categories = split_list(self.inputs.nt_categories.read(cx).value().as_ref());
        draft.description = self.inputs.nt_description.read(cx).value().to_string();
        let workspace = self.current_workspace().map(str::to_owned);
        let created = self.write(cx, |s| s.create_task_shown(workspace.as_deref(), &draft));
        if let Some(task) = created {
            self.modal = Modal::None;
            self.query.tab = view::Tab::Open;
            self.open_panel(&task.id, window, cx);
        }
    }

    // ---- ワークスペースと担当者 ----

    pub fn ask_workspace_name(
        &mut self,
        parent: Option<Id>,
        rename: Option<Id>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = rename
            .as_ref()
            .and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
            .map(|w| w.name.clone())
            .unwrap_or_default();
        self.inputs.name.update(cx, |s, cx| {
            s.set_value(current, window, cx);
            s.focus(window, cx);
        });
        self.modal = Modal::WorkspaceName { parent, rename };
        cx.notify();
    }

    /// 名前の入力欄で Enter。開いているダイアログに応じて、作成・名前の変更・担当者の追加をする。
    pub fn submit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.inputs.name.read(cx).value().to_string();
        match self.modal.clone() {
            Modal::WorkspaceName { parent, rename } => {
                let done = match rename {
                    Some(id) => self
                        .write(cx, |s| s.rename_workspace(&id, &name))
                        .map(|_| None),
                    None => self
                        .write(cx, |s| s.create_workspace(parent.as_deref(), &name))
                        .map(Some),
                };
                if let Some(created) = done {
                    Self::take(&self.inputs.name.clone(), window, cx);
                    self.modal = Modal::None;
                    if let Some(parent) = &parent {
                        self.collapsed.remove(parent);
                    }
                    if let Some(workspace) = created {
                        self.go(Nav::Workspace(workspace.id), cx);
                    }
                }
            }
            Modal::People if self.write(cx, |s| s.create_person(&name, false)).is_some() => {
                Self::take(&self.inputs.name.clone(), window, cx);
            }
            _ => {}
        }
    }

    pub fn confirm_delete_workspace(&mut self, cx: &mut Context<Self>) {
        let Modal::DeleteWorkspace(id) = self.modal.clone() else {
            return;
        };
        if self.write(cx, |s| s.delete_workspace(&id)).is_some() {
            self.modal = Modal::None;
        }
    }

    /// ディスクの使用量。
    pub fn usage(&self, cx: &App) -> malakt_core::Usage {
        self.db.read(cx).store.usage().unwrap_or_default()
    }

    pub fn open_people(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs.name.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        self.modal = Modal::People;
        cx.notify();
    }

    // ---- ロック ----

    pub fn lock_enabled(&self, cx: &App) -> bool {
        lock::is_enabled(&self.db.read(cx).store).unwrap_or(false)
    }

    pub fn submit_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // ロックの設定のダイアログでも同じ欄を使うので、ロック画面のときだけ解除する
        if self.unlocked(cx) {
            return;
        }
        let passphrase = Self::take(&self.inputs.passphrase.clone(), window, cx);
        let ok = lock::unlock(
            &self.db.read(cx).store,
            &lock::NoDeviceAuth,
            Some(&passphrase),
        );
        match ok {
            Ok(true) => {
                self.error = None;
                self.db.update(cx, |db, cx| {
                    db.unlocked = true;
                    cx.notify();
                });
            }
            Ok(false) => self.error = Some("パスフレーズが違います".into()),
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    pub fn open_lock_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in [&self.inputs.passphrase, &self.inputs.new_passphrase] {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.modal = Modal::Lock;
        cx.notify();
    }

    /// ロックを掛ける（すでに有効なら、パスフレーズを変える）。
    pub fn save_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = self.lock_enabled(cx);
        let current = Self::take(&self.inputs.passphrase.clone(), window, cx);
        let new = Self::take(&self.inputs.new_passphrase.clone(), window, cx);
        let current = enabled.then_some(current);
        if self
            .write(cx, |s| lock::set_passphrase(s, current.as_deref(), &new))
            .is_some()
        {
            self.modal = Modal::None;
            self.notice = Some("ロックを有効にしました。".into());
        }
    }

    pub fn disable_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = Self::take(&self.inputs.passphrase.clone(), window, cx);
        if self.write(cx, |s| lock::disable(s, &current)).is_some() {
            self.modal = Modal::None;
            self.notice = Some("ロックを無効にしました。".into());
        }
    }

    // ---- 完了チケット ----

    pub fn open_ticket_dialog(&mut self, task_id: &str, cx: &mut Context<Self>) {
        self.ticket_person = self.self_id.clone();
        self.issued_ticket = None;
        self.error = None;
        self.modal = Modal::Ticket(task_id.to_owned());
        cx.notify();
    }

    pub fn issue_ticket(&mut self, cx: &mut Context<Self>) {
        let Modal::Ticket(task_id) = self.modal.clone() else {
            return;
        };
        let person = self.ticket_person.clone();
        self.issued_ticket = self.write(cx, |s| s.issue_ticket(&task_id, person.as_deref()));
    }

    /// 発行したチケットをクリップボードに写す。
    pub fn copy_ticket(&mut self, cx: &mut Context<Self>) {
        if let Some(ticket) = &self.issued_ticket {
            cx.write_to_clipboard(ClipboardItem::new_string(ticket.secret.clone()));
            self.notice = Some("完了チケットをコピーしました。".into());
            cx.notify();
        }
    }

    pub fn revoke_tickets(&mut self, cx: &mut Context<Self>) {
        let Modal::Ticket(task_id) = self.modal.clone() else {
            return;
        };
        if let Some(count) = self.write(cx, |s| s.revoke_tickets(&task_id)) {
            self.issued_ticket = None;
            self.notice = Some(format!("{count} 枚の完了チケットを取り消しました。").into());
        }
    }

    /// そのタスクの、引き換えていない有効なチケットの数。
    pub fn active_tickets(&self, task_id: &str, cx: &App) -> usize {
        self.db
            .read(cx)
            .store
            .active_ticket_count(task_id)
            .unwrap_or(0)
    }

    // ---- 通知 ----

    /// 期限が今日のタスクの通知を作り、まだ出していない通知を OS の通知に出す。
    /// ロック中はタイトルを外に出さないよう、解除してから出す。
    pub fn deliver_notices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.unlocked(cx) {
            return;
        }
        let today = view::today();
        let (pending, changed) = self.db.update(cx, |db, cx| {
            let (pending, made) = db.take_notices(&today);
            // 通知が増えたときだけ、ほかのウィンドウにも読み直させる
            if made {
                cx.notify();
            }
            (pending, made)
        });
        if changed || !pending.is_empty() {
            self.reload(cx);
        }
        for notice in pending {
            let view = cx.entity().downgrade();
            let id = notice.id.clone();
            let mut note = gpui_kit::component::notification::Notification::new()
                .id1::<Notice>(SharedString::from(notice.id.clone()))
                .title(notice.headline())
                .system()
                .on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.open_notice(&id, window, cx));
                });
            if !notice.excerpt.is_empty() {
                note = note.message(notice.excerpt.clone());
            }
            window.push_notification(note, cx);
        }
    }

    /// 通知を開く。既読にして、そのタスクの詳細を出す。
    pub fn open_notice(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self
            .notices
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.task_id.clone())
        else {
            return;
        };
        self.write(cx, |s| s.mark_notice_read(id));
        self.modal = Modal::None;
        self.query.tab = if self
            .db
            .read(cx)
            .store
            .task(&task_id)
            .is_ok_and(|t| t.status.is_open())
        {
            view::Tab::Open
        } else {
            view::Tab::Closed
        };
        self.go(Nav::All, cx);
        self.open_panel(&task_id, window, cx);
    }

    pub fn mark_all_notices_read(&mut self, cx: &mut Context<Self>) {
        self.write(cx, |s| s.mark_all_notices_read());
    }

    // ---- ワークスペースの設定 ----

    pub fn open_workspace_settings(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = &self.db.read(cx).store;
        let loaded = store
            .workspace(id)
            .and_then(|w| Ok((w, store.task_defaults(id)?)));
        let (workspace, defaults) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let (days, date) = match &defaults.due {
            DueRule::Relative { days } => (days.to_string(), String::new()),
            DueRule::Fixed { date } => (String::new(), date.clone()),
            DueRule::None => (String::new(), String::new()),
        };
        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |s, cx| s.set_value(value, window, cx));
        };
        let inputs = &self.inputs;
        set(
            &inputs.ws_date.clone(),
            workspace.date.unwrap_or_default(),
            window,
            cx,
        );
        set(&inputs.def_days.clone(), days, window, cx);
        set(&inputs.def_date.clone(), date, window, cx);
        set(
            &inputs.def_time.clone(),
            defaults.due_time.clone(),
            window,
            cx,
        );
        set(
            &inputs.def_categories.clone(),
            defaults.categories.join("、"),
            window,
            cx,
        );
        let description = defaults.description.clone();
        self.inputs
            .def_description
            .update(cx, |s, cx| s.set_value(description, window, cx));
        self.defaults_draft = defaults;
        self.modal = Modal::WorkspaceSettings(id.to_owned());
        self.error = None;
        cx.notify();
    }

    pub fn toggle_default_assignee(&mut self, person: &str, cx: &mut Context<Self>) {
        toggle(&mut self.defaults_draft.assignee_ids, person);
        cx.notify();
    }

    pub fn toggle_default_link(&mut self, workspace: &str, cx: &mut Context<Self>) {
        toggle(&mut self.defaults_draft.link_workspace_ids, workspace);
        cx.notify();
    }

    pub fn set_default_priority(&mut self, priority: Option<u8>, cx: &mut Context<Self>) {
        self.defaults_draft.priority = priority;
        cx.notify();
    }

    /// 期限の決め方を切り替える。日数や日付は、保存するときに入力欄から読む。
    pub fn set_default_due(&mut self, due: DueRule, cx: &mut Context<Self>) {
        self.defaults_draft.due = due;
        cx.notify();
    }

    pub fn save_workspace_settings(&mut self, cx: &mut Context<Self>) {
        let Modal::WorkspaceSettings(id) = self.modal.clone() else {
            return;
        };
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let mut defaults = self.defaults_draft.clone();
        defaults.workspace_id = id.clone();
        defaults.due = match defaults.due {
            DueRule::None => DueRule::None,
            DueRule::Fixed { .. } => DueRule::Fixed {
                date: read(&self.inputs.def_date),
            },
            DueRule::Relative { .. } => {
                let text = read(&self.inputs.def_days);
                // 全角の数字やマイナスで入れても読む
                let ascii: String = text
                    .chars()
                    .map(|c| match c {
                        '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
                        '－' | 'ー' | '−' => '-',
                        '＋' => '+',
                        c => c,
                    })
                    .collect();
                match ascii.parse::<i32>() {
                    Ok(days) => DueRule::Relative { days },
                    Err(_) if ascii.is_empty() => DueRule::Relative { days: 0 },
                    Err(_) => {
                        self.error =
                            Some("基準日からの日数は、-7 のような整数で入力してください。".into());
                        cx.notify();
                        return;
                    }
                }
            }
        };
        defaults.due_time = read(&self.inputs.def_time);
        defaults.categories = split_list(&read(&self.inputs.def_categories));
        defaults.description = self.inputs.def_description.read(cx).value().to_string();
        let date = read(&self.inputs.ws_date);
        let saved = self.write(cx, |s| {
            // 先に両方を確かめて、片方だけ保存されることがないようにする
            malakt_core::defaults::clean(&defaults)?;
            s.set_workspace_date(&id, Some(&date))?;
            s.set_task_defaults(&defaults)
        });
        if saved.is_some() {
            self.modal = Modal::None;
            self.notice = Some("ワークスペースの設定を保存しました。".into());
        }
    }

    // ---- 添付 ----

    /// ファイルを選んで、開いているタスクに添付する。
    pub fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("添付".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.attach_paths(&paths, cx));
        })
        .detach();
    }

    pub fn attach_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        let Some(task_id) = self.detail.as_ref().map(|d| d.task.id.clone()) else {
            return;
        };
        let author = self.self_id.clone();
        let mut failed = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // 大きすぎるファイルは、読む前に断る
            let result = std::fs::metadata(path)
                .map_err(malakt_core::Error::from)
                .and_then(|meta| {
                    if meta.len() > malakt_core::MAX_ATTACHMENT_BYTES {
                        return Err(malakt_core::Error::Invalid(format!(
                            "添付できるのは 1 ファイル {} までです",
                            view::format_bytes(malakt_core::MAX_ATTACHMENT_BYTES)
                        )));
                    }
                    Ok(std::fs::read(path)?)
                })
                .and_then(|bytes| {
                    self.db.update(cx, |db, cx| {
                        cx.notify();
                        db.store
                            .add_attachment(&task_id, author.as_deref(), &name, &bytes)
                    })
                });
            if let Err(error) = result {
                failed.push(format!("{name}: {error}"));
            }
        }
        self.error = (!failed.is_empty()).then(|| failed.join("\n").into());
        cx.notify();
    }

    /// 添付を一時ファイルに書き出す。外のアプリで開くためのもので、
    /// 起動時・ロック時・終了時にまとめて消す。
    pub fn opened_copy(&self, id: &str, cx: &App) -> malakt_core::Result<std::path::PathBuf> {
        let db = self.db.read(cx);
        let store = &db.store;
        let attachment = store.attachment(id)?;
        let dir = db.opened_dir.join(&attachment.id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(&attachment.name);
        std::fs::write(&path, store.attachment_data(id)?)?;
        Ok(path)
    }

    /// 添付を OS の既定のアプリで開く。
    pub fn open_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.opened_copy(id, cx) {
            Ok(path) => {
                self.error = None;
                cx.open_with_system(&path);
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    /// 添付を、選んだ場所に保存する。
    pub fn save_attachment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(attachment) = self.db.read(cx).store.attachment(id) else {
            return;
        };
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| ".".into());
        let chosen = cx.prompt_for_new_path(&dir, Some(&attachment.name));
        let id = id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.save_attachment_to(&id, &path, cx));
        })
        .detach();
    }

    pub fn save_attachment_to(&mut self, id: &str, path: &std::path::Path, cx: &mut Context<Self>) {
        let result = self
            .db
            .read(cx)
            .store
            .attachment_data(id)
            .and_then(|bytes| Ok(std::fs::write(path, bytes)?));
        match result {
            Ok(()) => {
                self.error = None;
                self.notice = Some(format!("{} に保存しました。", path.display()).into());
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    // ---- エクスポート / インポート ----

    pub fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| ".".into());
        let name = format!("malakt-mahoz-{}.zip", self.today);
        let chosen = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.export_to(&path, cx));
        })
        .detach();
    }

    pub fn export_to(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        match archive::export_to_path(&self.db.read(cx).store, path) {
            Ok(manifest) => {
                self.error = None;
                self.notice = Some(
                    format!(
                        "{} 件のタスクを {} に書き出しました。",
                        manifest.counts.tasks,
                        path.display()
                    )
                    .into(),
                );
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    pub fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("インポート".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.import_from(&path, cx));
        })
        .detach();
    }

    pub fn import_from(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if let Some(manifest) = self.write(cx, |s| archive::import_from_path(s, path)) {
            self.notice =
                Some(format!("{} 件のタスクを取り込みました。", manifest.counts.tasks).into());
        }
    }

    /// Esc。いちばん手前のものから閉じる。
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.modal != Modal::None {
            self.modal = Modal::None;
            self.issued_ticket = None;
        } else if self.editing.is_some() {
            self.editing = None;
        } else if self.panel.is_some() {
            self.close_panel(cx);
        } else if !self.selected.is_empty() {
            self.selected.clear();
        }
        self.error = None;
        cx.notify();
    }
}

/// 通知の一覧に出す数。
const NOTICES_SHOWN: usize = 100;

/// 「、」や「,」で区切った入力を、空でない項目の並びにする。
fn split_list(text: &str) -> Vec<String> {
    text.split(['、', ',', '，'])
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

/// あれば外し、なければ足す。
fn toggle(list: &mut Vec<Id>, id: &str) {
    match list.iter().position(|x| x == id) {
        Some(ix) => {
            list.remove(ix);
        }
        None => list.push(id.to_owned()),
    }
}

/// 選んだ範囲と、その前後の文脈（探し直すための手がかり）。
pub fn anchor_at(source: &str, range: std::ops::Range<usize>) -> Anchor {
    const CONTEXT: usize = 32;
    let before = &source[..range.start];
    let after = &source[range.end..];
    let prefix_start = before
        .char_indices()
        .rev()
        .nth(CONTEXT - 1)
        .map_or(0, |(i, _)| i);
    let suffix_end = after
        .char_indices()
        .nth(CONTEXT)
        .map_or(after.len(), |(i, _)| i);
    Anchor {
        exact: source[range].to_owned(),
        prefix: before[prefix_start..].to_owned(),
        suffix: after[..suffix_end].to_owned(),
    }
}

/// 親ごとに `position` 順で、折りたたまれた下は飛ばして並べる。
pub fn build_tree(workspaces: &[Workspace], collapsed: &HashSet<Id>) -> Vec<TreeRow> {
    let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for (i, w) in workspaces.iter().enumerate() {
        children.entry(w.parent_id.as_deref()).or_default().push(i);
    }
    for list in children.values_mut() {
        list.sort_by_key(|&i| (workspaces[i].position, workspaces[i].id.as_str()));
    }
    let mut rows = Vec::with_capacity(workspaces.len());
    // 深い入れ子でもスタックを使い切らないよう、再帰しない
    let mut stack: Vec<(usize, usize)> = children
        .get(&None)
        .map(|roots| roots.iter().rev().map(|&i| (i, 0)).collect())
        .unwrap_or_default();
    while let Some((index, depth)) = stack.pop() {
        let id = workspaces[index].id.as_str();
        let kids = children.get(&Some(id));
        rows.push(TreeRow {
            index,
            depth,
            has_children: kids.is_some(),
        });
        if let Some(kids) = kids {
            if !collapsed.contains(id) {
                stack.extend(kids.iter().rev().map(|&i| (i, depth + 1)));
            }
        }
    }
    rows
}

/// アイコンを選ばなかったワークスペースに出すアイコン。
pub const DEFAULT_WORKSPACE_ICON: &str = "folder";

macro_rules! bundled_icons {
    (workspace: [$($name:literal),* $(,)?], other: [$($other:literal),* $(,)?] $(,)?) => {
        /// ワークスペースに付けられるアイコン。名前は同梱の線画アイコンのファイル名。
        pub const WORKSPACE_ICONS: [&str; 48] = [$($name),*];

        /// 同梱しているアイコンの SVG。
        pub fn icon_svg(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../assets/icons/", $name, ".svg"))),)*
                $($other => Some(include_bytes!(concat!("../assets/icons/", $other, ".svg"))),)*
                _ => None,
            }
        }
    };
}

bundled_icons! {
    workspace: [
        "folder",
        "briefcase",
        "house",
        "book",
        "bookmark",
        "calendar",
        "clipboard",
        "list-todo",
        "circle-check",
        "flag",
        "star",
        "heart",
        "target",
        "rocket",
        "lightbulb",
        "sparkles",
        "zap",
        "feather",
        "pen",
        "palette",
        "camera",
        "music",
        "mic",
        "gamepad-2",
        "code",
        "terminal",
        "bug",
        "wrench",
        "hammer",
        "package",
        "box",
        "archive",
        "inbox",
        "mail",
        "bell",
        "users",
        "globe",
        "plane",
        "truck",
        "coffee",
        "gift",
        "leaf",
        "sun",
        "moon",
        "pipette",
        "layers",
        "graduation-cap",
        "shopping-cart",
    ],
    other: [
        "settings",
        "trash",
        "ellipsis",
        "plus",
        "download",
        "upload",
        "menu",
        "circle-question-mark",
        "x",
        "monitor",
        "user",
        "circle-dot",
        "search",
        "calendar-days",
        "sliders-horizontal",
        "arrow-right",
        "arrow-left",
        "maximize-2",
        "rotate-ccw-clock",
        "circle-x",
        "paperclip",
        "chevron-down",
        "chevron-right",
        "check-check",
        "signal-high",
        "signal-medium",
        "signal-low",
        "circle",
        "link",
    ],
}

/// アイコンのファイルの場所。[`Assets`] が読む。
pub fn icon_path(icon: &str) -> String {
    format!("{ICON_DIR}{icon}.svg")
}

/// ワークスペースのアイコンのファイルの場所。選んでいなければ既定のアイコン。
pub fn workspace_icon_path(icon: Option<&str>) -> String {
    icon_path(icon.unwrap_or(DEFAULT_WORKSPACE_ICON))
}

const ICON_DIR: &str = "app-icons/";

/// アプリが使う絵の置き場。このアプリのアイコンはここから、ほかは UI 部品の同梱分から読む。
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        let own = path
            .strip_prefix(ICON_DIR)
            .and_then(|file| file.strip_suffix(".svg"));
        match own {
            // 取り込んだデータに知らない名前があっても、既定のアイコンで出す
            Some(name) => Ok(icon_svg(name)
                .or_else(|| icon_svg(DEFAULT_WORKSPACE_ICON))
                .map(Into::into)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

#[cfg(test)]
mod tests {
    // `super::*` は gpui の `test` マクロまで持ち込むので、使うものだけを書く
    use super::{build_tree, HashSet, Id, TreeRow, Workspace};

    #[test]
    fn every_workspace_icon_is_bundled() {
        use gpui_kit::AssetSource;
        let load = |icon| {
            let path = super::workspace_icon_path(icon);
            super::Assets.load(&path).unwrap().unwrap()
        };
        let folder = load(None);
        for icon in super::WORKSPACE_ICONS {
            assert!(load(Some(icon)).starts_with(b"<svg"), "{icon}");
        }
        assert_ne!(load(Some("rocket")), folder);
        // 知らない名前は既定のアイコンになる
        assert_eq!(load(Some("no-such-icon")), folder);
        // UI 部品のアイコンも読める
        assert!(super::Assets.load("icons/check.svg").unwrap().is_some());
    }

    fn ws(id: &str, parent: Option<&str>, position: i64) -> Workspace {
        Workspace {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            name: id.into(),
            date: None,
            icon: None,
            color: None,
            position,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn shape(rows: &[TreeRow], all: &[Workspace]) -> Vec<(String, usize, bool)> {
        rows.iter()
            .map(|r| (all[r.index].id.clone(), r.depth, r.has_children))
            .collect()
    }

    #[test]
    fn tree_is_ordered_by_position_and_nested() {
        let all = [
            ws("b", None, 1),
            ws("a", None, 0),
            ws("a2", Some("a"), 1),
            ws("a1", Some("a"), 0),
            ws("a1x", Some("a1"), 0),
        ];
        let rows = build_tree(&all, &HashSet::new());
        let expect = [
            ("a", 0, true),
            ("a1", 1, true),
            ("a1x", 2, false),
            ("a2", 1, false),
            ("b", 0, false),
        ];
        assert_eq!(
            shape(&rows, &all),
            expect.map(|(id, depth, kids)| (id.to_string(), depth, kids))
        );
    }

    #[test]
    fn collapsed_children_are_hidden_but_marked() {
        let all = [ws("a", None, 0), ws("a1", Some("a"), 0), ws("b", None, 1)];
        let collapsed: HashSet<Id> = ["a".to_string()].into();
        let rows = build_tree(&all, &collapsed);
        assert_eq!(
            shape(&rows, &all),
            [("a".to_string(), 0, true), ("b".to_string(), 0, false)]
        );
        assert!(build_tree(&[], &collapsed).is_empty());
    }
}
