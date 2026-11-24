//! 1 枚のウィンドウの状態と操作。描画は `render` にある。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::*;
use malakt_core::view::{self, ListQuery};
use malakt_core::{
    archive, lock, Change, Comment, Id, Person, Scope, Status, Store, Task as Todo, TaskDraft,
    TaskFilter, Workspace,
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
}

/// 詳細パネルで編集中の項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editing {
    Title,
    Description,
}

/// 詳細パネルに出すタスクと、その周りのデータ。
pub struct Detail {
    pub task: Todo,
    pub comments: Vec<Comment>,
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
    pub panel: Option<Id>,
    pub detail: Option<Detail>,
    pub editing: Option<Editing>,
    pub modal: Modal,
    pub error: Option<SharedString>,
    pub notice: Option<SharedString>,
    pub sidebar_open: bool,
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
            quick_add: input(window, cx, "タスクを追加して Enter"),
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
            subtask: input(window, cx, "サブタスクを追加して Enter"),
            start: input(window, cx, "YYYY-MM-DD"),
            due: input(window, cx, "YYYY-MM-DD"),
            category: input(window, cx, "カテゴリを追加して Enter"),
            description: area(window, cx, "説明"),
            comment: area(window, cx, "コメントを書く"),
            status_comment: area(window, cx, "コメントを書く"),
        };

        let mut subs = vec![cx.observe(&db, |this, _, cx| this.reload(cx))];
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
            panel: None,
            detail: None,
            editing: None,
            modal: Modal::None,
            error: None,
            notice: None,
            sidebar_open: false,
            today: view::today(),
            inputs,
            _subscriptions: subs,
        };
        this.reload(cx);
        this
    }

    pub fn unlocked(&self, cx: &App) -> bool {
        self.db.read(cx).unlocked
    }

    // ---- 読み込み ----

    /// データベースから読み直す。どのウィンドウで変えても、全部のウィンドウで呼ばれる。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.today = view::today();
        let result = self.load(cx);
        if let Err(error) = result {
            self.error = Some(error.to_string().into());
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

    pub fn submit_quick_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.inputs.quick_add.read(cx).value().to_string();
        if title.trim().is_empty() {
            return;
        }
        let workspace = self.current_workspace().map(str::to_owned);
        let draft = TaskDraft {
            title,
            ..Default::default()
        };
        if self
            .write(cx, |s| s.create_task(workspace.as_deref(), &draft))
            .is_some()
        {
            Self::take(&self.inputs.quick_add.clone(), window, cx);
            // 追加したものが見えるように、未完了のタブに戻す
            self.query.tab = view::Tab::Open;
        }
    }

    pub fn open_panel(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
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
            .write(cx, |s| s.create_task(workspace.as_deref(), &draft))
            .is_some()
        {
            Self::take(&self.inputs.subtask.clone(), window, cx);
        }
    }

    pub fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.panel.clone() else { return };
        let body = self.inputs.comment.read(cx).value().to_string();
        if body.trim().is_empty() {
            return;
        }
        let author = self.self_id.clone();
        if self
            .write(cx, |s| s.add_comment(&id, author.as_deref(), &body))
            .is_some()
        {
            Self::take_area(&self.inputs.comment.clone(), window, cx);
        }
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
            s.set_task_status(&id, status)?;
            if !body.trim().is_empty() {
                s.add_comment(&id, author.as_deref(), &body)?;
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
        if !self.selected.remove(id) {
            self.selected.insert(id.to_owned());
        }
        cx.notify();
    }

    pub fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() == self.visible.len() {
            self.selected.clear();
        } else {
            self.selected = self
                .visible
                .iter()
                .map(|&i| self.tasks[i].id.clone())
                .collect();
        }
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

    pub fn bulk_edit(&mut self, cx: &mut Context<Self>, f: impl Fn(&mut TaskDraft)) {
        let ids = self.selected_ids();
        let done = self.write(cx, |s| {
            for id in &ids {
                let mut draft = TaskDraft::from(&s.task(id)?);
                f(&mut draft);
                s.update_task(id, &draft)?;
            }
            Ok(())
        });
        if done.is_some() {
            self.notice = Some(format!("{} 件のタスクを変更しました。", ids.len()).into());
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
            Modal::People => {
                // 最初の 1 人は「自分」にする
                let is_self = self.people.is_empty();
                if self
                    .write(cx, |s| s.create_person(&name, is_self))
                    .is_some()
                {
                    Self::take(&self.inputs.name.clone(), window, cx);
                }
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

#[cfg(test)]
mod tests {
    // `super::*` は gpui の `test` マクロまで持ち込むので、使うものだけを書く
    use super::{build_tree, HashSet, Id, TreeRow, Workspace};

    fn ws(id: &str, parent: Option<&str>, position: i64) -> Workspace {
        Workspace {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            name: id.into(),
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
