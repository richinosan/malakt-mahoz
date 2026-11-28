//! CalDAV のサーバーとの同期。
//!
//! 設定の「CalDAV」でアカウントを足し、サーバーのカレンダーをワークスペースにつないだときだけ動く。
//! データが変わったら少し待ってから、そのほかは 1 分ごとに、つないだカレンダーと同期する。
//! サーバーとのやり取りは裏のスレッドで行い、つながらない間はアプリのデータだけが変わる
//! （つながったときに、たまっていた変更を送る）。

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
use malakt_caldav::http::{Discovery, Http};
use malakt_caldav::{Input, Outcome, Server};
use malakt_core::Id;

use crate::db::{self, Db};

/// データが変わってから送るまで待つ時間。続けて書いている間は送らない。
const SETTLE: Duration = Duration::from_millis(1500);
/// サーバーを見に行く間隔。
const POLL: Duration = Duration::from_secs(60);

/// パスワードの置き場。
pub trait Secrets: Send + Sync {
    fn get(&self, account_id: &str) -> Result<Option<String>, String>;
    fn set(&self, account_id: &str, password: &str) -> Result<(), String>;
    fn delete(&self, account_id: &str) -> Result<(), String>;
}

/// OS のキーチェーン（macOS のキーチェーン、Windows の資格情報マネージャー、Linux の Secret Service）。
pub struct Keychain;

impl Keychain {
    fn entry(account_id: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new("malakt-mahoz", &format!("caldav-{account_id}"))
            .map_err(|e| format!("OS のキーチェーンを開けませんでした: {e}"))
    }
}

impl Secrets for Keychain {
    fn get(&self, account_id: &str) -> Result<Option<String>, String> {
        match Self::entry(account_id)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!(
                "パスワードを OS のキーチェーンから読めませんでした: {e}"
            )),
        }
    }

    fn set(&self, account_id: &str, password: &str) -> Result<(), String> {
        Self::entry(account_id)?
            .set_password(password)
            .map_err(|e| format!("パスワードを OS のキーチェーンに保存できませんでした: {e}"))
    }

    fn delete(&self, account_id: &str) -> Result<(), String> {
        match Self::entry(account_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!(
                "パスワードを OS のキーチェーンから消せませんでした: {e}"
            )),
        }
    }
}

/// メモリの中のパスワード（テスト用）。
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, account_id: &str) -> Result<Option<String>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned())
    }

    fn set(&self, account_id: &str, password: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.into(), password.into());
        Ok(())
    }

    fn delete(&self, account_id: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account_id);
        Ok(())
    }
}

pub type Discover = Arc<dyn Fn(&str, &str, &str) -> Result<Discovery, String> + Send + Sync>;
pub type Connect = Arc<dyn Fn(&str, &str, &str) -> Box<dyn Server + Send> + Send + Sync>;

/// サーバーとキーチェーンへの入口。テストでは差し替える。
pub struct CalDav {
    pub secrets: Arc<dyn Secrets>,
    /// アカウントを調べる（URL、ユーザー名、パスワード）。
    pub discover: Discover,
    /// サーバーにつなぐ（調べた URL、ユーザー名、パスワード）。
    pub connect: Connect,
}

impl Global for CalDav {}

impl CalDav {
    pub fn system() -> Self {
        CalDav {
            secrets: Arc::new(Keychain),
            discover: Arc::new(Http::discover),
            connect: Arc::new(|base, user, password| Box::new(Http::new(base, user, password))),
        }
    }
}

/// アカウントごとの今の様子。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub syncing: bool,
    /// 最後の同期で失敗したこと。
    pub error: Option<String>,
}

/// 同期の進み具合。すべてのウィンドウで共有する。
pub struct Syncer {
    db: Entity<Db>,
    running: HashSet<Id>,
    again: HashSet<Id>,
    states: HashMap<Id, State>,
    /// 最後にそろったときの、アプリのタスクの印。同じなら、データが変わっても送らない。
    settled: HashMap<Id, u64>,
    /// 最後に画面に出した失敗。同じ失敗を 1 分ごとに出さない。
    shown: HashMap<Id, Vec<String>>,
    pending: Option<Task<()>>,
    _watch: Subscription,
}

/// 画面に出すこと（失敗と、両方で変わっていたなどの知らせ）。
pub struct Failed(pub SharedString);

impl EventEmitter<Failed> for Syncer {}

struct SharedSyncer(Entity<Syncer>);

impl Global for SharedSyncer {}

pub fn shared(cx: &App) -> Entity<Syncer> {
    cx.global::<SharedSyncer>().0.clone()
}

/// 本物のサーバーとキーチェーンにつなぐ。アプリの起動のときに 1 回呼ぶ。
pub fn connect_system(cx: &mut App) {
    connect(CalDav::system(), cx);
}

/// サーバーとキーチェーンへの入口を置き、すぐに同期する。
pub fn connect(caldav: CalDav, cx: &mut App) {
    cx.set_global(caldav);
    if let Some(syncer) = cx.try_global::<SharedSyncer>().map(|s| s.0.clone()) {
        syncer.update(cx, |s, cx| s.all(true, cx));
    }
}

pub fn init(cx: &mut App) {
    // テストがキーチェーンやネットワークに触らないよう、[`connect_system`] か、テストが
    // [`CalDav`] を置くまでは同期しない
    let db = db::shared(cx);
    let syncer = cx.new(|cx| Syncer {
        _watch: cx.observe(&db, |this: &mut Syncer, _, cx| this.soon(cx)),
        db,
        running: HashSet::new(),
        again: HashSet::new(),
        states: HashMap::new(),
        settled: HashMap::new(),
        shown: HashMap::new(),
        pending: None,
    });
    cx.set_global(SharedSyncer(syncer.clone()));
    let weak = syncer.downgrade();
    cx.spawn(async move |cx| loop {
        if weak.update(cx, |this, cx| this.all(true, cx)).is_err() {
            break;
        }
        cx.background_executor().timer(POLL).await;
    })
    .detach();
}

/// アプリのタスクの印。
fn fingerprint(input: &Input) -> u64 {
    let mut h = DefaultHasher::new();
    for c in &input.calendars {
        c.id.hash(&mut h);
    }
    for l in &input.locals {
        l.key.hash(&mut h);
        l.calendar.hash(&mut h);
        serde_json::to_string(&l.fields)
            .unwrap_or_default()
            .hash(&mut h);
    }
    h.finish()
}

impl Syncer {
    pub fn state(&self, account_id: &str) -> State {
        self.states.get(account_id).cloned().unwrap_or_default()
    }

    /// データが変わった。続けて変わる間は待ってから送る。
    fn soon(&mut self, cx: &mut Context<Self>) {
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            // 終了の途中なら、受け取る相手がもういない
            let _ = this.update(cx, |this, cx| {
                this.pending = None;
                this.all(false, cx);
            });
        }));
    }

    fn all(&mut self, poll: bool, cx: &mut Context<Self>) {
        let accounts = match self.db.read(cx).store.caldav_accounts() {
            Ok(a) => a,
            Err(e) => {
                cx.emit(Failed(
                    format!("CalDAV のアカウントを読めませんでした。{e}").into(),
                ));
                return;
            }
        };
        for account in accounts {
            self.request(&account.id, poll, cx);
        }
    }

    /// 1 つのアカウントを同期する。`poll` は、アプリのデータが変わっていなくてもサーバーを見に行く。
    pub fn request(&mut self, account_id: &str, poll: bool, cx: &mut Context<Self>) {
        if self.running.contains(account_id) {
            self.again.insert(account_id.to_owned());
            return;
        }
        if !cx.has_global::<CalDav>() {
            return;
        }
        let read = self.db.update(cx, |db, _| {
            let account = db.store.caldav_account(account_id)?;
            let input = db.store.caldav_input(account_id)?;
            Ok::<_, malakt_core::Error>((account, input))
        });
        let (account, input) = match read {
            Ok(r) => r,
            // 外したばかりのアカウント
            Err(malakt_core::Error::NotFound(_)) => return,
            Err(e) => {
                self.fail(
                    account_id,
                    vec![format!("同期するタスクを読めませんでした。{e}")],
                    cx,
                );
                return;
            }
        };
        if input.calendars.is_empty() {
            self.states.remove(account_id);
            return;
        }
        if !poll && self.settled.get(account_id) == Some(&fingerprint(&input)) {
            return;
        }
        self.running.insert(account_id.to_owned());
        self.states
            .entry(account_id.to_owned())
            .or_default()
            .syncing = true;
        cx.notify();
        let caldav = cx.global::<CalDav>();
        let (secrets, connect) = (caldav.secrets.clone(), caldav.connect.clone());
        let id = account_id.to_owned();
        let task = cx.background_executor().spawn(async move {
            let password = secrets
                .get(&id)?
                .ok_or("パスワードが保存されていません。アカウントを追加し直してください")?;
            let mut server = connect(&account.url, &account.username, &password);
            Ok::<Outcome, String>(malakt_caldav::sync(&mut *server, &input))
        });
        let id = account_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // 終了の途中なら、受け取る相手がもういない
            let _ = this.update(cx, |this, cx| this.finish(&id, result, cx));
        })
        .detach();
    }

    fn finish(&mut self, id: &str, result: Result<Outcome, String>, cx: &mut Context<Self>) {
        self.running.remove(id);
        let mut errors = Vec::new();
        let mut notices = Vec::new();
        match result {
            Err(e) => errors.push(e),
            Ok(out) => {
                errors.extend(out.errors.iter().cloned());
                notices.extend(out.notices.iter().cloned());
                let applied = self.db.update(cx, |db, cx| {
                    let applied = db.store.apply_caldav(id, &out);
                    if applied.as_ref().is_ok_and(|a| a.changed) {
                        cx.notify();
                    }
                    applied
                });
                match applied {
                    Ok(a) => errors.extend(a.errors),
                    // 同期の間に外したアカウント
                    Err(malakt_core::Error::NotFound(_)) => {}
                    Err(e) => errors.push(format!("同期の結果を保存できませんでした。{e}")),
                }
            }
        }
        // そろったときの印。失敗したものがあれば、次のデータの変更でもう一度送る
        if errors.is_empty() {
            let input = self.db.update(cx, |db, _| db.store.caldav_input(id));
            match input {
                Ok(input) => {
                    self.settled.insert(id.to_owned(), fingerprint(&input));
                }
                Err(_) => {
                    self.settled.remove(id);
                }
            }
        } else {
            self.settled.remove(id);
        }
        for n in notices {
            cx.emit(Failed(n.into()));
        }
        self.fail(id, errors, cx);
        if self.again.remove(id) {
            self.request(id, false, cx);
        }
    }

    /// 失敗を覚えて、前と違うときだけ画面に出す。空ならうまくいった。
    fn fail(&mut self, id: &str, errors: Vec<String>, cx: &mut Context<Self>) {
        let state = self.states.entry(id.to_owned()).or_default();
        state.syncing = false;
        state.error = errors.first().cloned();
        if errors.is_empty() {
            self.shown.remove(id);
        } else if self.shown.get(id) != Some(&errors) {
            cx.emit(Failed(failure_text(&errors).into()));
            self.shown.insert(id.to_owned(), errors);
        }
        cx.notify();
    }

    /// アカウントを外したあとの片付け。
    pub fn forget(&mut self, id: &str, cx: &mut Context<Self>) {
        self.states.remove(id);
        self.settled.remove(id);
        self.shown.remove(id);
        cx.notify();
    }
}

/// 失敗を 1 行にまとめる。いくつもあるときは、最初の 1 つと件数だけを出す。
fn failure_text(errors: &[String]) -> String {
    match errors {
        [only] => format!("CalDAV の同期に失敗しました。{only}"),
        [first, ..] => format!(
            "CalDAV の同期に失敗しました（{} 件）。{first}",
            errors.len()
        ),
        [] => String::new(),
    }
}
