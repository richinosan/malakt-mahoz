//! 所要時間の基準に GitHub の PR の時刻（作った時・1 回目のレビュー・マージ）を選んだタスクの、
//! PR の時刻を `gh` で読んでタスクに残す。
//!
//! 読むのは基準に PR の時刻を使うタスクだけ（[`timing::pr_jobs`]）。データが変わったとき（数秒待ってから）、
//! 1 時間ごと、基準を PR にしたとき・「PR の時刻を読み直す」を押したときに見る。どのウィンドウから
//! 頼まれても、読むのはアプリ全体で 1 か所。

use std::sync::Arc;

use gpui_kit::*;
use malakt_ai::{github, shell};
use malakt_core::timing::{self, PrJob, PrTimes};
use malakt_core::{now_ms, Id, Scope, TaskFilter};

/// PR を読めなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// `gh` が見つからない
    NoGh,
    Failed(String),
}

/// PR の URL から時刻を読む。テストでは差し替える。
pub type ReadFn = dyn Fn(&str) -> Result<github::PrTimes, ReadError> + Send + Sync;

/// データが変わってから見るまでの間。続けて変わる間は待つ。
const SETTLE_MS: i64 = 3_000;
/// `gh` の返事を待つ長さ。過ぎたら止める。
const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct PrSync {
    read: Arc<ReadFn>,
    running: bool,
    /// 読んでいる間に頼まれた（終わったら、もう一度見る）。頼まれたタスクがあれば、その id
    again: Option<Option<Id>>,
    /// 最後に見た時刻（ミリ秒）
    last: Option<i64>,
    /// データが変わった時刻（ミリ秒）。まだ見ていなければ入る
    dirty: Option<i64>,
}

impl Default for PrSync {
    fn default() -> Self {
        Self {
            read: Arc::new(read_with_gh),
            running: false,
            again: None,
            last: None,
            dirty: None,
        }
    }
}

impl Global for PrSync {}

/// 読み方を差し替える（テスト用）。
pub fn set_reader(read: Arc<ReadFn>, cx: &mut App) {
    cx.default_global::<PrSync>().read = read;
}

/// データが変わった。少し待ってから、読むものがあるかを見る。
pub fn nudge(cx: &mut App) {
    let sync = cx.default_global::<PrSync>();
    if sync.dirty.is_none() {
        sync.dirty = Some(now_ms());
    }
}

/// 時刻を見る（[`crate::db::watch`] から 2 秒ごと）。変わってから少したったか、前に見てから 1 時間たったら見る。
pub fn tick(cx: &mut App) {
    let now = now_ms();
    let sync = cx.default_global::<PrSync>();
    let due = sync
        .last
        .is_none_or(|last| now - last >= timing::PR_REFRESH_MS)
        || sync.dirty.is_some_and(|at| now - at >= SETTLE_MS);
    if due && !sync.running {
        run(None, cx);
    }
}

/// すぐに見る。`task` を渡すと、そのタスクの PR は読んだばかりでも読み直す。
pub fn read_now(task: Option<Id>, cx: &mut App) {
    let sync = cx.default_global::<PrSync>();
    if sync.running {
        sync.again = Some(task.or(sync.again.take().flatten()));
        return;
    }
    run(task, cx);
}

fn run(force: Option<Id>, cx: &mut App) {
    let now = now_ms();
    let read = {
        let sync = cx.default_global::<PrSync>();
        sync.running = true;
        sync.dirty = None;
        sync.last = Some(now);
        sync.read.clone()
    };
    let db = crate::db::shared(cx);
    let jobs = {
        let store = &db.read(cx).store;
        (|| -> malakt_core::Result<Vec<PrJob>> {
            if !store.uses_pr_basis()? {
                return Ok(Vec::new());
            }
            let filter = TaskFilter {
                include_descendants: true,
                ..Default::default()
            };
            let tasks = store.tasks(Scope::All, &filter)?;
            let workspaces = store.workspaces()?;
            let standard = store.time_basis_standard()?;
            Ok(timing::pr_jobs(
                &tasks,
                &workspaces,
                standard,
                now,
                force.as_deref(),
            ))
        })()
    };
    let jobs = match jobs {
        Ok(jobs) => jobs,
        Err(e) => {
            finish(
                vec![format!("PR の時刻を読むタスクを探せませんでした: {e}")],
                false,
                force.is_some(),
                cx,
            );
            return;
        }
    };
    if jobs.is_empty() {
        finish(Vec::new(), false, force.is_some(), cx);
        return;
    }
    cx.spawn(async move |cx| {
        let mut errors = Vec::new();
        let mut changed = false;
        for job in jobs {
            let (id, pr) = match job {
                PrJob::Clear { id } => (id, None),
                PrJob::Read { id, url } => {
                    let reader = read.clone();
                    let at = url.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move { reader(&at) })
                        .await;
                    match result {
                        Ok(t) => (
                            id,
                            Some(PrTimes {
                                url,
                                opened: t.opened,
                                reviewed: t.reviewed,
                                merged: t.merged,
                                checked: now_ms(),
                            }),
                        ),
                        Err(ReadError::NoGh) => {
                            // どの PR も読めないので、ここでやめる
                            errors.push(
                                "gh が見つかりません。GitHub CLI を入れて gh auth login してください"
                                    .to_owned(),
                            );
                            break;
                        }
                        Err(ReadError::Failed(e)) => {
                            errors.push(format!("{url}: {e}"));
                            continue;
                        }
                    }
                }
            };
            let wrote = cx.update(|cx| {
                db.update(cx, |db, _| match db.store.set_task_pr(&id, pr.as_ref()) {
                    // 読んでいる間にタスクが消された
                    Err(malakt_core::Error::NotFound(_)) => Ok(false),
                    other => other.map(|()| true),
                })
            });
            match wrote {
                Ok(true) => changed = true,
                Ok(false) => {}
                Err(e) => errors.push(format!("PR の時刻を保存できませんでした: {e}")),
            }
        }
        cx.update(|cx| finish(errors, changed, force.is_some(), cx));
    })
    .detach();
}

/// 読み終わった。変わったら全ウィンドウを読み直させ、失敗があれば画面に出す。頼まれていたらもう一度見る。
fn finish(errors: Vec<String>, changed: bool, asked: bool, cx: &mut App) {
    let error = (!errors.is_empty()).then(|| {
        format!(
            "GitHub の PR の時刻を読めませんでした: {}",
            errors.join(" / ")
        )
    });
    let db = crate::db::shared(cx);
    db.update(cx, |db, cx| {
        // 同じ失敗は 1 時間ごとに出し直さない。頼まれて読んだときは出す
        let before = db.pr_error.clone();
        match error {
            Some(error) if asked || before.as_ref().map(|e| &e.1) != Some(&error) => {
                let n = before.as_ref().map_or(1, |e| e.0 + 1);
                db.pr_error = Some((n, error));
            }
            Some(_) => {}
            None => db.pr_error = None,
        }
        if changed || db.pr_error != before {
            cx.notify();
        }
    });
    let sync = cx.default_global::<PrSync>();
    sync.running = false;
    if let Some(again) = sync.again.take() {
        run(again, cx);
    }
}

/// `gh pr view` で読む。
fn read_with_gh(url: &str) -> Result<github::PrTimes, ReadError> {
    let spec = github::parse_link(url)
        .and_then(|link| link.pr_times_command())
        .ok_or_else(|| ReadError::Failed("PR のリンクとして読めません".into()))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = shell::spawn(&spec, move |event| {
        // 待つのをやめたあとの出力は、送れなくてよい
        let _ = tx.send(event);
    })
    .map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ReadError::NoGh,
        _ => ReadError::Failed(format!("gh を起動できませんでした: {e}")),
    })?;
    let until = std::time::Instant::now() + GH_TIMEOUT;
    let (mut stdout, mut stderr) = (String::new(), String::new());
    loop {
        match rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(shell::Event::Stdout(line)) => {
                stdout.push_str(&line);
                stdout.push('\n');
            }
            Ok(shell::Event::Stderr(line)) => {
                stderr.push_str(&line);
                stderr.push('\n');
            }
            Ok(shell::Event::Exit(Some(0))) => {
                return github::parse_pr_times(&stdout).map_err(ReadError::Failed)
            }
            Ok(shell::Event::Exit(code)) => {
                let code = code.map_or("なし".to_owned(), |c| c.to_string());
                let tail: String = stderr.trim().chars().take(300).collect();
                return Err(ReadError::Failed(format!(
                    "gh が失敗しました（終了コード {code}）: {tail}"
                )));
            }
            Err(_) => {
                // 止められなくても、このコマンドの結果は使わない
                let _ = handle.kill();
                return Err(ReadError::Failed(format!(
                    "gh が {} 秒たっても終わりませんでした",
                    GH_TIMEOUT.as_secs()
                )));
            }
        }
    }
}
