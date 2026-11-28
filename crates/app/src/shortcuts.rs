//! キーボードのショートカット。Windows と macOS の、それぞれのふつうのキーに合わせる。
//!
//! 一覧のキー（↑↓、Enter、Delete、F2 など）は、入力欄やメニューにフォーカスがないときだけ受ける。
//! 入力欄の中では、同じキーは文字の編集のまま。

use gpui_kit::*;
use malakt_core::{Id, TaskDraft};

use crate::panes::PaneOverlay;
use malakt_core::view::ListRow;

use crate::state::{Editing, MainView, Modal, Nav};

actions!(
    malakt,
    [
        NewTask,
        Find,
        Rename,
        DeleteSelected,
        OpenSelected,
        CursorUp,
        CursorDown,
        CursorFirst,
        CursorLast,
        CursorPageUp,
        CursorPageDown,
        ExtendUp,
        ExtendDown,
        ToggleRow,
        SelectAll,
        CopyLink,
        Duplicate,
        Paste,
        Undo,
        Redo,
        OpenSettings,
        ShowKeys,
        ToggleFullscreen,
        Reload
    ]
);

const MAC: bool = cfg!(target_os = "macos");

/// 1 回の貼り付けで作るタスクの数の上限。
const PASTE_MAX: usize = 200;

/// PageUp / PageDown で動く行の数。
const PAGE: isize = 10;

/// macOS では ⌘、ほかでは Ctrl と一緒に押すキー。
#[macro_export]
macro_rules! secondary {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

/// キーを割り当てる。
pub fn bind(cx: &mut App) {
    let at = Some("MainView");
    let mut keys = vec![
        KeyBinding::new(secondary!("n"), NewTask, at),
        KeyBinding::new(secondary!("f"), Find, at),
        KeyBinding::new("f2", Rename, at),
        KeyBinding::new("delete", DeleteSelected, at),
        KeyBinding::new("enter", OpenSelected, at),
        KeyBinding::new("up", CursorUp, at),
        KeyBinding::new("down", CursorDown, at),
        KeyBinding::new("home", CursorFirst, at),
        KeyBinding::new("end", CursorLast, at),
        KeyBinding::new("pageup", CursorPageUp, at),
        KeyBinding::new("pagedown", CursorPageDown, at),
        KeyBinding::new("shift-up", ExtendUp, at),
        KeyBinding::new("shift-down", ExtendDown, at),
        KeyBinding::new("space", ToggleRow, at),
        KeyBinding::new(secondary!("a"), SelectAll, at),
        KeyBinding::new(secondary!("c"), CopyLink, at),
        KeyBinding::new(secondary!("d"), Duplicate, at),
        KeyBinding::new(secondary!("v"), Paste, at),
        KeyBinding::new(secondary!("z"), Undo, at),
        KeyBinding::new(secondary!("shift-z"), Redo, at),
        KeyBinding::new(secondary!(","), OpenSettings, at),
        // キー操作の一覧は、どのウィンドウ・どの場所でも開く
        KeyBinding::new("f1", ShowKeys, None),
        KeyBinding::new(secondary!("/"), ShowKeys, None),
        KeyBinding::new(secondary!("shift-/"), ShowKeys, None),
        // メニューには最後に割り当てたキー（Ctrl+? / ⌘?）が出る
        KeyBinding::new(secondary!("?"), ShowKeys, None),
        KeyBinding::new("f5", Reload, at),
        KeyBinding::new(secondary!("r"), Reload, at),
    ];
    if MAC {
        keys.push(KeyBinding::new("cmd-backspace", DeleteSelected, at));
        keys.push(KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, None));
    } else {
        keys.push(KeyBinding::new("f11", ToggleFullscreen, None));
        keys.push(KeyBinding::new("ctrl-y", Redo, at));
    }
    cx.bind_keys(keys);
    // 全画面は、AI チャットのウィンドウでも同じキーで切り替える。画面の中で受けなかったときは、前に出ているウィンドウ
    cx.on_action(|_: &ToggleFullscreen, cx| {
        if let Some(window) = cx.active_window() {
            // 閉じたばかりのウィンドウなら、切り替える先がない
            let _ = window.update(cx, |_, window, _| window.toggle_fullscreen());
        }
    });
}

/// 「今すぐロック」のキー。
pub const LOCK_KEY: &str = if MAC { "⇧⌘L" } else { "Ctrl+Shift+L" };
/// 「新しいウィンドウ」のキー。
pub const NEW_WINDOW_KEY: &str = if MAC { "⇧⌘N" } else { "Ctrl+Shift+N" };

/// 「キー操作」の一覧を開くキー。
pub const KEYS_KEY: &str = if MAC { "⌘?" } else { "Ctrl+?" };

fn pick(windows: &'static str, mac: &'static str) -> &'static str {
    if MAC {
        mac
    } else {
        windows
    }
}

/// キーの一覧に出す、タスクの画面のキー。
pub fn list_help() -> Vec<(&'static str, &'static str)> {
    vec![
        (pick("Ctrl+N", "⌘N"), "新しいタスク"),
        (pick("Ctrl+F", "⌘F"), "検索"),
        ("↑ ↓", "タスクを選ぶ"),
        ("Home / End", "最初 / 最後のタスク"),
        ("PageUp / PageDown", "10 行ずつ動く"),
        ("Enter", "タスクを開く"),
        ("F2", "名前を変更"),
        (pick("Delete", "⌘⌫ / Delete"), "削除"),
        ("Space", "チェックを付ける / 外す"),
        ("Shift+↑ ↓", "続けてチェックを付ける"),
        (pick("Ctrl+A", "⌘A"), "すべてにチェックを付ける"),
        (pick("Ctrl+C", "⌘C"), "リンクをコピー"),
        (pick("Ctrl+D", "⌘D"), "複製"),
        (pick("Ctrl+V", "⌘V"), "貼り付けてタスクを追加"),
        (pick("Ctrl+Z", "⌘Z"), "元に戻す"),
        (pick("Ctrl+Y / Ctrl+Shift+Z", "⇧⌘Z"), "やり直す"),
        ("Esc", "閉じる"),
        (pick("Ctrl+J", "⌘J"), "AI チャット"),
        (pick("Ctrl+,", "⌘,"), "設定"),
        (pick("F5 / Ctrl+R", "⌘R"), "読み込み直す"),
        ("Ctrl+B", "ペインの操作"),
    ]
}

/// キーの一覧に出す、どのウィンドウでも使えるキー。
pub fn app_help() -> Vec<(&'static str, &'static str)> {
    vec![
        (pick("Ctrl+? / Ctrl+/ / F1", "⌘? / ⌘/ / F1"), "キー操作"),
        (NEW_WINDOW_KEY, "新しいウィンドウ"),
        (LOCK_KEY, "今すぐロック"),
        (pick("F11", "⌃⌘F"), "全画面"),
        (pick("Ctrl+Q", "⌘Q"), "終了"),
    ]
}

impl MainView {
    /// キーの割り当てを受けるところを、いちばん外の要素に付ける。
    pub(crate) fn with_shortcuts(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        /// 一覧のキー。入力欄やメニューの中では受けず、そちらに渡す。
        fn list<A: Action>(
            d: Stateful<Div>,
            cx: &mut Context<MainView>,
            run: fn(&mut MainView, &mut Window, &mut Context<MainView>) -> bool,
        ) -> Stateful<Div> {
            d.on_action(cx.listener(move |this, _: &A, window, cx| {
                if !this.list_keys(window) || !this.in_pane(window, cx, run) {
                    cx.propagate();
                }
            }))
        }
        /// どこにフォーカスがあっても受けるキー。
        fn anywhere<A: Action>(
            d: Stateful<Div>,
            cx: &mut Context<MainView>,
            run: fn(&mut MainView, &mut Window, &mut Context<MainView>),
        ) -> Stateful<Div> {
            d.on_action(cx.listener(move |this, _: &A, window, cx| {
                if this.covered() {
                    cx.propagate();
                    return;
                }
                this.in_pane(window, cx, |view, window, cx| {
                    run(view, window, cx);
                    true
                });
            }))
        }
        let d = anywhere::<NewTask>(d, cx, |v, window, cx| v.add_task(window, cx));
        let d = anywhere::<Find>(d, cx, |v, window, cx| v.find(window, cx));
        let d = anywhere::<Reload>(d, cx, |v, _, cx| v.reload(cx));
        // F2 は、選んでいるタスクがなければ、見ているワークスペースの名前を左のメニューで変える
        let d = d.on_action(cx.listener(|this, _: &Rename, window, cx| {
            if !this.list_keys(window) {
                cx.propagate();
                return;
            }
            if this.in_pane(window, cx, |v, window, cx| v.rename_task(window, cx)) {
                return;
            }
            match this.shown_nav(cx) {
                Nav::Workspace(id) => this.rename_workspace(id, window, cx),
                _ => cx.propagate(),
            }
        }));
        let d = list::<DeleteSelected>(d, cx, |v, _, cx| v.delete_targets(cx));
        let d = list::<OpenSelected>(d, cx, |v, window, cx| v.open_cursor(window, cx));
        let d = list::<CursorUp>(d, cx, |v, _, cx| v.move_cursor(-1, false, cx));
        let d = list::<CursorDown>(d, cx, |v, _, cx| v.move_cursor(1, false, cx));
        let d = list::<CursorPageUp>(d, cx, |v, _, cx| v.move_cursor(-PAGE, false, cx));
        let d = list::<CursorPageDown>(d, cx, |v, _, cx| v.move_cursor(PAGE, false, cx));
        let d = list::<CursorFirst>(d, cx, |v, _, cx| v.move_cursor(isize::MIN, false, cx));
        let d = list::<CursorLast>(d, cx, |v, _, cx| v.move_cursor(isize::MAX, false, cx));
        let d = list::<ExtendUp>(d, cx, |v, _, cx| v.move_cursor(-1, true, cx));
        let d = list::<ExtendDown>(d, cx, |v, _, cx| v.move_cursor(1, true, cx));
        let d = list::<ToggleRow>(d, cx, |v, _, cx| v.toggle_cursor(cx));
        let d = list::<SelectAll>(d, cx, |v, _, cx| v.select_all(cx));
        let d = list::<CopyLink>(d, cx, |v, _, cx| v.copy_links(cx));
        let d = list::<Duplicate>(d, cx, |v, _, cx| v.duplicate_targets(cx));
        let d = list::<Paste>(d, cx, |v, _, cx| v.paste(cx));
        // 元に戻す・やり直しは、どのペインで押しても、データ全体のいちばん新しい操作に働く
        let d = d
            .on_action(cx.listener(|this, _: &Undo, window, cx| {
                if !this.list_keys(window) || !this.undo(false, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Redo, window, cx| {
                if !this.list_keys(window) || !this.undo(true, cx) {
                    cx.propagate();
                }
            }));
        d.on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
            if this.covered() {
                cx.propagate();
                return;
            }
            this.open_appearance(window, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowKeys, window, cx| this.toggle_keys(window, cx)))
        // 押したウィンドウを全画面にする（ブラウザのページの中で押したときも、そのウィンドウ）
        .on_action(|_: &ToggleFullscreen, window, _| window.toggle_fullscreen())
    }

    /// キーの一覧を出す。出ていれば閉じる。
    pub fn toggle_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane_overlay == Some(PaneOverlay::Help) {
            self.pane_overlay = None;
            cx.notify();
        } else {
            self.open_keys(window, cx);
        }
    }

    /// キーの一覧を出す。今のキーの入力先の場所のキーを先頭にする。
    pub fn open_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane_overlay == Some(PaneOverlay::Help) {
            return;
        }
        let panes = self.pane_keys;
        let help = cx.new(|cx| crate::keys_help::KeysHelp::new(Some(panes), window, cx));
        cx.subscribe_in(&help, window, |this, _, _: &DismissEvent, _, cx| {
            if this.pane_overlay == Some(PaneOverlay::Help) {
                this.pane_overlay = None;
                cx.notify();
            }
        })
        .detach();
        self.keys_help = Some(help);
        self.pane_overlay = Some(PaneOverlay::Help);
        cx.notify();
    }

    /// キーの一覧を閉じたあと、開く前のキーの入力先に戻す。描くたびに呼ぶ。
    pub(crate) fn return_from_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane_overlay == Some(PaneOverlay::Help) {
            return;
        }
        let Some(help) = &self.keys_help else {
            return;
        };
        if let Some(back) = help.update(cx, |help, _| help.take_back()) {
            match back {
                Some(back) => window.focus(&back, cx),
                None => self.focus.focus(window, cx),
            }
        }
    }

    /// ダイアログや、ペインのキーの待ちが上に出ている。
    fn covered(&self) -> bool {
        self.modal != Modal::None
            || self.sub_modal.is_some()
            || self.pane_overlay.is_some()
            || self.pane_prefix
            || self.pane_resize
    }

    /// 一覧のキーを受けてよいか。入力欄・メニュー・AI チャットにフォーカスがあるときは受けない。
    fn list_keys(&self, window: &Window) -> bool {
        !self.covered()
            && window
                .context_stack()
                .last()
                .is_some_and(|context| context.contains("MainView"))
    }

    /// 今のペインの中身に、キーの操作をさせる。
    fn in_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        run: impl FnOnce(&mut MainView, &mut Window, &mut Context<MainView>) -> bool,
    ) -> bool {
        match self.focused_child() {
            Some(child) => child.update(cx, |child, cx| run(child, window, cx)),
            None => run(self, window, cx),
        }
    }

    /// キーで選んでいる行の位置。
    pub fn cursor_index(&self) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        self.visible
            .iter()
            .position(|&i| &self.tasks[i].id == cursor)
    }

    /// キーで選んでいる行が、一覧の何行目か。タスクでも、サブワークスペースの見出しでも。
    fn cursor_row(&self) -> Option<usize> {
        match self.cursor_index() {
            Some(ix) => self
                .rows
                .iter()
                .position(|row| matches!(row, ListRow::Task { index, .. } if *index == ix)),
            None => self.cursor_folder().map(|(row, _)| row),
        }
    }

    /// 一覧のその行の、タスクかワークスペースの ID。
    fn row_id(&self, row: usize) -> Id {
        match self.rows[row] {
            ListRow::Task { index, .. } => self.tasks[self.visible[index]].id.clone(),
            ListRow::Folder { workspace, .. } => self.workspaces[workspace].id.clone(),
        }
    }

    /// 作ったばかりのタスクを読み込んで、キーで選んでいる行が見えるところまで一覧を動かす。
    fn reveal_cursor(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
        if let Some(row) = self.cursor_row() {
            self.list_scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
    }

    /// キーの操作の相手。チェックを付けたタスク、なければキーで選んでいる行、なければ開いているタスク。
    fn targets(&self) -> Vec<Id> {
        let selected = self.selected_ids();
        if !selected.is_empty() {
            return selected;
        }
        // サブワークスペースの見出しを選んでいる間は、タスクを相手にしない
        if self.cursor_folder().is_some() {
            return Vec::new();
        }
        self.cursor_index()
            .map(|ix| self.tasks[self.visible[ix]].id.clone())
            .or_else(|| self.panel.clone())
            .into_iter()
            .collect()
    }

    fn move_cursor(&mut self, by: isize, extend: bool, cx: &mut Context<Self>) -> bool {
        if self.nav == Nav::Home || self.editing.is_some() {
            return false;
        }
        if extend {
            return self.extend_cursor(by, cx);
        }
        // サブワークスペースの見出しにも止まる
        if self.rows.is_empty() {
            return false;
        }
        let last = self.rows.len() - 1;
        let to = match self.cursor_row() {
            _ if by == isize::MIN => 0,
            _ if by == isize::MAX => last,
            Some(at) => at.saturating_add_signed(by).min(last),
            // まだ選んでいなければ、下へは最初の行、上へは最後の行から始める
            None if by < 0 => last,
            None => 0,
        };
        let id = self.row_id(to);
        if matches!(self.rows[to], ListRow::Task { .. }) {
            self.select_anchor = Some(id.clone());
        }
        self.cursor = Some(id);
        self.list_scroll.scroll_to_item(to, ScrollStrategy::Top);
        cx.notify();
        true
    }

    /// Shift を押しながらの ↑↓。タスクだけを相手に、範囲をまとめて選ぶ。
    fn extend_cursor(&mut self, by: isize, cx: &mut Context<Self>) -> bool {
        if self.visible.is_empty() {
            return false;
        }
        let last = self.visible.len() - 1;
        // 見出しを選んでいるときは、その行から進む向きにある最初のタスクへ移る
        let beside = self.cursor_folder().map(|(row, _)| {
            let task = |row: &ListRow| match *row {
                ListRow::Task { index, .. } => Some(index),
                ListRow::Folder { .. } => None,
            };
            if by < 0 {
                self.rows[..row].iter().rev().find_map(task)
            } else {
                self.rows[row..].iter().find_map(task)
            }
        });
        let to = match (self.cursor_index(), beside) {
            (Some(at), _) => at.saturating_add_signed(by).min(last),
            (None, Some(Some(at))) => at,
            // その向きにタスクがなければ、動かさない
            (None, Some(None)) => return true,
            (None, None) if by < 0 => last,
            (None, None) => 0,
        };
        let id = self.tasks[self.visible[to]].id.clone();
        let from = self
            .select_anchor
            .as_ref()
            .and_then(|anchor| {
                self.visible
                    .iter()
                    .position(|&i| &self.tasks[i].id == anchor)
            })
            .unwrap_or(to);
        self.selected = (from.min(to)..=from.max(to))
            .map(|ix| self.tasks[self.visible[ix]].id.clone())
            .collect();
        if self.select_anchor.is_none() {
            self.select_anchor = Some(id.clone());
        }
        self.cursor = Some(id);
        if let Some(row) = self.cursor_row() {
            self.list_scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
        cx.notify();
        true
    }

    fn toggle_cursor(&mut self, cx: &mut Context<Self>) -> bool {
        // 見出しなら、開く・閉じる
        if let Some(id) = self.cursor_folder().map(|(_, w)| w.id.clone()) {
            self.toggle_list_folder(&id, cx);
            return true;
        }
        let Some(ix) = self.cursor_index() else {
            return false;
        };
        let id = self.tasks[self.visible[ix]].id.clone();
        self.click_select(&id, false, cx);
        true
    }

    fn select_all(&mut self, cx: &mut Context<Self>) -> bool {
        if self.nav == Nav::Home || self.visible.is_empty() {
            return false;
        }
        self.selected = self
            .visible
            .iter()
            .map(|&i| self.tasks[i].id.clone())
            .collect();
        cx.notify();
        true
    }

    fn open_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // 見出しなら、そのワークスペースへ移る
        if let Some(id) = self.cursor_folder().map(|(_, w)| w.id.clone()) {
            self.go_here(Nav::Workspace(id), cx);
            return true;
        }
        let Some(ix) = self.cursor_index() else {
            return false;
        };
        let id = self.tasks[self.visible[ix]].id.clone();
        self.open_panel(&id, window, cx);
        true
    }

    /// F2。選んでいるタスクのタイトルを、詳細でその場で変える。相手のタスクがなければ `false`。
    fn rename_task(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.editing.is_some() {
            return true;
        }
        let task = self
            .cursor_index()
            .map(|ix| self.tasks[self.visible[ix]].id.clone())
            .or_else(|| self.panel.clone());
        let Some(id) = task else {
            return false;
        };
        if self.panel.as_deref() != Some(id.as_str()) {
            self.open_panel(&id, window, cx);
        }
        self.start_editing(Editing::Title, window, cx);
        // すぐ打ち替えられるように、タイトルを全部選んでおく
        self.inputs
            .title
            .update(cx, |title, cx| title.select_all(window, cx));
        true
    }

    /// Delete。消す前に確認を出す。
    fn delete_targets(&mut self, cx: &mut Context<Self>) -> bool {
        let ids = self.targets();
        if ids.is_empty() {
            return false;
        }
        self.modal = Modal::DeleteTasks(ids);
        cx.notify();
        true
    }

    fn copy_links(&mut self, cx: &mut Context<Self>) -> bool {
        let ids = self.targets();
        if ids.is_empty() {
            return false;
        }
        let links: Vec<String> = ids
            .iter()
            .map(|id| malakt_core::tasklink::url(id))
            .collect();
        cx.write_to_clipboard(ClipboardItem::new_string(links.join("\n")));
        self.show_notice(if ids.len() == 1 {
            "リンクをコピーしました。".into()
        } else {
            format!("{} 件のリンクをコピーしました。", ids.len()).into()
        });
        cx.notify();
        true
    }

    /// 右クリックのメニューの「コピー」。選んでいる行の上なら選んでいる全部を、ほかの行ならその 1 件をコピーする。
    pub fn copy_task(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected.contains(id) {
            self.copy_links(cx);
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(malakt_core::tasklink::url(id)));
        self.show_notice("リンクをコピーしました。".into());
        cx.notify();
    }

    /// 右クリックのメニューの「貼り付け」。
    pub fn paste_here(&mut self, cx: &mut Context<Self>) {
        self.paste(cx);
    }

    /// 同じ中身のタスクを、同じワークスペースにもう 1 つ作る。
    fn duplicate_targets(&mut self, cx: &mut Context<Self>) -> bool {
        let ids = self.targets();
        if ids.is_empty() {
            return false;
        }
        let made = self.write(cx, |store| {
            let mut last = None;
            for id in &ids {
                let task = store.task(id)?;
                let mut draft = TaskDraft::from(&task);
                draft.title = format!("{} のコピー", task.title);
                last = Some(store.create_task(task.workspace_id.as_deref(), &draft)?.id);
            }
            Ok(last)
        });
        if let Some(last) = made {
            self.show_notice(format!("{} 件のタスクを複製しました。", ids.len()).into());
            // 続けて名前を変えたり開いたりできるよう、できたタスクを選ぶ
            self.cursor = last;
            self.reveal_cursor(cx);
        }
        true
    }

    /// いちばん新しい操作を元に戻す。`redo` なら、戻した操作をやり直す。相手がなければ `false`。
    pub fn undo(&mut self, redo: bool, cx: &mut Context<Self>) -> bool {
        let result = self.db.update(cx, |db, cx| {
            let result = if redo {
                db.store.redo()
            } else {
                db.store.undo()
            };
            cx.notify();
            result
        });
        match result {
            Ok(false) => return false,
            Ok(true) => {
                self.error = None;
                self.show_notice(
                    if redo {
                        "やり直しました。"
                    } else {
                        "元に戻しました。"
                    }
                    .into(),
                );
            }
            Err(error) => {
                let what = if redo {
                    "やり直せませんでした"
                } else {
                    "元に戻せませんでした"
                };
                self.error = Some(format!("{what}: {error}").into());
            }
        }
        cx.notify();
        true
    }

    /// クリップボードの中身からタスクを作る。タスクのリンクなら、そのタスクの写しを今の場所に作り、
    /// ほかの文字なら、1 行を 1 つのタスクにする。
    fn paste(&mut self, cx: &mut Context<Self>) -> bool {
        if self.nav == Nav::Home {
            return false;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return false;
        };
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            return false;
        }
        if lines.len() > PASTE_MAX {
            self.error = Some(
                format!(
                    "貼り付けられるのは {PASTE_MAX} 行までです（クリップボードは {} 行）。",
                    lines.len()
                )
                .into(),
            );
            cx.notify();
            return true;
        }
        let workspace = self.current_workspace().map(str::to_owned);
        let made = self.write(cx, |store| {
            // 先に全部の下書きを作り、1 つでも作れなければ何も足さない
            let mut drafts = Vec::new();
            for line in &lines {
                let linked = match line.strip_prefix(malakt_core::tasklink::PREFIX) {
                    Some(id) => store.task(id).ok(),
                    None => None,
                };
                drafts.push(match linked {
                    Some(task) => {
                        let mut draft = TaskDraft::from(&task);
                        // 親のタスクは元の場所にあるので、写しは親なしで作る
                        draft.parent_id = None;
                        draft
                    }
                    None => store.draft_for(workspace.as_deref(), line)?,
                });
            }
            let mut last = None;
            for draft in &drafts {
                last = Some(store.create_task_shown(workspace.as_deref(), draft)?.id);
            }
            Ok((drafts.len(), last))
        });
        if let Some((count, last)) = made {
            self.show_notice(format!("{count} 件のタスクを追加しました。").into());
            self.query.tab = malakt_core::view::Tab::Open;
            self.cursor = last;
            self.reveal_cursor(cx);
        }
        true
    }

    /// 検索欄にフォーカスする。ホームには一覧がないので、すべてのタスクを開く。
    fn find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav == Nav::Home {
            self.go_here(Nav::All, cx);
        }
        self.inputs
            .search
            .update(cx, |search, cx| search.focus(window, cx));
        cx.notify();
    }
}
