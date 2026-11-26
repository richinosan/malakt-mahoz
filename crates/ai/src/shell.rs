//! コマンドの実行。出力を 1 行ずつ受け取り、途中で強制終了できる。
//!
//! AI のコマンドが動くシェルは [`Shell`] で 1 つに決める。ユーザーが今開いているターミナルの
//! タブのシェル、AI のタブなら設定で選んだシェル（選んでいなければ、Windows は PowerShell、
//! macOS と Linux は bash）。

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
    /// 画面でコマンドの前に出す印（`PS>` や `$`）。空なら `$`。
    pub mark: String,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// 標準入力に流す文字。なければ、標準入力は閉じておく。
    pub stdin: Option<String>,
}

impl Spec {
    /// 標準のシェル（[`Shell::standard`]）に 1 行のコマンドを渡す。
    pub fn shell(command: &str) -> Self {
        Shell::standard().spec(command)
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

/// WSL の中で動かすコマンドを入れる環境変数。引数にすると、`wsl.exe` を通る間に引用符が崩れる。
const ENV_COMMAND: &str = "MALAKT_MAHOZ_COMMAND";

/// AI のコマンドが動くシェル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shell {
    /// PowerShell。`legacy` は Windows に最初から入っている Windows PowerShell 5.1。
    PowerShell { program: String, legacy: bool },
    /// Windows のコマンドプロンプト（cmd.exe）。
    Cmd,
    /// WSL のディストリビューションの中の、その人のログインシェル。
    Wsl { distro: String },
    /// bash・zsh・sh などの、macOS と Linux のシェル。`program` は場所か名前。
    Unix { program: String },
}

impl Shell {
    /// 設定でシェルを選んでいないときのシェル。Windows は PowerShell（PowerShell 7 が入っていれば
    /// それ、なければ Windows PowerShell）、macOS と Linux は bash（なければ sh）。
    /// アプリが動いている間は変えない（AI への指示が会話の途中で変わらないように）。
    pub fn standard() -> Self {
        static STANDARD: std::sync::OnceLock<Shell> = std::sync::OnceLock::new();
        STANDARD
            .get_or_init(|| Self::standard_for(cfg!(windows), malakt_term::env_path::has))
            .clone()
    }

    fn standard_for(windows: bool, has: impl Fn(&str) -> bool) -> Self {
        if windows {
            if has("pwsh.exe") {
                Self::PowerShell {
                    program: "pwsh.exe".into(),
                    legacy: false,
                }
            } else {
                Self::PowerShell {
                    program: "powershell.exe".into(),
                    legacy: true,
                }
            }
        } else if has("bash") {
            Self::Unix {
                program: "bash".into(),
            }
        } else {
            Self::Unix {
                program: "sh".into(),
            }
        }
    }

    /// ターミナルのタブのシェル（[`malakt_term::Profile`]）と同じシェル。
    pub fn of_profile(profile: &malakt_term::Profile) -> Self {
        if profile.kind == malakt_term::profile::Kind::Wsl {
            return Self::Wsl {
                distro: profile.name.clone(),
            };
        }
        let stem = std::path::Path::new(&profile.program.replace('\\', "/"))
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match stem.as_str() {
            "pwsh" => Self::PowerShell {
                program: profile.program.clone(),
                legacy: false,
            },
            "powershell" => Self::PowerShell {
                program: profile.program.clone(),
                legacy: true,
            },
            "cmd" => Self::Cmd,
            _ => Self::Unix {
                program: profile.program.clone(),
            },
        }
    }

    /// 画面と AI に見せる名前。
    pub fn label(&self) -> String {
        match self {
            Self::PowerShell { legacy: false, .. } => "PowerShell 7".into(),
            Self::PowerShell { legacy: true, .. } => "Windows PowerShell 5.1".into(),
            Self::Cmd => "コマンドプロンプト（cmd.exe）".into(),
            Self::Wsl { distro } => format!("WSL の {distro}"),
            Self::Unix { program } => unix_name(program),
        }
    }

    /// 短い名前。タブとメニューに出す。
    pub fn name(&self) -> String {
        match self {
            Self::PowerShell { legacy: false, .. } => "PowerShell".into(),
            Self::PowerShell { legacy: true, .. } => "Windows PowerShell".into(),
            Self::Cmd => "cmd".into(),
            Self::Wsl { distro } => distro.clone(),
            Self::Unix { program } => unix_name(program),
        }
    }

    /// 入力の行の頭に出すプロンプト。`dir` は今の場所。
    pub fn prompt(&self, dir: &str) -> String {
        match self {
            Self::PowerShell { .. } => format!("PS {dir}>"),
            Self::Cmd => format!("{dir}>"),
            Self::Wsl { .. } | Self::Unix { .. } => format!("{dir} $"),
        }
    }

    /// AI に伝える、このシェルの書き方。
    pub fn guide(&self) -> String {
        match self {
            Self::PowerShell { legacy: false, .. } => {
                "PowerShell の書き方で書いてください。bash や cmd の書き方は使えません。".into()
            }
            Self::PowerShell { legacy: true, .. } => {
                "PowerShell の書き方で書いてください。bash や cmd の書き方は使えません。\
                 `&&` と `||` も使えないので、コマンドは `;` でつなぎ、前のコマンドの成功で分けるときは \
                 `if ($?) { ... }` を使ってください。"
                    .into()
            }
            Self::Cmd => "cmd の書き方で書いてください。コマンドは `&` や `&&` でつなぎ、`;` ではつなげません。\
                 sh や PowerShell の書き方は使えません。"
                .into(),
            Self::Wsl { .. } => "Linux の中のログインシェルです。bash の書き方で書いてください。\
                 PowerShell や cmd の書き方は使えません。Windows のファイルは /mnt/c/... で指します。"
                .into(),
            Self::Unix { program } => {
                let name = unix_name(program);
                match name.as_str() {
                    "bash" | "zsh" => format!(
                        "{name} の書き方で書いてください。PowerShell や cmd の書き方は使えません。"
                    ),
                    "sh" | "dash" => "POSIX の sh の書き方で書いてください。bash だけの書き方\
                         （`[[ ]]`、配列など）と、PowerShell や cmd の書き方は使えません。"
                        .into(),
                    _ => format!(
                        "{name} の書き方で書いてください。bash と書き方が違うところに気をつけてください。\
                         PowerShell や cmd の書き方は使えません。"
                    ),
                }
            }
        }
    }

    /// 画面に出すコマンドの行の頭（`PS>`、`>`、`$`）。
    pub fn mark(&self) -> &'static str {
        match self {
            Self::PowerShell { .. } => "PS>",
            Self::Cmd => ">",
            Self::Wsl { .. } | Self::Unix { .. } => "$",
        }
    }

    /// WSL の中で動くか。WSL の中の `cd` は、アプリでは扱えない。
    pub fn is_wsl(&self) -> bool {
        matches!(self, Self::Wsl { .. })
    }

    /// 1 行のコマンドを、このシェルで動かす形にする。
    pub fn spec(&self, command: &str) -> Spec {
        let (program, args, env, stdin) = match self {
            Self::PowerShell { program, .. } => {
                // 引用符が Windows の引数の書き方で崩れないよう、スクリプトは Base64 で渡す
                let args = [
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-OutputFormat",
                    "Text",
                    "-EncodedCommand",
                ]
                .iter()
                .map(|a| (*a).to_owned())
                .chain(std::iter::once(encode_powershell(&powershell_script(
                    command,
                ))))
                .collect();
                (program.clone(), args, Vec::new(), None)
            }
            Self::Cmd => (
                "cmd".to_owned(),
                vec!["/C".to_owned(), command.to_owned()],
                Vec::new(),
                None,
            ),
            Self::Wsl { distro } => (
                "wsl.exe".to_owned(),
                ["-d", distro, "--", "sh"]
                    .iter()
                    .map(|a| (*a).to_owned())
                    .collect(),
                vec![(ENV_COMMAND.to_owned(), command.to_owned())],
                // その人のログインシェル（zsh など）に、設定（PATH）を読ませて動かす
                Some(format!(
                    "exec \"${{SHELL:-bash}}\" -lic \"${ENV_COMMAND}\"\n"
                )),
            ),
            Self::Unix { program } => (
                program.clone(),
                vec!["-c".to_owned(), command.to_owned()],
                Vec::new(),
                None,
            ),
        };
        Spec {
            program,
            args,
            display: command.into(),
            mark: self.mark().into(),
            env,
            stdin,
            ..Default::default()
        }
    }
}

/// シェルの場所から、名前（`/bin/zsh` なら `zsh`）。
fn unix_name(program: &str) -> String {
    std::path::Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_owned())
}

/// PowerShell に渡すスクリプト。出力を UTF-8 にし、進み具合の表示を止め、
/// 失敗したら終了コードを返す（外のコマンドの終了コードはそのまま）。
fn powershell_script(command: &str) -> String {
    format!(
        "$ProgressPreference = 'SilentlyContinue'\n\
         try {{ $OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false }} catch {{ }}\n\
         {command}\n\
         if (-not $?) {{ if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}; exit 1 }}\n"
    )
}

/// PowerShell の `-EncodedCommand` の形（UTF-16LE を Base64 にしたもの）。
fn encode_powershell(script: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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

/// 起こすプログラム。Windows では PATH（`path`）と PATHEXT から探し、`.cmd` や `.bat` も見つける。
fn program_path(program: &str, path: Option<&std::ffi::OsStr>) -> PathBuf {
    if !cfg!(windows) {
        return program.into();
    }
    let dirs: Vec<PathBuf> = path
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(str::to_owned)
        .collect();
    find_program(program, &dirs, &exts, |path| path.is_file()).unwrap_or_else(|| program.into())
}

/// 引数を足す。Windows の cmd に渡す 1 行は、cmd の書き方のまま渡す。
/// ふつうに渡すと `"` が `\"` に書き換わり、cmd はそれを読めないので、引用符を含むコマンドが壊れる。
fn add_args(command: &mut Command, spec: &Spec) {
    #[cfg(windows)]
    if let [flag, line] = spec.args.as_slice() {
        if spec.program == "cmd" && flag == "/C" {
            use std::os::windows::process::CommandExt;
            // /S なら、cmd は外側の引用符を 1 組だけ外し、中はそのまま読む
            command.raw_arg(format!("/S /C \"{line}\""));
            return;
        }
    }
    command.args(&spec.args);
}

/// WSL に渡す環境変数の名前（`WSLENV`）。WSL の中には、ここに名前を書いたものだけが届く。
/// 場所は、Linux から見た場所（`/mnt/c/...`）に読み替えさせる。
fn wslenv(spec: &Spec) -> Option<String> {
    let program = spec.program.to_ascii_lowercase();
    if spec.env.is_empty() || !(program == "wsl.exe" || program == "wsl") {
        return None;
    }
    let mut names: Vec<String> = std::env::var("WSLENV")
        .ok()
        .filter(|v| !v.is_empty())
        .into_iter()
        .collect();
    for (name, value) in &spec.env {
        let is_path = std::path::Path::new(value).is_absolute() && !value.contains('\n');
        names.push(if is_path && name != ENV_COMMAND {
            format!("{name}/p")
        } else {
            name.clone()
        });
    }
    Some(names.join(":"))
}

/// コマンドを起こす。出力と終了は、別のスレッドから `on_event` に届く。
/// 最後に必ず [`Event::Exit`] が 1 回届く。
pub fn spawn(
    spec: &Spec,
    on_event: impl Fn(Event) + Send + Sync + 'static,
) -> std::io::Result<Handle> {
    // アプリを起動したあとに入れたコマンド（slack-cli など）も見つかるように、PATH を読み直す
    let fresh = malakt_term::env_path::fresh();
    let path = fresh.clone().or_else(|| std::env::var_os("PATH"));
    let mut command = Command::new(program_path(&spec.program, path.as_deref()));
    add_args(&mut command, spec);
    if let Some(fresh) = fresh {
        command.env("PATH", fresh);
    }
    if let Some(wslenv) = wslenv(spec) {
        command.env("WSLENV", wslenv);
    }
    command
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

    fn profile(
        id: &str,
        name: &str,
        kind: malakt_term::profile::Kind,
        program: &str,
    ) -> malakt_term::Profile {
        malakt_term::Profile {
            id: id.into(),
            name: name.into(),
            kind,
            program: program.into(),
            args: Vec::new(),
        }
    }

    #[test]
    fn the_standard_shell_is_powershell_on_windows_and_bash_elsewhere() {
        let pwsh = Shell::standard_for(true, |name| name == "pwsh.exe");
        assert_eq!(pwsh.label(), "PowerShell 7");
        // PowerShell 7 がなくても、Windows PowerShell はどの Windows にもある
        let legacy = Shell::standard_for(true, |_| false);
        assert_eq!(legacy.label(), "Windows PowerShell 5.1");
        assert!(legacy.guide().contains("`&&`"));
        assert_eq!(
            Shell::standard_for(false, |name| name == "bash").label(),
            "bash"
        );
        assert_eq!(Shell::standard_for(false, |_| false).label(), "sh");
    }

    #[test]
    fn a_terminal_tab_maps_to_the_same_shell() {
        use malakt_term::profile::Kind;
        let wsl = Shell::of_profile(&profile("wsl:Ubuntu", "Ubuntu", Kind::Wsl, "wsl.exe"));
        assert_eq!(
            wsl,
            Shell::Wsl {
                distro: "Ubuntu".into()
            }
        );
        assert_eq!(wsl.label(), "WSL の Ubuntu");
        assert!(wsl.guide().contains("bash の書き方"));
        assert_eq!(
            Shell::of_profile(&profile("pwsh", "PowerShell", Kind::Shell, "pwsh.exe")).label(),
            "PowerShell 7"
        );
        assert_eq!(
            Shell::of_profile(&profile(
                "powershell",
                "Windows PowerShell",
                Kind::Shell,
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            ))
            .label(),
            "Windows PowerShell 5.1"
        );
        assert_eq!(
            Shell::of_profile(&profile(
                "cmd",
                "コマンドプロンプト",
                Kind::Shell,
                "cmd.exe"
            )),
            Shell::Cmd
        );
        let zsh = Shell::of_profile(&profile("zsh", "zsh", Kind::Shell, "/bin/zsh"));
        assert_eq!(zsh.label(), "zsh");
        assert_eq!(zsh.mark(), "$");
        let fish = Shell::of_profile(&profile("fish", "fish", Kind::Shell, "/usr/bin/fish"));
        assert!(fish.guide().contains("bash と書き方が違う"));
    }

    #[test]
    fn each_shell_is_started_in_its_own_way() {
        let pwsh = Shell::PowerShell {
            program: "pwsh.exe".into(),
            legacy: false,
        }
        .spec("echo \"a b\"");
        assert_eq!(pwsh.program, "pwsh.exe");
        assert_eq!(pwsh.display, "echo \"a b\"");
        assert_eq!(&pwsh.args[pwsh.args.len() - 2], "-EncodedCommand");
        assert!(pwsh.args.contains(&"-NoProfile".to_owned()));

        let cmd = Shell::Cmd.spec("echo a& echo b");
        assert_eq!(
            (cmd.program.as_str(), cmd.args.as_slice()),
            ("cmd", &["/C".to_owned(), "echo a& echo b".to_owned()][..])
        );

        // WSL のコマンドは、引用符が崩れないように環境変数で渡す
        let wsl = Shell::Wsl {
            distro: "Ubuntu".into(),
        }
        .spec("ls 'a b' | wc -l");
        assert_eq!(wsl.program, "wsl.exe");
        assert_eq!(wsl.args, ["-d", "Ubuntu", "--", "sh"]);
        assert_eq!(
            wsl.env,
            [(ENV_COMMAND.to_owned(), "ls 'a b' | wc -l".to_owned())]
        );
        assert!(wsl
            .stdin
            .unwrap()
            .contains("-lic \"$MALAKT_MAHOZ_COMMAND\""));
        assert_eq!(wslenv(&Shell::Cmd.spec("x")), None);

        let bash = Shell::Unix {
            program: "bash".into(),
        }
        .spec("echo $HOME");
        assert_eq!(bash.args, ["-c", "echo $HOME"]);
    }

    #[test]
    fn powershell_scripts_are_encoded_as_utf16_base64() {
        // `powershell -EncodedCommand` の説明にある例と同じ形
        assert_eq!(encode_powershell("dir"), "ZABpAHIA");
        assert_eq!(encode_powershell("a"), "YQA=");
        assert_eq!(encode_powershell("ab"), "YQBiAA==");
        assert_eq!(encode_powershell(""), "");
        let script = powershell_script("gh pr list");
        assert!(script.contains("\ngh pr list\n"));
        assert!(script.contains("exit $LASTEXITCODE"));
    }

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
            "echo out; [Console]::Error.WriteLine('err'); exit 3"
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
            "echo $env:MALAKT_TEST; (Get-Location).Path"
        } else {
            "echo $MALAKT_TEST; pwd"
        });
        let value = "こんにちは";
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
