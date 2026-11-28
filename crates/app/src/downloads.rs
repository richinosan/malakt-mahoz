//! アプリ内ブラウザでダウンロードしたファイルと、ページを PDF に保存したファイルの一覧。
//!
//! 上のバーのアイコンに、まだ見ていない数を出す。押すと一覧を出し、項目を押すと OS のファイルマネージャー
//! （エクスプローラー・Finder）で、そのファイルを選んだ状態で開く。
//!
//! 一覧はデータベースの設定に残す。残すのは [`MAX_ITEMS`] 件まで（古いものから消す）。
//! 進み具合は残さない（書き込みを増やさない）。一覧から消しても、ファイルそのものは消さない。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::db::{self, Db};
use crate::download_rules::{self, Rules};

/// 一覧に残す数。超えたら古いものから消す。
pub const MAX_ITEMS: usize = 50;
/// 一覧を残す設定。
const SETTING: &str = "browser.downloads";
/// ダウンロード中のファイルの大きさを見る間隔（進み具合を知らせない OS の分）。
const POLL: Duration = Duration::from_millis(400);
/// PDF のファイル名に使う文字の数の上限。
const MAX_NAME_CHARS: usize = 120;

/// 何で作ったファイルか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// ページからのダウンロード
    File,
    /// ページを PDF に保存した
    Pdf,
}

/// 今どうなっているか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Running,
    Done,
    Failed { reason: String },
}

/// 一覧の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    /// ダウンロードした URL。PDF なら、そのページの URL。
    pub url: String,
    /// 保存した場所。
    pub path: PathBuf,
    pub kind: Kind,
    #[serde(flatten)]
    pub state: State,
    /// 受け取った大きさ（バイト）。
    #[serde(default)]
    pub received: u64,
    /// 全体の大きさ。分からなければ `None`。
    #[serde(default)]
    pub total: Option<u64>,
    /// 始めた時刻（UNIX ミリ秒）。
    pub started_at: i64,
    /// 一覧を開いて見たか。見ていないものの数を、上のバーのアイコンに出す。
    #[serde(default)]
    pub seen: bool,
    /// ファイルが見つからない（一覧を開いたときに調べる）。
    #[serde(skip)]
    pub missing: bool,
    /// 保存先を聞いていて、まだ選ばれていない。
    #[serde(skip)]
    pub waiting: bool,
}

impl Item {
    /// ファイルの名前。
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    pub fn running(&self) -> bool {
        self.state == State::Running
    }

    /// 進み具合（0〜1）。全体の大きさが分からなければ `None`。
    pub fn ratio(&self) -> Option<f32> {
        let total = self.total.filter(|total| *total > 0)?;
        Some((self.received as f64 / total as f64).clamp(0., 1.) as f32)
    }

    /// 名前の下に出す 1 行。
    pub fn detail(&self, now_ms: i64) -> String {
        use malakt_core::view::{format_bytes, format_relative};
        match &self.state {
            State::Running if self.waiting => "保存先の確認待ち".to_owned(),
            State::Running => match (self.kind, self.total) {
                (Kind::Pdf, _) => "PDF を作成中".to_owned(),
                (Kind::File, Some(total)) => {
                    format!("{} / {}", format_bytes(self.received), format_bytes(total))
                }
                (Kind::File, None) => format_bytes(self.received),
            },
            State::Done if self.missing => "ファイルが見つかりません".to_owned(),
            State::Done => format!(
                "{} · {}",
                format_bytes(self.received),
                format_relative(self.started_at, now_ms)
            ),
            State::Failed { reason } => reason.clone(),
        }
    }
}

/// ページから届いた、ダウンロードの知らせ。
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    /// 始まった。`path` に保存する。`ask` なら、保存先を聞いて、終わったらそこへ移す。
    Started {
        url: String,
        path: PathBuf,
        ask: bool,
    },
    /// 進んだ。進み具合を知らせる OS（Windows・Linux）だけが送る。
    Progress {
        url: String,
        received: u64,
        total: Option<u64>,
    },
    /// 終わった。`path` は分かる OS だけ（macOS は `None`）。
    Finished {
        url: String,
        path: Option<PathBuf>,
        ok: bool,
    },
}

pub struct Downloads {
    db: Entity<Db>,
    /// 新しいものが前。
    pub items: Vec<Item>,
    next: u64,
    /// 一覧を保存できなかった、ファイルを開けなかった。一覧の上に出す。
    pub error: Option<SharedString>,
    /// ダウンロード中のファイルの大きさを見ている。
    polling: Option<Task<()>>,
    /// 保存先の決まり（設定の「ダウンロード」）。
    pub rules: Rules,
    /// 保存先を聞いているダウンロード。一覧の番号ごと。
    asking: HashMap<u64, Ask>,
}

/// 保存先を聞いているダウンロードの様子。選ばれて、ダウンロードも終わったら、選ばれた場所へ移す。
#[derive(Default)]
struct Ask {
    /// 選ばれた場所。`Some(None)` はキャンセル。
    choice: Option<Option<PathBuf>>,
    /// ダウンロードが終わった。
    finished: bool,
}

pub struct SharedDownloads(pub Entity<Downloads>);

impl Global for SharedDownloads {}

pub fn init(cx: &mut App) {
    let db = db::shared(cx);
    let downloads = cx.new(|cx| Downloads::new(db, cx));
    cx.set_global(SharedDownloads(downloads));
}

pub fn shared(cx: &App) -> Entity<Downloads> {
    cx.global::<SharedDownloads>().0.clone()
}

impl Downloads {
    fn new(db: Entity<Db>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            db,
            items: Vec::new(),
            next: 1,
            error: None,
            polling: None,
            rules: Rules::default(),
            asking: HashMap::new(),
        };
        this.restore(cx);
        this.restore_rules(cx);
        this
    }

    /// 保存先の決まりを読む。読めなければ標準の決まりで始めて、読めなかったことを出す。
    fn restore_rules(&mut self, cx: &mut Context<Self>) {
        let saved = self
            .db
            .read(cx)
            .store
            .setting(download_rules::SETTING)
            .map_err(|e| e.to_string())
            .and_then(|saved| match saved {
                Some(saved) => serde_json::from_str::<Rules>(&saved).map_err(|e| e.to_string()),
                None => Ok(Rules::default()),
            })
            .and_then(|rules| rules.check().map(|()| rules));
        match saved {
            Ok(rules) => self.rules = rules,
            Err(e) => {
                self.error = Some(format!("ダウンロードの設定を読めませんでした: {e}").into())
            }
        }
        download_rules::set_current(&self.rules);
    }

    /// 保存先の決まりを変えて覚える。保存できなければ、前の決まりのままにする。
    pub fn set_rules(&mut self, rules: Rules, cx: &mut Context<Self>) -> Result<(), String> {
        if self.rules == rules {
            return Ok(());
        }
        rules.check()?;
        let value = serde_json::to_string(&rules)
            .map_err(|e| format!("ダウンロードの設定を保存できませんでした: {e}"))?;
        self.db
            .update(cx, |db, _| {
                db.store.set_setting(download_rules::SETTING, Some(&value))
            })
            .map_err(|e| format!("ダウンロードの設定を保存できませんでした: {e}"))?;
        download_rules::set_current(&rules);
        self.rules = rules;
        cx.notify();
        Ok(())
    }

    /// 前に残した一覧を読む。途中で終わったもの（アプリを閉じた）は、失敗にする。
    fn restore(&mut self, cx: &mut Context<Self>) {
        let saved = match self.db.read(cx).store.setting(SETTING) {
            Ok(saved) => saved,
            Err(e) => {
                self.error = Some(format!("ダウンロードの一覧を読めませんでした: {e}").into());
                return;
            }
        };
        let Some(saved) = saved else {
            return;
        };
        match serde_json::from_str::<Vec<Item>>(&saved) {
            Ok(items) => {
                self.items = restored(items);
                self.next = self.items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
            }
            Err(e) => {
                self.error = Some(format!("ダウンロードの一覧を読めませんでした: {e}").into())
            }
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let value = match serde_json::to_string(&self.items) {
            Ok(value) => value,
            Err(e) => {
                self.error = Some(format!("ダウンロードの一覧を保存できませんでした: {e}").into());
                return;
            }
        };
        let value = (!self.items.is_empty()).then_some(value);
        let saved = self
            .db
            .update(cx, |db, _| db.store.set_setting(SETTING, value.as_deref()));
        if let Err(e) = saved {
            self.error = Some(format!("ダウンロードの一覧を保存できませんでした: {e}").into());
        }
    }

    /// まだ見ていない数。上のバーのアイコンに出す。
    pub fn unseen(&self) -> u32 {
        self.items.iter().filter(|i| !i.seen).count() as u32
    }

    /// ダウンロード中のものの、まとめた進み具合。何も動いていなければ `None`。
    /// 全体の大きさが分からないものがあれば `Some(None)`。
    pub fn activity(&self) -> Option<Option<f32>> {
        let running: Vec<&Item> = self.items.iter().filter(|i| i.running()).collect();
        if running.is_empty() {
            return None;
        }
        let mut received = 0u64;
        let mut total = 0u64;
        for item in running {
            match item.total.filter(|t| *t > 0) {
                Some(t) if item.kind == Kind::File => {
                    received += item.received.min(t);
                    total += t;
                }
                _ => return Some(None),
            }
        }
        Some(Some((received as f64 / total as f64) as f32))
    }

    fn add(&mut self, url: String, path: PathBuf, kind: Kind, cx: &mut Context<Self>) -> u64 {
        let id = self.next;
        self.next += 1;
        self.items.insert(
            0,
            Item {
                id,
                url,
                path,
                kind,
                state: State::Running,
                received: 0,
                total: None,
                started_at: malakt_core::now_ms(),
                seen: false,
                missing: false,
                waiting: false,
            },
        );
        self.items.truncate(MAX_ITEMS);
        self.save(cx);
        self.poll(cx);
        cx.notify();
        id
    }

    /// ページからの知らせ。
    pub fn on_note(&mut self, note: Note, cx: &mut Context<Self>) {
        match note {
            Note::Started { url, path, ask } => {
                let id = self.add(url, path, Kind::File, cx);
                if ask {
                    self.ask_where(id, cx);
                }
            }
            Note::Progress {
                url,
                received,
                total,
            } => {
                let Some(item) = self
                    .items
                    .iter_mut()
                    .find(|i| i.running() && i.kind == Kind::File && i.url == url)
                else {
                    return;
                };
                item.received = received;
                item.total = total.or(item.total);
                cx.notify();
            }
            Note::Finished { url, path, ok } => {
                let Some(at) = finished_at(&self.items, &url, path.as_deref()) else {
                    return;
                };
                let id = self.items[at].id;
                let asked = self.asking.get_mut(&id);
                let item = &mut self.items[at];
                if let Some(path) = path {
                    item.path = path;
                }
                item.seen = false;
                if let Some(ask) = asked {
                    if ok {
                        // 保存先が選ばれていれば移す。まだなら、選ばれたときに移す
                        if let Ok(meta) = std::fs::metadata(&item.path) {
                            item.received = meta.len();
                            item.total = Some(meta.len());
                        }
                        ask.finished = true;
                        self.settle(id, cx);
                        return;
                    }
                    self.asking.remove(&id);
                    item.waiting = false;
                }
                if ok {
                    item.state = State::Done;
                    // 終わったら、大きさはファイルから読む
                    if let Ok(meta) = std::fs::metadata(&item.path) {
                        item.received = meta.len();
                        item.total = Some(meta.len());
                    }
                } else {
                    item.state = State::Failed {
                        reason: "ダウンロードできませんでした".to_owned(),
                    };
                }
                self.save(cx);
                cx.notify();
            }
        }
    }

    /// 保存先を聞く（設定の「保存先を毎回確認する」）。ダウンロードは決まりの場所に続けて置き、
    /// 選ばれて終わったら、選ばれた場所へ移す。キャンセルなら、ファイルを消して一覧からも消す。
    fn ask_where(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
            return;
        };
        item.waiting = true;
        let dir = item
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_download_dir);
        let chosen = cx.prompt_for_new_path(&dir, Some(&item.name()));
        self.asking.insert(id, Ask::default());
        cx.spawn(async move |this, cx| {
            let choice = match chosen.await {
                Ok(Ok(choice)) => Ok(choice),
                Ok(Err(e)) => Err(format!("保存先を選べませんでした: {e}")),
                Err(_) => Err("保存先を選ぶ画面が閉じられました".to_owned()),
            };
            // アプリを閉じている途中なら、反映する先がない
            let _ = this.update(cx, |this, cx| this.chose(id, choice, cx));
        })
        .detach();
    }

    /// 保存先が選ばれた（`None` はキャンセル）。選べなかったときは、決まりの場所に残して失敗を出す。
    fn chose(&mut self, id: u64, choice: Result<Option<PathBuf>, String>, cx: &mut Context<Self>) {
        let Some(ask) = self.asking.get_mut(&id) else {
            return;
        };
        match choice {
            Ok(choice) => {
                ask.choice = Some(choice);
                self.settle(id, cx);
            }
            Err(e) => {
                self.asking.remove(&id);
                if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                    item.waiting = false;
                    self.error =
                        Some(format!("{e}（{} に保存します）", item.path.display()).into());
                }
                cx.notify();
            }
        }
    }

    /// 選ばれていて、ダウンロードも終わっていたら、選ばれた場所へ移す（キャンセルなら消す）。
    fn settle(&mut self, id: u64, cx: &mut Context<Self>) {
        let ready = self
            .asking
            .get(&id)
            .is_some_and(|ask| ask.finished && ask.choice.is_some());
        if !ready {
            cx.notify();
            return;
        }
        let Some(Ask {
            choice: Some(choice),
            ..
        }) = self.asking.remove(&id)
        else {
            return;
        };
        let Some(at) = self.items.iter().position(|i| i.id == id) else {
            return;
        };
        let from = self.items[at].path.clone();
        let Some(to) = choice else {
            // キャンセル: 決まりの場所に置いたファイルを消して、一覧からも消す
            self.items.remove(at);
            if let Err(e) = std::fs::remove_file(&from) {
                self.error = Some(
                    format!(
                        "キャンセルしたダウンロード {} を消せませんでした: {e}",
                        from.display()
                    )
                    .into(),
                );
            }
            self.save(cx);
            cx.notify();
            return;
        };
        let moved = cx.background_spawn({
            let to = to.clone();
            async move { move_file(&from, &to) }
        });
        cx.spawn(async move |this, cx| {
            let moved = moved.await;
            // アプリを閉じている途中なら、反映する先がない
            let _ = this.update(cx, |this, cx| {
                let Some(item) = this.items.iter_mut().find(|i| i.id == id) else {
                    return;
                };
                item.waiting = false;
                item.state = match moved {
                    Ok(()) => {
                        item.path = to;
                        State::Done
                    }
                    Err(reason) => State::Failed { reason },
                };
                this.save(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// PDF に保存し始めた。保存し終わったら [`Downloads::pdf_done`] を呼ぶ。
    pub fn pdf_started(&mut self, url: String, path: PathBuf, cx: &mut Context<Self>) -> u64 {
        self.add(url, path, Kind::Pdf, cx)
    }

    pub fn pdf_done(&mut self, id: u64, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && i.running()) else {
            return;
        };
        item.seen = false;
        match result {
            Ok(()) => {
                item.state = State::Done;
                if let Ok(meta) = std::fs::metadata(&item.path) {
                    item.received = meta.len();
                    item.total = Some(meta.len());
                }
            }
            Err(e) => {
                item.state = State::Failed {
                    reason: format!("PDF を保存できませんでした: {e}"),
                }
            }
        }
        self.save(cx);
        cx.notify();
    }

    /// 一覧を開いた。今あるものを見たことにし、消えたファイルを調べる。
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        for item in &mut self.items {
            item.missing = item.state == State::Done && !item.path.exists();
        }
        if self.items.iter().any(|i| !i.seen) {
            for item in &mut self.items {
                item.seen = true;
            }
            self.save(cx);
        }
        cx.notify();
    }

    /// 一覧から消す。ファイルは消さない。
    pub fn remove(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.items.len();
        self.items.retain(|i| i.id != id || i.running());
        if self.items.len() != before {
            self.save(cx);
            cx.notify();
        }
    }

    /// 終わったものを一覧からすべて消す。ダウンロード中のものは残す。
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let before = self.items.len();
        self.items.retain(|i| i.running());
        if self.items.len() != before {
            self.save(cx);
            cx.notify();
        }
    }

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// OS のファイルマネージャーで、ファイルを選んだ状態で開く。
    pub fn reveal(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(item) = self.item(id) else {
            return;
        };
        let path = item.path.clone();
        if !path.exists() {
            if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                item.missing = true;
            }
            cx.notify();
            return;
        }
        self.error = None;
        let shown = cx.background_spawn(async move { reveal(&path) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = shown.await {
                // アプリを閉じていれば、出す先がない
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(e.into());
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// ファイルを OS の標準のアプリで開く。
    pub fn open_file(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(item) = self.item(id) else {
            return;
        };
        if !item.path.exists() {
            if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                item.missing = true;
            }
            cx.notify();
            return;
        }
        cx.open_with_system(&item.path.clone());
    }

    /// 進み具合を知らせない OS（macOS）のために、ダウンロード中のファイルの大きさを見る。
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.polling.is_some() {
            return;
        }
        self.polling = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            let going = this.update(cx, |this, cx| {
                let mut changed = false;
                for item in this.items.iter_mut().filter(|i| i.running()) {
                    if item.kind != Kind::File || item.total.is_some() {
                        continue;
                    }
                    if let Some(size) = partial_size(&item.path) {
                        if size > item.received {
                            item.received = size;
                            changed = true;
                        }
                    }
                }
                if changed {
                    cx.notify();
                }
                let going = this.items.iter().any(|i| i.running());
                if !going {
                    this.polling = None;
                }
                going
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        }));
    }
}

/// 残した一覧を読み直したもの。途中で終わったものは失敗にする。
fn restored(items: Vec<Item>) -> Vec<Item> {
    items
        .into_iter()
        .take(MAX_ITEMS)
        .map(|mut item| {
            if item.running() {
                item.state = State::Failed {
                    reason: "途中でアプリを終了しました".to_owned(),
                };
            }
            item
        })
        .collect()
}

/// 終わった知らせが、どのダウンロードのものか。場所が分かれば場所で、分からなければ URL で探す。
/// 同じ URL のものが並んでいるときは、先に始めたもの。場所が分かっていて合うものがなければ、
/// もう終わったものの知らせ（部品が同じ知らせを何度か送ることがある）なので、ほかのものには当てない。
fn finished_at(items: &[Item], url: &str, path: Option<&Path>) -> Option<usize> {
    let running = |i: &Item| i.running() && i.kind == Kind::File;
    if let Some(path) = path {
        if let Some(at) = items.iter().position(|i| running(i) && i.path == path) {
            return Some(at);
        }
        if items.iter().any(|i| i.path == path) {
            return None;
        }
    }
    items
        .iter()
        .enumerate()
        .rev()
        .find(|(_, i)| running(i) && i.url == url)
        .map(|(at, _)| at)
}

/// ダウンロード中のファイルの大きさ。部品によっては、終わるまで別の名前で書く。
fn partial_size(path: &Path) -> Option<u64> {
    let mut candidates = vec![path.to_path_buf()];
    for ext in ["crdownload", "download", "wkdownload"] {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{ext}"));
        candidates.push(PathBuf::from(name));
    }
    candidates
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .max()
}

/// PDF を保存するフォルダを変える。テストで使う。
pub struct DownloadDir(pub PathBuf);

impl Global for DownloadDir {}

/// 保存先のフォルダ（設定で選んだフォルダ、なければ OS のダウンロードのフォルダ）。日付のフォルダの外。
pub fn root_dir(cx: &App) -> PathBuf {
    let rules = &shared(cx).read(cx).rules;
    rules.folder.clone().unwrap_or_else(|| base_dir(cx))
}

/// 保存先を選んでいないときのフォルダ。テストでは [`DownloadDir`] に変える。
fn base_dir(cx: &App) -> PathBuf {
    match cx.try_global::<DownloadDir>() {
        Some(dir) => dir.0.clone(),
        None => default_download_dir(),
    }
}

/// 今日保存するフォルダ（日付のフォルダがあれば、その中）。
pub fn download_dir(cx: &App) -> Result<PathBuf, String> {
    shared(cx)
        .read(cx)
        .rules
        .dir(&base_dir(cx), download_rules::today())
}

/// OS のダウンロードのフォルダ。決まっていなければ（Linux で XDG の設定がないなど）ホームの `Downloads`。
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// ページからのダウンロードを置く場所。部品が選んだ名前を、設定の決まりのフォルダに置く
/// （Linux で XDG の設定がないと、部品はアプリを起動したフォルダを選ぶ）。
pub fn download_target(suggested: &Path) -> PathBuf {
    let base = default_download_dir();
    // 決まりは読んだときと変えたときに確かめているので、ここで失敗することはない
    let dir = download_rules::current()
        .dir(&base, download_rules::today())
        .unwrap_or(base);
    if suggested.parent() == Some(dir.as_path()) {
        return suggested.to_path_buf();
    }
    match suggested.file_name() {
        Some(name) => unique_path(&dir, &name.to_string_lossy()),
        None => suggested.to_path_buf(),
    }
}

/// ページの題名から、PDF のファイル名を作る。ファイル名に使えない文字は `_` にする。
pub fn pdf_name(title: &str) -> String {
    let mut name: String = title
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(MAX_NAME_CHARS)
        .collect();
    // Windows は末尾の点と空白を消すので、先に消しておく
    name = name.trim().trim_end_matches(['.', ' ']).trim().to_owned();
    if name.is_empty() {
        name = "ページ".to_owned();
    }
    if !name.to_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    name
}

/// ファイルを移す。ドライブをまたぐときは、写してから元を消す。
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| {
        format!(
            "{} を {} に移せませんでした: {e}",
            from.display(),
            to.display()
        )
    };
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(failed)?;
    std::fs::remove_file(from).map_err(failed)
}

/// `dir` の中で、まだない名前にする（`a.pdf` があれば `a (1).pdf`）。
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(path)
}

/// OS のファイルマネージャーで、`path` を選んだ状態で開く。
pub fn reveal(path: &Path) -> Result<(), String> {
    let failed =
        |e: std::io::Error| format!("{} をフォルダで表示できませんでした: {e}", path.display());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer は引数の書き方にうるさいので、そのまま渡す。うまく開いても 1 で終わるので、終わり方は見ない
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .and_then(|mut child| child.wait())
            .map(|_| ())
            .map_err(failed)
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .status()
            .map_err(failed)?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} をフォルダで表示できませんでした",
                path.display()
            ))
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        // ファイルを選んで開ける（FileManager1）ファイルマネージャーがなければ、入っているフォルダを開く
        let uri = file_uri(path);
        let selected = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if selected {
            return Ok(());
        }
        let dir = path.parent().unwrap_or(path);
        let status = std::process::Command::new("xdg-open")
            .arg(dir)
            .status()
            .map_err(failed)?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} をフォルダで表示できませんでした",
                path.display()
            ))
        }
    }
}

/// `file://` の URL。使えない文字は % で書く。
#[cfg(not(any(windows, target_os = "macos")))]
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(*byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, url: &str, path: &str, state: State) -> Item {
        Item {
            id,
            url: url.into(),
            path: path.into(),
            kind: Kind::File,
            state,
            received: 0,
            total: None,
            started_at: 0,
            seen: false,
            missing: false,
            waiting: false,
        }
    }

    #[::core::prelude::v1::test]
    fn pdf_names_drop_characters_files_cannot_have() {
        assert_eq!(pdf_name("a/b: c?"), "a_b_ c_.pdf");
        assert_eq!(pdf_name("  "), "ページ.pdf");
        assert_eq!(pdf_name("報告書.PDF"), "報告書.PDF");
        assert_eq!(pdf_name("終わり. "), "終わり.pdf");
        // 文字の数で切る（バイトで切らない）
        let long = "あ".repeat(300);
        assert_eq!(pdf_name(&long).chars().count(), MAX_NAME_CHARS + 4);
    }

    #[::core::prelude::v1::test]
    fn a_taken_name_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a.pdf"), dir.path().join("a.pdf"));
        std::fs::write(dir.path().join("a.pdf"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.pdf"),
            dir.path().join("a (1).pdf")
        );
        std::fs::write(dir.path().join("a (1).pdf"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.pdf"),
            dir.path().join("a (2).pdf")
        );
    }

    #[::core::prelude::v1::test]
    fn the_finish_finds_the_download_by_path_then_by_the_oldest_url() {
        // 新しいものが前
        let items = vec![
            item(3, "https://x/a", "/d/a (1).zip", State::Running),
            item(2, "https://x/b", "/d/b.zip", State::Running),
            item(1, "https://x/a", "/d/a.zip", State::Running),
        ];
        assert_eq!(
            finished_at(&items, "https://x/a", Some(Path::new("/d/a (1).zip"))),
            Some(0)
        );
        assert_eq!(finished_at(&items, "https://x/a", None), Some(2));
        assert_eq!(finished_at(&items, "https://x/b", None), Some(1));
        assert_eq!(finished_at(&items, "https://x/c", None), None);
        let done = vec![item(1, "https://x/a", "/d/a.zip", State::Done)];
        assert_eq!(
            finished_at(&done, "https://x/a", None),
            None,
            "2 回目の知らせ"
        );
        // 終わったものと同じ場所の知らせは、同じ URL のほかのダウンロードに当てない
        let again = vec![
            item(2, "https://x/a", "/d/a (1).zip", State::Running),
            item(1, "https://x/a", "/d/a.zip", State::Done),
        ];
        assert_eq!(
            finished_at(&again, "https://x/a", Some(Path::new("/d/a.zip"))),
            None
        );
        // 部品が別の名前を付けたときは、URL で探す
        assert_eq!(
            finished_at(&again, "https://x/a", Some(Path::new("/d/other.zip"))),
            Some(0)
        );
    }

    #[::core::prelude::v1::test]
    fn a_download_cut_off_by_quitting_is_shown_as_failed() {
        let items = restored(vec![
            item(2, "https://x/a", "/d/a.zip", State::Running),
            item(1, "https://x/b", "/d/b.zip", State::Done),
        ]);
        assert!(matches!(items[0].state, State::Failed { .. }));
        assert_eq!(items[1].state, State::Done);
    }

    #[::core::prelude::v1::test]
    fn the_list_survives_a_round_trip() {
        let mut a = item(1, "https://x/a", "/d/a.zip", State::Done);
        a.received = 10;
        a.total = Some(10);
        a.seen = true;
        let b = item(
            2,
            "https://x/b",
            "/d/b.pdf",
            State::Failed { reason: "x".into() },
        );
        let json = serde_json::to_string(&vec![a.clone(), b.clone()]).unwrap();
        let back: Vec<Item> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, vec![a, b]);
    }

    #[::core::prelude::v1::test]
    fn the_detail_line_shows_the_size() {
        let mut a = item(1, "https://x/a", "/d/a.zip", State::Running);
        a.received = 1024;
        assert_eq!(a.detail(0), "1.0 KB");
        a.total = Some(2048);
        assert_eq!(a.detail(0), "1.0 KB / 2.0 KB");
        assert_eq!(a.ratio(), Some(0.5));
        a.state = State::Done;
        a.missing = true;
        assert_eq!(a.detail(0), "ファイルが見つかりません");
    }
}
