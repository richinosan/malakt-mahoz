//! コマンドとしての使い方。ウィンドウを開かずに、完了チケットを引き換える。
//!
//! ```text
//! malakt-mahoz redeem <チケット> --message "公開しました"
//! malakt-mahoz redeem <チケット> --message-file report.md
//! malakt-mahoz ticket <チケット>
//! ```
//!
//! チケットは、引数の代わりに環境変数 `MALAKT_MAHOZ_TICKET` でも渡せる
//! （コマンドの履歴に残したくないとき）。結果は JSON を 1 行で出す。

use std::io::Write;

use malakt_core::Store;

pub const USAGE: &str = "\
使い方:
  malakt-mahoz                                   アプリを開く
  malakt-mahoz redeem <チケット> --message <本文>   完了チケットを引き換えて、完了報告を書く
  malakt-mahoz redeem <チケット> --message-file <ファイル>
  malakt-mahoz ticket <チケット>                   完了チケットのタスクを読む（引き換えない）
  malakt-mahoz --version

チケットは、環境変数 MALAKT_MAHOZ_TICKET でも渡せます。
データベースの場所は、環境変数 MALAKT_MAHOZ_DB で変えられます。
";

/// 引数がコマンドか。そうでなければ、アプリを開く。
pub fn is_command(args: &[String]) -> bool {
    !args.is_empty()
}

/// 終了コード。0 は成功、1 は失敗、2 は使い方の誤り。
pub fn run(
    args: &[String],
    open: impl FnOnce() -> malakt_core::Result<Store>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    match execute(args, open, out) {
        Ok(()) => 0,
        Err(Failure::Usage(message)) => {
            let _ = writeln!(err, "{message}\n\n{USAGE}");
            2
        }
        Err(Failure::Failed(message)) => {
            let _ = writeln!(err, "{message}");
            1
        }
    }
}

enum Failure {
    Usage(String),
    Failed(String),
}

impl From<malakt_core::Error> for Failure {
    fn from(error: malakt_core::Error) -> Self {
        Failure::Failed(error.to_string())
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Failure::Failed(error.to_string())
    }
}

fn execute(
    args: &[String],
    open: impl FnOnce() -> malakt_core::Result<Store>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let usage = |message: &str| Failure::Usage(message.to_owned());
    let (command, rest) = args
        .split_first()
        .ok_or_else(|| usage("コマンドがありません"))?;
    match command.as_str() {
        "--help" | "-h" | "help" => {
            write!(out, "{USAGE}")?;
            Ok(())
        }
        "--version" | "-V" => {
            writeln!(out, "malakt-mahoz {}", env!("CARGO_PKG_VERSION"))?;
            Ok(())
        }
        "redeem" | "ticket" => {
            let mut ticket = None;
            let mut message = None;
            let mut rest = rest.iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--message" | "-m" => {
                        let text = rest
                            .next()
                            .ok_or_else(|| usage("--message に本文がありません"))?;
                        message = Some(text.clone());
                    }
                    "--message-file" => {
                        let path = rest
                            .next()
                            .ok_or_else(|| usage("--message-file にファイルがありません"))?;
                        let bytes = std::fs::read(path)
                            .map_err(|e| Failure::Failed(format!("{path}: {e}")))?;
                        message = Some(malakt_core::text::decode_utf8(&bytes)?.to_owned());
                    }
                    flag if flag.starts_with('-') => {
                        return Err(Failure::Usage(format!("知らない指定です: {flag}")))
                    }
                    value if ticket.is_none() => ticket = Some(value.to_owned()),
                    extra => return Err(Failure::Usage(format!("余分な引数です: {extra}"))),
                }
            }
            let ticket = ticket
                .or_else(|| std::env::var("MALAKT_MAHOZ_TICKET").ok())
                .filter(|t| !t.is_empty())
                .ok_or_else(|| usage("完了チケットを指定してください"))?;
            if command == "ticket" {
                if message.is_some() {
                    return Err(usage("ticket は読むだけなので、本文は要りません"));
                }
                let task = open()?.ticket_task(&ticket)?;
                writeln!(out, "{}", json(&task)?)?;
            } else {
                let message = message
                    .ok_or_else(|| usage("完了報告の本文を --message で指定してください"))?;
                let comment = open()?.redeem_ticket(&ticket, &message)?;
                writeln!(out, "{}", json(&comment)?)?;
            }
            Ok(())
        }
        other => Err(Failure::Usage(format!("知らないコマンドです: {other}"))),
    }
}

fn json(value: &impl serde::Serialize) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|e| Failure::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{run, USAGE};
    use malakt_core::{Status, Store, TaskDraft};

    fn call(store_path: &std::path::Path, args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, || Store::open(store_path), &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn redeem_and_read_through_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let (task, secret) = {
            let mut s = Store::open(&path).unwrap();
            let draft = TaskDraft {
                title: "告知を出す".into(),
                ..Default::default()
            };
            let task = s.create_task(None, &draft).unwrap();
            let secret = s.issue_ticket(&task.id, None).unwrap().secret;
            (task, secret)
        };

        // 読むだけでは何も変わらない
        let (code, out, _) = call(&path, &["ticket", &secret]);
        assert_eq!(code, 0);
        let read: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(read["title"], "告知を出す");
        assert_eq!(read["status"], "needs_action");

        // 本文はファイルからも渡せる（BOM や CRLF が付いていてもよい）
        let report = dir.path().join("report.md");
        std::fs::write(&report, "\u{feff}公開しました\r\n- 記事\r\n").unwrap();
        let (code, out, err) = call(
            &path,
            &[
                "redeem",
                &secret,
                "--message-file",
                report.to_str().unwrap(),
            ],
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out.lines().count(), 1);
        let comment: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(comment["body"], "公開しました\n- 記事");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.task(&task.id).unwrap().status, Status::Completed);

        // やり直しても同じ報告が返り、増えない
        let (code, again, _) = call(&path, &["redeem", &secret, "-m", "別の文"]);
        assert_eq!((code, again), (0, out));
        assert_eq!(s.comments(&task.id).unwrap().len(), 1);
    }

    #[test]
    fn failures_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        drop(Store::open(&path).unwrap());
        let wrong = format!("mmct_{}", "A".repeat(43));
        let (code, out, err) = call(&path, &["redeem", &wrong, "--message", "x"]);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("完了チケット"), "{err}");
        // 使い方の誤りは、データベースを開く前に断る
        let missing = dir.path().join("none").join("data.sqlite3");
        for args in [
            &["redeem", wrong.as_str()][..],
            &["redeem", wrong.as_str(), "--message"],
            &["redeem", wrong.as_str(), "--oops"],
            &["redeem", wrong.as_str(), "extra", "-m", "x"],
            &["ticket", wrong.as_str(), "-m", "x"],
            &["frobnicate"],
        ] {
            let (code, _, err) = call(&missing, args);
            assert_eq!(code, 2, "{args:?}");
            assert!(err.contains("使い方"), "{args:?}");
            assert!(!missing.exists());
        }
        let (code, out, _) = call(&missing, &["--help"]);
        assert_eq!((code, out.as_str()), (0, USAGE));
        let (code, out, _) = call(&missing, &["--version"]);
        assert_eq!(code, 0);
        assert!(out.starts_with("malakt-mahoz "));
    }
}
