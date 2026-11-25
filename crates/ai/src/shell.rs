//! コマンドの実行。出力を 1 行ずつ受け取り、途中で強制終了できる。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 実行するコマンド。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    /// 画面に出す形。
    pub display: String,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// 標準入力に流す文字。なければ、標準入力は閉じておく。
    pub stdin: Option<String>,
}

impl Spec {
    /// OS のシェルに 1 行のコマンドを渡す。
    pub fn shell(command: &str) -> Self {
        let (program, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        Self {
            program: program.into(),
            args: vec![flag.into(), command.into()],
            display: command.into(),
            ..Default::default()
        }
    }

    /// シェルを通さずに、プログラムと引数をそのまま渡す。
    pub fn direct(program: &str, args: Vec<String>) -> Self {
        let display = std::iter::once(program)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            program: program.into(),
            args,
            display,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Stdout(String),
    Stderr(String),
    /// 終了。シグナルで止まったときは `None`。
    Exit(Option<i32>),
}

/// 実行中のコマンド。
#[derive(Debug, Clone)]
pub struct Handle {
    pid: u32,
    done: Arc<AtomicBool>,
}

impl Handle {
    /// 強制終了する。コマンドが起こした子のプロセスも止める。
    pub fn kill(&self) -> std::io::Result<()> {
        // 終わったあとの pid は、別のプロセスに使い回されているかもしれない
        if self.done.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut killer = if cfg!(windows) {
            let mut c = Command::new("taskkill");
            c.args(["/PID", &self.pid.to_string(), "/T", "/F"]);
            c
        } else {
            // 自分のプロセスグループを持たせてあるので、グループごと止める
            let mut c = Command::new("kill");
            c.args(["-KILL", "--", &format!("-{}", self.pid)]);
            c
        };
        hide_window(&mut killer);
        killer
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|_| ())
    }
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_: &mut Command) {}

/// Windows のコマンドが出力に使う文字コード（日本語の Windows なら 932）。
#[cfg(windows)]
fn oem_code_page() -> u32 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn GetOEMCP() -> u32;
    }
    GetOEMCP()
}

#[cfg(not(windows))]
fn oem_code_page() -> u32 {
    65001
}

/// コマンドの出力 1 行を文字列にする。UTF-8 として読めればそのまま、読めなければ
/// `code_page`（Windows の cmd や古いコマンドが使う Shift_JIS など）として読む。
fn decode(line: &[u8], code_page: u32) -> String {
    if let Ok(text) = std::str::from_utf8(line) {
        return text.to_owned();
    }
    let label = match code_page {
        932 => "shift_jis".to_owned(),
        936 => "gbk".to_owned(),
        949 => "euc-kr".to_owned(),
        950 => "big5".to_owned(),
        874 | 1250..=1258 => format!("windows-{code_page}"),
        866 => "ibm866".to_owned(),
        // そのほかは、読めるところまで出す
        _ => return String::from_utf8_lossy(line).into_owned(),
    };
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(encoding) => encoding.decode(line).0.into_owned(),
        None => String::from_utf8_lossy(line).into_owned(),
    }
}

fn pump(reader: impl Read, mut emit: impl FnMut(String)) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let text = decode(&line, oem_code_page());
                emit(text.trim_end_matches(['\n', '\r']).to_owned());
            }
        }
    }
}

/// PATH から、実行するファイルを探す。`exts` は、名前に足して試す拡張子（Windows の PATHEXT）。
/// Windows の `gh` は `gh.exe` とは限らず、`gh.cmd` のこともある。
fn find_program(
    name: &str,
    dirs: &[PathBuf],
    exts: &[String],
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<PathBuf> {
    if name.contains(['/', '\\']) {
        return None;
    }
    let has_ext = std::path::Path::new(name).extension().is_some();
    dirs.iter().find_map(|dir| {
        if has_ext && exists(&dir.join(name)) {
            return Some(dir.join(name));
        }
        exts.iter()
            .map(|ext| dir.join(format!("{name}{ext}")))
            .find(|path| exists(path))
    })
}

/// 起こすプログラム。Windows では PATH と PATHEXT から探し、`.cmd` や `.bat` も見つける。
fn program_path(program: &str) -> PathBuf {
    if !cfg!(windows) {
        return program.into();
    }
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(str::to_owned)
        .collect();
    find_program(program, &dirs, &exts, |path| path.is_file()).unwrap_or_else(|| program.into())
}

/// コマンドを起こす。出力と終了は、別のスレッドから `on_event` に届く。
/// 最後に必ず [`Event::Exit`] が 1 回届く。
pub fn spawn(
    spec: &Spec,
    on_event: impl Fn(Event) + Send + Sync + 'static,
) -> std::io::Result<Handle> {
    let mut command = Command::new(program_path(&spec.program));
    command
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    hide_window(&mut command);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    if let (Some(text), Some(mut pipe)) = (spec.stdin.clone(), child.stdin.take()) {
        // 出力を読むのと並べて書く。書き終わったら閉じて、読む側に終わりを知らせる
        std::thread::spawn(move || {
            // 読まずに終わったコマンドには、書けなくてよい
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let handle = Handle {
        pid: child.id(),
        done: Arc::new(AtomicBool::new(false)),
    };
    let on_event = Arc::new(on_event);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let done = handle.done.clone();
    std::thread::spawn(move || {
        let out = stdout.map(|pipe| {
            let on_event = on_event.clone();
            std::thread::spawn(move || pump(pipe, |line| on_event(Event::Stdout(line))))
        });
        if let Some(pipe) = stderr {
            pump(pipe, |line| on_event(Event::Stderr(line)));
        }
        if let Some(out) = out {
            // 読むスレッドが panic しても、終了は必ず知らせる
            let _ = out.join();
        }
        let code = child.wait().ok().and_then(|status| status.code());
        done.store(true, Ordering::SeqCst);
        on_event(Event::Exit(code));
    });
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_found_with_the_extensions_of_the_path() {
        let dirs = [PathBuf::from("a"), PathBuf::from("b")];
        let exts = [".EXE".to_owned(), ".CMD".to_owned()];
        let found = |name: &str, files: &[&str]| {
            let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            find_program(name, &dirs, &exts, |path| files.iter().any(|f| f == path))
        };
        // gh.exe がなく、gh.cmd だけがある
        assert_eq!(
            found("gh", &["b/gh.CMD"]),
            Some(PathBuf::from("b").join("gh.CMD"))
        );
        // 先のフォルダーが勝ち、同じフォルダーでは PATHEXT の順
        assert_eq!(
            found("gh", &["a/gh.CMD", "b/gh.EXE"]),
            Some(PathBuf::from("a").join("gh.CMD"))
        );
        assert_eq!(
            found("gh.exe", &["b/gh.exe"]),
            Some(PathBuf::from("b").join("gh.exe"))
        );
        assert_eq!(found("gh", &[]), None);
        assert_eq!(found("./gh", &["a/gh.EXE"]), None);
    }
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn collect(spec: &Spec) -> (Handle, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let handle = spawn(spec, move |event| {
            // 受け取る側が先に終わったテストでは、送れなくてよい
            let _ = tx.lock().unwrap().send(event);
        })
        .unwrap();
        (handle, rx)
    }

    fn wait_exit(rx: &mpsc::Receiver<Event>) -> (Vec<Event>, Option<i32>) {
        let mut events = Vec::new();
        loop {
            match rx
                .recv_timeout(Duration::from_secs(20))
                .expect("終了が届く")
            {
                Event::Exit(code) => return (events, code),
                other => events.push(other),
            }
        }
    }

    #[test]
    fn output_and_exit_code_are_delivered() {
        let command = if cfg!(windows) {
            "echo out& echo err 1>&2& exit /b 3"
        } else {
            "echo out; echo err >&2; exit 3"
        };
        let (_, rx) = collect(&Spec::shell(command));
        let (events, code) = wait_exit(&rx);
        assert_eq!(code, Some(3));
        assert!(events.contains(&Event::Stdout("out".into())));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Stderr(s) if s.trim() == "err")));
    }

    #[test]
    fn a_running_command_can_be_killed_with_its_children() {
        let command = if cfg!(windows) {
            "ping -n 60 127.0.0.1"
        } else {
            "sleep 60 & sleep 60"
        };
        let started = Instant::now();
        let (handle, rx) = collect(&Spec::shell(command));
        std::thread::sleep(Duration::from_millis(300));
        handle.kill().unwrap();
        let (_, code) = wait_exit(&rx);
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_ne!(code, Some(0));
        // 終わったあとにもう一度呼んでも、何も起きない
        handle.kill().unwrap();
    }

    #[test]
    fn a_missing_program_fails_to_start() {
        let spec = Spec::direct("malakt-mahoz-no-such-program", vec!["a".into()]);
        assert_eq!(spec.display, "malakt-mahoz-no-such-program a");
        let error = spawn(&spec, |_| {}).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn output_in_the_windows_code_page_is_readable() {
        // UTF-8 はそのまま
        assert_eq!(decode("こんにちは".as_bytes(), 932), "こんにちは");
        // 日本語の Windows の cmd が出す Shift_JIS
        let sjis = [0x82, 0xb1, 0x82, 0xf1, 0x82, 0xc9, 0x82, 0xbf, 0x82, 0xcd];
        assert_eq!(decode(&sjis, 932), "こんにちは");
        // 西欧の Windows-1252
        assert_eq!(decode(&[b'c', b'a', b'f', 0xe9], 1252), "café");
        // 分からない文字コードでも、読めるところは出す
        assert_eq!(decode(&[b'o', b'k', 0xff], 437), "ok\u{fffd}");
        assert_eq!(decode(&[b'o', b'k', 0xff], 65001), "ok\u{fffd}");
    }

    #[test]
    fn text_is_fed_to_standard_input() {
        let mut spec = if cfg!(windows) {
            Spec::direct("findstr", vec!["a".into()])
        } else {
            Spec::direct("sh", Vec::new())
        };
        spec.stdin = Some(if cfg!(windows) {
            "abc\r\n".into()
        } else {
            "echo \"$MALAKT_TEST\" abc\n".into()
        });
        spec.env.push(("MALAKT_TEST".into(), "x y".into()));
        let (_, rx) = collect(&spec);
        let (events, code) = wait_exit(&rx);
        assert_eq!(code, Some(0));
        let line = if cfg!(windows) { "abc" } else { "x y abc" };
        assert!(events.contains(&Event::Stdout(line.into())), "{events:?}");
    }

    #[test]
    fn env_and_cwd_are_passed() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = Spec::shell(if cfg!(windows) {
            "echo %MALAKT_TEST%& cd"
        } else {
            "echo $MALAKT_TEST; pwd"
        });
        // Windows の cmd は端末の文字コードで出すので、CI（英語の Windows）では日本語を出せない。
        // 日本語の出力は `output_in_the_windows_code_page_is_readable` で確かめる
        let value = if cfg!(windows) {
            "hello"
        } else {
            "こんにちは"
        };
        spec.env.push(("MALAKT_TEST".into(), value.into()));
        spec.cwd = Some(dir.path().to_owned());
        let (_, rx) = collect(&spec);
        let (events, code) = wait_exit(&rx);
        assert_eq!(code, Some(0));
        assert_eq!(events[0], Event::Stdout(value.into()));
        let name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(matches!(&events[1], Event::Stdout(s) if s.contains(&name)));
    }
}
