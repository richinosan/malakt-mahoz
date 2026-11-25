//! アプリの中のターミナルで動かす Claude Code に、このアプリの MCP サーバーを登録する。
//! ターミナルと同じシェルで `claude mcp add --scope user` を呼ぶので、`claude` と入力するだけで使える。
//! `claude` がなければ何もしない。登録済みで場所も同じなら、書き換えない。

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use malakt_term::profile::Kind;
use malakt_term::Profile;

use crate::mcp::{ENV_BIN, NAME};

/// 1 つのコマンドを待つ長さ。シェルの設定で止まったままになるのを防ぐ。
const TIMEOUT: Duration = Duration::from_secs(30);

/// プログラムと引数。
pub type Line = (String, Vec<String>);

/// 登録の手順。どれも、ターミナルと同じシェルの中で動かす。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Steps {
    /// `claude` があれば成功する。
    pub exists: Line,
    /// 登録の中身を出す。
    pub get: Line,
    /// 出力にこの文字があれば、今の場所で登録済み。`None` なら、`get` の成功が登録済み。
    pub needle: Option<String>,
    pub remove: Line,
    pub add: Line,
}

fn line(program: &str, head: &[&str], tail: &[&str]) -> Line {
    let args = head.iter().chain(tail).map(|a| (*a).to_owned()).collect();
    (program.to_owned(), args)
}

/// シェルごとの手順。`exe` は、このアプリの実行ファイル。
pub fn steps(profile: &Profile, exe: &str, windows: bool) -> Steps {
    // ログインシェルの設定（PATH）を読ませる
    steps_with(profile, exe, windows, "-lic")
}

fn steps_with(profile: &Profile, exe: &str, windows: bool, flag: &str) -> Steps {
    if profile.kind == Kind::Wsl || !windows {
        // sh・bash・zsh・fish のどれでも通る書き方にする。
        // 実行ファイルの場所は環境変数から取る（WSL の中では /mnt/c/... に読み替えられている）
        let exists = "command -v claude >/dev/null 2>&1".to_owned();
        let get = format!("claude mcp get {NAME} 2>/dev/null | grep -qF -- \"${ENV_BIN}\"");
        let remove = format!("claude mcp remove --scope user {NAME}");
        let add = format!("claude mcp add --scope user {NAME} -- \"${ENV_BIN}\" mcp");
        let wrap = |command: String| -> Line {
            let mut args = Vec::new();
            let program = if profile.kind == Kind::Wsl {
                args.extend(profile.args.iter().cloned());
                args.extend(["--".to_owned(), "bash".to_owned()]);
                profile.program.clone()
            } else {
                profile.program.clone()
            };
            args.extend([flag.to_owned(), command]);
            (program, args)
        };
        Steps {
            exists: wrap(exists),
            get: wrap(get),
            needle: None,
            remove: wrap(remove),
            add: wrap(add),
        }
    } else {
        let cmd = ["/d", "/c"];
        Steps {
            exists: line("cmd.exe", &cmd, &["where", "claude"]),
            get: line("cmd.exe", &cmd, &["claude", "mcp", "get", NAME]),
            needle: Some(exe.to_owned()),
            remove: line(
                "cmd.exe",
                &cmd,
                &["claude", "mcp", "remove", "--scope", "user", NAME],
            ),
            add: line(
                "cmd.exe",
                &cmd,
                &[
                    "claude", "mcp", "add", "--scope", "user", NAME, "--", exe, "mcp",
                ],
            ),
        }
    }
}

struct Output {
    ok: bool,
    text: String,
}

fn run(line: &Line, env: &[(String, String)]) -> Result<Output, String> {
    let mut command = Command::new(&line.0);
    command
        .args(&line.1)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // コンソールの窓を出さない
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            // 読めなかった分は、出力がなかったものとして扱う
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        })
    })
    .collect();
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if started.elapsed() > TIMEOUT => {
                // もう終わっていれば、止めるものがない
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} 秒待っても終わりませんでした",
                    TIMEOUT.as_secs()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let text = readers
        .into_iter()
        .filter_map(|reader| reader.join().ok())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Output {
        ok: status.success(),
        text,
    })
}

/// 登録する。新しく登録したら `true`、`claude` がないか登録済みなら `false`。
pub fn register(steps: &Steps, env: &[(String, String)]) -> Result<bool, String> {
    // シェルを起こせない、`claude` がない、のどちらも「使っていない」として扱う
    if !run(&steps.exists, env).is_ok_and(|out| out.ok) {
        return Ok(false);
    }
    let found = run(&steps.get, env)?;
    let same = match &steps.needle {
        Some(needle) => found.text.contains(needle.as_str()),
        None => found.ok,
    };
    if same {
        return Ok(false);
    }
    // 古い場所の登録があれば消す。なければ失敗するが、そのまま足す
    let _ = run(&steps.remove, env);
    let added = run(&steps.add, env)?;
    if added.ok {
        Ok(true)
    } else {
        let reason = added
            .text
            .trim()
            .lines()
            .last()
            .unwrap_or_default()
            .to_owned();
        Err(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wsl() -> Profile {
        Profile {
            id: "wsl:Ubuntu".into(),
            name: "Ubuntu".into(),
            kind: Kind::Wsl,
            program: "wsl.exe".into(),
            args: vec!["-d".into(), "Ubuntu".into()],
        }
    }

    fn shell(program: &str) -> Profile {
        Profile {
            id: program.into(),
            name: program.into(),
            kind: Kind::Shell,
            program: program.into(),
            args: Vec::new(),
        }
    }

    #[test]
    fn wsl_runs_inside_the_distribution_with_the_translated_path() {
        let steps = steps(&wsl(), r"C:\Apps\malakt-mahoz.exe", true);
        assert_eq!(steps.add.0, "wsl.exe");
        assert_eq!(
            steps.add.1,
            [
                "-d",
                "Ubuntu",
                "--",
                "bash",
                "-lic",
                "claude mcp add --scope user malakt-mahoz -- \"$MALAKT_MAHOZ_BIN\" mcp"
            ]
        );
        assert_eq!(steps.needle, None);
    }

    #[test]
    fn windows_shells_register_the_exe_itself() {
        let exe = r"C:\Program Files\Malakt Mahoz\malakt-mahoz.exe";
        let steps = steps(&shell("pwsh.exe"), exe, true);
        assert_eq!(steps.add.0, "cmd.exe");
        assert_eq!(
            steps.add.1,
            [
                "/d",
                "/c",
                "claude",
                "mcp",
                "add",
                "--scope",
                "user",
                "malakt-mahoz",
                "--",
                exe,
                "mcp"
            ]
        );
        assert_eq!(steps.needle.as_deref(), Some(exe));
    }

    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use super::shell;
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;

        /// 登録を 1 つのファイルに覚える、にせの `claude`。
        fn fake_claude(dir: &Path) {
            let script = r#"#!/bin/sh
state="$FAKE_STATE"
case "$2" in
  get) [ -f "$state" ] && cat "$state" || { echo "No MCP server named \"$3\"."; exit 1; } ;;
  remove) echo remove >> "$state.log"; rm -f "$state" ;;
  add) [ -n "$FAKE_FAIL" ] && { echo "書けませんでした" >&2; exit 1; }
       echo add >> "$state.log"; echo "Command: $7" > "$state" ;;
esac
"#;
            let path = dir.join("claude");
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn env(dir: &Path, exe: &str) -> Vec<(String, String)> {
            let path = std::env::var("PATH").unwrap_or_default();
            vec![
                ("PATH".into(), format!("{}:{path}", dir.display())),
                ("HOME".into(), dir.display().to_string()),
                ("ENV".into(), String::new()),
                ("FAKE_STATE".into(), dir.join("state").display().to_string()),
                (ENV_BIN.into(), exe.into()),
            ]
        }

        #[test]
        fn claude_is_registered_once_and_again_when_the_app_moves() {
            let dir = tempfile::tempdir().unwrap();
            fake_claude(dir.path());
            // テストでは、シェルの設定を読ませずに、にせの `claude` を PATH から見つけさせる
            let steps = steps_with(&shell("sh"), "", false, "-c");
            let log = dir.path().join("state.log");

            let first = env(dir.path(), "/apps/one/malakt-mahoz");
            assert_eq!(register(&steps, &first), Ok(true));
            assert_eq!(
                std::fs::read_to_string(dir.path().join("state")).unwrap(),
                "Command: /apps/one/malakt-mahoz\n"
            );
            // 同じ場所なら、書き換えない
            assert_eq!(register(&steps, &first), Ok(false));
            assert_eq!(std::fs::read_to_string(&log).unwrap(), "remove\nadd\n");

            let moved = env(dir.path(), "/apps/two/malakt-mahoz");
            assert_eq!(register(&steps, &moved), Ok(true));
            assert_eq!(
                std::fs::read_to_string(dir.path().join("state")).unwrap(),
                "Command: /apps/two/malakt-mahoz\n"
            );
        }

        #[test]
        fn nothing_happens_without_claude() {
            let dir = tempfile::tempdir().unwrap();
            // テストでは、シェルの設定を読ませずに、にせの `claude` を PATH から見つけさせる
            let steps = steps_with(&shell("sh"), "", false, "-c");
            let mut env = env(dir.path(), "/apps/malakt-mahoz");
            env[0].1 = "/usr/bin:/bin".into();
            if std::path::Path::new("/usr/bin/claude").exists() {
                return;
            }
            assert_eq!(register(&steps, &env), Ok(false));
        }

        #[test]
        fn a_failed_registration_carries_the_reason() {
            let dir = tempfile::tempdir().unwrap();
            fake_claude(dir.path());
            // テストでは、シェルの設定を読ませずに、にせの `claude` を PATH から見つけさせる
            let steps = steps_with(&shell("sh"), "", false, "-c");
            let mut env = env(dir.path(), "/apps/malakt-mahoz");
            env.push(("FAKE_FAIL".into(), "1".into()));
            assert_eq!(register(&steps, &env), Err("書けませんでした".to_owned()));
        }
    }
}
