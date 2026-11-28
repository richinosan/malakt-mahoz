//! 本物のシェルを PTY で起こして、端末としての動きを確かめる。

use std::sync::mpsc;
use std::time::{Duration, Instant};

use malakt_term::{Event, Options, Terminal};

fn shell(script: &str) -> Options {
    let (program, flag) = if cfg!(windows) {
        ("cmd.exe", "/C")
    } else {
        ("/bin/sh", "-c")
    };
    Options {
        program: program.into(),
        args: vec![flag.into(), script.into()],
        cols: 40,
        rows: 10,
        ..Default::default()
    }
}

fn start(options: &Options) -> (Terminal, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let terminal = Terminal::spawn(options, move |event| {
        // 受け取る側が先に終わったテストでは、送れなくてよい
        let _ = tx.lock().unwrap().send(event);
    })
    .unwrap();
    (terminal, rx)
}

/// 画面が `ready` になるまで待つ。
fn wait(terminal: &Terminal, what: &str, ready: impl Fn(&malakt_term::Snapshot) -> bool) {
    let started = Instant::now();
    loop {
        let snapshot = terminal.snapshot();
        if ready(&snapshot) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{what} にならない。画面:\n{}",
            snapshot.text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_exit(rx: &mpsc::Receiver<Event>) -> Option<i32> {
    loop {
        match rx
            .recv_timeout(Duration::from_secs(20))
            .expect("終了が届く")
        {
            Event::Exit(code) => return code,
            _ => continue,
        }
    }
}

#[test]
fn output_and_exit_code_arrive() {
    let script = if cfg!(windows) {
        "echo hello& exit /b 3"
    } else {
        "echo hello; exit 3"
    };
    let (terminal, rx) = start(&shell(script));
    assert_eq!(wait_exit(&rx), Some(3));
    wait(&terminal, "hello", |s| s.text().contains("hello"));
    assert!(terminal.exited());
    // 終わったあとの入力は、どこにも届かずに捨てられる
    terminal.input("x");
}

#[test]
fn a_missing_program_fails_to_start() {
    let options = Options {
        program: "malakt-mahoz-no-such-shell".into(),
        cols: 80,
        rows: 24,
        ..Default::default()
    };
    // Unix では子プロセスの中で失敗し、終了として届く。Windows では起こすときに失敗する
    match Terminal::spawn(&options, |_| {}) {
        Err(_) => {}
        Ok(terminal) => {
            let started = Instant::now();
            while !terminal.exited() {
                assert!(started.elapsed() < Duration::from_secs(20));
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use malakt_term::{style, Action, Button, Caret, Key, Select};

    fn cell(s: &malakt_term::Snapshot, col: usize, row: usize) -> malakt_term::Cell {
        s.cells[row * s.cols + col]
    }

    #[test]
    fn colors_and_styles_are_kept_per_cell() {
        // 16 色、256 色、TrueColor、太字、下線、反転
        let script = r"printf '\033[31mR\033[0m\033[38;5;208mO\033[0m\033[38;2;1;2;3mT\033[0m\033[1mB\033[0m\033[4mU\033[0m\033[7mI\033[0m\033[48;2;10;20;30mG\033[0m'; sleep 5";
        let (terminal, _rx) = start(&shell(script));
        wait(&terminal, "ROTBUIG", |s| s.line(0) == "ROTBUIG");
        let s = terminal.snapshot();
        assert_eq!(cell(&s, 0, 0).fg, 0xff7b72);
        assert_eq!(cell(&s, 1, 0).fg, 0xff8700);
        assert_eq!(cell(&s, 2, 0).fg, 0x010203);
        assert_ne!(cell(&s, 3, 0).style & style::BOLD, 0);
        assert_ne!(cell(&s, 4, 0).style & style::UNDERLINE, 0);
        assert_eq!(cell(&s, 5, 0).fg, malakt_term::BACKGROUND);
        assert_eq!(cell(&s, 5, 0).bg, malakt_term::FOREGROUND);
        assert_eq!(cell(&s, 6, 0).bg, 0x0a141e);
        assert_eq!(cell(&s, 7, 0).bg, malakt_term::BACKGROUND);
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let (terminal, _rx) = start(&shell("printf 'aあb'; sleep 5"));
        wait(&terminal, "aあb", |s| s.line(0) == "aあb");
        let s = terminal.snapshot();
        assert_eq!(cell(&s, 1, 0).c, 'あ');
        assert_ne!(cell(&s, 1, 0).style & style::WIDE, 0);
        assert_ne!(cell(&s, 2, 0).style & style::SPACER, 0);
        assert_eq!(cell(&s, 3, 0).c, 'b');
        assert_eq!(s.cursor, Some((4, 0, Caret::Block)));
    }

    #[test]
    fn a_url_that_wraps_is_one_link() {
        // 40 桁。「go https://」の 3 桁目から始まる URL が、次の行に折り返す
        let url = "https://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/end";
        let (terminal, _rx) = start(&shell(&format!("printf 'go {url}. ok'; sleep 5")));
        wait(&terminal, "url", |s| s.line(1).ends_with(". ok"));
        // 2 行目の URL の上でも、1 行目からの URL 全体
        let link = terminal.link_at((2, 1)).expect("リンク");
        assert_eq!(link.url, url);
        assert_eq!(link.cells.first(), Some(&(3, 0)));
        assert_eq!(link.cells.len(), url.len());
        // URL の外（「go」と句点）はリンクではない
        assert_eq!(terminal.link_at((0, 0)), None);
        let dot = url.len() + 3 - 40;
        assert_eq!(terminal.link_at((dot, 1)), None);
    }

    #[test]
    fn a_hyperlink_opens_its_own_url() {
        // OSC 8。見えている文字と開く URL が違う
        let script = r"printf 'a \033]8;;https://example.com/x\033\\here\033]8;;\033\\ b \033]8;;file:///etc\033\\f\033]8;;\033\\'; sleep 5";
        let (terminal, _rx) = start(&shell(script));
        wait(&terminal, "here", |s| s.line(0) == "a here b f");
        let link = terminal.link_at((3, 0)).expect("リンク");
        assert_eq!(link.url, "https://example.com/x");
        assert_eq!(link.cells, [(2, 0), (3, 0), (4, 0), (5, 0)]);
        // Web でない URL は開かない
        assert_eq!(terminal.link_at((9, 0)), None);
        assert_eq!(terminal.link_at((7, 0)), None);
    }

    #[test]
    fn typed_keys_reach_the_program() {
        let (terminal, rx) = start(&shell("read line; echo \"got:$line\"; exit 0"));
        terminal.input("こんにちは");
        assert!(terminal.key(&Key {
            name: "enter",
            ..Default::default()
        }));
        assert_eq!(wait_exit(&rx), Some(0));
        wait(&terminal, "got", |s| s.text().contains("got:こんにちは"));
    }

    /// IME は、変換を取り消したときなどに空の文字を確定してくる。そのあとの入力も届く。
    #[test]
    fn input_still_arrives_after_an_empty_one() {
        let (terminal, _rx) = start(&shell("read a; echo got-$a; read b; echo again-$b"));
        terminal.input("");
        terminal.write(Vec::new());
        terminal.paste("");
        terminal.input("one\r");
        wait(&terminal, "1 つ目", |s| s.text().contains("got-one"));
        terminal.input("");
        terminal.input("two\r");
        wait(&terminal, "2 つ目", |s| s.text().contains("again-two"));
    }

    /// カーソルを隠すプログラム（Claude Code など）でも、入力の場所は分かる。IME の変換をそこに出す。
    #[test]
    fn the_input_place_is_known_while_the_cursor_is_hidden() {
        let (terminal, _rx) = start(&shell(
            "printf 'ab\\033[?25l'; read a; printf '\\033[?25hshown'; read b",
        ));
        wait(&terminal, "隠れたカーソル", |s| {
            s.line(0) == "ab" && s.cursor.is_none()
        });
        assert_eq!(terminal.snapshot().input_at, Some((2, 0)));
        terminal.input("\r");
        wait(&terminal, "出たカーソル", |s| {
            s.text().contains("shown") && s.cursor.is_some()
        });
        let shown = terminal.snapshot();
        assert_eq!(shown.cursor.map(|(col, row, _)| (col, row)), shown.input_at);
    }

    #[test]
    fn ctrl_c_interrupts_the_program() {
        let (terminal, rx) = start(&shell("echo ready; sleep 60"));
        wait(&terminal, "ready", |s| s.text().contains("ready"));
        let started = Instant::now();
        assert!(terminal.key(&Key {
            name: "c",
            ctrl: true,
            ..Default::default()
        }));
        assert_ne!(wait_exit(&rx), Some(0));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn the_program_sees_the_size_and_a_resize() {
        let (terminal, _rx) = start(&shell(
            "stty size; trap 'stty size' WINCH; while :; do sleep 0.05; done",
        ));
        wait(&terminal, "10 40", |s| s.line(0) == "10 40");
        terminal.resize(60, 20, (8, 16));
        assert_eq!(terminal.size(), (60, 20));
        wait(&terminal, "20 60", |s| s.text().contains("20 60"));
        assert_eq!(terminal.snapshot().cols, 60);
        assert_eq!(terminal.snapshot().rows, 20);
    }

    #[test]
    fn the_alternate_screen_comes_and_goes() {
        let (terminal, _rx) = start(&shell(
            r"echo main; printf '\033[?1049h\033[2J\033[HTUI'; read x; printf '\033[?1049l'; echo back; sleep 5",
        ));
        wait(&terminal, "TUI", |s| s.line(0) == "TUI");
        assert!(terminal.alt_screen());
        assert!(!terminal.snapshot().text().contains("main"));
        // 画面全体を使うプログラムでは、ホイールは ↑↓ として届く
        terminal.wheel(1, (0, 0), &Key::default());
        terminal.input("\r");
        wait(&terminal, "back", |s| s.text().contains("back"));
        assert!(!terminal.alt_screen());
        assert!(terminal.snapshot().text().contains("main"));
    }

    #[test]
    fn the_mouse_is_reported_to_a_program_that_asks() {
        // SGR のマウスを有効にして、届いた報告を 16 進で出す
        let (terminal, _rx) = start(&shell(
            r"printf '\033[?1000h\033[?1006h'; stty raw -echo; dd bs=1 count=9 2>/dev/null | od -An -c; stty sane; sleep 5",
        ));
        let none = Key::default();
        assert!(!terminal.mouse(Button::Left, Action::Press, (2, 3), &none));
        wait(&terminal, "マウスの報告を受ける", |_| {
            terminal.mouse_mode()
        });
        assert!(terminal.mouse(Button::Left, Action::Press, (2, 3), &none));
        wait(&terminal, "報告", |s| {
            s.text()
                .split_whitespace()
                .collect::<String>()
                .contains("[<0;3;4M")
        });
        // ボタンを押さずに動かしただけでは、送らない
        assert!(terminal.mouse(Button::None, Action::Move, (5, 5), &none));
    }

    #[test]
    fn old_output_is_dropped_past_the_scrollback_limit() {
        let lines = malakt_term::SCROLLBACK + 500;
        let (terminal, rx) = start(&shell(&format!(
            "i=0; while [ $i -lt {lines} ]; do echo line$i; i=$((i+1)); done"
        )));
        assert_eq!(wait_exit(&rx), Some(0));
        let last = format!("line{}", lines - 1);
        wait(&terminal, "最後の行", |s| s.text().contains(&last));
        assert_eq!(terminal.snapshot().history, malakt_term::SCROLLBACK);
        // さかのぼると古い行が出て、カーソルは出ない。入力すると今の出力に戻る
        terminal.wheel(3, (0, 0), &Key::default());
        let s = terminal.snapshot();
        assert_eq!(s.offset, 3);
        assert_eq!(s.cursor, None);
        terminal.input("x");
        assert_eq!(terminal.snapshot().offset, 0);
        terminal.page(true);
        assert_eq!(terminal.snapshot().offset, 10);
        terminal.to_bottom();
        assert_eq!(terminal.snapshot().offset, 0);
    }

    #[test]
    fn text_can_be_selected_and_read() {
        let (terminal, _rx) = start(&shell("echo 'hello world'; echo second; sleep 5"));
        wait(&terminal, "second", |s| s.text().contains("second"));
        assert_eq!(terminal.selected_text(), None);
        terminal.select(Select::Chars, (0, 0), false);
        terminal.select_to((4, 0), true);
        assert_eq!(terminal.selected_text().as_deref(), Some("hello"));
        let s = terminal.snapshot();
        assert_ne!(s.cells[0].style & style::SELECTED, 0);
        assert_eq!(s.cells[5].style & style::SELECTED, 0);
        terminal.select(Select::Words, (7, 0), false);
        assert_eq!(terminal.selected_text().as_deref(), Some("world"));
        terminal.select(Select::Lines, (2, 1), false);
        assert_eq!(terminal.selected_text().as_deref(), Some("second\n"));
        terminal.select_all();
        assert!(terminal
            .selected_text()
            .unwrap()
            .starts_with("hello world\nsecond"));
        terminal.clear_selection();
        assert_eq!(terminal.selected_text(), None);
    }

    #[test]
    fn a_paste_is_bracketed_when_the_program_asks() {
        let (terminal, _rx) = start(&shell(
            r"printf '\033[?2004h'; stty raw -echo; dd bs=1 count=15 2>/dev/null | od -An -c; stty sane; sleep 5",
        ));
        // 貼り付けの印を受けるようになるまで待つ
        std::thread::sleep(Duration::from_millis(300));
        terminal.paste("a\nb");
        wait(&terminal, "貼り付け", |s| {
            let text: String = s.text().split_whitespace().collect();
            text.contains("[200~a\\rb033[201~")
        });
    }

    #[test]
    fn env_and_cwd_are_passed_and_the_title_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = shell(
            r"echo $MALAKT_TASK_TITLE $TERM $COLORTERM; pwd; printf '\033]0;my title\007'; sleep 5",
        );
        options.cols = 120;
        options
            .env
            .push(("MALAKT_TASK_TITLE".into(), "資料".into()));
        options.cwd = Some(dir.path().to_owned());
        let (terminal, rx) = start(&options);
        let name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        wait(&terminal, "pwd", |s| s.text().contains(&name));
        assert_eq!(terminal.snapshot().line(0), "資料 xterm-256color truecolor");
        let started = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                Event::Title(Some(title)) => {
                    assert_eq!(title, "my title");
                    break;
                }
                _ => assert!(started.elapsed() < Duration::from_secs(20)),
            }
        }
    }

    #[test]
    fn dropping_the_terminal_ends_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("alive");
        let script = format!(
            "trap 'rm -f {0}; exit' HUP; touch {0}; echo ready; while :; do sleep 0.05; done",
            mark.display()
        );
        let (terminal, _rx) = start(&shell(&script));
        wait(&terminal, "ready", |s| s.text().contains("ready"));
        assert!(mark.exists());
        drop(terminal);
        let started = Instant::now();
        while mark.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "シェルが終わらない"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
