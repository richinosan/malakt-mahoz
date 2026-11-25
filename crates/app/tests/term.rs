//! ターミナルの、画面を通したシナリオテスト。本物のシェル（`/bin/sh`）を PTY で起こし、
//! キーとマウスで操作して確かめる。
#![cfg(unix)]

use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext, Bounds, ClipboardItem, Entity, Modifiers, Pixels,
    Point, ScrollDelta, Size, TestAppContext, WindowBounds, WindowOptions,
};
use malakt_core::{Store, TaskDraft};
use malakt_mahoz::ai::{self, ShellPlace};
use malakt_mahoz::term::{self, Terms, MAX_TERMS};
use malakt_mahoz::{db, MainView};
use malakt_term::profile::Kind;
use malakt_term::{Profile, Snapshot};

fn profile(id: &str, program: &str) -> Profile {
    Profile {
        id: id.into(),
        name: id.into(),
        kind: Kind::Shell,
        program: program.into(),
        args: Vec::new(),
    }
}

/// アプリを用意して、ウィンドウを開く。シェルは `sh`（標準）と `bash` の 2 つにする。
fn start(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<MainView>) {
    // ターミナルの出力は、別のスレッドから届く
    cx.executor().allow_parking();
    // プロンプトを決めておく
    std::env::set_var("PS1", "$ ");
    std::env::set_var("ENV", "");
    cx.update(|cx| {
        malakt_mahoz::init(Store::open_in_memory().unwrap(), cx);
        term::shared(cx).update(cx, |terms, cx| {
            terms.set_profiles(
                vec![profile("sh", "/bin/sh"), profile("other", "/bin/sh")],
                cx,
            )
        });
    });
    let opened = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: wide(),
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| MainView::new(window, cx))
        })
        .expect("ウィンドウを開く")
    });
    cx.run_until_parked();
    opened
}

fn wide() -> Size<Pixels> {
    size(px(2000.), px(1125.))
}

fn ui(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App),
) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn terms<R>(cx: &mut TestAppContext, f: impl FnOnce(&Terms) -> R) -> R {
    cx.update(|cx| f(term::shared(cx).read(cx)))
}

/// 見ているターミナルの画面。
fn screen(cx: &mut TestAppContext) -> Snapshot {
    terms(cx, |t| {
        t.active_tab()
            .expect("ターミナルを見ている")
            .terminal
            .snapshot()
    })
}

/// シェルは別のプロセスなので、条件がそろうまで待つ。
fn wait(cx: &mut TestAppContext, what: &str, f: impl Fn(&Terms) -> bool) {
    for _ in 0..1500 {
        cx.run_until_parked();
        if terms(cx, &f) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let shown = terms(cx, |t| {
        t.active_tab().map(|tab| tab.terminal.snapshot().text())
    });
    panic!("待っても起きませんでした: {what}\n画面:\n{shown:?}");
}

/// 見ているターミナルに、ちょうどその中身の行があるか（打ったコマンドの行とは区別する）。
fn has_line(cx: &mut TestAppContext, line: &str) -> bool {
    screen(cx).text().lines().any(|shown| shown == line)
}

fn wait_text(cx: &mut TestAppContext, text: &str) {
    wait(cx, text, |t| {
        t.active_tab()
            .is_some_and(|tab| tab.terminal.snapshot().text().contains(text))
    });
}

/// チャットを開いて、「＋」でターミナルを開く。プロンプトが出るまで待つ。
fn open_terminal(cx: &mut TestAppContext, window: AnyWindowHandle) {
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    ui(cx, window, |window, cx| window.click("term-new", cx));
    wait_text(cx, "$");
    // 画面を描いて、場所の大きさをターミナルに伝える
    ui(cx, window, |_, _| {});
}

/// 1 行打って Enter を押す。
fn run(cx: &mut TestAppContext, window: AnyWindowHandle, line: &str) {
    ui(cx, window, |window, cx| {
        window.input(line, cx);
        window.press("enter", cx);
    });
}

/// 「＋」でターミナルが開き、打ったコマンドが動く。全角の文字も、空白も、そのまま届く。
#[gpui_kit::test]
fn a_terminal_opens_and_runs_what_is_typed(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    ui(cx, window, |window, _| {
        assert!(
            window.find("ai-shell-prompt").visible(),
            "最初は AI のシェル"
        );
        assert!(window.try_find("term-screen").is_none());
    });
    ui(cx, window, |window, cx| window.click("term-new", cx));
    assert_eq!(terms(cx, |t| t.tabs.len()), 1);
    assert_eq!(terms(cx, |t| t.tabs[0].label().to_string()), "sh");
    ui(cx, window, |window, _| {
        assert!(window.find("term-screen").visible());
        assert!(window.find(("term-tab", 1usize)).visible());
        assert!(window.try_find("ai-shell-prompt").is_none());
    });
    wait_text(cx, "$");
    // 開いたら、そのまま打てる
    run(cx, window, "echo sum-$((40+2)) 日本語 ok");
    wait_text(cx, "sum-42 日本語 ok");
}

/// ターミナルにいる間のキーは、アプリのショートカットではなく、動いているプログラムに届く。
#[gpui_kit::test]
fn keys_go_to_the_program_not_to_the_app(cx: &mut TestAppContext) {
    let (window, view) = start(cx);
    open_terminal(cx, window);
    // Ctrl+C はプログラムを止める（AI の中止や、コピーにはならない）
    run(cx, window, "sleep 60; echo not-reached");
    ui(cx, window, |window, cx| window.press("ctrl-c", cx));
    run(cx, window, "echo after-$((1+1))");
    wait_text(cx, "after-2");
    assert!(!has_line(cx, "not-reached"));
    // Esc も、↑ も、プログラムに届く。チャットは閉じず、一覧のカーソルも動かない
    run(cx, window, "echo first");
    wait_text(cx, "first\n");
    ui(cx, window, |window, cx| {
        window.press("escape", cx);
        window.press("up", cx);
        // Ctrl+J は、端末では改行
        window.press("ctrl-j", cx);
    });
    cx.update(|cx| assert!(view.read(cx).ai_open, "Ctrl+J でチャットを閉じない"));
    ui(cx, window, |window, _| {
        assert!(window.find("term-screen").visible());
    });
    // Esc（^[）と ↑（^[[A）が、そのままシェルに届いている
    wait_text(cx, "^[^[[A");
    wait_text(cx, "not found");
    // Ctrl+D でシェルが終わり、タブが閉じて AI のシェルに戻る
    run(cx, window, "echo ok-$((5+5))");
    wait_text(cx, "ok-10");
    ui(cx, window, |window, cx| window.press("ctrl-d", cx));
    wait(cx, "タブが閉じる", |t| t.tabs.is_empty());
    ui(cx, window, |window, _| {
        assert!(window.find("ai-shell-prompt").visible());
    });
}

/// 画面全体を使うプログラム（代替画面）が動き、終わると元の出力に戻る。
#[gpui_kit::test]
fn a_full_screen_program_takes_the_screen_and_gives_it_back(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    run(cx, window, "echo before");
    wait_text(cx, "before\n");
    run(
        cx,
        window,
        r"printf '\033[?1049h\033[2J\033[H\033[38;2;255;128;0mTUI\033[0m'; read x; printf '\033[?1049l'",
    );
    wait(cx, "代替画面", |t| {
        t.active_tab().is_some_and(|tab| tab.terminal.alt_screen())
    });
    let shown = screen(cx);
    assert_eq!(shown.line(0), "TUI");
    assert_eq!(shown.cells[0].fg, 0xff8000, "TrueColor の色");
    assert!(!shown.text().contains("before"));
    ui(cx, window, |window, cx| window.press("enter", cx));
    wait(cx, "元の画面", |t| {
        t.active_tab().is_some_and(|tab| !tab.terminal.alt_screen())
    });
    assert!(screen(cx).text().contains("before"));
}

/// シェルを選んで開くと、次の「＋」でも同じシェルが開く。タブで切り替え、× で閉じる。
#[gpui_kit::test]
fn shells_are_chosen_and_tabs_switch_and_close(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    run(cx, window, "echo one");
    wait_text(cx, "one\n");
    // 一覧の 2 つ目を選ぶ
    ui(cx, window, |window, cx| window.click("term-shells", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(terms(cx, |t| t.tabs.len()), 2);
    assert_eq!(terms(cx, |t| t.tabs[1].shell.to_string()), "other");
    assert_eq!(terms(cx, |t| t.active), Some(2));
    assert_eq!(
        cx.update(|cx| db::shared(cx)
            .read(cx)
            .store
            .setting("term.profile")
            .unwrap())
            .as_deref(),
        Some("other")
    );
    // 「＋」は、選んだシェルを開く
    ui(cx, window, |window, cx| window.click("term-new", cx));
    assert_eq!(terms(cx, |t| t.tabs[2].shell.to_string()), "other");
    // タブで切り替える。前のターミナルは動いたまま
    ui(cx, window, |window, cx| {
        window.click(("term-tab", 1usize), cx)
    });
    assert_eq!(terms(cx, |t| t.active), Some(1));
    assert!(screen(cx).text().contains("one\n"));
    // AI のシェルにも戻れる
    ui(cx, window, |window, cx| window.click("term-tab-ai", cx));
    assert_eq!(terms(cx, |t| t.active), None);
    ui(cx, window, |window, _| {
        assert!(window.find("ai-shell-prompt").visible());
        assert!(window.try_find("term-screen").is_none());
    });
    // × で閉じる。見ているタブを閉じたら、となりのタブを見せる
    ui(cx, window, |window, cx| {
        window.click(("term-tab", 2usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("term-close", 2usize), cx)
    });
    assert_eq!(
        terms(cx, |t| t.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>()),
        [1, 3]
    );
    assert_eq!(terms(cx, |t| t.active), Some(3));
    ui(cx, window, |window, cx| {
        window.click(("term-close", 1usize), cx)
    });
    assert_eq!(terms(cx, |t| t.active), Some(3), "見ていないタブを閉じた");
}

/// `exit` で終わったターミナルは閉じ、失敗で終わったものは出力を読めるように残る。
#[gpui_kit::test]
fn a_failed_shell_stays_and_a_finished_one_closes(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    run(cx, window, "echo last-words; exit 3");
    wait(cx, "終了コードが届く", |t| {
        t.tabs[0].exit == Some(Some(3))
    });
    ui(cx, window, |window, _| {
        assert!(window.find("term-exit").visible());
    });
    // 終わる前の出力は、終了の知らせより後に届くことがある
    for _ in 0..500 {
        if has_line(cx, "last-words") {
            break;
        }
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(has_line(cx, "last-words"), "{:?}", screen(cx).text());
    ui(cx, window, |window, cx| {
        window.click(("term-close", 1usize), cx)
    });
    ui(cx, window, |window, cx| window.click("term-new", cx));
    wait_text(cx, "$");
    run(cx, window, "exit");
    wait(cx, "タブが閉じる", |t| t.tabs.is_empty());
}

/// 開けるターミナルの数には上限があり、越えると何が起きたかが出る。
#[gpui_kit::test]
fn terminals_are_limited(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    for _ in 0..MAX_TERMS {
        ui(cx, window, |window, cx| window.click("term-new", cx));
    }
    assert_eq!(terms(cx, |t| t.tabs.len()), MAX_TERMS);
    ui(cx, window, |window, _| {
        assert!(window.try_find("term-error").is_none());
    });
    ui(cx, window, |window, cx| window.click("term-new", cx));
    assert_eq!(terms(cx, |t| t.tabs.len()), MAX_TERMS);
    assert_eq!(
        terms(cx, |t| t.error.clone()).unwrap().to_string(),
        format!("ターミナルは {MAX_TERMS} 個までです。")
    );
    ui(cx, window, |window, cx| {
        assert!(window.find("term-error").visible());
        window.click("term-error-close", cx);
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("term-error").is_none());
    });
}

/// 起動できないシェルは、タブを作らずに、何を起動できなかったかを出す。
#[gpui_kit::test]
fn a_shell_that_cannot_start_is_reported(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    cx.update(|cx| {
        term::shared(cx).update(cx, |terms, cx| {
            terms.set_profiles(vec![profile("gone", "/no/such/shell")], cx)
        })
    });
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    ui(cx, window, |window, cx| window.click("term-new", cx));
    // Unix では子プロセスの中で失敗し、終了として届く
    wait(cx, "失敗が出る", |t| {
        t.error.is_some() || t.tabs.first().is_some_and(|tab| tab.exit.is_some())
    });
    ui(cx, window, |window, _| {
        let reported =
            window.try_find("term-error").is_some() || window.try_find("term-exit").is_some();
        assert!(reported, "失敗が画面に出る");
    });
}

/// タスクからターミナルを開くと、ワークスペースに決めたフォルダーで始まり、タスクの情報が渡る。
#[gpui_kit::test]
fn a_task_opens_a_terminal_with_its_folder_and_context(cx: &mut TestAppContext) {
    let (window, view) = start(cx);
    let dir = tempfile::tempdir().unwrap();
    let dir_text = dir.path().to_string_lossy().to_string();
    let (workspace, task) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let workspace = db.store.create_workspace(None, "仕事").unwrap();
            let draft = TaskDraft {
                title: "資料を直す".into(),
                ..Default::default()
            };
            let task = db.store.create_task(Some(&workspace.id), &draft).unwrap();
            cx.notify();
            (workspace.id, task.id)
        })
    });
    // ワークスペースの設定で、フォルダーを決める
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_workspace_settings(&workspace, window, cx)
        });
    });
    ui(cx, window, |window, cx| {
        window.click("ws-term-dir", cx);
        window.input("/no/such/folder", cx);
        window.click("ws-settings-save", cx);
    });
    cx.update(|cx| {
        let error = view
            .read(cx)
            .error
            .clone()
            .expect("ないフォルダーは保存しない");
        assert_eq!(
            error.to_string(),
            "フォルダーが見つかりません: /no/such/folder"
        );
    });
    let key = term::workspace_dir_key(&workspace);
    let saved = |cx: &mut TestAppContext| {
        cx.update(|cx| db::shared(cx).read(cx).store.setting(&key).unwrap())
    };
    assert_eq!(saved(cx), None);
    ui(cx, window, |window, cx| {
        window.click("ws-term-dir", cx);
        window.press("secondary-a", cx);
        window.input(&dir_text, cx);
        window.click("ws-settings-save", cx);
    });
    assert_eq!(saved(cx).as_deref(), Some(dir_text.as_str()));

    // タスクの詳細のボタンで開く。チャットとシェルを閉じていても、見える所に出る
    cx.update(|cx| {
        ai::shared(cx).update(cx, |s, _| s.shell_place = ShellPlace::Closed);
        assert!(!view.read(cx).ai_open);
    });
    ui(cx, window, |window, cx| {
        view.update(cx, |view, cx| view.open_panel(&task, window, cx));
    });
    ui(cx, window, |window, cx| window.click("task-terminal", cx));
    cx.update(|cx| {
        assert!(view.read(cx).ai_open);
        assert_eq!(ai::shared(cx).read(cx).shell_place, ShellPlace::Docked);
    });
    assert_eq!(terms(cx, |t| t.tabs[0].label().to_string()), "資料を直す");
    wait_text(cx, "$");
    ui(cx, window, |window, _| {
        assert!(window.find("term-screen").visible());
    });
    // そのまま打てる（キーの入力先がターミナルに移っている）
    run(
        cx,
        window,
        "pwd; echo \"[$MALAKT_TASK_TITLE][$MALAKT_WORKSPACE][$MALAKT_TASK_ID]\"",
    );
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    wait_text(cx, &format!("[資料を直す][仕事][{task}]"));
    assert!(screen(cx).text().contains(&name));
}

/// シェルの場所を広げると、ターミナルの桁と行が増え、動いているプログラムにも伝わる。
#[gpui_kit::test]
fn the_terminal_follows_the_size_of_its_place(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    let before = screen(cx);
    assert!(
        before.cols >= 20 && before.rows >= 5,
        "{}x{}",
        before.cols,
        before.rows
    );
    run(cx, window, "stty size");
    wait_text(cx, &format!("{} {}", before.rows, before.cols));
    ui(cx, window, |window, cx| {
        assert!(window.find("ai-chat").visible());
        window.click("ai-shell-wide", cx);
    });
    ui(cx, window, |window, _| {
        assert!(
            window.try_find("ai-chat").is_none(),
            "チャットの場所まで広がる"
        );
    });
    let after = screen(cx);
    assert!(after.cols > before.cols || after.rows > before.rows);
    run(cx, window, "stty size");
    wait_text(cx, &format!("{} {}", after.rows, after.cols));
    // 元に戻すと、チャットが戻る
    ui(cx, window, |window, cx| window.click("ai-shell-wide", cx));
    ui(cx, window, |window, _| {
        assert!(window.find("ai-chat").visible());
    });
    let back = screen(cx);
    assert_eq!((back.cols, back.rows), (before.cols, before.rows));
}

/// マウスで選んでコピーし、貼り付けられる。ホイールで出力をさかのぼれる。
#[gpui_kit::test]
fn text_is_selected_copied_pasted_and_scrolled(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    run(cx, window, "clear; echo copy-me-please");
    wait(cx, "出力", |t| {
        t.active_tab()
            .is_some_and(|tab| tab.terminal.snapshot().line(0) == "copy-me-please")
    });
    // 出力の行をダブルクリックして、単語を選ぶ
    let area = cx
        .update_window(window, |_, window, _| window.find("term-screen").bounds())
        .unwrap();
    let shown = screen(cx);
    let cell_height = f32::from(area.size.height) / (shown.rows as f32 + 1.);
    let at = point(px(60.), px(12. + cell_height * 0.5));
    ui(cx, window, |window, cx| {
        window.click_with_options(
            "term-screen",
            gpui_kit::test::ClickOptions::new()
                .with_offset(at)
                .with_count(2),
            cx,
        );
    });
    assert_eq!(
        terms(cx, |t| t.active_tab().unwrap().terminal.selected_text()).as_deref(),
        Some("copy-me-please")
    );
    ui(cx, window, |window, cx| window.press("ctrl-shift-c", cx));
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("copy-me-please"));
    // 貼り付ける
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("echo pasted-$((2*3))".into())));
    ui(cx, window, |window, cx| {
        window.press("ctrl-shift-v", cx);
        window.press("enter", cx);
    });
    wait_text(cx, "pasted-6");

    // 画面より長い出力を、ホイールでさかのぼる
    run(
        cx,
        window,
        "i=0; while [ $i -lt 200 ]; do echo row$i; i=$((i+1)); done",
    );
    wait_text(cx, "row199");
    assert_eq!(screen(cx).offset, 0);
    ui(cx, window, |window, cx| {
        window.scroll("term-screen", ScrollDelta::Lines(point(0., 5.)), cx);
    });
    let scrolled = screen(cx);
    assert_eq!(scrolled.offset, 5);
    assert!(!scrolled.text().contains("row199"));
    // 打つと、今の出力に戻る
    ui(cx, window, |window, cx| window.input("x", cx));
    assert_eq!(screen(cx).offset, 0);
}

/// マウスを受け取るプログラムには、クリックがそのまま届く。
#[gpui_kit::test]
fn clicks_reach_a_program_that_asks_for_the_mouse(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    run(
        cx,
        window,
        r"printf '\033[?1000h\033[?1006h'; stty raw -echo; dd bs=1 count=9 2>/dev/null | od -An -c; stty sane; printf '\033[?1000l'",
    );
    wait(cx, "マウスを受け取る", |t| {
        t.active_tab().is_some_and(|tab| tab.terminal.mouse_mode())
    });
    ui(cx, window, |window, cx| {
        window.click_at("term-screen", point(px(12.), px(12.)), cx);
    });
    // 左上のマス（1 桁目、1 行目）を押した報告
    wait(cx, "報告", |t| {
        t.active_tab().is_some_and(|tab| {
            let text: String = tab.terminal.snapshot().text().split_whitespace().collect();
            text.contains("[<0;1;1")
        })
    });
    assert_eq!(
        terms(cx, |t| t.active_tab().unwrap().terminal.selected_text()),
        None,
        "アプリの側では選ばない"
    );
}

/// シェルを別ウィンドウに出しても、ターミナルは動いたままで、そのウィンドウで打てる。
#[gpui_kit::test]
fn the_terminal_keeps_running_in_its_own_window(cx: &mut TestAppContext) {
    let (window, _view) = start(cx);
    open_terminal(cx, window);
    run(cx, window, "echo docked");
    wait_text(cx, "docked\n");
    ui(cx, window, |window, cx| window.click("ai-shell-detach", cx));
    let shell_window = match cx.update(|cx| ai::shared(cx).read(cx).shell_place) {
        ShellPlace::Window(handle) => handle,
        other => panic!("シェルが別ウィンドウに出る: {other:?}"),
    };
    ui(cx, shell_window, |window, cx| {
        assert!(window.find("term-screen").visible());
        // ターミナルを押して、キーの入力先にする
        window.click_with_modifiers("term-screen", Modifiers::shift(), cx);
    });
    assert!(screen(cx).text().contains("docked\n"));
    run(cx, shell_window, "echo window-$((3+4))");
    wait_text(cx, "window-7");
    // 別ウィンドウでも、タブを足して閉じられる
    ui(cx, shell_window, |window, cx| window.click("term-new", cx));
    assert_eq!(terms(cx, |t| t.tabs.len()), 2);
    ui(cx, shell_window, |window, cx| {
        window.click(("term-close", 2usize), cx)
    });
    assert_eq!(terms(cx, |t| t.active), Some(1));
    // 元の場所に戻しても、続きから使える
    ui(cx, shell_window, |window, cx| {
        window.click("ai-shell-attach", cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.find("term-screen").visible());
    });
    assert!(screen(cx).text().contains("window-7"));
}

/// ロックしている間は、ターミナルを開けない。
#[gpui_kit::test]
fn no_terminal_opens_while_locked(cx: &mut TestAppContext) {
    let (_window, _view) = start(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.unlocked = false;
            cx.notify();
        });
        let opened = term::shared(cx).update(cx, |terms, cx| terms.open(None, None, cx));
        assert_eq!(opened, None);
    });
    assert!(terms(cx, |t| t.tabs.is_empty()));
}
