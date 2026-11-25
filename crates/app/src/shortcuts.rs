//! キーボードのショートカット。Windows と macOS の、それぞれのふつうのキーに合わせる。
//!
//! 一覧のキー（↑↓、Enter、Delete、F2 など）は、入力欄やメニューにフォーカスがないときだけ受ける。
//! 入力欄の中では、同じキーは文字の編集のまま。

use gpui_kit::*;
use malakt_core::{Id, TaskDraft};

use crate::panes::PaneOverlay;
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
        OpenSettings,
        ShowKeys,
        ToggleFullscreen,
        Reload
    ]
);

const MAC: bool = cfg!(target_os = "macos");

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
        KeyBinding::new(secondary!(","), OpenSettings, at),
        KeyBinding::new("f1", ShowKeys, at),
        KeyBinding::new(secondary!("/"), ShowKeys, at),
        KeyBinding::new("f5", Reload, at),
        KeyBinding::new(secondary!("r"), Reload, at),
    ];
    if MAC {
        keys.push(KeyBinding::new("cmd-backspace", DeleteSelected, at));
        keys.push(KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, None));
    } else {
        keys.push(KeyBinding::new("f11", ToggleFullscreen, None));
    }
    cx.bind_keys(keys);
    // 全画面は、AI チャットのウィンドウでも同じキーで切り替える
    cx.on_action(|_: &ToggleFullscreen, cx| {
        if let Some(window) = cx.active_window() {
            // 閉じたばかりのウィンドウなら、切り替える先がない
            let _ = window.update(cx, |_, window, _| window.toggle_fullscreen());
        }
    });
}

/// キーの一覧に出す、キーと説明。
pub fn help() -> Vec<(&'static str, &'static str)> {
    let pick = |windows: &'static str, mac: &'static str| if MAC { mac } else { windows };
    vec![
        (pick("Ctrl+N", "⌘N"), "新しいタスク"),
        (pick("Ctrl+Shift+N", "⇧⌘N"), "新しいウィンドウ"),
        (pick("Ctrl+F", "⌘F"), "検索"),
        ("↑ ↓", "タスクを選ぶ"),
        ("Home / End", "最初 / 最後のタスク"),
        ("Enter", "タスクを開く"),
        ("F2", "名前を変更"),
        (pick("Delete", "⌘⌫ / Delete"), "削除"),
        ("Space", "チェックを付ける / 外す"),
        ("Shift+↑ ↓", "続けてチェックを付ける"),
        (pick("Ctrl+A", "⌘A"), "すべてにチェックを付ける"),
        (pick("Ctrl+C", "⌘C"), "リンクをコピー"),
        (pick("Ctrl+D", "⌘D"), "複製"),
        ("Esc", "閉じる"),
        (pick("Ctrl+J", "⌘J"), "AI チャット"),
        (pick("Ctrl+,", "⌘,"), "フォントとカラー"),
        (pick("Ctrl+L", "⌘L"), "今すぐロック"),
        (pick("F11", "⌃⌘F"), "全画面"),
        (pick("F5", "⌘R"), "読み込み直す"),
        (pick("F1", "F1 / ⌘/"), "キー操作"),
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
        let d = anywhere::<NewTask>(d, cx, |v, window, cx| v.open_new_task(window, cx));
        let d = anywhere::<Find>(d, cx, |v, window, cx| v.find(window, cx));
        let d = anywhere::<Reload>(d, cx, |v, _, cx| v.reload(cx));
        let d = list::<Rename>(d, cx, |v, window, cx| v.rename(window, cx));
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
        d.on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
            if this.covered() {
                cx.propagate();
                return;
            }
            this.open_appearance(window, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowKeys, _, cx| this.toggle_keys(cx)))
    }

    /// キーの一覧を出す。出ていれば閉じる。
    pub fn toggle_keys(&mut self, cx: &mut Context<Self>) {
        self.pane_overlay =
            (self.pane_overlay != Some(PaneOverlay::Help)).then_some(PaneOverlay::Help);
        cx.notify();
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

    /// キーの操作の相手。チェックを付けたタスク、なければキーで選んでいる行、なければ開いているタスク。
    fn targets(&self) -> Vec<Id> {
        let selected = self.selected_ids();
        if !selected.is_empty() {
            return selected;
        }
        self.cursor_index()
            .map(|ix| self.tasks[self.visible[ix]].id.clone())
            .or_else(|| self.panel.clone())
            .into_iter()
            .collect()
    }

    fn move_cursor(&mut self, by: isize, extend: bool, cx: &mut Context<Self>) -> bool {
        if self.nav == Nav::Home || self.visible.is_empty() || self.editing.is_some() {
            return false;
        }
        let last = self.visible.len() - 1;
        let to = match self.cursor_index() {
            _ if by == isize::MIN => 0,
            _ if by == isize::MAX => last,
            Some(at) => at.saturating_add_signed(by).min(last),
            // まだ選んでいなければ、下へは最初の行、上へは最後の行から始める
            None if by < 0 => last,
            None => 0,
        };
        let id = self.tasks[self.visible[to]].id.clone();
        if extend {
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
        } else {
            self.select_anchor = Some(id.clone());
        }
        self.cursor = Some(id);
        self.list_scroll.scroll_to_item(to, ScrollStrategy::Top);
        cx.notify();
        true
    }

    fn toggle_cursor(&mut self, cx: &mut Context<Self>) -> bool {
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
        let Some(ix) = self.cursor_index() else {
            return false;
        };
        let id = self.tasks[self.visible[ix]].id.clone();
        self.open_panel(&id, window, cx);
        true
    }

    /// F2。選んでいるタスクのタイトルを、なければ見ているワークスペースの名前を変える。
    fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.editing.is_some() {
            return false;
        }
        let task = self
            .cursor_index()
            .map(|ix| self.tasks[self.visible[ix]].id.clone())
            .or_else(|| self.panel.clone());
        if let Some(id) = task {
            if self.panel.as_deref() != Some(id.as_str()) {
                self.open_panel(&id, window, cx);
            }
            self.start_editing(Editing::Title, window, cx);
            return self.editing.is_some();
        }
        if let Nav::Workspace(id) = self.nav.clone() {
            self.ask_workspace_name(None, Some(id), window, cx);
            return true;
        }
        false
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
        self.notice = Some(if ids.len() == 1 {
            "リンクをコピーしました。".into()
        } else {
            format!("{} 件のリンクをコピーしました。", ids.len()).into()
        });
        cx.notify();
        true
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
            self.notice = Some(format!("{} 件のタスクを複製しました。", ids.len()).into());
            // 続けて名前を変えたり開いたりできるよう、できたタスクを選ぶ
            self.cursor = last;
            cx.notify();
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
