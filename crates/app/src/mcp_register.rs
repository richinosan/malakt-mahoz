//! アプリの中のターミナルで動かす Claude Code と Cursor に、このアプリの MCP サーバーを登録する。
//! Claude Code は、ターミナルと同じシェルで `claude mcp add --scope user` を呼ぶ。Cursor は `~/.cursor/mcp.json` に書き足す。
//! どちらも、使っていなければ何もしない。登録済みで場所も同じなら、書き換えない。
//! WSL の中には、`malakt-mahoz` の名前でこのアプリを呼ぶコマンドも置く。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use malakt_term::profile::Kind;
use malakt_term::Profile;

use serde_json::{json, Value};

use crate::mcp::{ENV_BIN, NAME};

/// 1 つのコマンドを待つ長さ。シェルの設定で止まったままになるのを防ぐ。
const TIMEOUT: Duration = Duration::from_secs(30);

/// 動かすコマンド。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub program: String,
    pub args: Vec<String>,
    /// 標準入力に流すスクリプト。
    pub stdin: Option<String>,
    /// 足す環境変数。WSL の中にも渡す。
    pub env: Vec<(String, String)>,
}

/// WSL の中で動かすコマンドを入れる環境変数。引数にすると、`wsl.exe` を通る間に引用符が崩れる。
const ENV_RUN: &str = "MALAKT_MAHOZ_RUN";

/// WSL の中で、スクリプトを `sh` に標準入力から読ませる。
fn wsl_sh(profile: &Profile, script: String, env: Vec<(String, String)>) -> Line {
    let mut args = profile.args.clone();
    args.extend(["--".to_owned(), "sh".to_owned()]);
    Line {
        program: profile.program.clone(),
        args,
        stdin: Some(script),
        env,
    }
}

/// WSL の中の、その人のログインシェル（zsh など）でコマンドを動かす。設定（PATH）を読ませる。
fn wsl_login(profile: &Profile, command: String) -> Line {
    let script = format!("exec \"${{SHELL:-bash}}\" -lic \"${ENV_RUN}\"\n");
    wsl_sh(profile, script, vec![(ENV_RUN.to_owned(), command)])
}

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
    Line {
        program: program.to_owned(),
        args: head.iter().chain(tail).map(|a| (*a).to_owned()).collect(),
        ..Line::default()
    }
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
            if profile.kind == Kind::Wsl {
                wsl_login(profile, command)
            } else {
                line(&profile.program, &[flag, &command], &[])
            }
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
    let mut command = Command::new(&line.program);
    command
        .args(&line.args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .envs(line.env.iter().map(|(k, v)| (k, v)))
        .stdin(if line.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // コンソールの窓を出さない
        command.creation_flags(0x0800_0000);
    }
    if !line.env.is_empty() {
        let mut names: Vec<&str> = env
            .iter()
            .filter(|(name, _)| name == "WSLENV")
            .map(|(_, value)| value.as_str())
            .collect();
        names.extend(line.env.iter().map(|(name, _)| name.as_str()));
        command.env("WSLENV", names.join(":"));
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    if let (Some(script), Some(mut pipe)) = (line.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            // 読まずに終わったシェルには、書けなくてよい
            let _ = pipe.write_all(script.as_bytes());
        });
    }
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

/// Cursor の設定（`mcp.json`）に、このアプリの MCP サーバーを書き足す。ほかのサーバーはそのまま残す。
/// 書き換えたら `true`、同じ中身がもうあれば `false`。読めないファイルは書き換えない。
pub fn merge_cursor(file: &Path, exe: &str) -> Result<bool, String> {
    let shown = file.display();
    let mut root = match std::fs::read_to_string(file) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("{shown} を読めません（{e}）"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{shown} を読めません（{e}）")),
    };
    let wanted = crate::mcp::config_json(Path::new(exe), None)["mcpServers"][NAME].clone();
    let Some(top) = root.as_object_mut() else {
        return Err(format!("{shown} の形が違います"));
    };
    let servers = top.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err(format!("{shown} の mcpServers の形が違います"));
    };
    if servers.get(NAME) == Some(&wanted) {
        return Ok(false);
    }
    servers.insert(NAME.to_owned(), wanted);
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n";
    std::fs::write(file, text).map_err(|e| format!("{shown} に書けません（{e}）"))?;
    Ok(true)
}

/// WSL の中の Cursor の設定の場所（Windows から見た場所）と、WSL から見たこのアプリの場所を調べる。
/// Cursor を使っていなければ、何も出さない。
fn cursor_probe(profile: &Profile) -> Line {
    let command = format!(
        "test -d ~/.cursor && printf '{MARK}%s\\n{MARK}%s\\n' \"$(wslpath -w ~/.cursor)\" \"${ENV_BIN}\""
    );
    wsl_sh(profile, command + "\n", Vec::new())
}

/// `malakt-mahoz` の名前でこのアプリを呼ぶコマンドを、`~/.local/bin` に置くスクリプト。
/// フォルダーがなければ何もしない。ほかの人が置いた同じ名前のファイルは書き換えない。
fn command_script() -> String {
    format!(
        r#"d="$HOME/.local/bin"; f="$d/{NAME}"
[ -d "$d" ] || exit 0
[ -n "${ENV_BIN}" ] || exit 0
if [ -e "$f" ] && ! grep -q '^# {NAME}$' "$f"; then exit 0; fi
printf '#!/bin/sh\n# {NAME}\nexec "%s" "$@"\n' "${ENV_BIN}" > "$f.new" && chmod +x "$f.new" || exit 1
if cmp -s "$f.new" "$f"; then rm -f "$f.new"; else mv -f "$f.new" "$f"; fi
"#
    )
}

/// WSL の中で、`malakt-mahoz mcp` のように名前でこのアプリを呼べるようにする。
pub fn install_command(profile: &Profile, env: &[(String, String)]) -> Result<(), String> {
    if profile.kind != Kind::Wsl {
        return Ok(());
    }
    let out = run(&wsl_sh(profile, command_script(), Vec::new()), env)?;
    if out.ok {
        Ok(())
    } else {
        Err(out
            .text
            .trim()
            .lines()
            .last()
            .unwrap_or_default()
            .to_owned())
    }
}

/// シェルの設定が出す文字と見分けるための目印。
const MARK: &str = "malakt-mahoz:";

/// [`cursor_probe`] の出力から、設定のフォルダーとアプリの場所を取り出す。
fn parse_probe(text: &str) -> Option<(PathBuf, String)> {
    let mut found = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix(MARK))
        .filter(|value| !value.is_empty());
    Some((PathBuf::from(found.next()?), found.next()?.to_owned()))
}

/// このシェルで使う Cursor（`agent` コマンドとエディター）に登録する。
/// 書き換えたら `Some(true)`、登録済みなら `Some(false)`、Cursor のフォルダー（`~/.cursor`）がなければ `None`。
/// `home` は、このアプリを動かしている人のホーム。
pub fn register_cursor(
    profile: &Profile,
    exe: &str,
    env: &[(String, String)],
    home: Option<&Path>,
) -> Result<Option<bool>, String> {
    let (dir, exe) = if profile.kind == Kind::Wsl {
        // シェルを起こせない、Cursor がない、のどちらも「使っていない」として扱う
        let Some(found) = run(&cursor_probe(profile), env)
            .ok()
            .and_then(|out| parse_probe(&out.text))
        else {
            return Ok(None);
        };
        found
    } else {
        let Some(home) = home else {
            return Ok(None);
        };
        (home.join(".cursor"), exe.to_owned())
    };
    if !dir.is_dir() {
        return Ok(None);
    }
    merge_cursor(&dir.join("mcp.json"), &exe).map(Some)
}

/// ターミナルのメニューから登録できるエージェント。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

/// 見ているシェルで、このアプリを登録するコマンド。実行ファイルの場所は、ターミナルに渡した環境変数から取る。
/// `shell` は、シェルの名前（[`Profile::id`]）。
pub fn command(agent: Agent, shell: &str, style: malakt_term::PathStyle) -> String {
    use malakt_term::PathStyle;
    let exe = match (style, shell) {
        (PathStyle::Windows, "cmd") => format!("\"%{ENV_BIN}%\""),
        (PathStyle::Windows, _) => format!("\"$env:{ENV_BIN}\""),
        (PathStyle::Posix | PathStyle::Wsl, _) => format!("\"${ENV_BIN}\""),
    };
    match agent {
        Agent::Claude => format!("claude mcp add --scope user {NAME} -- {exe} mcp"),
        Agent::Codex => format!("codex mcp add {NAME} -- {exe} mcp"),
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
        assert_eq!(steps.add.program, "wsl.exe");
        assert_eq!(steps.add.args, ["-d", "Ubuntu", "--", "sh"]);
        // その人のログインシェルで動かす。コマンドは環境変数で渡す
        assert_eq!(
            steps.add.stdin.as_deref(),
            Some("exec \"${SHELL:-bash}\" -lic \"$MALAKT_MAHOZ_RUN\"\n")
        );
        assert_eq!(
            steps.add.env,
            [(
                "MALAKT_MAHOZ_RUN".to_owned(),
                "claude mcp add --scope user malakt-mahoz -- \"$MALAKT_MAHOZ_BIN\" mcp".to_owned()
            )]
        );
        assert_eq!(steps.needle, None);
    }

    #[test]
    fn windows_shells_register_the_exe_itself() {
        let exe = r"C:\Program Files\Malakt Mahoz\malakt-mahoz.exe";
        let steps = steps(&shell("pwsh.exe"), exe, true);
        assert_eq!(steps.add.program, "cmd.exe");
        assert_eq!(
            steps.add.args,
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

    #[test]
    fn cursor_gets_the_server_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let env = Vec::new();
        // Cursor を使っていなければ、何も作らない
        assert_eq!(
            register_cursor(&shell("sh"), "/apps/mm", &env, Some(home)),
            Ok(None)
        );
        assert!(!home.join(".cursor").exists());

        let file = home.join(".cursor").join("mcp.json");
        std::fs::create_dir(home.join(".cursor")).unwrap();
        assert_eq!(
            register_cursor(&shell("sh"), "/apps/mm", &env, Some(home)),
            Ok(Some(true))
        );
        let read =
            || serde_json::from_str::<Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            read(),
            json!({"mcpServers": {"malakt-mahoz": {"type": "stdio", "command": "/apps/mm", "args": ["mcp"]}}})
        );
        // 同じ中身なら書き換えない
        assert_eq!(
            register_cursor(&shell("sh"), "/apps/mm", &env, Some(home)),
            Ok(Some(false))
        );

        std::fs::write(
            &file,
            r#"{"mcpServers": {"other": {"url": "https://example.com/mcp"}}, "extra": 1}"#,
        )
        .unwrap();
        assert_eq!(
            register_cursor(&shell("sh"), "/apps/two", &env, Some(home)),
            Ok(Some(true))
        );
        let after = read();
        assert_eq!(
            after["mcpServers"]["other"]["url"],
            "https://example.com/mcp"
        );
        assert_eq!(after["extra"], 1);
        assert_eq!(after["mcpServers"]["malakt-mahoz"]["command"], "/apps/two");
    }

    #[test]
    fn the_command_fits_the_shell() {
        use malakt_term::PathStyle;
        assert_eq!(
            command(Agent::Claude, "wsl:Ubuntu", PathStyle::Wsl),
            "claude mcp add --scope user malakt-mahoz -- \"$MALAKT_MAHOZ_BIN\" mcp"
        );
        assert_eq!(
            command(Agent::Codex, "zsh", PathStyle::Posix),
            "codex mcp add malakt-mahoz -- \"$MALAKT_MAHOZ_BIN\" mcp"
        );
        assert_eq!(
            command(Agent::Claude, "pwsh", PathStyle::Windows),
            "claude mcp add --scope user malakt-mahoz -- \"$env:MALAKT_MAHOZ_BIN\" mcp"
        );
        assert_eq!(
            command(Agent::Codex, "cmd", PathStyle::Windows),
            "codex mcp add malakt-mahoz -- \"%MALAKT_MAHOZ_BIN%\" mcp"
        );
    }

    #[test]
    fn a_broken_cursor_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mcp.json");
        for broken in ["{ これは JSON ではない", "[1, 2]", r#"{"mcpServers": 3}"#] {
            std::fs::write(&file, broken).unwrap();
            assert!(merge_cursor(&file, "/apps/mm").is_err());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
        }
    }

    #[test]
    fn wsl_reports_where_cursor_keeps_its_settings() {
        let line = cursor_probe(&wsl());
        assert_eq!(line.program, "wsl.exe");
        assert_eq!(line.args, ["-d", "Ubuntu", "--", "sh"]);
        assert_eq!(
            line.stdin.as_deref(),
            Some("test -d ~/.cursor && printf 'malakt-mahoz:%s\\nmalakt-mahoz:%s\\n' \"$(wslpath -w ~/.cursor)\" \"$MALAKT_MAHOZ_BIN\"\n")
        );
        let out = "bash: no job control in this shell\r\nmalakt-mahoz:\\\\wsl.localhost\\Ubuntu\\home\\me\\.cursor\r\nmalakt-mahoz:/mnt/c/Apps/malakt-mahoz.exe\r\n";
        assert_eq!(
            parse_probe(out),
            Some((
                PathBuf::from(r"\\wsl.localhost\Ubuntu\home\me\.cursor"),
                "/mnt/c/Apps/malakt-mahoz.exe".to_owned()
            ))
        );
        assert_eq!(parse_probe("bash: wslpath: command not found\n"), None);
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

        /// WSL の代わりに、手元の `sh` にスクリプトを読ませる。
        fn local(line: Line) -> Line {
            Line {
                program: "sh".into(),
                args: Vec::new(),
                ..line
            }
        }

        #[test]
        fn the_login_shell_gets_the_command_through_the_environment() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out");
            let command = format!(
                "printf '%s|' \"it's\" \"$MALAKT_MAHOZ_BIN\" > '{}'",
                out.display()
            );
            let line = local(wsl_login(&super::wsl(), command));
            let env = vec![
                ("SHELL".to_owned(), "sh".to_owned()),
                ("ENV".to_owned(), String::new()),
                (ENV_BIN.to_owned(), "/mnt/c/My Apps/mm.exe".to_owned()),
            ];
            assert!(run(&line, &env).unwrap().ok);
            assert_eq!(
                std::fs::read_to_string(out).unwrap(),
                "it's|/mnt/c/My Apps/mm.exe|"
            );
        }

        #[test]
        fn the_command_is_placed_in_local_bin_and_follows_the_app() {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path();
            let file = home.join(".local/bin/malakt-mahoz");
            let env = |exe: &str| {
                vec![
                    ("HOME".to_owned(), home.display().to_string()),
                    (ENV_BIN.to_owned(), exe.to_owned()),
                ]
            };
            let install = |exe: &str| {
                let line = local(wsl_sh(&super::wsl(), command_script(), Vec::new()));
                run(&line, &env(exe)).unwrap().ok
            };
            // ~/.local/bin がなければ、何も作らない
            assert!(install("/mnt/c/Apps/mm.exe"));
            assert!(!home.join(".local").exists());

            std::fs::create_dir_all(home.join(".local/bin")).unwrap();
            // 実行ファイルの代わりに、引数を出すスクリプトを呼ばせる
            let exe = home.join("my app.sh");
            std::fs::write(&exe, "#!/bin/sh\necho \"called:$1:$2\"\n").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(install(&exe.display().to_string()));
            let out = std::process::Command::new(&file)
                .args(["mcp", "a b"])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), "called:mcp:a b\n");

            // アプリの場所が変わったら、書き直す
            assert!(install("/mnt/d/New/mm.exe"));
            let text = std::fs::read_to_string(&file).unwrap();
            assert_eq!(
                text,
                "#!/bin/sh\n# malakt-mahoz\nexec \"/mnt/d/New/mm.exe\" \"$@\"\n"
            );
            assert!(!home.join(".local/bin/malakt-mahoz.new").exists());

            // ほかの人が置いた同じ名前のファイルは、書き換えない
            std::fs::write(&file, "#!/bin/sh\necho mine\n").unwrap();
            assert!(install("/mnt/c/Apps/mm.exe"));
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "#!/bin/sh\necho mine\n"
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
