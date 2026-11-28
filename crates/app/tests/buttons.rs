//! 画面に出ているボタンを 1 つ残らず押して、押したら何かが変わることを確かめる。
//!
//! 画面ごとに、見えている押せるものをすべて拾い、1 つずつ新しい状態から押す。押して開いた
//! メニューやダイアログの中のものも、同じように 1 つずつ押す。画面もデータも変わらなかった
//! ものを「反応しないボタン」として並べ、1 つでもあれば失敗にする。
//!
//! 押したときの処理がないボタンは、`crate::press::button` の型でビルドの時点で止めている。
//! このテストは画面を全部開いて押すので重く、macOS の CI では止まったまま終わらなかった。
//! そのため `mise run test` と pre-commit からは外し、手で回す: `mise run test-buttons`

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, ElementId, Entity, Modifiers, MouseButton,
    Pixels, Point, Role, Size, TestAppContext, TestDispatcher, VisualTestContext, WindowBounds,
    WindowOptions,
};
use malakt_core::panes::{Action, Dir};
use malakt_core::view::Priority;
use malakt_core::{NewComment, Scope, Status, Store, TaskDraft, TaskFilter};
use malakt_mahoz::state::{Column, CustomizeSection, Editing};
use malakt_mahoz::{db, MainView, Nav};

/// 押せるものではない入れ物や入力欄。押しても何も変わらないのが正しい。
const NOT_BUTTONS: &[&str] = &[
    "appearance-scroll",
    "pane",
    "pane-bar",
    "pane-border",
    "pane-mode",
    "pane-view",
    "main-view",
    "sidebar",
    "sidebar-nav",
    "sidebar-resize",
    "rail",
    "rail-nav",
    "detail-panel",
    "dialog",
    // ダイアログの外側の暗い所。真ん中にはダイアログがあるので、端を押して別に確かめる
    "dialog-backdrop",
    "dialog-under-backdrop",
    // メニューの入れ物。中の項目は 1 つずつ押す
    "popup-menu",
    "col-head",
    "col-resize",
    "input-state",
    "input",
    "task-search",
    "quick-add",
    "list-page",
    "icon-list",
    "icon-search",
    "dialog-under",
    "font-list",
    "font-search",
    "customize-search",
    "history-list",
    "notices-scroll",
    "ws-settings-scroll",
    "home",
    "row-cursor",
    "folder-cursor",
    "keys-help",
    "app-keys",
    "browser-keys",
    "terminal-keys",
    // ブラウザの入れ物と、ページ・失敗・使用量の文字。ボタンは tests/browser.rs で押す
    "browser",
    "browser-page",
    "browser-error",
    "browser-usage",
    "browser-find",
    "browser-find-input",
    "browser-find-missing",
];

/// 押せるものの縦と横の、いちばん小さい大きさ（拡大率を掛ける前の値）。
const MIN_HIT: f32 = 17.5;
/// スイッチは部品の高さが決まっている。ラベルごと押せるので、横に広い。
const WIDE_SWITCHES: &[&str] = &["cards-switch", "float-switch", "lock-switch"];

/// 詳細のパネルの上の端に並ぶボタン。
const PANEL_BAR: &[&str] = &["copy-link", "ticket-open", "close-panel"];

/// 押さないもの。外のアプリで開く操作は、テストの土台では動かせない。
/// ウィンドウの最小化・最大化・閉じる（Windows のタイトルバー）は OS が受けるので、押しても画面は変わらない。
const NEVER: &[&str] = &[
    "att-open",
    "window-minimize",
    "window-maximize",
    "window-restore",
    "window-close",
];

/// フォントとカラーの設定で、必ず押すもの。
const MUST: &[&str] = &[
    "preset",
    "preset-save",
    "layout",
    "own-preset",
    "own-preset-name",
    "own-preset-delete",
    "customize-nav",
    "export-all",
    "import-all",
    "font-open",
    "font",
    "font-reset",
    "accent-reset",
    "light-reset",
    "dark-reset",
    "color-open",
    "scene-open",
    "scene",
    "scene-reset",
    "clear-slider",
    "blur-slider",
    "cards-switch",
    "float-switch",
    "density",
    "due-format",
    "feel",
    "shell-choice",
    "lock-switch",
    "auto-lock",
    "locked-notices",
    "alert-timed",
    "alert-date",
    "alert-hour",
    "task-alerts",
];

/// ロックを設定したときだけ出るもの。ロックは tests/ui.rs で確かめる。
const LOCK: &[&str] = &["lock-disable", "lock-save", "unlock"];

/// アイコンを選ぶ画面のもの。カテゴリは 5 つとも tests/ui.rs で押して確かめる。
/// 画像を追加するボタンは OS のファイル選択を開くので、選んだあとの動きを tests/ui.rs で確かめる。
const ICON_PICKER: &[&str] = &["icon-category", "icon-add-image"];

/// 入力欄。名前が `-input` で終わるもののほかに、ここにあるもの。
const INPUTS: &[&str] = &["ws-term-dir", "def-categories"];

/// 押しても画面とデータが変わらないのが正しいもの。ID（番号つきは番号まで）と理由。
const QUIET: &[(&str, &str)] = &[
    // OS の画面や外のアプリを開く。テストの中では何も出ない
    ("att-add", "OS のファイル選択を開く"),
    ("att-save#0", "OS の保存先の選択を開く"),
    ("ws-export", "OS の保存先の選択を開く"),
    ("ws-import", "OS のファイル選択を開く"),
    // 今いる場所・今選んでいる値をもう一度押す
    ("nav-home", "今いる画面"),
    ("rail-home", "今いる画面"),
    ("nav-all", "今いる画面"),
    ("tab-open", "今のタブ"),
    ("ws#0", "今いるワークスペース"),
    ("タスクの詳細 > title#1", "今開いているタスク"),
    ("説明を編集中の詳細 > title#1", "今開いているタスク"),
    ("担当を選んでいる詳細 > title#1", "今開いているタスク"),
    (
        "見出しを右クリックしたメニュー > due#3",
        "見出しのメニューの下に隠れている",
    ),
    ("def-prio#0", "今選んでいる優先度"),
    ("def-due-none", "今選んでいる期限の決め方"),
    ("bulk-mode#0", "今選んでいる変え方"),
    ("bulk-cat-mode#0", "今選んでいる変え方"),
    ("ticket-who#0", "今選んでいる人"),
    ("font#0", "今選んでいるフォント"),
    ("scene#0", "今選んでいる背景の画像"),
    ("カスタマイズ > preset#0", "今選んでいるテーマ"),
    (
        "カスタマイズで検索したところ > preset#0",
        "今選んでいるテーマ",
    ),
    ("カスタマイズ > customize-nav#0", "今開いているカテゴリ"),
    ("ホーム > customize-nav#0", "いちばん上に見えている見出し"),
    (
        "担当者のダイアログ > customize-nav#12",
        "今開いているカテゴリ",
    ),
    (
        "カスタマイズ（自分のテーマあり） > customize-nav#0",
        "今開いているカテゴリ",
    ),
    (
        "カスタマイズのレイアウト > customize-nav#1",
        "今開いているカテゴリ",
    ),
    (
        "カスタマイズの背景 > customize-nav#2",
        "今開いているカテゴリ",
    ),
    (
        "背景の画像を出しているカスタマイズ > customize-nav#2",
        "今開いているカテゴリ",
    ),
    (
        "カスタマイズの色と書体 > customize-nav#3",
        "今開いているカテゴリ",
    ),
    (
        "カスタマイズを変えたところ > customize-nav#3",
        "今開いているカテゴリ",
    ),
    (
        "カスタマイズの表示 > customize-nav#4",
        "今開いているカテゴリ",
    ),
    (
        "設定の AI のモデル > customize-nav#5",
        "今開いているカテゴリ",
    ),
    ("設定の AI の動作 > customize-nav#6", "今開いているカテゴリ"),
    (
        "設定の AI のルール > customize-nav#7",
        "今開いているカテゴリ",
    ),
    ("設定の GitHub > customize-nav#8", "今開いているカテゴリ"),
    (
        "ロックのダイアログ > customize-nav#9",
        "今開いているカテゴリ",
    ),
    (
        "ロックを有効にしたダイアログ > customize-nav#9",
        "今開いているカテゴリ",
    ),
    (
        "ロックを有効にした通知 > customize-nav#10",
        "今開いているカテゴリ",
    ),
    ("設定のデータ > customize-nav#15", "今開いているカテゴリ"),
    (
        "設定のデータ > customize-nav#14",
        "いちばん下までスクロールしていて、ウィンドウの見出しはもう見えている",
    ),
    ("export-all", "OS の保存先の選択を開く"),
    ("import-all", "OS のファイル選択を開く"),
    ("設定の通知 > customize-nav#10", "今開いているカテゴリ"),
    ("alert-hour#0", "今選んでいる時刻"),
    (
        "カスタマイズ（自分のテーマあり） > preset#0",
        "今選んでいるテーマ",
    ),
    ("own-preset#0", "今選んでいるテーマ"),
    ("layout#0", "今選んでいるレイアウト"),
    ("density#1", "今選んでいる密度"),
    ("due-format#0", "今選んでいる期限の表示"),
    ("feel#0", "今選んでいるホバー"),
    ("scene-file", "OS のファイル選択を開く"),
    // 入力が空のときは送らない
    ("pane-keys#0", "今選んでいるキーの割り当て"),
    ("quick-add-submit", "タイトルが空"),
    ("comment-submit", "コメントが空"),
    ("comment-review", "コメントが空"),
];

/// AI チャットの中は tests/ai.rs で、ターミナルの中は tests/term.rs で確かめる。
/// ここでは開け閉めだけを見る。
fn ai_inside(name: &str) -> bool {
    name.starts_with("ai-") && name != "ai-toggle" || name.starts_with("term-")
}

struct World {
    window: AnyWindowHandle,
    view: Entity<MainView>,
    workspace: String,
    task: String,
}

fn window_size(width: f32, height: f32) -> Size<Pixels> {
    let scale = malakt_mahoz::UI_SCALE;
    size(px(width * scale), px(height * scale))
}

/// ひととおりのデータが入った保存先。どの画面にも、押せるものが出そろうようにする。
fn seed() -> (Store, String, String) {
    let mut s = Store::open_in_memory().unwrap();
    s.ensure_self().unwrap();
    let me = s.people().unwrap().into_iter().find(|p| p.is_self).unwrap();
    let sato = s.create_person("佐藤", false).unwrap();
    let w = s.create_workspace(None, "文化祭").unwrap();
    s.create_workspace(Some(&w.id), "会場").unwrap();
    let other = s.create_workspace(None, "引っ越し").unwrap();
    let draft = TaskDraft {
        title: "会場を決める".into(),
        description: "候補を 3 つ出して、下見をする。".into(),
        priority: 1,
        // 期限が今日のタスクの次に並ぶ
        start: Some(malakt_core::view::today()),
        due: Some(malakt_core::view::add_days(&malakt_core::view::today(), 3)),
        assignee_ids: vec![me.id.clone(), sato.id.clone()],
        categories: vec!["設営".into()],
        ..Default::default()
    };
    let task = s.create_task(Some(&w.id), &draft).unwrap();
    let sub = TaskDraft {
        title: "下見の日を決める".into(),
        parent_id: Some(task.id.clone()),
        ..Default::default()
    };
    s.create_task(Some(&w.id), &sub).unwrap();
    s.link_task(&task.id, &other.id).unwrap();
    let pinned = s
        .add_comment(&task.id, Some(&me.id), "駅から近いところ")
        .unwrap();
    s.set_comment_pinned(&pinned.id, true).unwrap();
    s.add_comment(&task.id, Some(&sato.id), "予算は 5 万円まで")
        .unwrap();
    let review = NewComment {
        body: "下見の日程も書いてください".into(),
        review: true,
        ..Default::default()
    };
    s.add_comment_with(&task.id, Some(&sato.id), &review)
        .unwrap();
    s.add_attachment(&task.id, Some(&me.id), "見取り図.txt", b"plan")
        .unwrap();
    // 期限が今日のタスクは、通知に出る。日付だけの期限は 0:00 に知らせ、テストを走らせる時刻によらず出す
    s.set_due_alert_hour(0).unwrap();
    let today = TaskDraft {
        title: "申込書を出す".into(),
        due: Some(malakt_core::view::today()),
        assignee_ids: vec![me.id.clone()],
        ..Default::default()
    };
    s.create_task(Some(&w.id), &today).unwrap();
    s.set_task_status(&task.id, Status::InProcess).unwrap();
    s.add_task_in(None, "機材を借りる").unwrap();
    let done = s.add_task_in(Some(&other.id), "段ボールを集める").unwrap();
    s.set_task_status(&done.id, Status::Completed).unwrap();
    (s, w.id, task.id)
}

/// 新しいデータと新しいウィンドウで始める。前のウィンドウは閉じる。
fn fresh(cx: &mut TestAppContext, size: Size<Pixels>) -> World {
    let (store, workspace, task) = seed();
    cx.update(|cx| {
        for window in cx.windows() {
            // 閉じたあとのウィンドウなら、することがない
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        db::shared(cx).update(cx, |db, _| {
            db.store = store;
            // 前の画面で「今すぐロック」を押していても、ロックを解いて始める
            db.unlocked = true;
        });
        // 前の画面で開いたターミナルとブラウザのタブを閉じる
        malakt_mahoz::term::shared(cx).update(cx, |terms, cx| terms.close_all(cx));
        // 前の画面で変えた書体と色を、新しいデータ（何も選んでいない）に合わせて戻す
        malakt_mahoz::theme::init(cx);
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(String::new()));
    });
    cx.run_until_parked();
    let (window, view) = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size,
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let mut view = MainView::new(window, cx);
                view.motion = false;
                view
            })
        })
        .expect("ウィンドウを開く")
    });
    let window: AnyWindowHandle = window;
    cx.run_until_parked();
    World {
        window,
        view,
        workspace,
        task,
    }
}

fn frame(cx: &mut TestAppContext, world: &World) {
    cx.update_window(world.window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(world.window, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// `QUIET` と照らす書き方。番号つきは `名前#番号`。
fn key_of(id: &ElementId) -> String {
    match id {
        ElementId::NamedInteger(name, n) => format!("{name}#{n}"),
        other => name_of(other),
    }
}

fn name_of(id: &ElementId) -> String {
    match id {
        ElementId::Name(name) => name.to_string(),
        ElementId::NamedInteger(name, _) => name.to_string(),
        other => format!("{other:?}"),
    }
}

/// 今の画面で押せるもの（見えていて、入れ物や入力欄ではないもの）。
fn targets(cx: &mut TestAppContext, world: &World) -> Vec<ElementId> {
    cx.update_window(world.window, |_, window, _| {
        let elements = window.elements();
        // 詳細のパネルは一覧の上に重なる。下に隠れたものは押せない
        let panel = elements
            .iter()
            .find(|e| {
                e.path()
                    .last()
                    .is_some_and(|id| name_of(id) == "detail-panel")
            })
            .map(|e| e.bounds());
        // ダイアログが開いている間は、その中のものしか押せない
        let dialog = elements
            .iter()
            .any(|e| e.path().last().is_some_and(|id| name_of(id) == "dialog"));
        // 一覧の下の端で半分だけ見えている行は、スクロールしないと押せない。一覧は追加行の上で終わる
        let list_bottom = elements
            .iter()
            .find(|e| e.path().last().is_some_and(|id| name_of(id) == "quick-add"))
            .map(|e| e.bounds().top() - px(12.));
        // 低い画面では一覧が画面ごとスクロールする。その下の端より下の行も、スクロールしないと押せない
        let page_bottom = elements
            .iter()
            .find(|e| e.path().last().is_some_and(|id| name_of(id) == "list-page"))
            .map(|e| e.bounds().bottom());
        let list_bottom = match (list_bottom, page_bottom) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // 「設定」のスクロールの上下の端で切れているものも、スクロールしないと押せない
        let settings_view = elements
            .iter()
            .find(|e| {
                e.path()
                    .last()
                    .is_some_and(|id| name_of(id) == "appearance-scroll")
            })
            .map(|e| e.bounds());
        let mut found: Vec<ElementId> = Vec::new();
        for element in &elements {
            if settings_view.is_some_and(|view| {
                let bounds = element.bounds();
                (bounds.top() < view.top() || bounds.bottom() > view.bottom())
                    && element
                        .path()
                        .iter()
                        .any(|id| name_of(id) == "appearance-scroll")
            }) {
                continue;
            }
            if list_bottom.is_some_and(|bottom| {
                element.bounds().center().y > bottom
                    && element.path().iter().any(|id| name_of(id) == "task-list")
            }) {
                continue;
            }
            let Some(id) = element.path().last() else {
                continue;
            };
            let name = name_of(id);
            if !element.visible()
                || element.disabled() == Some(true)
                || matches!(
                    element.role(),
                    Some(Role::TextInput | Role::MultilineTextInput)
                )
                || NOT_BUTTONS.contains(&name.as_str())
                || INPUTS.contains(&name.as_str())
                || NEVER.contains(&name.as_str())
                || (dialog && !element.path().iter().any(|id| name_of(id) == "dialog"))
                || name.ends_with("-input")
                || ai_inside(&name)
                || panel.is_some_and(|panel| {
                    // パネルは上のバーの下から下の端まで。パネルの上のボタンは隠れていない
                    let center = element.bounds().center();
                    center.x >= panel.left()
                        && center.x <= panel.right()
                        && !PANEL_BAR.contains(&name.as_str())
                        && !element
                            .path()
                            .iter()
                            .any(|id| name_of(id) == "detail-panel")
                })
                || !matches!(id, ElementId::Name(_) | ElementId::NamedInteger(..))
            {
                continue;
            }
            // 同じ ID が 2 つ出ていると、どちらを押すか決まらない
            let same = elements
                .iter()
                .filter(|other| other.path().last() == Some(id))
                .count();
            if same == 1 && !found.contains(id) {
                found.push(id.clone());
            }
        }
        found
    })
    .unwrap()
}

/// 押せるもの同士が重なっている組。片方がもう片方の中にあるもの（行の中のボタンなど）は数えない。
fn overlapping(cx: &mut TestAppContext, world: &World) -> Vec<String> {
    let ids = targets(cx, world);
    cx.update_window(world.window, |_, window, _| {
        let found: Vec<_> = ids.iter().map(|id| window.find(id.clone())).collect();
        let mut pairs = Vec::new();
        for (i, a) in found.iter().enumerate() {
            for b in &found[i + 1..] {
                let nested = a.path().starts_with(b.path()) || b.path().starts_with(a.path());
                // スクロールする一覧の行は、見えている範囲の外まで大きさを持つ
                let scrolled = [a, b].iter().any(|e| {
                    e.path()
                        .iter()
                        .any(|id| ["task-list", "due-list"].contains(&name_of(id).as_str()))
                });
                let shared = a.bounds().intersect(&b.bounds());
                // 境界が触れているだけのものは重なりではない
                if !nested && !scrolled && shared.size.width > px(1.) && shared.size.height > px(1.)
                {
                    pairs.push(format!(
                        "{} と {}",
                        key_of(a.path().last().unwrap()),
                        key_of(b.path().last().unwrap())
                    ));
                }
            }
        }
        pairs
    })
    .unwrap()
}

/// 押しにくいほど小さいもの。縦か横が `MIN_HIT` に届かないものを並べる。
fn too_small(cx: &mut TestAppContext, world: &World) -> Vec<String> {
    let ids = targets(cx, world);
    let least = px(MIN_HIT * malakt_mahoz::UI_SCALE);
    cx.update_window(world.window, |_, window, _| {
        ids.iter()
            .filter(|id| !WIDE_SWITCHES.contains(&name_of(id).as_str()))
            .filter_map(|id| {
                let size = window.find(id.clone()).bounds().size;
                (size.width < least || size.height < least)
                    .then(|| format!("{}（{:?} × {:?}）", key_of(id), size.width, size.height))
            })
            .collect()
    })
    .unwrap()
}

/// 画面とデータの様子を 1 つの文字列にまとめる。押す前と後で比べる。
fn fingerprint(cx: &mut TestAppContext, world: &World) -> String {
    frame(cx, world);
    let elements = cx
        .update_window(world.window, |_, window, _| {
            window
                .elements()
                .iter()
                .map(|e| {
                    format!(
                        "{:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {}",
                        e.path(),
                        e.bounds(),
                        e.checked(),
                        e.selected(),
                        e.expanded(),
                        e.value(),
                        e.label(),
                        e.disabled(),
                        e.visible()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap();
    cx.update(|cx| {
        let v = world.view.read(cx);
        let state = format!(
            "{} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {} {} {} {} {} {} {} {:?} {:?} {} {} {} {} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
            v.layout.encode(),
            v.pane_keys,
            v.pane_overlay,
            v.pane_presets,
            v.nav,
            v.panel,
            v.modal,
            v.sub_modal,
            v.editing,
            v.comment_mode,
            v.query,
            v.notice,
            v.error,
            v.selected,
            v.sidebar_open,
            v.sidebar_hidden,
            v.ai_open,
            v.assignee_picker,
            v.column_menu.is_some(),
            v.filters_open,
            v.due_mine,
            v.list_open,
            v.collapsed,
            v.unread_notices,
            v.notices.len(),
            v.issued_ticket.as_ref().map_or("", |t| t.secret.as_str()),
            v.pending_anchor.is_some(),
            v.ticket_person,
            v.expanded_threads,
            v.bulk_mode,
            v.bulk_people,
            v.bulk_category_add,
            v.naming_task,
            v.quick,
            v.defaults_draft,
            v.columns,
        );
        let db = db::shared(cx);
        let store = &db.read(cx).store;
        let tasks = store.tasks(Scope::All, &TaskFilter::default()).unwrap();
        let mut data = format!(
            "{:?} {:?} {:?} {:?}",
            tasks,
            store.workspaces().unwrap(),
            store.people().unwrap(),
            store.categories().unwrap()
        );
        for task in &tasks {
            data.push_str(&format!(
                "{:?} {}",
                store.comments(&task.id).unwrap(),
                store.changes(&task.id).unwrap().len()
            ));
        }
        for key in [
            "theme",
            "table.columns",
            "sidebar.width",
            "font.family",
            "color.accent",
            "color.background.light",
            "color.background.dark",
            "scene",
            "scene.clear",
            "scene.blur",
            "list.cards",
            "panels.float",
            "presets",
            "panel.overlay",
            "density",
            "due.format",
            "feel",
            "due_alerts.timed",
            "due_alerts.date",
            "due_alerts.hour",
        ] {
            data.push_str(&format!("{:?}", store.setting(key).unwrap()));
        }
        let theme = gpui_kit::component::Theme::global(cx);
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        format!(
            "{elements}\n{state}\n{data}\n{:?} {} {:?} {:?} {} {clipboard:?} {}",
            v.color_pickers
                .each_ref()
                .map(|picker| (picker.read(cx).is_open(), picker.read(cx).value())),
            theme.mode.is_dark(),
            theme.background,
            theme.primary,
            theme.font_family,
            cx.windows().len()
        )
    })
}

/// 押す。今の画面に出ていなければ押さずに `false` を返す。
fn click(cx: &mut TestAppContext, world: &World, id: &ElementId) -> bool {
    let found = cx
        .update_window(world.window, |_, window, cx| {
            window.render_frame(cx);
            let found = window
                .try_find(id.clone())
                .is_some_and(|element| element.visible());
            if found {
                window.click(id.clone(), cx);
                window.render_frame(cx);
            }
            found
        })
        .unwrap();
    cx.run_until_parked();
    found
}

/// ダイアログが出ていたら、外側の暗い所を押す。押して画面が変わったかを返す。
/// ダイアログが出ていないとき、発行したチケットを見せているとき（閉じない決まり）は `None`。
fn click_outside(cx: &mut TestAppContext, world: &World) -> Option<bool> {
    let at = cx
        .update_window(world.window, |_, window, cx| {
            window.render_frame(cx);
            if world.view.read(cx).issued_ticket.is_some() {
                return None;
            }
            let dialog = window.try_find("dialog")?;
            Some(dialog.bounds().origin - Point::new(px(4.), px(4.)))
        })
        .unwrap()?;
    let before = fingerprint(cx, world);
    let mut v = VisualTestContext::from_window(world.window, cx);
    v.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    v.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
    v.run_until_parked();
    Some(before != fingerprint(cx, world))
}

fn with_view(
    cx: &mut TestAppContext,
    world: &World,
    f: impl FnOnce(&mut MainView, &mut gpui_kit::Window, &mut gpui_kit::Context<MainView>),
) {
    cx.update_window(world.window, |_, window, cx| {
        world.view.update(cx, |view, cx| f(view, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

type Setup = fn(&mut TestAppContext, &World);

/// 確かめる画面。名前、ウィンドウの大きさ、その画面にする手順。
fn screens() -> Vec<(&'static str, Size<Pixels>, Setup)> {
    let wide = window_size(1600., 2400.);
    vec![
        ("ホーム", wide, |cx, w| {
            with_view(cx, w, |v, _, cx| v.go(Nav::Home, cx))
        }),
        ("すべてのタスク", wide, |cx, w| {
            with_view(cx, w, |v, _, cx| v.go(Nav::All, cx))
        }),
        ("タスクを選んだ一覧", wide, |cx, w| {
            let task = w.task.clone();
            with_view(cx, w, |v, _, cx| {
                v.go(Nav::All, cx);
                v.toggle_selected(&task, cx);
            })
        }),
        ("ワークスペース", wide, |cx, w| {
            let workspace = w.workspace.clone();
            with_view(cx, w, |v, _, cx| v.go(Nav::Workspace(workspace), cx))
        }),
        ("アイコンだけのメニュー", wide, |cx, w| {
            with_view(cx, w, |v, _, cx| {
                v.sidebar_hidden = true;
                v.go(Nav::Home, cx);
            })
        }),
        ("タスクの詳細", wide, |cx, w| {
            let task = w.task.clone();
            with_view(cx, w, |v, window, cx| {
                v.go(Nav::All, cx);
                v.open_panel(&task, window, cx);
            })
        }),
        ("説明を編集中の詳細", wide, |cx, w| {
            let task = w.task.clone();
            with_view(cx, w, |v, window, cx| {
                v.go(Nav::All, cx);
                v.open_panel(&task, window, cx);
                v.start_editing(Editing::Description, window, cx);
            })
        }),
        (
            "見出しを右クリックしたメニュー",
            wide,
            |cx, w| {
                with_view(cx, w, |v, window, cx| {
                    v.go(Nav::All, cx);
                    v.toggle_column_menu(Some(Column::Due), window, cx);
                })
            },
        ),
        ("絞り込んだ一覧", wide, |cx, w| {
            with_view(cx, w, |v, _, cx| {
                v.go(Nav::All, cx);
                v.query.priority = Some(Priority::High);
                v.reload(cx);
            })
        }),
        (
            "共有タスクを選んだワークスペース",
            wide,
            |cx, w| {
                let (workspace, task) = (w.workspace.clone(), w.task.clone());
                with_view(cx, w, |v, _, cx| {
                    v.go(Nav::Workspace(workspace), cx);
                    v.toggle_selected(&task, cx);
                })
            },
        ),
        ("担当者のダイアログ", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| v.open_people(window, cx))
        }),
        ("ロックのダイアログ", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| v.open_lock_settings(window, cx))
        }),
        (
            "ロックを有効にしたダイアログ",
            wide,
            |cx, w| {
                with_view(cx, w, |v, window, cx| {
                    v.write(cx, malakt_core::lock::enable);
                    v.open_lock_settings(window, cx);
                })
            },
        ),
        ("カスタマイズ", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| v.open_appearance(window, cx))
        }),
        (
            "カスタマイズ（自分のテーマあり）",
            wide,
            |cx, w| {
                with_view(cx, w, |v, window, cx| {
                    v.open_appearance(window, cx);
                    v.save_preset(window, cx);
                    v.save_preset_name(window, cx);
                })
            },
        ),
        ("カスタマイズのレイアウト", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Layout, window, cx);
            })
        }),
        ("カスタマイズの背景", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Backdrop, window, cx);
            })
        }),
        ("カスタマイズの色と書体", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Look, window, cx);
            })
        }),
        ("カスタマイズの表示", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Display, window, cx);
            })
        }),
        ("設定の AI のモデル", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::AiModel, window, cx);
            })
        }),
        ("設定の AI の動作", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::AiBehavior, window, cx);
            })
        }),
        ("設定の AI のルール", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::AiRules, window, cx);
            })
        }),
        ("設定の GitHub", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::AiGithub, window, cx);
            })
        }),
        ("設定のデータ", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Data, window, cx);
            })
        }),
        ("ロックを有効にした通知", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.write(cx, malakt_core::lock::enable);
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Notices, window, cx);
            })
        }),
        ("設定の通知", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Notices, window, cx);
            })
        }),
        (
            "カスタマイズで検索したところ",
            wide,
            |cx, w| {
                with_view(cx, w, |v, window, cx| {
                    v.open_appearance(window, cx);
                    v.inputs
                        .customize_search
                        .update(cx, |s, cx| s.set_value("ガラス", window, cx));
                })
            },
        ),
        ("カスタマイズを変えたところ", wide, |cx, w| {
            use malakt_mahoz::theme::{Color, Slot};
            with_view(cx, w, |v, window, cx| {
                v.set_font(Some("Noto Sans"), cx);
                for (slot, hex) in [
                    (Slot::Accent, "#1d4ed8"),
                    (Slot::Light, "#fdf6e3"),
                    (Slot::Dark, "#002b36"),
                ] {
                    v.set_color(slot, Color::parse(hex), window, cx);
                }
                v.set_scene(malakt_mahoz::theme::Scene::Sky, window, cx);
                v.open_appearance(window, cx);
                v.show_appearance_section(CustomizeSection::Look, window, cx);
            })
        }),
        (
            "背景の画像を出しているカスタマイズ",
            wide,
            |cx, w| {
                with_view(cx, w, |v, window, cx| {
                    v.set_scene(malakt_mahoz::theme::Scene::Sky, window, cx);
                    v.open_appearance(window, cx);
                    v.show_appearance_section(CustomizeSection::Backdrop, window, cx);
                })
            },
        ),
        ("背景の画像の一覧", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.open_scene_picker(cx);
            })
        }),
        ("フォントの一覧", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.open_appearance(window, cx);
                v.open_font_picker(window, cx);
                // この PC に入っている書体の代わり
                v.fonts = ["LINE Seed JP", "Noto Sans", "Noto Serif"]
                    .map(String::from)
                    .to_vec();
            })
        }),
        ("担当を選んでいる詳細", wide, |cx, w| {
            let task = w.task.clone();
            with_view(cx, w, |v, window, cx| {
                v.go(Nav::All, cx);
                v.open_panel(&task, window, cx);
                v.assignee_picker = true;
                cx.notify();
            })
        }),
        (
            "完了チケットを発行したところ",
            wide,
            |cx, w| {
                let task = w.task.clone();
                with_view(cx, w, |v, window, cx| {
                    v.go(Nav::All, cx);
                    v.open_panel(&task, window, cx);
                    v.open_ticket_dialog(&task, cx);
                });
                frame(cx, w);
                assert!(click(cx, w, &ElementId::Name("ticket-issue".into())));
            },
        ),
        (
            "タスクが多い一覧",
            window_size(1280., 800.),
            |cx, w| {
                cx.update(|cx| {
                    db::shared(cx).update(cx, |db, _| {
                        for n in 0..30 {
                            db.store.add_task_in(None, &format!("タスク {n}")).unwrap();
                        }
                    })
                });
                with_view(cx, w, |v, _, cx| {
                    v.reload(cx);
                    v.go(Nav::All, cx)
                })
            },
        ),
        ("ペインを分けた画面", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.go(Nav::Home, cx);
                v.pane_action(Action::Split(Dir::Right), window, cx);
            })
        }),
        ("ペインのキーの一覧", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.go(Nav::Home, cx);
                v.pane_action(Action::Help, window, cx);
            })
        }),
        ("配置のプリセット", wide, |cx, w| {
            with_view(cx, w, |v, window, cx| {
                v.go(Nav::Home, cx);
                v.pane_action(Action::Split(Dir::Right), window, cx);
                v.save_pane_preset(cx);
                v.pane_action(Action::Split(Dir::Down), window, cx);
                v.pane_action(Action::Presets, window, cx);
            })
        }),
        ("狭いウィンドウ", window_size(420., 900.), |cx, w| {
            with_view(cx, w, |v, _, cx| v.go(Nav::All, cx))
        }),
        // このあとの画面にも自動アップデートがつながったままになるので、最後に置く
        ("新しい版を取ってある画面", wide, |cx, _| {
            use malakt_mahoz::update::{Staged, Updater};
            cx.update(|cx| {
                cx.set_global(Updater {
                    available: true,
                    check: std::sync::Arc::new(|_| {
                        let dir = std::env::temp_dir().join("malakt-mahoz-test-no-such-dir");
                        Ok(Some(Staged {
                            version: "0.2.0".into(),
                            payload: dir.join("payload"),
                            dir,
                        }))
                    }),
                    install: std::sync::Arc::new(|_| Ok(())),
                });
                malakt_mahoz::update::check_now(cx);
            });
            cx.run_until_parked();
        }),
    ]
}

/// 画面を開いたところで見えるもの。
struct Opened {
    first: Vec<ElementId>,
    overlaps: Vec<String>,
    small: Vec<String>,
}

/// 新しい状態から 1 回押した結果。
#[derive(Default)]
struct Pressed {
    /// 押せた（画面に出ていた）
    found: bool,
    /// 画面かデータが変わった
    changed: bool,
    /// 押して新しく出てきたもの
    opened: Vec<ElementId>,
    /// 外側の暗い所を押して閉じたか。ダイアログが出ていなければ `None`
    outside: Option<bool>,
}

/// 1 つ目に押すもの: 画面の番号と、その画面で押すものの番号。
type First = (usize, usize);
/// 2 つ目に押すもの: 1 つ目と、それを押して出てきたものの番号。
type Second = (usize, usize, usize);

fn open_screen(cx: &mut TestAppContext, (_, size, setup): &(&str, Size<Pixels>, Setup)) -> Opened {
    let world = fresh(cx, *size);
    setup(cx, &world);
    frame(cx, &world);
    Opened {
        first: targets(cx, &world),
        overlaps: overlapping(cx, &world),
        small: too_small(cx, &world),
    }
}

fn press_first(
    cx: &mut TestAppContext,
    (size, setup, first, one): &(Size<Pixels>, Setup, &[ElementId], &ElementId),
) -> Pressed {
    let world = fresh(cx, *size);
    setup(cx, &world);
    let before = fingerprint(cx, &world);
    if !click(cx, &world, one) {
        return Pressed::default();
    }
    let after = fingerprint(cx, &world);
    if before == after {
        return Pressed {
            found: true,
            ..Pressed::default()
        };
    }
    let opened = targets(cx, &world)
        .into_iter()
        .filter(|id| !first.contains(id))
        .collect();
    // 開いたダイアログは、外側の暗い所を押すと閉じる
    let world = fresh(cx, *size);
    setup(cx, &world);
    frame(cx, &world);
    click(cx, &world, one);
    Pressed {
        found: true,
        changed: true,
        opened,
        outside: click_outside(cx, &world),
    }
}

fn press_second(
    cx: &mut TestAppContext,
    (size, setup, one, two): &(Size<Pixels>, Setup, &ElementId, &ElementId),
) -> Pressed {
    let world = fresh(cx, *size);
    setup(cx, &world);
    frame(cx, &world);
    click(cx, &world, one);
    let before = fingerprint(cx, &world);
    // 開き直したら出てこないものは、押せない
    if !click(cx, &world, two) {
        return Pressed::default();
    }
    let after = fingerprint(cx, &world);
    Pressed {
        found: true,
        changed: before != after,
        opened: Vec::new(),
        // 中のボタンを押したあと（一覧を重ねたあとなど）も、外側を押すと閉じる
        outside: click_outside(cx, &world),
    }
}

/// `items` を CPU の数のスレッドで分けて `work` に渡し、結果を `items` の順で返す。
/// スレッドごとにアプリを 1 つ作り、空いたスレッドが次のものを取る。
/// どれも `fresh` で新しいデータと新しいウィンドウから始めるので、どのスレッドで押しても同じ結果になる。
fn pool<T: Sync, R: Send>(items: &[T], work: fn(&mut TestAppContext, &T) -> R) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(items.len()));
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|n| {
                let (next, done) = (&next, &done);
                std::thread::Builder::new()
                    .name(format!("every_button_responds-{n}"))
                    .stack_size(8 << 20)
                    .spawn_scoped(scope, move || {
                        let dispatcher = TestDispatcher::new(0);
                        let scheduler = dispatcher.scheduler().clone();
                        let mut cx = TestAppContext::build(
                            dispatcher.clone(),
                            Some("every_button_responds"),
                        );
                        // タスクから開くターミナルの出力は、別のスレッドから届く
                        cx.executor().allow_parking();
                        cx.update(|cx| malakt_mahoz::init(Store::open_in_memory().unwrap(), cx));
                        loop {
                            let at = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(at) else {
                                break;
                            };
                            let result = work(&mut cx, item);
                            done.lock().unwrap().push((at, result));
                        }
                        cx.run_until_parked();
                        cx.update(|cx| {
                            cx.background_executor().forbid_parking();
                            cx.quit();
                        });
                        cx.run_until_parked();
                        drop(cx);
                        dispatcher.drain_tasks();
                        drop(dispatcher);
                        scheduler.end_test();
                    })
                    .expect("スレッドを立てる")
            })
            .collect();
        for handle in handles {
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_by_key(|(at, _)| *at);
    done.into_iter().map(|(_, result)| result).collect()
}

/// 画面に出ているボタンを 1 つずつ、新しい状態から押す。
///
/// 1 回押すたびに新しいデータと新しいウィンドウで始めるので、押す順は結果に関わらない。
/// そこで、画面を開く・1 つ目を押す・2 つ目を押す、をそれぞれ CPU の数のスレッドで分けて押す。
/// どれを押すか（同じ名前の中のボタンは、前の画面までに押していれば押さない）と結果のまとめは、
/// 画面の順に 1 つのスレッドで決める。
#[test]
#[ignore = "重いので手で回す: mise run test-buttons"]
fn every_button_responds() {
    let screens = screens();
    let quiet = |screen: &str, id: &ElementId| {
        let key = key_of(id);
        QUIET
            .iter()
            .any(|(quiet, _)| *quiet == key || *quiet == format!("{screen} > {key}"))
    };

    let opened = pool(&screens, open_screen);
    let mut overlaps: Vec<String> = Vec::new();
    let mut small: Vec<String> = Vec::new();
    let mut firsts: Vec<First> = Vec::new();
    for (s, ((screen, _, _), o)) in screens.iter().zip(&opened).enumerate() {
        assert!(!o.first.is_empty(), "{screen}: 押せるものが見つからない");
        overlaps.extend(o.overlaps.iter().map(|pair| format!("{screen} > {pair}")));
        small.extend(o.small.iter().map(|one| format!("{screen} > {one}")));
        firsts.extend((0..o.first.len()).map(|i| (s, i)));
    }
    let jobs: Vec<_> = firsts
        .iter()
        .map(|&(s, i)| {
            let (_, size, setup) = screens[s];
            (size, setup, opened[s].first.as_slice(), &opened[s].first[i])
        })
        .collect();
    let first: std::collections::HashMap<First, Pressed> = firsts
        .iter()
        .copied()
        .zip(pool(&jobs, press_first))
        .collect();

    // 2 つ目に押すものは、前の画面までに押した名前で決まる。足りない結果を押しては、決め直す
    let mut second: std::collections::HashMap<Second, Pressed> = std::collections::HashMap::new();
    loop {
        let mut dead: Vec<String> = Vec::new();
        let mut pressed: BTreeSet<String> = BTreeSet::new();
        let mut stuck: Vec<String> = Vec::new();
        let mut outside = 0;
        let mut missing: Vec<Second> = Vec::new();
        let mut tally = |result: Option<bool>, what: String, outside: &mut usize| match result {
            Some(true) => *outside += 1,
            Some(false) => stuck.push(what),
            None => {}
        };
        for (s, (screen, _, _)) in screens.iter().enumerate() {
            for (i, one) in opened[s].first.iter().enumerate() {
                let r = &first[&(s, i)];
                if !r.found {
                    continue;
                }
                pressed.insert(name_of(one));
                if !r.changed {
                    if !quiet(screen, one) {
                        dead.push(format!("{screen} > {one:?}"));
                    }
                    continue;
                }
                // 押して新しく出てきたもの（メニューやダイアログの中）も、1 つずつ押す
                for (j, two) in r.opened.iter().enumerate() {
                    if pressed.contains(&name_of(two)) {
                        continue;
                    }
                    let Some(p) = second.get(&(s, i, j)) else {
                        // まだ押していない。押せるものとして先へ進み、押したあとで決め直す
                        missing.push((s, i, j));
                        pressed.insert(name_of(two));
                        continue;
                    };
                    if !p.found {
                        continue;
                    }
                    pressed.insert(name_of(two));
                    if !p.changed && !quiet(screen, two) {
                        dead.push(format!("{screen} > {one:?} > {two:?}"));
                    }
                    tally(
                        p.outside,
                        format!("{screen} > {one:?} > {two:?}"),
                        &mut outside,
                    );
                }
                tally(r.outside, format!("{screen} > {one:?}"), &mut outside);
            }
        }
        if !missing.is_empty() {
            let jobs: Vec<_> = missing
                .iter()
                .map(|&(s, i, j)| {
                    let (_, size, setup) = screens[s];
                    (size, setup, &opened[s].first[i], &first[&(s, i)].opened[j])
                })
                .collect();
            let results = pool(&jobs, press_second);
            second.extend(missing.into_iter().zip(results));
            continue;
        }
        return finish(dead, pressed, overlaps, small, stuck, outside);
    }
}

/// まとめた結果を確かめる。
fn finish(
    dead: Vec<String>,
    pressed: BTreeSet<String>,
    overlaps: Vec<String>,
    small: Vec<String>,
    stuck: Vec<String>,
    outside: usize,
) {
    assert!(
        overlaps.is_empty(),
        "押せるものが重なっている:\n{}",
        overlaps.join("\n")
    );
    assert!(
        small.is_empty(),
        "押せる範囲が小さすぎるものがある:\n{}",
        small.join("\n")
    );
    assert!(
        stuck.is_empty(),
        "外側を押しても閉じないダイアログがある:\n{}",
        stuck.join("\n")
    );
    assert!(
        outside >= 20,
        "外側を押して確かめたダイアログが少ない: {outside}"
    );
    println!(
        "押したもの {} 種類、外側を押したダイアログ {outside} 回",
        pressed.len()
    );
    let source = [
        include_str!("../src/render.rs"),
        include_str!("../src/ai_view.rs"),
        include_str!("../src/panes.rs"),
    ]
    .concat();
    let mut missed: Vec<&str> = Vec::new();
    for (at, _) in source.match_indices("button(") {
        // `icon_button(` などのほかの関数は数えない
        if source[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let rest = source[at + "button(".len()..].trim_start();
        let rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
        if let Some(name) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) {
            if !pressed.contains(name)
                && !ai_inside(name)
                && !NEVER.contains(&name)
                && !LOCK.contains(&name)
                && !ICON_PICKER.contains(&name)
                && !missed.contains(&name)
            {
                missed.push(name);
            }
        }
    }
    // ボタンの部品で作っていない押せるもの（行や色の見本）は、名前で確かめる
    for name in MUST {
        assert!(
            pressed.contains(*name),
            "{name} を押していない: {pressed:?}"
        );
    }
    assert!(
        missed.is_empty(),
        "どの画面でも押していないボタンがある。画面を足すか、押せない理由を書く: {missed:?}"
    );
    assert!(
        dead.is_empty(),
        "押しても何も変わらないボタンがある:\n{}",
        dead.join("\n")
    );
}
