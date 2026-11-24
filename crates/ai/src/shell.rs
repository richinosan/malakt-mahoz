//! コマンドの実行。出力を 1 行ずつ受け取り、途中で強制終了できる。

use std::io::{BufRead, BufReader, Read};
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

fn pump(reader: impl Read, mut emit: impl FnMut(String)) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                // 文字コードが UTF-8 でない出力も、読めるところまで出す
                let text = String::from_utf8_lossy(&line);
                emit(text.trim_end_matches(['\n', '\r']).to_owned());
            }
        }
    }
}

/// コマンドを起こす。出力と終了は、別のスレッドから `on_event` に届く。
/// 最後に必ず [`Event::Exit`] が 1 回届く。
pub fn spawn(
    spec: &Spec,
    on_event: impl Fn(Event) + Send + Sync + 'static,
) -> std::io::Result<Handle> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
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
    fn env_and_cwd_are_passed() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = Spec::shell(if cfg!(windows) {
            "echo %MALAKT_TEST%& cd"
        } else {
            "echo $MALAKT_TEST; pwd"
        });
        spec.env.push(("MALAKT_TEST".into(), "こんにちは".into()));
        spec.cwd = Some(dir.path().to_owned());
        let (_, rx) = collect(&spec);
        let (events, code) = wait_exit(&rx);
        assert_eq!(code, Some(0));
        assert_eq!(events[0], Event::Stdout("こんにちは".into()));
        let name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(matches!(&events[1], Event::Stdout(s) if s.contains(&name)));
    }
}
