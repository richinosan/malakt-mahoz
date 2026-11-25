//! 1 枚のウィンドウの状態と操作。描画は `render` にある。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::WindowExt;
use gpui_kit::*;
use malakt_core::bulk::{self, BulkOp, ListMode};
use malakt_core::notice::Notice;
use malakt_core::ticket::IssuedTicket;
use malakt_core::view::{self, ListQuery};
use malakt_core::when;
use malakt_core::{
    archive, lock, mention, Anchor, Attachment, Change, Comment, DueRule, Id, NewComment, Person,
    Scope, Status, Store, Task as Todo, TaskDefaults, TaskDraft, TaskFilter, Workspace,
};

use crate::ai::{self, Session, ShellPlace};
use crate::ai_view::{AiPane, PaneMode};
use crate::db::{self, Db};
use crate::passkey::{self, Passkey};

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
    DeleteWorkspace(Id),
    DeleteTasks(Vec<Id>),
    /// 移動先のワークスペースを選ぶ
    MoveTasks(Vec<Id>),
    People,
    History,
    /// 状態の変更（コメントを添えられる）
    Status(Id, Status),
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
    /// 通知の一覧
    Notices,
    /// 完了チケットの発行
    Ticket(Id),
    /// 書体と色
    Appearance,
}

/// 左のメニューで、その場で名前を書いているワークスペース。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceNaming {
    pub id: Id,
    /// 追加したばかりで、最初の名前を書いている
    pub created: bool,
}

/// 追加したばかりのタスクのタイトル。
pub const NEW_TASK_TITLE: &str = "新規タスク";

/// 追加したばかりの担当者の名前。
pub const NEW_PERSON_NAME: &str = "新規担当者";

/// 追加したばかりのワークスペースの名前。
pub const NEW_WORKSPACE_NAME: &str = "新規ワークスペース";

/// ダイアログの上に、もう 1 枚重ねて出すダイアログ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubModal {
    /// 書体を選ぶ
    Font,
    /// 背景の画像を選ぶ
    Scene,
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
    /// コメントに貼ったリンクが指すタスク。
    pub linked: Vec<Todo>,
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
    pub icon_search: Entity<InputState>,
    pub workspace_name: Entity<InputState>,
    pub font_search: Entity<InputState>,
    pub name: Entity<InputState>,
    pub title: Entity<InputState>,
    pub subtask: Entity<InputState>,
    pub start: Entity<InputState>,
    pub due: Entity<InputState>,
    pub due_time: Entity<InputState>,
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
    pub ws_context: Entity<TextareaState>,
    pub bulk_due: Entity<InputState>,
    pub bulk_categories: Entity<InputState>,
}

pub struct MainView {
    pub db: Entity<Db>,
    pub focus: FocusHandle,
    pub nav: Nav,
    pub collapsed: HashSet<Id>,
    pub include_descendants: bool,
    /// 追加したばかりで、詳細で最初のタイトルを書いているタスク。
    pub naming_task: Option<Id>,
    /// 担当者の一覧で名前を書いている担当者。
    pub person_naming: Option<Id>,
    /// 左のメニューで名前を書いているワークスペース。
    pub workspace_naming: Option<WorkspaceNaming>,
    /// アイコンを選ぶ画面で選んでいるカテゴリ。
    pub icon_category: Option<crate::icons::Category>,
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
    /// このウィンドウの右に AI チャットを出しているか。
    pub ai_open: bool,
    pub ai_pane: Option<Entity<AiPane>>,
    pub ai_session: Entity<Session>,
    /// 広いウィンドウで、左のメニューを閉じている
    pub sidebar_hidden: bool,
    /// 左のメニューの幅（拡大率を掛ける前の値）。境界をドラッグして変えられ、保存される。
    pub sidebar_width: f32,
    /// 左のメニューの境界をドラッグしている間だけ `true`。
    pub sidebar_resizing: bool,
    /// 上のバーの空いているところを押している間だけ `true`（ウィンドウを動かし始めるため）。
    pub header_pressed: bool,
    /// 表の列。並び順どおり。幅と、隠しているかを持つ。
    pub columns: Vec<TableColumn>,
    /// 一覧に使える幅（拡大率を掛ける前の値）。描くたびに、ウィンドウかペインの幅から決める
    pub list_width: f32,
    /// 幅を変えている列と、押し始めた位置・そのときの幅。
    pub column_resize: Option<(Column, f32, f32)>,
    /// タスクの一覧と、期限が近いタスクの一覧のスクロール位置。
    pub list_scroll: UniformListScrollHandle,
    pub due_scroll: UniformListScrollHandle,
    /// 低いペインで、一覧を画面ごとスクロールする位置
    pub page_scroll: gpui_kit::ScrollHandle,
    /// 開いている見出しのメニュー。
    pub column_menu: Option<ColumnMenu>,
    /// 見出しをドラッグして動かしている列。
    pub column_drag: Option<ColumnDrag>,
    /// ウィンドウの位置と大きさ（x, y, 幅, 高さ）と、最大化しているか。
    pub window_place: Option<([f32; 4], bool)>,
    /// ペインの配置と、今のペイン。
    pub layout: malakt_core::panes::Layout,
    /// このウィンドウが中身を兼ねているペイン。
    pub primary: malakt_core::panes::PaneId,
    /// 分けて増えたペインの中身。
    pub children: HashMap<malakt_core::panes::PaneId, Entity<MainView>>,
    /// ほかのウィンドウの中にペインとして置かれている。
    pub embedded: bool,
    /// ペインとして置かれている場所（ウィンドウの中の位置と大きさ）。
    pub pane_bounds: Option<Bounds<Pixels>>,
    /// ペインを並べる場所（ウィンドウの中の位置と大きさ）。
    pub pane_area: Bounds<Pixels>,
    /// Ctrl+B を押して、次のキーを待っている。
    pub pane_prefix: bool,
    /// リサイズモード。
    pub pane_resize: bool,
    pub pane_keys: malakt_core::panes::Keys,
    pub pane_overlay: Option<crate::panes::PaneOverlay>,
    /// 自分で保存した配置（名前、配置）
    pub pane_presets: Vec<(String, String)>,
    pub border_drag: Option<crate::panes::BorderDrag>,
    /// ドラッグして動かしているタスク。
    pub task_drag: Option<TaskDrag>,
    /// 詳細で、担当者と役を選ぶところを開いている。
    pub assignee_picker: bool,
    /// ダイアログの上に重ねて出しているダイアログ。
    pub sub_modal: Option<SubModal>,
    /// 書体を選ぶ一覧に出す名前（この PC に入っている書体）。
    pub fonts: Vec<String>,
    pub font_scroll: UniformListScrollHandle,
    /// アクセント、背景（ライト）、背景（ダーク）の色を選ぶところ。
    pub color_pickers: [Entity<ColorPickerState>; 3],
    /// 面の透明度と、背景の画像のぼかし。
    pub clear_slider: Entity<SliderState>,
    pub blur_slider: Entity<SliderState>,
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
            icon_search: input(window, cx, "アイコンを検索"),
            workspace_name: input(window, cx, "ワークスペース名"),
            font_search: input(window, cx, "フォントを検索"),
            name: input(window, cx, "名前"),
            title: input(window, cx, "タイトル"),
            subtask: input(window, cx, "サブタスクを追加…"),
            start: input(window, cx, "日付 時刻"),
            due: input(window, cx, "日付"),
            due_time: input(window, cx, "時刻"),
            category: input(window, cx, "カテゴリを追加して Enter"),
            description: area(window, cx, "説明"),
            comment: area(window, cx, "コメントを追加"),
            status_comment: area(window, cx, "コメントを追加"),
            ws_date: input(window, cx, "YYYY-MM-DD"),
            def_days: input(window, cx, "例: -7（7 日前）"),
            def_date: input(window, cx, "YYYY-MM-DD"),
            def_time: input(window, cx, "HH:MM（空なら日付だけ）"),
            def_categories: input(window, cx, "「、」で区切って入力"),
            def_description: area(window, cx, "説明の雛形"),
            ws_context: area(
                window,
                cx,
                "例: このワークスペースのタスクは優先度を高にする",
            ),
            bulk_due: input(window, cx, "11/27 14:00"),
            bulk_categories: input(window, cx, "「、」で区切って入力"),
        };

        let ai_session = ai::shared(cx);
        let mut subs = vec![
            cx.observe(&db, |this, _, cx| this.reload(cx)),
            // チャットを別ウィンドウに出した・戻したときに、右の表示を合わせる
            cx.observe(&ai_session, |_, _, cx| cx.notify()),
            // 今見ている場所を、AI に伝えられるようにしておく
            cx.observe_self(|this, cx| {
                this.finish_naming_task(cx);
                let place = this.ai_place();
                let workspace = match &this.nav {
                    Nav::Workspace(id) => Some(id.clone()),
                    _ => None,
                };
                let session = this.ai_session.read(cx);
                if session.place != place || session.workspace != workspace {
                    this.ai_session.update(cx, |s, _| {
                        s.place = place;
                        s.workspace = workspace;
                    });
                }
            }),
            // リマインダーとの連携で失敗したことを出す
            cx.subscribe(
                &crate::reminders::shared(cx),
                |this, _, failed: &crate::reminders::Failed, cx| {
                    this.error = Some(failed.0.clone());
                    cx.notify();
                },
            ),
            // 新しい版を取ってきたら、上のバーに印を出す
            cx.observe(&crate::update::shared(cx), |_, _, cx| cx.notify()),
            cx.subscribe(
                &crate::update::shared(cx),
                |this, _, failed: &crate::update::Failed, cx| {
                    this.error = Some(failed.0.clone());
                    cx.notify();
                },
            ),
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
        on_enter!(name, save_person_name);
        // 担当者の名前も、ほかの場所を押したときに保存する
        subs.push(cx.subscribe_in(
            &inputs.name,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) && this.person_naming.is_some() {
                    this.save_person_name(window, cx);
                }
            },
        ));
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
        on_enter!(workspace_name, save_workspace_name);
        // ワークスペースの名前も、ほかの場所を押したときに保存する
        subs.push(cx.subscribe_in(
            &inputs.workspace_name,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) && this.workspace_naming.is_some() {
                    this.save_workspace_name(window, cx);
                }
            },
        ));
        on_enter!(subtask, submit_subtask);
        on_enter!(start, save_start);
        on_enter!(due, save_due);
        on_enter!(due_time, save_due_time);
        // 期限は、書き換えてからほかの場所を押したときにも保存する
        subs.push(cx.subscribe_in(
            &inputs.due,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) {
                    let due = this.current_due().unwrap_or_default();
                    if input.read(cx).value().trim() != malakt_core::date_of(&due) {
                        this.save_due(window, cx);
                    }
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &inputs.due_time,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) {
                    let due = this.current_due().unwrap_or_default();
                    let time = malakt_core::time_of(&due).unwrap_or_default();
                    if input.read(cx).value().trim() != time {
                        this.save_due_time(window, cx);
                    }
                }
            },
        ));
        on_enter!(category, submit_category);
        subs.push(cx.subscribe_in(
            &inputs.font_search,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.font_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            },
        ));
        let slider = |max: u8, cx: &mut Context<Self>| {
            cx.new(|_| SliderState::new().min(0.).max(f32::from(max)).step(1.))
        };
        let clear_slider = slider(crate::theme::MAX_CLEAR, cx);
        let blur_slider = slider(100, cx);
        subs.push(cx.subscribe_in(
            &clear_slider,
            window,
            |this, _, event: &SliderEvent, _, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                if let Err(error) = crate::theme::set_clear(value.end() as u8, cx) {
                    this.error = Some(format!("透明度を保存できませんでした。{error}").into());
                }
                cx.notify();
            },
        ));
        subs.push(cx.subscribe_in(
            &blur_slider,
            window,
            |this, _, event: &SliderEvent, _, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                if let Err(error) = crate::theme::set_blur(value.end() as u8, cx) {
                    this.error = Some(error.into());
                }
                cx.notify();
            },
        ));
        let color_pickers = crate::theme::Slot::ALL.map(|slot| {
            let picker = cx.new(|cx| ColorPickerState::new(window, cx));
            subs.push(cx.subscribe_in(
                &picker,
                window,
                move |this, _, event: &ColorPickerEvent, window, cx| {
                    let ColorPickerEvent::Change(color) = event;
                    this.set_color(slot, color.map(Into::into), window, cx);
                },
            ));
            picker
        });
        subs.push(cx.subscribe_in(
            &inputs.icon_search,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
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
            icon_category: None,
            workspace_naming: None,
            person_naming: None,
            naming_task: None,
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
            // 起動のときに背景の画像を読めなかったら、最初のウィンドウが知らせる
            error: crate::update::take_failure(cx)
                .or_else(|| crate::theme::take_failure(cx).map(Into::into)),
            notice: None,
            sidebar_open: false,
            ai_open: false,
            ai_pane: None,
            ai_session,
            sidebar_hidden: false,
            sidebar_width: SIDEBAR_WIDTH,
            sidebar_resizing: false,
            header_pressed: false,
            assignee_picker: false,
            columns: Column::ALL.map(TableColumn::new).to_vec(),
            list_width: f32::INFINITY,
            column_resize: None,
            column_menu: None,
            list_scroll: UniformListScrollHandle::new(),
            due_scroll: UniformListScrollHandle::new(),
            page_scroll: gpui_kit::ScrollHandle::new(),
            column_drag: None,
            task_drag: None,
            layout: Default::default(),
            primary: 0,
            children: HashMap::new(),
            embedded: false,
            pane_bounds: None,
            pane_area: Bounds::default(),
            pane_prefix: false,
            pane_resize: false,
            pane_keys: Default::default(),
            pane_overlay: None,
            sub_modal: None,
            fonts: Vec::new(),
            font_scroll: UniformListScrollHandle::new(),
            color_pickers,
            clear_slider,
            blur_slider,
            pane_presets: Vec::new(),
            border_drag: None,
            window_place: None,
            motion: true,
            today: view::today(),
            inputs,
            _subscriptions: subs,
        };
        this.load_sidebar_width(cx);
        this.load_columns(cx);
        this.load_pane_keys(cx);
        this.load_pane_presets(cx);
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
        // 前に閉じたときの AI チャットとシェルを、最初のウィンドウで戻す
        if let Some((chat_window, shell_window)) =
            this.ai_session.update(cx, |s, _| s.restore.take())
        {
            if this.ai_session.read(cx).open {
                this.ai_open = true;
                let main = cx.weak_entity();
                this.ai_pane =
                    Some(cx.new(|cx| AiPane::new(PaneMode::Docked, Some(main), window, cx)));
            }
            let session = this.ai_session.clone();
            cx.defer(move |cx| {
                for (wanted, mode) in [
                    (chat_window, PaneMode::ChatWindow),
                    (shell_window, PaneMode::ShellWindow),
                ] {
                    if !wanted {
                        continue;
                    }
                    let opened = crate::open_ai_window(mode, cx);
                    session.update(cx, |s, cx| {
                        match (opened, mode) {
                            (Ok(handle), PaneMode::ChatWindow) => s.chat_window = Some(handle),
                            (Ok(handle), _) => s.shell_place = ShellPlace::Window(handle),
                            (Err(e), _) => {
                                s.error = Some(format!("ウィンドウを開けませんでした: {e}").into())
                            }
                        }
                        cx.notify();
                    });
                }
            });
        }
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
                    linked: {
                        let comments = store.comments(id)?;
                        let mut ids: Vec<&str> = Vec::new();
                        for c in &comments {
                            for linked in malakt_core::tasklink::find(&c.body) {
                                if !ids.contains(&linked) {
                                    ids.push(linked);
                                }
                            }
                        }
                        // 消したタスクへのリンクは、見つからないものとしてカードに出す
                        ids.into_iter()
                            .filter_map(|linked| store.task(linked).ok())
                            .collect()
                    },
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

    /// 今のペインを、その場所にする。
    pub fn go(&mut self, nav: Nav, cx: &mut Context<Self>) {
        if let Some(child) = self.focused_child() {
            self.sidebar_open = false;
            child.update(cx, |c, cx| c.go_here(nav, cx));
            cx.notify();
            return;
        }
        self.go_here(nav, cx);
    }

    /// このビューが持つ画面を、その場所にする。
    pub fn go_here(&mut self, nav: Nav, cx: &mut Context<Self>) {
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
        self.assignee_picker = false;
        self.reload(cx);
        self.fill_when_inputs(window, cx);
    }

    /// 詳細の開始日と期限の入力欄に、保存されている値を入れる。期限は日付と時刻の欄に分ける。
    fn fill_when_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        let start = detail.task.start.clone().unwrap_or_default();
        let due = detail.task.due.clone().unwrap_or_default();
        let date = malakt_core::date_of(&due).to_owned();
        let time = malakt_core::time_of(&due).unwrap_or_default().to_owned();
        for (input, value) in [
            (&self.inputs.start, start),
            (&self.inputs.due, date),
            (&self.inputs.due_time, time),
        ] {
            input.update(cx, |s, cx| s.set_value(value, window, cx));
        }
    }

    /// 詳細のタスクの、いまの期限。
    fn current_due(&self) -> Option<String> {
        self.detail.as_ref()?.task.due.clone()
    }

    pub fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.panel = None;
        self.detail = None;
        self.editing = None;
        self.assignee_picker = false;
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

    pub fn save_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.inputs.title.read(cx).value().to_string();
        self.edit_task(cx, |d| d.title = title);
        if self.error.is_none() {
            self.editing = None;
            self.leave_input(&self.inputs.title.clone(), window, cx);
        }
    }

    /// Enter で書き終えた入力欄は消えるので、フォーカスを画面に戻して Esc などのキーを受けられるようにする。
    /// ほかの場所を押して書き終えたときは、押した先のフォーカスをそのままにする。
    fn leave_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input.read(cx).focus_handle(cx).is_focused(window) {
            self.focus.focus(window, cx);
        }
    }

    pub fn save_description(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let description = self.inputs.description.read(cx).value().to_string();
        self.edit_task(cx, |d| d.description = description);
        if self.error.is_none() {
            self.editing = None;
        }
    }

    pub fn save_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.inputs.start.read(cx).value().to_string();
        let base = self.detail.as_ref().and_then(|d| d.task.start.clone());
        let value = when::resolve(&text, &view::now(), base.as_deref());
        self.edit_task(cx, |d| d.start = Some(value));
        self.after_when_edit(window, cx);
    }

    /// 期限の日付の欄を保存する。「11/27」「明日」「来週金曜 14時」のような書き方も読む。
    /// 日付だけを書いたときは、時刻の欄の時刻を残す。
    pub fn save_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.inputs.due.read(cx).value().to_string();
        let current = self.current_due();
        let mut value = when::resolve(&text, &view::now(), None);
        if let Some(time) = current.as_deref().and_then(malakt_core::time_of) {
            if malakt_core::is_valid_date(&value) {
                value = format!("{value} {time}");
            }
        }
        self.edit_task(cx, |d| d.due = Some(value));
        self.after_when_edit(window, cx);
    }

    /// 期限の時刻の欄を保存する。空にすると日付だけの期限に戻る。期限がなければ今日の時刻にする。
    pub fn save_due_time(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.inputs.due_time.read(cx).value().trim().to_owned();
        let current = self.current_due();
        let value = if text.is_empty() {
            current
                .as_deref()
                .map(|due| malakt_core::date_of(due).to_owned())
        } else {
            Some(when::resolve(&text, &view::now(), current.as_deref()))
        };
        self.edit_task(cx, |d| d.due = value);
        self.after_when_edit(window, cx);
    }

    /// 保存できたら、入力欄を保存した形（`2026-11-27` と `14:00`）に直す。
    /// 保存できなかったときは、書いたものを残す。
    fn after_when_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.error.is_none() {
            self.reload(cx);
            self.fill_when_inputs(window, cx);
        }
    }

    /// 期限を今日から `days` 日後にする。時刻は残す。`None` なら期限をなくす。
    pub fn set_due_in(&mut self, days: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_due();
        let value =
            days.map(|d| view::with_time_of(&view::add_days(&self.today, d), current.as_deref()));
        self.edit_task(cx, |d| d.due = value);
        self.after_when_edit(window, cx);
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

    // ---- タスクのドラッグ ----

    /// タスクの行を押した。少し動かすまでは、ただのクリックとして扱う。
    pub fn start_task_drag(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.iter().find(|t| t.id == id) else {
            return;
        };
        self.task_drag = Some(TaskDrag {
            id: task.id.clone(),
            title: task.title.clone().into(),
            start: at,
            at,
            moved: false,
            over: None,
        });
        cx.notify();
    }

    /// ドラッグ中のマウスの位置。行やワークスペースの上にいるときは、先に `drag_task_over` が呼ばれている。
    pub fn drag_task(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.task_drag else {
            return;
        };
        let (dx, dy) = (
            f32::from(at.x - drag.start.x),
            f32::from(at.y - drag.start.y),
        );
        if !drag.moved && dx.abs().max(dy.abs()) <= 6. {
            return;
        }
        drag.moved = true;
        // 落とせる場所から出たら、落とし先を消す
        if drag.over.as_ref().is_some_and(|(_, seen)| *seen != at) {
            drag.over = None;
        }
        drag.at = at;
        cx.notify();
    }

    /// ドラッグ中に、落とせる場所の上にマウスが来た。
    pub fn drag_task_over(
        &mut self,
        target: DropTarget,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = self.task_drag.as_mut().filter(|d| d.moved) {
            if drag.over.as_ref().map(|(t, _)| t) != Some(&target) {
                cx.notify();
            }
            drag.over = Some((target, at));
        }
    }

    /// ドラッグ中のタスクを、今の落とし先に落とす。
    pub fn finish_task_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.task_drag.take() else {
            return;
        };
        cx.notify();
        let Some((target, _)) = drag.over.filter(|_| drag.moved) else {
            return;
        };
        match target {
            DropTarget::Workspace(workspace) => {
                let old = self.tasks.iter().find(|t| t.id == drag.id);
                if old.is_none_or(|t| t.workspace_id == workspace) {
                    return;
                }
                let done = self.write(cx, |s| s.move_task(&drag.id, workspace.as_deref()));
                if done.is_some() {
                    let name = self.workspace_name(workspace.as_deref());
                    self.notice =
                        Some(format!("「{}」を「{name}」に移しました。", drag.title).into());
                }
            }
            DropTarget::Row(to) => {
                let mut order: Vec<Id> = self
                    .visible
                    .iter()
                    .map(|&i| self.tasks[i].id.clone())
                    .collect();
                let Some(from) = order.iter().position(|id| *id == drag.id) else {
                    return;
                };
                if from == to || to >= order.len() {
                    return;
                }
                let moved = order.remove(from);
                order.insert(to, moved);
                // 並べ替えた結果が見えるように、手動の順で出す
                if self.write(cx, |s| s.set_task_order(&order)).is_some() {
                    self.query.sort = view::Sort::Manual;
                    self.query.descending = false;
                    self.reload(cx);
                }
            }
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
            let value = when::resolve(&value, &view::now(), None);
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

    // ---- 新しいタスク ----

    /// 「新規タスク」というタイトルで追加して、詳細でそのままタイトルを書けるようにする。
    /// いまのワークスペースの既定値が入る。上の入力欄に書きかけたタイトルがあれば、それで作る。
    pub fn add_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.current_workspace().map(str::to_owned);
        let typed = self.inputs.quick_add.read(cx).value().trim().to_owned();
        let title = if typed.is_empty() {
            NEW_TASK_TITLE
        } else {
            typed.as_str()
        };
        let draft = match self
            .db
            .read(cx)
            .store
            .draft_for(workspace.as_deref(), title)
        {
            Ok(draft) => draft,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let Some(task) = self.write(cx, |s| s.create_task_shown(workspace.as_deref(), &draft))
        else {
            return;
        };
        Self::take(&self.inputs.quick_add.clone(), window, cx);
        self.query.tab = view::Tab::Open;
        if typed.is_empty() {
            // タイトルを書き終えるまで、AI に整えさせない
            let id = task.id.clone();
            self.ai_session.update(cx, |session, _| session.hold(id));
        }
        self.open_panel(&task.id, window, cx);
        if typed.is_empty() {
            self.naming_task = Some(task.id);
            self.start_editing(Editing::Title, window, cx);
            // すぐ打ち替えられるように、タイトルを全部選んでおく
            self.inputs
                .title
                .update(cx, |s, cx| s.select_all(window, cx));
        }
    }

    /// 最初のタイトルを書き終えた（保存した、取り消した、ほかのタスクに移った）ら、AI に整えさせる。
    fn finish_naming_task(&mut self, cx: &mut Context<Self>) {
        let Some(id) = &self.naming_task else { return };
        if self.editing == Some(Editing::Title) && self.panel.as_ref() == Some(id) {
            return;
        }
        if let Some(id) = self.naming_task.take() {
            self.ai_session
                .update(cx, |session, cx| session.release(&id, cx));
        }
    }

    // ---- ワークスペースと担当者 ----

    /// 「新規ワークスペース」という名前で追加して、左のメニューでそのまま名前を書けるようにする。
    pub fn add_workspace(
        &mut self,
        parent: Option<Id>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.write(cx, |s| {
            s.create_workspace(parent.as_deref(), NEW_WORKSPACE_NAME)
        }) else {
            return;
        };
        if let Some(parent) = &parent {
            self.collapsed.remove(parent);
        }
        // 名前を書く行が見えるように、左のメニューを出す
        self.sidebar_hidden = false;
        self.sidebar_open = true;
        self.go(Nav::Workspace(workspace.id.clone()), cx);
        self.name_workspace(workspace.id, true, window, cx);
    }

    /// 左のメニューの行で、ワークスペースの名前を書き直す。
    pub fn rename_workspace(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        self.name_workspace(id, false, window, cx);
    }

    fn name_workspace(
        &mut self,
        id: Id,
        created: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.workspace_name(Some(&id));
        self.inputs
            .workspace_name
            .update(cx, |s, cx| s.set_value(name, window, cx));
        self.workspace_naming = Some(WorkspaceNaming { id, created });
        // メニューから始めたときは、メニューが閉じてからフォーカスを移す。
        // すぐ打ち替えられるように、名前を全部選んでおく
        let input = self.inputs.workspace_name.clone();
        cx.defer_in(window, move |this, window, cx| {
            if this.workspace_naming.is_none() {
                return;
            }
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        });
        cx.notify();
    }

    /// 書いた名前を保存する。Enter か、ほかの場所を押したとき。
    /// 追加したばかりのワークスペースに名前を付けたら、AI にアイコンを選ばせる。
    pub fn save_workspace_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(naming) = self.workspace_naming.clone() else {
            return;
        };
        let name = self
            .inputs
            .workspace_name
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let current = self.workspace_name(Some(&naming.id));
        // 空にしたときと変えていないときは、今の名前のままにする
        if !name.is_empty() && name != current {
            if self
                .write(cx, |s| s.rename_workspace(&naming.id, &name))
                .is_none()
            {
                return;
            }
            if naming.created {
                self.ai_session.update(cx, |session, cx| {
                    session.suggest_icon(naming.id.clone(), name.clone(), cx)
                });
            }
        }
        self.workspace_naming = None;
        self.leave_input(&self.inputs.workspace_name.clone(), window, cx);
        cx.notify();
    }

    /// 「新規担当者」という名前で追加して、一覧のその行でそのまま名前を書けるようにする。
    pub fn add_person(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(person) = self.write(cx, |s| s.create_person(NEW_PERSON_NAME, false)) {
            // 一覧を読み直して、増えた行を出す
            self.reload(cx);
            self.rename_person(person.id, window, cx);
        }
    }

    /// 担当者の一覧の行で、名前を書き直す。
    pub fn rename_person(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self
            .people
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
        else {
            return;
        };
        self.inputs.name.update(cx, |s, cx| {
            s.set_value(name, window, cx);
            s.focus(window, cx);
            // すぐ打ち替えられるように、名前を全部選んでおく
            s.select_all(window, cx);
        });
        self.person_naming = Some(id);
        cx.notify();
    }

    /// 書いた担当者の名前を保存する。Enter か、ほかの場所を押したとき。
    pub fn save_person_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.person_naming.clone() else {
            return;
        };
        let name = self.inputs.name.read(cx).value().trim().to_owned();
        let current = self.people.iter().find(|p| p.id == id).cloned();
        // 空にしたときと変えていないときは、今の名前のままにする
        if let Some(person) = current.filter(|p| !name.is_empty() && p.name != name) {
            if self
                .write(cx, |s| s.update_person(&id, &name, &person.note))
                .is_none()
            {
                return;
            }
        }
        self.person_naming = None;
        self.leave_input(&self.inputs.name.clone(), window, cx);
        cx.notify();
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

    // ---- 書体と色 ----

    pub fn open_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_color_pickers(window, cx);
        // メニューから開くとフォーカスがなくなるので、Esc を受けられるように戻す
        self.focus.focus(window, cx);
        self.sub_modal = None;
        self.modal = Modal::Appearance;
        cx.notify();
    }

    /// 色を選ぶところに、今の画面の色を出す。
    fn sync_color_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (slot, picker) in crate::theme::Slot::ALL.into_iter().zip(&self.color_pickers) {
            let color = crate::theme::shown(slot, cx);
            picker.update(cx, |picker, cx| picker.set_value(color, window, cx));
        }
        let (clear, blur) = (crate::theme::clear(cx), crate::theme::blur(cx));
        self.clear_slider
            .update(cx, |s, cx| s.set_value(f32::from(clear), window, cx));
        self.blur_slider
            .update(cx, |s, cx| s.set_value(f32::from(blur), window, cx));
    }

    pub fn open_scene_picker(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.sub_modal = Some(SubModal::Scene);
        cx.notify();
    }

    /// 背景の画像を変える。読めない画像のときは、前のままにして理由を出す。
    pub fn set_scene(
        &mut self,
        scene: crate::theme::Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match crate::theme::set_scene(scene, cx) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(error.into()),
        }
        // 背景の画像に合う色に変わるので、色を選ぶところも合わせる
        self.sync_color_pickers(window, cx);
        cx.notify();
    }

    /// 自分の画像を、OS のファイル選択で選ぶ。
    pub fn pick_scene_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("背景の画像".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            // ウィンドウを閉じたあとなら、することがない
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_scene(crate::theme::Scene::File(path), window, cx)
            });
        })
        .detach();
    }

    /// タスクの一覧を、広い画面でもカードで出す。
    pub fn set_cards(&mut self, on: bool, cx: &mut Context<Self>) {
        match crate::theme::set_cards(on, cx) {
            Ok(()) => self.error = None,
            Err(error) => {
                self.error =
                    Some(format!("一覧の表示のしかたを保存できませんでした。{error}").into())
            }
        }
        cx.notify();
    }

    pub fn open_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fonts = crate::theme::fonts(cx);
        self.inputs.font_search.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        // 今の書体が見えるところから始める
        let current = self.current_font(cx);
        let at = self.fonts.iter().position(|name| *name == current);
        self.font_scroll
            .scroll_to_item(at.unwrap_or(0), ScrollStrategy::Center);
        self.sub_modal = Some(SubModal::Font);
        cx.notify();
    }

    /// 今使っている書体の名前。
    pub fn current_font(&self, cx: &App) -> String {
        crate::theme::look(cx)
            .font
            .unwrap_or_else(|| crate::theme::DEFAULT_FONT.to_owned())
    }

    /// 検索に合う書体。
    pub fn found_fonts(&self, cx: &App) -> Vec<String> {
        let query = self.inputs.font_search.read(cx).value().to_lowercase();
        self.fonts
            .iter()
            .filter(|name| name.to_lowercase().contains(query.trim()))
            .cloned()
            .collect()
    }

    /// 書体を変える。`None` は同梱の書体。
    pub fn set_font(&mut self, font: Option<&str>, cx: &mut Context<Self>) {
        match crate::theme::set_font(font, cx) {
            Ok(()) => self.error = None,
            Err(error) => {
                self.error = Some(format!("フォントを保存できませんでした。{error}").into())
            }
        }
        cx.notify();
    }

    /// 色を変える。`None` は同梱の配色の色。
    pub fn set_color(
        &mut self,
        slot: crate::theme::Slot,
        color: Option<crate::theme::Color>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match crate::theme::set_color(slot, color, cx) {
            Ok(()) => self.error = None,
            Err(error) => {
                self.error =
                    Some(format!("{}の色を保存できませんでした。{error}", slot.label()).into())
            }
        }
        // 保存できなかったときも、色を選ぶところは今の画面の色に合わせる
        self.sync_color_pickers(window, cx);
        cx.notify();
    }

    pub fn open_people(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // メニューから開くとフォーカスがなくなるので、Esc を受けられるように戻す
        self.focus.focus(window, cx);
        self.person_naming = None;
        self.modal = Modal::People;
        cx.notify();
    }

    // ---- 左のメニューの幅 ----

    /// 保存してある幅を読む。
    fn load_sidebar_width(&mut self, cx: &App) {
        let saved = self.db.read(cx).store.setting(SIDEBAR_WIDTH_SETTING);
        if let Ok(Some(saved)) = saved {
            if let Ok(width) = saved.parse::<f32>() {
                self.sidebar_width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
            }
        }
    }

    /// ドラッグ中の幅。`width` は拡大率を掛ける前の値。
    pub fn drag_sidebar(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX).round();
        if width != self.sidebar_width {
            self.sidebar_width = width;
            cx.notify();
        }
    }

    /// ドラッグを終えて、幅を保存する。
    pub fn finish_sidebar_drag(&mut self, cx: &mut Context<Self>) {
        if !self.sidebar_resizing {
            return;
        }
        self.sidebar_resizing = false;
        let width = self.sidebar_width.to_string();
        self.write(cx, |s| s.set_setting(SIDEBAR_WIDTH_SETTING, Some(&width)));
        cx.notify();
    }

    /// 境界をダブルクリックして、最初の幅に戻す。
    pub fn reset_sidebar_width(&mut self, cx: &mut Context<Self>) {
        self.sidebar_width = SIDEBAR_WIDTH;
        self.sidebar_resizing = true;
        self.finish_sidebar_drag(cx);
    }

    /// 開いているタスクへのリンクをクリップボードに入れる。
    pub fn copy_task_link(&mut self, id: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(malakt_core::tasklink::url(id)));
        self.notice = Some("リンクをコピーしました。".into());
        cx.notify();
    }

    // ---- 表の列 ----

    /// 保存してある列の並び・幅・表示を読む。読めない項目は最初の形のままにする。
    fn load_columns(&mut self, cx: &App) {
        if let Ok(Some(saved)) = self.db.read(cx).store.setting(COLUMNS_SETTING) {
            self.columns = parse_columns(&saved);
        }
    }

    fn save_columns(&mut self, cx: &mut Context<Self>) {
        let saved = format_columns(&self.columns);
        self.write(cx, |s| s.set_setting(COLUMNS_SETTING, Some(&saved)));
        cx.notify();
    }

    /// 今出している列。タイトルに `TITLE_MIN` の幅が残らないときは、右の列から出さない。
    pub fn shown_columns(&self) -> impl Iterator<Item = &TableColumn> {
        let mut room = self.list_width - TABLE_FIXED - TITLE_MIN;
        self.columns.iter().filter(|c| !c.hidden).filter(move |c| {
            room -= c.width + COLUMN_GAP;
            room >= 0.
        })
    }

    /// 列の左の境界を押して、幅を変え始める。`x` はウィンドウの中の位置。
    pub fn start_column_resize(&mut self, column: Column, x: f32, cx: &mut Context<Self>) {
        if let Some(c) = self.columns.iter().find(|c| c.column == column) {
            self.column_resize = Some((column, x, c.width));
            cx.notify();
        }
    }

    /// ドラッグ中の幅。境界は列の左にあるので、左へ動かすと広がる。
    pub fn drag_column(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((column, start_x, start_width)) = self.column_resize else {
            return;
        };
        let width = (start_width - (x - start_x) / crate::UI_SCALE)
            .clamp(COLUMN_MIN, COLUMN_MAX)
            .round();
        if let Some(c) = self.columns.iter_mut().find(|c| c.column == column) {
            if c.width != width {
                c.width = width;
                cx.notify();
            }
        }
    }

    pub fn finish_column_resize(&mut self, cx: &mut Context<Self>) {
        if self.column_resize.take().is_some() {
            self.save_columns(cx);
        }
    }

    /// 境界をダブルクリックして、最初の幅に戻す。
    pub fn reset_column_width(&mut self, column: Column, cx: &mut Context<Self>) {
        self.column_resize = None;
        if let Some(c) = self.columns.iter_mut().find(|c| c.column == column) {
            c.width = column.default_width();
        }
        self.save_columns(cx);
    }

    /// 列を出す・隠す。
    pub fn toggle_column(&mut self, column: Column, cx: &mut Context<Self>) {
        if let Some(c) = self.columns.iter_mut().find(|c| c.column == column) {
            c.hidden = !c.hidden;
        }
        self.save_columns(cx);
    }

    /// 列を、`target` の列がある位置へ動かす。
    pub fn move_column(&mut self, column: Column, target: Column, cx: &mut Context<Self>) {
        let from = self.columns.iter().position(|c| c.column == column);
        let to = self.columns.iter().position(|c| c.column == target);
        if let (Some(from), Some(to)) = (from, to) {
            if from != to {
                let moved = self.columns.remove(from);
                self.columns.insert(to, moved);
                self.save_columns(cx);
            }
        }
    }

    /// 見出しのドラッグを終える。ほかの列の上で離したら、そこへ動かす。
    pub fn finish_column_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.column_drag.take() {
            if let Some(target) = drag.over.filter(|_| drag.moved) {
                self.move_column(drag.column, target, cx);
            }
            cx.notify();
        }
    }

    /// その見出しの列で並べ替えているときの向き（`true` は降順）。`column` が `None` はタイトル。
    pub fn column_sorted(&self, column: Option<Column>) -> Option<bool> {
        let sort = column_sort(column)?;
        // 更新は新しい順がもとの向きなので、昇順（古い順）は逆にする
        (sort == self.query.sort).then_some(self.query.descending != (sort == view::Sort::Updated))
    }

    /// 見出しのメニューを開く。開いている見出しをもう一度押すと閉じる。
    pub fn toggle_column_menu(
        &mut self,
        column: Option<Column>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .column_menu
            .take()
            .is_some_and(|open| open.column == column)
        {
            cx.notify();
            return;
        }
        let sort = column_sort(column);
        let flip = sort == Some(view::Sort::Updated);
        let current = self.column_sorted(column);
        let columns: Vec<(Column, bool)> =
            self.columns.iter().map(|c| (c.column, c.hidden)).collect();
        let view = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if let Some(sort) = sort {
                for (text, descending) in [("昇順で並べ替え", false), ("降順で並べ替え", true)]
                {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(text)
                            .checked(current == Some(descending))
                            .on_click(move |_, _, cx| {
                                // ウィンドウを閉じたあとなら、反映する先がない
                                let _ = view.update(cx, |this, cx| {
                                    this.sort_by(sort, descending != flip, cx)
                                });
                            }),
                    );
                }
                menu = menu.separator();
            }
            if let Some(column) = column {
                let view = view.clone();
                menu = menu
                    .item(
                        PopupMenuItem::new("この列を隠す").on_click(move |_, _, cx| {
                            // ウィンドウを閉じたあとなら、反映する先がない
                            let _ = view.update(cx, |this, cx| this.toggle_column(column, cx));
                        }),
                    )
                    .separator();
            }
            for (other, hidden) in columns.iter().copied() {
                let view = view.clone();
                menu = menu.item(PopupMenuItem::new(other.label()).checked(!hidden).on_click(
                    move |_, _, cx| {
                        // ウィンドウを閉じたあとなら、反映する先がない
                        let _ = view.update(cx, |this, cx| this.toggle_column(other, cx));
                    },
                ));
            }
            menu
        });
        let closed = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
            this.column_menu = None;
            this.focus.focus(window, cx);
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        self.column_menu = Some(ColumnMenu {
            column,
            menu,
            _closed: closed,
        });
        cx.notify();
    }

    /// 見出しのメニューから並べ替える。同じ列で同じ向きをもう一度選ぶと、最初の並び順に戻る。
    pub fn sort_by(&mut self, sort: view::Sort, descending: bool, cx: &mut Context<Self>) {
        if self.query.sort == sort && self.query.descending == descending {
            self.query.sort = view::Sort::default();
            self.query.descending = false;
        } else {
            self.query.sort = sort;
            self.query.descending = descending;
        }
        self.reload(cx);
    }

    // ---- ロック ----

    pub fn lock_enabled(&self, cx: &App) -> bool {
        lock::is_enabled(&self.db.read(cx).store).unwrap_or(false)
    }

    pub fn verifying(&self, cx: &App) -> bool {
        self.db.read(cx).verifying
    }

    /// パスキーで本人確認をして、通ったら `done` を実行する。
    fn with_passkey(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: fn(&mut Self, &mut Context<Self>),
    ) {
        if self.verifying(cx) {
            return;
        }
        let verify = cx.global::<Passkey>().0.clone();
        let request = passkey::Request {
            app: crate::APP_NAME.into(),
            action: action.into(),
            window: passkey::native_window(window),
        };
        self.error = None;
        let db = self.db.clone();
        db.update(cx, |db, cx| {
            db.verifying = true;
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            // OS のダイアログに答えるまで戻らないので、画面のスレッドでは待たない
            let result = cx.background_spawn(async move { verify(request) }).await;
            db.update(cx, |db, cx| {
                db.verifying = false;
                cx.notify();
            });
            // 待っている間にウィンドウを閉じたなら、結果を出す先がない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => done(this, cx),
                    Err(failure) => this.error = Some(failure.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// ロック画面を、パスキーで開く。
    pub fn request_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.unlocked(cx) {
            return;
        }
        self.with_passkey("ロックを解除", window, cx, |this, cx| {
            this.db.update(cx, |db, cx| {
                db.unlocked = true;
                cx.notify();
            });
        });
    }

    /// ロックを有効にする。この端末のパスキーで確認できたときだけ有効になる。
    pub fn enable_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_passkey("ロックを有効にする", window, cx, |this, cx| {
            if this.write(cx, lock::enable).is_some() {
                this.notice = Some("ロックを有効にしました。".into());
            }
        });
    }

    pub fn disable_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_passkey("ロックを無効にする", window, cx, |this, cx| {
            if this.write(cx, lock::disable).is_some() {
                this.notice = Some("ロックを無効にしました。".into());
            }
        });
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

    // ---- リマインダー ----

    /// OS のリマインダーとの連携を始める・解除する。
    pub fn toggle_reminders(&mut self, cx: &mut Context<Self>) {
        let on = !crate::reminders::enabled(cx);
        match crate::reminders::set_enabled(on, cx) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("設定を保存できませんでした。{error}").into()),
        }
        cx.notify();
    }

    // ---- 自動アップデート ----

    /// 自動アップデートを有効にする・無効にする。
    pub fn toggle_auto_update(&mut self, cx: &mut Context<Self>) {
        let on = !crate::update::enabled(cx);
        self.error = crate::update::set_enabled(on, cx).err().map(Into::into);
        cx.notify();
    }

    /// 取ってきた新しい版に今すぐ入れ替えて、起動し直す。
    pub fn update_now(&mut self, cx: &mut Context<Self>) {
        self.error = crate::update::install_and_restart(cx).err().map(Into::into);
        cx.notify();
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
        let loaded = loaded.and_then(|(w, d)| {
            let context = store.setting(&crate::ai::workspace_context_key(id))?;
            Ok((w, d, context.unwrap_or_default()))
        });
        let (workspace, defaults, context) = match loaded {
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
        self.inputs
            .ws_context
            .update(cx, |s, cx| s.set_value(context, window, cx));
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
                date: when::resolve(&read(&self.inputs.def_date), &view::now(), None),
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
        let date = when::resolve(&read(&self.inputs.ws_date), &view::now(), None);
        let context = self.inputs.ws_context.read(cx).value().trim().to_owned();
        if let Some(error) = crate::ai::workspace_context_error(&context) {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        let context_key = crate::ai::workspace_context_key(&id);
        let saved = self.write(cx, |s| {
            // 先に両方を確かめて、片方だけ保存されることがないようにする
            malakt_core::defaults::clean(&defaults)?;
            s.set_workspace_date(&id, Some(&date))?;
            s.set_task_defaults(&defaults)?;
            // 空にしたら、保存してあるものを消す
            s.set_setting(
                &context_key,
                (!context.is_empty()).then_some(context.as_str()),
            )
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

    // ---- AI チャット ----

    /// AI チャットを開く・閉じる。別ウィンドウに出しているときは、そのウィンドウを手前に出す。
    pub fn toggle_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.ai_session.read(cx).chat_window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
            // そのウィンドウはもうない
            self.ai_session.update(cx, |s, _| s.chat_window = None);
        }
        self.ai_open = !self.ai_open;
        let open = self.ai_open;
        self.ai_session.update(cx, |s, cx| s.set_open(open, cx));
        if self.ai_open {
            let pane = match &self.ai_pane {
                Some(pane) => pane.clone(),
                None => {
                    let main = cx.weak_entity();
                    let pane = cx.new(|cx| AiPane::new(PaneMode::Docked, Some(main), window, cx));
                    self.ai_pane = Some(pane.clone());
                    pane
                }
            };
            pane.update(cx, |pane, cx| pane.focus_input(window, cx));
        }
        cx.notify();
    }

    /// このウィンドウの右に、チャットを出すか。
    pub fn ai_docked(&self, cx: &App) -> bool {
        self.ai_open && self.ai_session.read(cx).chat_window.is_none()
    }

    /// AI に伝える、ユーザーが今開いている場所。
    pub fn ai_place(&self) -> String {
        let place = match &self.nav {
            Nav::Home => "ユーザーはホームを開いています。".to_owned(),
            Nav::All => "ユーザーは「すべてのタスク」を開いています。".to_owned(),
            Nav::Unfiled => "ユーザーは「未所属」のタスクの一覧を開いています。".to_owned(),
            Nav::Workspace(id) => format!(
                "ユーザーはワークスペース「{}」（ID: {id}）を開いています。場所を言わずにタスクを作るよう頼まれたら、ここに作ってください。",
                self.workspace_name(Some(id))
            ),
        };
        let mut place = place;
        if !matches!(self.nav, Nav::Home) {
            // 一覧に今出ているもの
            place.push_str(match self.query.tab {
                view::Tab::Open => "\n一覧は未完了のタブです。",
                view::Tab::Closed => "\n一覧は完了のタブです。",
            });
            if !self.query.q.trim().is_empty() {
                place.push_str(&format!("検索の言葉は「{}」です。", self.query.q.trim()));
            }
            place.push_str(&format!(
                "\n一覧に出ているタスク（{} 件）:\n",
                self.visible.len()
            ));
            for &ix in self.visible.iter().take(AI_PLACE_TASKS) {
                let task = &self.tasks[ix];
                place.push_str(&format!(
                    "- {}（ID: {}、{}）\n",
                    task.title,
                    task.id,
                    task.status.label()
                ));
            }
        }
        if !self.selected.is_empty() {
            let mut ids: Vec<&str> = self.selected.iter().map(String::as_str).collect();
            ids.sort_unstable();
            place.push_str(&format!(
                "\nチェックを付けて選んでいるタスクの ID: {}\n",
                ids.join("、")
            ));
        }
        if let Some(detail) = &self.detail {
            place.push_str(&format!(
                "\n詳細を開いているタスクは「{}」（ID: {}、{}）です。「これ」「このタスク」は、このタスクのことです。\n",
                detail.task.title,
                detail.task.id,
                detail.task.status.label()
            ));
            if !detail.task.description.trim().is_empty() {
                place.push_str(&format!(
                    "説明: {}\n",
                    malakt_ai::clip_tail(detail.task.description.trim(), AI_PLACE_CHARS)
                ));
            }
        }
        place
    }

    /// Esc。重ねたダイアログを閉じたときは、下のダイアログが次の Esc を受けられるようにする
    /// （重ねたほうの入力欄がなくなると、フォーカスの行き先がなくなる）。
    pub fn dismiss_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let over = self.sub_modal.is_some();
        self.dismiss(cx);
        if over {
            self.focus.focus(window, cx);
        }
    }

    /// Esc。今開いているものを 1 つだけ閉じる。
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
        if self.sub_modal.take().is_some() {
        } else if self.pane_overlay.is_some() {
            self.pane_overlay = None;
        } else if self.pane_prefix || self.pane_resize {
            self.pane_prefix = false;
            self.pane_resize = false;
        } else if self.person_naming.take().is_some() {
            // 書きかけの名前を捨てる。担当者のダイアログは開いたまま
        } else if self.modal != Modal::None {
            self.modal = Modal::None;
            self.issued_ticket = None;
        } else if self.workspace_naming.take().is_some() {
            // 書きかけの名前を捨てる
        } else {
            // 今のペインで開いているものを閉じ、何もなければ拡大表示を戻す
            let closed = match self.focused_child() {
                Some(child) => child.update(cx, |c, cx| c.dismiss_own(cx)),
                None => self.dismiss_own(cx),
            };
            if !closed && self.layout.zoomed() {
                self.layout.toggle_zoom();
            }
        }
    }

    /// このビューで開いているものを 1 つ閉じる。閉じたら `true`。
    pub fn dismiss_own(&mut self, cx: &mut Context<Self>) -> bool {
        self.error = None;
        cx.notify();
        if self.sub_modal.take().is_some() || self.person_naming.take().is_some() {
        } else if self.modal != Modal::None {
            self.modal = Modal::None;
            self.issued_ticket = None;
        } else if self.editing.is_some() {
            self.editing = None;
        } else if self.assignee_picker {
            self.assignee_picker = false;
        } else if self.panel.is_some() {
            self.close_panel(cx);
        } else if !self.selected.is_empty() {
            self.selected.clear();
        } else {
            return false;
        }
        true
    }
}

/// AI に伝える、一覧に出ているタスクの数。
const AI_PLACE_TASKS: usize = 30;
/// AI に伝える、開いているタスクの説明の文字数。
const AI_PLACE_CHARS: usize = 1500;

/// 通知の一覧に出す数。
const NOTICES_SHOWN: usize = 100;

/// 左のメニューの最初の幅と、変えられる範囲（拡大率を掛ける前の値）。
pub const SIDEBAR_WIDTH: f32 = 256.;
pub const SIDEBAR_MIN: f32 = 200.;
pub const SIDEBAR_MAX: f32 = 480.;
const SIDEBAR_WIDTH_SETTING: &str = "sidebar.width";

/// 表の列（タイトルのほか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Status,
    Priority,
    Assignee,
    Due,
    Updated,
}

impl Column {
    pub const ALL: [Column; 5] = [
        Column::Status,
        Column::Priority,
        Column::Assignee,
        Column::Due,
        Column::Updated,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Column::Status => "状態",
            Column::Priority => "優先度",
            Column::Assignee => "担当",
            Column::Due => "期限",
            Column::Updated => "更新",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Column::Status => "status",
            Column::Priority => "priority",
            Column::Assignee => "assignee",
            Column::Due => "due",
            Column::Updated => "updated",
        }
    }

    /// 最初の幅（拡大率を掛ける前の値）。
    pub fn default_width(self) -> f32 {
        match self {
            Column::Status => 96.,
            Column::Priority => 84.,
            Column::Assignee => 160.,
            Column::Due => 150.,
            Column::Updated => 84.,
        }
    }

    /// この列で並べ替えるときの並び順。担当では並べ替えない。
    pub fn sort(self) -> Option<view::Sort> {
        match self {
            Column::Status => Some(view::Sort::Status),
            Column::Priority => Some(view::Sort::Priority),
            Column::Assignee => None,
            Column::Due => Some(view::Sort::Due),
            Column::Updated => Some(view::Sort::Updated),
        }
    }
}

/// 表でタイトルに必ず残す幅。最初の列幅なら、表にするいちばん狭い幅でも全部の列が入る。
pub const TITLE_MIN: f32 = 140.;
/// 列と列の間。
const COLUMN_GAP: f32 = 12.;
/// 表の左右の余白と、選択のチェックボックスの幅。
const TABLE_FIXED: f32 = 48. + 2. + 24. + 24.;

#[derive(Debug, Clone, PartialEq)]
pub struct TableColumn {
    pub column: Column,
    pub width: f32,
    pub hidden: bool,
}

impl TableColumn {
    fn new(column: Column) -> Self {
        TableColumn {
            column,
            width: column.default_width(),
            hidden: false,
        }
    }
}

/// 見出しの列で並べ替えるときの並び順。`None` はタイトル。
fn column_sort(column: Option<Column>) -> Option<view::Sort> {
    match column {
        Some(column) => column.sort(),
        None => Some(view::Sort::Title),
    }
}

/// 開いている見出しのメニュー。
pub struct ColumnMenu {
    /// `None` はタイトル。
    pub column: Option<Column>,
    pub menu: Entity<PopupMenu>,
    _closed: Subscription,
}

/// タスクのドラッグ。押し始めた位置から少し動くまでは、ただのクリックとして扱う。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskDrag {
    pub id: Id,
    pub title: SharedString,
    pub start: Point<Pixels>,
    pub at: Point<Pixels>,
    pub moved: bool,
    /// 今の落とし先と、それを見たときのマウスの位置。
    pub over: Option<(DropTarget, Point<Pixels>)>,
}

/// ドラッグしたタスクを落とせる場所。
#[derive(Debug, Clone, PartialEq)]
pub enum DropTarget {
    /// 一覧のこの行の位置へ並べ替える。
    Row(usize),
    /// このワークスペースへ移す。`None` は未所属。
    Workspace(Option<Id>),
}

/// 見出しのドラッグ。押し始めた位置から少し動くまでは、ただのクリックとして扱う。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDrag {
    pub column: Column,
    pub start_x: f32,
    pub moved: bool,
    /// 今マウスが上にある列。
    pub over: Option<Column>,
}

/// 列の幅を変えられる範囲（拡大率を掛ける前の値）。
pub const COLUMN_MIN: f32 = 56.;
pub const COLUMN_MAX: f32 = 480.;
const COLUMNS_SETTING: &str = "table.columns";

/// `status:96,-assignee:160` の形。頭の `-` は隠している列。
fn format_columns(columns: &[TableColumn]) -> String {
    columns
        .iter()
        .map(|c| {
            let hidden = if c.hidden { "-" } else { "" };
            format!("{hidden}{}:{}", c.column.key(), c.width)
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 保存した列を読む。知らない名前や重複は飛ばし、保存になかった列は最後に足す。
fn parse_columns(saved: &str) -> Vec<TableColumn> {
    let mut columns: Vec<TableColumn> = Vec::new();
    for part in saved.split(',') {
        let (hidden, part) = match part.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, part),
        };
        let (key, width) = part.split_once(':').unwrap_or((part, ""));
        let Some(column) = Column::ALL.into_iter().find(|c| c.key() == key) else {
            continue;
        };
        if columns.iter().any(|c| c.column == column) {
            continue;
        }
        columns.push(TableColumn {
            column,
            width: width
                .parse::<f32>()
                .map_or(column.default_width(), |w| w.clamp(COLUMN_MIN, COLUMN_MAX)),
            hidden,
        });
    }
    for column in Column::ALL {
        if !columns.iter().any(|c| c.column == column) {
            columns.push(TableColumn::new(column));
        }
    }
    columns
}

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
        "x",
        "message-square-plus",
        "picture-in-picture-2",
        "panel-right",
        "copy",
        "eraser",
        "square",
        "arrow-up",
        "refresh-cw",
        "check",
        "play",
        "menu",
        "circle-question-mark",
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
        "rotate-ccw",
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
                .map(Into::into)
                .or_else(|| crate::icons::svg(name).map(Into::into))
                .or_else(|| icon_svg(DEFAULT_WORKSPACE_ICON).map(Into::into))),
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
