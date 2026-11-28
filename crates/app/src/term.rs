//! ターミナル。シェル（WSL、PowerShell、zsh など）をアプリの中で開き、`claude` や `vim` のような
//! 画面全体を使うプログラムもそのまま動かす。AI のタブと同じ場所に、タブとして並ぶ。

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_ai::shell::Shell;
use malakt_term::{style, Action, Button as MouseKey, Caret, Key, Options, Profile, Snapshot};
use malakt_term::{Select, Terminal};

use crate::db::{self, Db};

/// 同時に開けるターミナルの数。
pub const MAX_TERMS: usize = 8;
/// 「＋」で開くシェルを覚える設定。
const PROFILE_SETTING: &str = "term.profile";
/// AI のタブで動かすシェル（「カスタマイズ」で選ぶ）。なければ標準のシェル。
const AI_SHELL_SETTING: &str = "ai.shell";
pub use malakt_ai::tools::{expand_dir, workspace_dir_key};

/// WSL に渡す環境変数の名前を、今の `WSLENV` に足す。
fn wsl_env(before: &str, names: &[String]) -> String {
    before
        .split(':')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .chain(names.iter().cloned())
        .collect::<Vec<_>>()
        .join(":")
}

/// 文字コードの設定がないとき（macOS で Finder から起動したときなど）に、シェルに渡す `LANG`。
/// 設定がないままだと、`vim` などが日本語を読めない。
fn default_locale(env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    if cfg!(windows) {
        return None;
    }
    let set = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|name| env(name).is_some_and(|value| !value.is_empty()));
    (!set).then_some(if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    })
}

/// 参照する寸法を、画面全体の拡大率に合わせる。
const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * crate::UI_SCALE)
}

const PAD: Pixels = px(8.);

/// ターミナルに渡す、タスクの文脈。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskContext {
    pub id: String,
    pub title: String,
    /// ワークスペースの名前。未所属なら空。
    pub workspace: String,
    /// 始めるフォルダー。
    pub dir: Option<PathBuf>,
}

/// 保存したシェルの名前（[`Profile::id`]）から、シェルを決める。シェルの一覧を調べる前に使う。
fn shell_of_id(id: &str) -> Shell {
    match id {
        "pwsh" => Shell::PowerShell {
            program: "pwsh.exe".into(),
            legacy: false,
        },
        "powershell" => Shell::PowerShell {
            program: "powershell.exe".into(),
            legacy: true,
        },
        "cmd" => Shell::Cmd,
        _ => match id.strip_prefix("wsl:") {
            Some(distro) => Shell::Wsl {
                distro: distro.to_owned(),
            },
            None => Shell::Unix {
                program: id.to_owned(),
            },
        },
    }
}

/// 開いているターミナル 1 つ。
pub struct Tab {
    pub id: u64,
    /// シェルの名前（zsh、Ubuntu など）。
    pub shell: SharedString,
    /// タスクから開いたときの、タスクのタイトル。
    pub task: Option<SharedString>,
    /// プログラムが付けたタイトル。
    pub title: Option<SharedString>,
    pub terminal: Arc<Terminal>,
    /// ドロップしたファイルの場所の書き方。
    pub paths: malakt_term::PathStyle,
    /// 開いたシェル。
    pub profile: Profile,
    /// シェルに渡した環境変数。
    env: Vec<(String, String)>,
    /// 終わっていれば、終了コード（シグナルなら `None`）。
    pub exit: Option<Option<i32>>,
}

impl Tab {
    /// タブに出す名前。
    pub fn label(&self) -> SharedString {
        self.task
            .clone()
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| self.shell.clone())
    }
}

/// PATH の最後に、このアプリのフォルダーを足す。もう入っているか、PATH に書けない場所なら `None`。
fn path_with(before: &std::ffi::OsStr, dir: &std::path::Path) -> Option<String> {
    let mut all: Vec<PathBuf> = std::env::split_paths(before).collect();
    if all.iter().any(|have| have == dir) {
        return None;
    }
    all.push(dir.to_owned());
    std::env::join_paths(all).ok()?.into_string().ok()
}

/// 開いているターミナルの一覧。ウィンドウをまたいで 1 つ。
pub struct Terms {
    pub(crate) db: Entity<Db>,
    pub tabs: Vec<Tab>,
    /// アプリ内ブラウザのタブ。番号はターミナルと同じ数え方で、作った順に並ぶ。
    pub pages: Vec<crate::browser::Page>,
    /// 見ているターミナルかブラウザのタブ。`None` なら AI のタブ。
    pub active: Option<u64>,
    /// 開けるシェル。調べ終わるまでは `None`。
    profiles: Option<Vec<Profile>>,
    /// 「＋」で開くシェル。
    pub profile: Option<String>,
    /// AI のタブで動かすシェル。`None` なら標準（[`Shell::standard`]）。
    pub ai_profile: Option<String>,
    pub(crate) next: u64,
    pub error: Option<SharedString>,
    /// うまくいったことの知らせ。`NOTICE_LIFE` たつと閉じる。
    pub notice: Option<SharedString>,
    /// 知らせにマウスが乗っている。乗っている間は閉じない
    notice_hovered: bool,
    /// 時間がたった知らせ
    notice_expired: bool,
    /// 知らせが消えていく途中
    pub notice_closing: bool,
    notice_task: Option<Task<()>>,
    /// Cursor の設定を探すホーム。`None` なら、メニューから登録できない。
    pub cursor_home: Option<PathBuf>,
    /// 開いたターミナルに、キーの入力先を移す。
    pub focus_wanted: bool,
    /// ターミナルを開いたときに、そこの Claude Code へ MCP サーバーを登録する。
    pub register_mcp: bool,
    /// このアプリを起動してから登録を確かめたシェル。
    registered: std::collections::HashSet<String>,
    /// ブラウザの場所の欄に、キーの入力先を移す。
    pub focus_bar: bool,
    /// ブラウザの部品からの知らせを送る口。
    pub(crate) page_events: async_channel::Sender<(u64, crate::browser::Event)>,
    /// ウィンドウごとの、描いた回。ページを見せるかを決めるのに使う。
    pub(crate) frames: crate::browser::Frames,
    /// メニューを閉じたあとに、描き直しを待っているウィンドウ。
    pub(crate) rechecking: Vec<WindowId>,
    /// Linux の GTK の知らせを進めている。
    pub(crate) pumping: bool,
    /// 最後に保存した、ブラウザのタブの場所。
    pub(crate) saved_pages: Option<String>,
    /// 最後に小さなウィンドウで開いたリンクと、その時刻。
    pub(crate) popup_sent: Option<(String, std::time::Instant)>,
    /// ブラウザのデータの大きさ。数えていなければ `None`。
    pub usage: Option<u64>,
    /// 閉じたブラウザのタブの場所。新しいものが後ろ。Ctrl+Shift+T で開き直す。
    pub closed_pages: Vec<String>,
    /// ウィンドウごとの、ブラウザの画面のキーの入力先。ページの中で押したキーを届ける。
    pub(crate) browser_focus: Vec<(WindowId, WeakFocusHandle)>,
    /// 次に見せたときにキーの入力先を移すページ。
    pub(crate) focus_page: Option<u64>,
    /// AI のタブの入力行に、キーの入力先を移す。
    pub focus_ai: bool,
    /// 印刷のダイアログの送信先。`true` なら PDF に保存、`false` ならプリンター。
    pub print_pdf: bool,
}

pub struct SharedTerms(pub Entity<Terms>);

impl Global for SharedTerms {}

/// AI のコマンドが動くシェルと、それがターミナルのタブのシェルか（[`Terms::command_shell`]）。
/// ターミナルをまだ用意していなければ、標準のシェル。
pub fn command_shell(cx: &App) -> (Shell, bool) {
    match cx.try_global::<SharedTerms>() {
        Some(terms) => terms.0.read(cx).command_shell(),
        None => (Shell::standard(), false),
    }
}

/// AI のタブで動かすシェル（[`Terms::ai_shell`]）。
pub fn ai_shell(cx: &App) -> Shell {
    match cx.try_global::<SharedTerms>() {
        Some(terms) => terms.0.read(cx).ai_shell(),
        None => Shell::standard(),
    }
}

pub fn init(cx: &mut App) {
    let db = db::shared(cx);
    let terms = cx.new(|cx| Terms::new(db, cx));
    cx.set_global(SharedTerms(terms));
}

pub fn shared(cx: &App) -> Entity<Terms> {
    cx.global::<SharedTerms>().0.clone()
}

impl Terms {
    fn new(db: Entity<Db>, cx: &mut Context<Self>) -> Self {
        // 読めなければ、標準のシェルで開く
        let profile = db
            .read(cx)
            .store
            .setting(PROFILE_SETTING)
            .unwrap_or_default();
        let ai_profile = db
            .read(cx)
            .store
            .setting(AI_SHELL_SETTING)
            .unwrap_or_default();
        let (page_events, events) = async_channel::unbounded();
        cx.spawn(async move |this, cx| {
            while let Ok((id, event)) = events.recv().await {
                let alive = this.update(cx, |this, cx| this.on_page_event(id, event, cx));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut terms = Self {
            db,
            tabs: Vec::new(),
            pages: Vec::new(),
            active: None,
            profiles: None,
            profile,
            ai_profile,
            next: 1,
            error: None,
            notice: None,
            notice_hovered: false,
            notice_expired: false,
            notice_closing: false,
            notice_task: None,
            cursor_home: None,
            focus_wanted: false,
            register_mcp: false,
            registered: std::collections::HashSet::new(),
            focus_bar: false,
            page_events,
            frames: Default::default(),
            rechecking: Vec::new(),
            pumping: false,
            saved_pages: None,
            popup_sent: None,
            usage: None,
            closed_pages: Vec::new(),
            browser_focus: Vec::new(),
            focus_page: None,
            focus_ai: false,
            print_pdf: true,
        };
        terms.restore_pages(cx);
        terms
    }

    /// AI のタブで動かすシェル。「カスタマイズ」で選んだもの、選んでいなければ標準のシェル。
    pub fn ai_shell(&self) -> Shell {
        let Some(id) = self.ai_profile.as_deref() else {
            return Shell::standard();
        };
        match self.profiles().iter().find(|p| p.id == id) {
            Some(profile) => Shell::of_profile(profile),
            // 一覧を調べ終わる前でも、保存した名前から分かる
            None => shell_of_id(id),
        }
    }

    /// AI のコマンドが動くシェルと、それがターミナルのタブのシェルか。
    /// ターミナルのタブを見ていればそのタブのシェル、AI のタブなら [`Terms::ai_shell`]。
    pub fn command_shell(&self) -> (Shell, bool) {
        match self.active_tab() {
            Some(tab) => (Shell::of_profile(&tab.profile), true),
            None => (self.ai_shell(), false),
        }
    }

    /// AI のタブで動かすシェルを選ぶ。`None` で標準に戻す。
    pub fn set_ai_profile(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        if self.ai_profile.as_deref() == id {
            return;
        }
        let saved = self
            .db
            .update(cx, |db, _| db.store.set_setting(AI_SHELL_SETTING, id));
        match saved {
            Ok(()) => {
                self.ai_profile = id.map(str::to_owned);
                cx.notify();
            }
            Err(e) => self.fail(format!("AI のタブのシェルを保存できませんでした: {e}"), cx),
        }
    }

    /// 開けるシェル。まだ調べていなければ空。
    pub fn profiles(&self) -> &[Profile] {
        self.profiles.as_deref().unwrap_or_default()
    }

    /// 開けるシェルを差し替える。テスト用。
    pub fn set_profiles(&mut self, profiles: Vec<Profile>, cx: &mut Context<Self>) {
        self.profiles = Some(profiles);
        cx.notify();
    }

    /// 開けるシェルを調べる。Windows では `wsl.exe` を呼ぶので、裏で調べる。
    pub fn load_profiles(&mut self, cx: &mut Context<Self>) {
        if self.profiles.is_some() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { malakt_term::profile::detect() })
                .await;
            // アプリが終わっていれば、渡す先がない
            let _ = this.update(cx, |this, cx| {
                if this.profiles.is_none() {
                    this.profiles = Some(found);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 「＋」で開くシェルの名前。
    pub fn default_profile(&self) -> Option<&Profile> {
        malakt_term::profile::pick(self.profiles(), self.profile.as_deref())
    }

    pub fn tab(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    /// AI のタブに出す名前。動くシェルを添える。
    pub fn ai_label(&self) -> SharedString {
        format!("AI（{}）", self.ai_shell().name()).into()
    }

    /// 見ているタブの名前。タブの場所を別ウィンドウにしたときの、ウィンドウの名前にもなる。
    pub fn active_label(&self) -> SharedString {
        match self.active {
            Some(id) => match (self.tab(id), self.page(id)) {
                (Some(tab), _) => tab.label(),
                (None, Some(page)) => page.label(),
                (None, None) => self.ai_label(),
            },
            None => self.ai_label(),
        }
    }

    pub fn activate(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        let shown_here =
            |id: u64| self.tab(id).is_some() || self.page(id).is_some_and(|page| !page.popup);
        if id.is_none_or(shown_here) {
            self.active = id;
            self.focus_wanted = id.is_some_and(|id| self.tab(id).is_some());
            // 何も開いていないタブは、すぐに場所を書けるようにする。開いているページは、ページの中でキーを使える
            let blank = id
                .and_then(|id| self.page(id))
                .map(|page| page.url == crate::browser::BLANK);
            self.focus_bar = blank == Some(true);
            self.focus_page = id.filter(|_| blank == Some(false));
            self.focus_ai = id.is_none();
            cx.notify();
        }
    }

    /// タブの並び（作った順）。ターミナルとブラウザのタブの番号。
    pub fn order(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .tabs
            .iter()
            .map(|tab| tab.id)
            .chain(
                self.pages
                    .iter()
                    .filter(|page| !page.popup)
                    .map(|page| page.id),
            )
            .collect();
        ids.sort_unstable();
        ids
    }

    /// タブの列（AI のタブ、ターミナル、ブラウザ）。AI のタブは `None`。
    fn strip(&self) -> Vec<Option<u64>> {
        std::iter::once(None)
            .chain(self.order().into_iter().map(Some))
            .collect()
    }

    /// となりのタブを見せる（Ctrl+Tab / Ctrl+Shift+Tab）。端からは反対の端へ回る。
    pub fn step_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let strip = self.strip();
        let at = strip.iter().position(|id| *id == self.active).unwrap_or(0);
        let len = strip.len();
        let next = if forward {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        };
        self.activate(strip[next], cx);
    }

    /// `n` 番目（1 から）のタブを見せる。`9` は最後のタブ。
    pub fn select_tab(&mut self, n: usize, cx: &mut Context<Self>) {
        let strip = self.strip();
        let at = if n >= 9 {
            strip.len() - 1
        } else {
            n.max(1) - 1
        };
        if let Some(id) = strip.get(at) {
            self.activate(*id, cx);
        }
    }

    /// 見ているターミナルかブラウザのタブを閉じる。AI のタブは閉じない。
    pub fn close_active(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.close(id, cx);
        }
    }

    /// ターミナルを開く。`profile` を選んだときは、次の「＋」でも同じシェルを開く。
    pub fn open(
        &mut self,
        profile: Option<&str>,
        task: Option<TaskContext>,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        self.error = None;
        if !self.db.read(cx).unlocked {
            return None;
        }
        if self.tabs.len() >= MAX_TERMS {
            self.fail(format!("ターミナルは {MAX_TERMS} 個までです。"), cx);
            return None;
        }
        if self.profiles.is_none() {
            // 画面のスレッドで調べる（テストと、一覧を待たずに開いたとき）
            self.profiles = Some(malakt_term::profile::detect());
        }
        let wanted = profile.or(self.profile.as_deref());
        let Some(found) = malakt_term::profile::pick(self.profiles(), wanted).cloned() else {
            self.fail("開けるシェルが見つかりませんでした。".to_owned(), cx);
            return None;
        };
        if let Some(chosen) = profile.filter(|id| self.profile.as_deref() != Some(id)) {
            let saved = self.db.update(cx, |db, _| {
                db.store.set_setting(PROFILE_SETTING, Some(chosen))
            });
            match saved {
                Ok(()) => self.profile = Some(chosen.to_owned()),
                Err(e) => {
                    self.fail(format!("シェルの選択を保存できませんでした: {e}"), cx);
                }
            }
        }
        let dir = task
            .as_ref()
            .and_then(|task| task.dir.clone())
            .or_else(dirs::home_dir);
        let mut options = Options::of(&found, dir);
        // このアプリのコマンドとデータベースの場所。ターミナルで動く Claude Code などが見つけられる
        let mut wslenv: Vec<String> = Vec::new();
        for (name, value) in crate::mcp::shell_env() {
            // WSL の中では、Linux から見た場所に読み替える
            wslenv.push(format!("{name}/p"));
            options.env.push((name, value));
        }
        if let Some(task) = &task {
            for (name, value) in [
                ("MALAKT_TASK_ID", task.id.clone()),
                ("MALAKT_TASK_TITLE", task.title.clone()),
                ("MALAKT_WORKSPACE", task.workspace.clone()),
            ] {
                wslenv.push(name.to_owned());
                options.env.push((name.to_owned(), value));
            }
        }
        if found.kind != malakt_term::profile::Kind::Wsl {
            // `malakt-mahoz mcp` のように、名前でこのアプリのコマンドを呼べるようにする
            let exe = std::env::current_exe().ok();
            let dir = exe.as_deref().and_then(std::path::Path::parent);
            let before = std::env::var_os("PATH").unwrap_or_default();
            if let Some(path) = dir.and_then(|dir| path_with(&before, dir)) {
                options.env.push(("PATH".to_owned(), path));
            }
        }
        if found.kind == malakt_term::profile::Kind::Wsl {
            // WSL の中にも渡す
            let before = std::env::var("WSLENV").unwrap_or_default();
            options
                .env
                .push(("WSLENV".to_owned(), wsl_env(&before, &wslenv)));
        }
        if let Some(locale) = default_locale(|name| std::env::var(name).ok()) {
            options.env.push(("LANG".to_owned(), locale.to_owned()));
        }
        self.register_agents(&found, &options.env, cx);
        let (tx, rx) = async_channel::unbounded();
        let terminal = match Terminal::spawn(&options, move |event| {
            // ターミナルを閉じたあとは、送れなくてよい
            let _ = tx.send_blocking(event);
        }) {
            Ok(terminal) => Arc::new(terminal),
            Err(e) => {
                self.fail(format!("{} を起動できませんでした: {e}", found.name), cx);
                return None;
            }
        };
        let id = self.next;
        self.next += 1;
        self.tabs.push(Tab {
            id,
            shell: found.name.clone().into(),
            task: task.map(|task| task.title.into()),
            title: None,
            terminal,
            paths: found.path_style(cfg!(windows)),
            profile: found.clone(),
            env: options.env.clone(),
            exit: None,
        });
        self.active = Some(id);
        self.focus_wanted = true;
        cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                // たまっている分は、まとめて 1 回で画面に出す
                let mut events = vec![first];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                let alive = this.update(cx, |this, cx| {
                    for event in events {
                        this.on_event(id, event, cx);
                    }
                    cx.notify();
                    this.tab(id).is_some()
                });
                if !alive.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
        Some(id)
    }

    fn on_event(&mut self, id: u64, event: malakt_term::Event, cx: &mut Context<Self>) {
        use malakt_term::Event;
        let Some(at) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        match event {
            Event::Wakeup | Event::Bell => {}
            Event::Title(title) => {
                self.tabs[at].title = title.filter(|t| !t.trim().is_empty()).map(Into::into);
            }
            Event::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            // `exit` で終わったターミナルは閉じる。失敗で終わったものは、出力を読めるように残す
            Event::Exit(Some(0)) => self.close(id, cx),
            Event::Exit(code) => self.tabs[at].exit = Some(code),
        }
    }

    /// すべてのターミナルを閉じる。アプリを閉じるときに、動いているシェルとその中のプログラムを終わらせる。
    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        self.tabs.clear();
        // ブラウザのタブの場所は残したまま、次に起動したときに並べ直す
        self.pages.clear();
        self.active = None;
        cx.notify();
    }

    /// ターミナルを閉じる。動いているシェルも終わる。
    pub fn close(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(page) = self.page(id).filter(|page| page.popup) {
            // 小さなウィンドウのページは、ウィンドウごと閉じる
            let window = page.popup_window;
            self.remove_page(id, cx);
            if let Some(window) = window {
                cx.defer(move |cx| {
                    // 捨てたページの後片付けを、ウィンドウを閉じる前に済ませる
                    crate::webview::settle();
                    // もう閉じたウィンドウは、そのままでよい
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                });
            }
            cx.notify();
            return;
        }
        let order = self.order();
        let Some(at) = order.iter().position(|have| *have == id) else {
            return;
        };
        if let Some(found) = self.tabs.iter().position(|tab| tab.id == id) {
            self.tabs.remove(found);
        } else {
            self.remove_page(id, cx);
        }
        if self.active == Some(id) {
            // 右どなり、なければ左どなり、それもなければ AI のタブを見せる
            let next = order
                .get(at + 1)
                .or_else(|| at.checked_sub(1).and_then(|at| order.get(at)))
                .copied();
            self.activate(next, cx);
        }
        cx.notify();
    }

    /// 見ているターミナルの入力の行に、このアプリを MCP に登録するコマンドを入れる。Enter は押さない。
    pub fn type_mcp_command(&mut self, agent: crate::mcp_register::Agent, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab().filter(|tab| tab.exit.is_none()) else {
            return;
        };
        let command = crate::mcp_register::command(agent, &tab.profile.id, tab.paths);
        tab.terminal.clear_selection();
        tab.terminal.paste(&command);
        self.focus_wanted = true;
        cx.notify();
    }

    /// 見ているターミナルのシェルで使う Cursor に、MCP サーバーを登録する。
    /// うまくいったことの知らせを出す。`NOTICE_LIFE` たつと消えていく。
    fn show_notice(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.notice = Some(text);
        // 知らせの行は出し直すたびに描き直すので、前のマウスの位置は残らない
        self.notice_hovered = false;
        self.notice_expired = false;
        self.notice_closing = false;
        self.notice_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::state::NOTICE_LIFE)
                .await;
            // アプリが終わっていれば、閉じる知らせもない
            let _ = this.update(cx, |this, cx| {
                this.notice_expired = true;
                this.close_notice_if_idle(cx);
            });
        }));
        cx.notify();
    }

    /// 時間がたった知らせを、マウスが乗っていなければ消していく。
    fn close_notice_if_idle(&mut self, cx: &mut Context<Self>) {
        if self.notice.is_none()
            || !self.notice_expired
            || self.notice_hovered
            || self.notice_closing
        {
            return;
        }
        // 失敗の後ろに隠れている知らせは、すぐ消す
        if self.error.is_some() {
            self.notice = None;
            cx.notify();
            return;
        }
        self.notice_closing = true;
        self.notice_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::state::NOTICE_FADE)
                .await;
            // アプリが終わっていれば、閉じる知らせもない
            let _ = this.update(cx, |this, cx| {
                if this.notice_closing {
                    this.notice = None;
                    this.notice_closing = false;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// 知らせにマウスが乗った・離れた。乗っている間は閉じず、消えかけていたら戻す。
    pub fn hover_notice(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.notice_hovered = hovered;
        if hovered {
            if self.notice_closing {
                self.notice_closing = false;
                self.notice_task = None;
                cx.notify();
            }
        } else {
            self.close_notice_if_idle(cx);
        }
    }

    pub fn add_to_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let (profile, env) = (tab.profile.clone(), tab.env.clone());
        let home = self.cursor_home.clone();
        self.error = None;
        self.notice = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let exe = env.iter().find(|(name, _)| name == crate::mcp::ENV_BIN);
                    let exe = exe.map(|(_, exe)| exe.clone()).unwrap_or_default();
                    crate::mcp_register::register_cursor(&profile, &exe, &env, home.as_deref())
                })
                .await;
            // アプリが終わっていれば、渡す先がない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Some(true)) => this.show_notice("Cursor に登録しました。".into(), cx),
                    Ok(Some(false)) => this.show_notice("Cursor に登録済みです。".into(), cx),
                    Ok(None) => {
                        this.error =
                            Some("Cursor のフォルダー（~/.cursor）が見つかりません。".into());
                    }
                    Err(reason) => {
                        this.error =
                            Some(format!("Cursor に登録できませんでした: {reason}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// このシェルの Claude Code と Cursor に、MCP サーバーを登録する。
    /// シェルごとに、起動してから 1 回だけ確かめる。
    fn register_agents(
        &mut self,
        profile: &Profile,
        env: &[(String, String)],
        cx: &mut Context<Self>,
    ) {
        if !self.register_mcp || self.registered.contains(&profile.id) {
            return;
        }
        let Some((_, exe)) = env.iter().find(|(name, _)| name == crate::mcp::ENV_BIN) else {
            return;
        };
        self.registered.insert(profile.id.clone());
        let steps = crate::mcp_register::steps(profile, exe, cfg!(windows));
        let (profile, exe, env) = (profile.clone(), exe.clone(), env.to_vec());
        let name = profile.name.clone();
        cx.spawn(async move |this, cx| {
            let failed = cx
                .background_executor()
                .spawn(async move {
                    let mut failed = Vec::new();
                    if let Err(reason) = crate::mcp_register::register(&steps, &env) {
                        failed.push(format!("Claude Code: {reason}"));
                    }
                    let home = dirs::home_dir();
                    if let Err(reason) =
                        crate::mcp_register::register_cursor(&profile, &exe, &env, home.as_deref())
                    {
                        failed.push(format!("Cursor: {reason}"));
                    }
                    if let Err(reason) = crate::mcp_register::install_command(&profile, &env) {
                        failed.push(format!("malakt-mahoz コマンド: {reason}"));
                    }
                    failed
                })
                .await;
            if !failed.is_empty() {
                // アプリが終わっていれば、渡す先がない
                let _ = this.update(cx, |this, cx| {
                    this.fail(
                        format!(
                            "{name} で MCP を登録できませんでした（{}）",
                            failed.join("、")
                        ),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    pub(crate) fn fail(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = Some(text.into());
        cx.notify();
    }
}

fn hsla(rgb: u32) -> Hsla {
    gpui_kit::rgb(rgb).into()
}

/// 1 行の中の、同じ見た目で続く文字。
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    col: usize,
    text: String,
    fg: u32,
    style: u8,
}

const TEXT_STYLE: u8 = style::BOLD | style::ITALIC | style::UNDERLINE | style::STRIKE;

/// 1 行を、描くまとまりに分ける。ASCII の文字は同じ見た目ならまとめ、全角や記号は 1 文字ずつ
/// マスの位置に置く（書体によって幅が違っても、桁がずれない）。
fn segments(snapshot: &Snapshot, row: usize) -> Vec<Segment> {
    let start = row * snapshot.cols;
    let mut out: Vec<Segment> = Vec::new();
    let mut open = false;
    for col in 0..snapshot.cols {
        let cell = snapshot.cells[start + col];
        let look = cell.style & TEXT_STYLE;
        let plain = look & (style::UNDERLINE | style::STRIKE) == 0;
        if cell.style & style::SPACER != 0 || (cell.c == ' ' && plain) {
            open = false;
            continue;
        }
        let ascii = cell.c.is_ascii() && cell.style & style::WIDE == 0;
        if let Some(last) = out.last_mut().filter(|_| open && ascii) {
            if last.fg == cell.fg && last.style == look {
                last.text.push(cell.c);
                continue;
            }
        }
        let mut text = String::from(cell.c);
        if let Ok(at) = snapshot
            .combining
            .binary_search_by_key(&(start + col), |(at, _)| *at)
        {
            text.push_str(&snapshot.combining[at].1);
        }
        out.push(Segment {
            col,
            text,
            fg: cell.fg,
            style: look,
        });
        open = ascii;
    }
    out
}

/// 1 行の中の、背景を塗る範囲（桁の範囲と色）。
fn backgrounds(snapshot: &Snapshot, row: usize) -> Vec<(Range<usize>, u32)> {
    let start = row * snapshot.cols;
    let mut out: Vec<(Range<usize>, u32)> = Vec::new();
    for col in 0..snapshot.cols {
        let cell = snapshot.cells[start + col];
        let color = if cell.style & style::SELECTED != 0 {
            malakt_term::SELECTION
        } else {
            cell.bg
        };
        if color == malakt_term::BACKGROUND {
            continue;
        }
        match out.last_mut() {
            Some((range, last)) if range.end == col && *last == color => range.end = col + 1,
            _ => out.push((col..col + 1, color)),
        }
    }
    out
}

/// ターミナルの画面。AI のタブと同じ場所に出す。
pub struct TermPane {
    terms: Entity<Terms>,
    pub focus: FocusHandle,
    /// IME で変換中の文字。
    ime: Option<String>,
    /// 画面の左上と、1 マスの大きさ。最後に描いたときのもの。
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    /// 画面（文字を描く所）。最後に描いたときのもの。
    screen: Bounds<Pixels>,
    /// ドラッグで文字を選んでいる。
    selecting: bool,
    /// プログラムに知らせた、最後のマウスの場所。
    reported: Option<(usize, usize)>,
    /// ホイールの、1 行に満たない分。
    wheel: f32,
    /// さかのぼれる行を動かすスクロールバー。
    scroll: crate::scroll::TermScroll,
    /// Ctrl（macOS は Cmd）を押しながらマウスを乗せているマス。そこのリンクに下線を引く。
    hover: Option<(usize, usize)>,
    /// `hover` のマスにあるリンク。最後に描いたときのもの。
    link: Option<malakt_term::Link>,
    /// リンクを開いたクリック。離したときもプログラムに知らせない。
    opened: bool,
    _subscriptions: Vec<Subscription>,
}

/// ターミナルの中でも、プログラムに渡さずにアプリが受けるキー（Windows Terminal と同じ）。
/// タブの切り替え（Ctrl+Tab、Ctrl+Alt+数字）と全画面（F11）。Ctrl+Shift+T / W は、コピーと同じ組み合わせで先に分ける。
/// リンクを開くキー。Windows Terminal・ターミナル.app と同じく、macOS は Cmd、ほかは Ctrl。
fn opens_links(mods: Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        mods.platform && !mods.control && !mods.alt && !mods.shift
    } else {
        mods.control && !mods.platform && !mods.alt && !mods.shift
    }
}

fn passes_to_app(keystroke: &Keystroke) -> bool {
    let mods = keystroke.modifiers;
    let name = keystroke.key.as_str();
    if mods.control && !mods.alt && name == "tab" {
        return true;
    }
    if cfg!(target_os = "macos") {
        return false;
    }
    let plain = !mods.control && !mods.alt && !mods.shift;
    (plain && name == "f11")
        || (mods.control
            && mods.alt
            && !mods.shift
            && matches!(name, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}

impl TermPane {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let terms = shared(cx);
        let focus = cx.focus_handle();
        let weak = cx.weak_entity();
        let subscriptions = vec![
            cx.observe(&terms, |_, _, cx| cx.notify()),
            // ターミナルにいる間は、アプリのショートカットより先にキーを受け取る
            // （Ctrl+C や Esc を、動いているプログラムに届ける）
            cx.intercept_keystrokes(move |event, window, cx| {
                // 画面がなくなっていれば、受け取る先がない
                let _ = weak.update(cx, |this, cx| {
                    if this.focus.is_focused(window) && this.key_down(&event.keystroke, cx) {
                        cx.stop_propagation();
                    }
                });
            }),
        ];
        Self {
            terms,
            focus,
            ime: None,
            origin: Point::default(),
            cell: size(px(8.), px(16.)),
            screen: Bounds::default(),
            selecting: false,
            reported: None,
            wheel: 0.,
            scroll: Default::default(),
            hover: None,
            link: None,
            opened: false,
            _subscriptions: subscriptions,
        }
    }

    /// キーの入力先が、このターミナルか。
    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    fn terminal(&self, cx: &App) -> Option<Arc<Terminal>> {
        self.terms
            .read(cx)
            .active_tab()
            .map(|tab| tab.terminal.clone())
    }

    /// キーを受け取る。ターミナルで使ったら `true`。文字の入力は、文字として別に届く。
    fn key_down(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminal(cx) else {
            return false;
        };
        let mods = keystroke.modifiers;
        let name = keystroke.key.as_str();
        // コピーと貼り付け。macOS は Cmd、ほかは Ctrl+Shift（Ctrl+C はプログラムに届ける）
        let combo = if cfg!(target_os = "macos") {
            mods.platform && !mods.control
        } else {
            mods.control && mods.shift && !mods.alt
        };
        if combo {
            return match name {
                "c" => {
                    self.copy(&terminal, cx);
                    true
                }
                "v" => {
                    self.paste(&terminal, cx);
                    true
                }
                "a" => {
                    terminal.select_all();
                    cx.notify();
                    true
                }
                _ => false,
            };
        }
        if mods.platform {
            return false;
        }
        if passes_to_app(keystroke) {
            return false;
        }
        if name == "insert" && mods.shift {
            self.paste(&terminal, cx);
            return true;
        }
        // 選んでいる間の Ctrl+C はコピー。選んでいなければ、プログラムを止めるキーとして届ける
        let stops = name == "c" && mods.control && !mods.alt && !cfg!(target_os = "macos");
        if stops && terminal.selected_text().is_some() {
            self.copy(&terminal, cx);
            terminal.clear_selection();
            cx.notify();
            return true;
        }
        if cfg!(windows) && name == "v" && mods.control && !mods.alt {
            self.paste(&terminal, cx);
            return true;
        }
        // 出力をさかのぼる
        if mods.shift && !terminal.alt_screen() && matches!(name, "pageup" | "pagedown") {
            terminal.page(name == "pageup");
            cx.notify();
            return true;
        }
        // 変換中のキーは IME のもの
        if self.ime.is_some() {
            return false;
        }
        // macOS の Option は、文字を打つキー（Option+S で ß など）
        let single = name.chars().count() == 1;
        if cfg!(target_os = "macos") && mods.alt && !mods.control && single {
            return false;
        }
        if name == "space" && !mods.control && !mods.alt {
            terminal.clear_selection();
            terminal.input(" ");
            return true;
        }
        let key = Key {
            name,
            ctrl: mods.control,
            alt: mods.alt,
            shift: mods.shift,
        };
        if terminal.key(&key) {
            terminal.clear_selection();
            cx.notify();
            return true;
        }
        false
    }

    fn copy(&mut self, terminal: &Terminal, cx: &mut Context<Self>) {
        if let Some(text) = terminal.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// ドロップしたファイルの場所を、入力の行に入れる。Claude Code などは、画像の場所を画像として受け取る。
    pub fn drop_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let Some((terminal, style)) = self
            .terms
            .read(cx)
            .active_tab()
            .filter(|tab| tab.exit.is_none())
            .map(|tab| (tab.terminal.clone(), tab.paths))
        else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        terminal.clear_selection();
        terminal.paste(&malakt_term::drop_text(paths, style));
        // 続けて打てるように、キーの入力先を移す
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn paste(&mut self, terminal: &Terminal, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            terminal.clear_selection();
            terminal.paste(&text);
            cx.notify();
        }
    }

    /// マウスの場所のマス（桁、行）と、マスの右半分かどうか。
    fn cell_at(&self, at: Point<Pixels>, terminal: &Terminal) -> ((usize, usize), bool) {
        let (cols, rows) = terminal.size();
        let x = f32::from(at.x - self.origin.x) / f32::from(self.cell.width);
        let y = f32::from(at.y - self.origin.y) / f32::from(self.cell.height);
        let col = (x.max(0.) as usize).min(cols.saturating_sub(1));
        let row = (y.max(0.) as usize).min(rows.saturating_sub(1));
        ((col, row), x.fract() >= 0.5)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(terminal) = self.terminal(cx) else {
            return;
        };
        let (at, right) = self.cell_at(event.position, &terminal);
        // Ctrl+クリック（macOS は Cmd+クリック）のリンクは、OS の標準のブラウザで開く
        if event.button == MouseButton::Left && opens_links(event.modifiers) {
            if let Some(link) = terminal.link_at(at) {
                cx.open_url(&link.url);
                self.opened = true;
                // 開いたブラウザに移ると Ctrl を離したことが届かないので、下線はここで消す
                self.hover_at(None, &terminal, cx);
                return;
            }
        }
        let key = mods_key(event.modifiers);
        let button = match event.button {
            MouseButton::Left => MouseKey::Left,
            MouseButton::Middle => MouseKey::Middle,
            MouseButton::Right => MouseKey::Right,
            _ => return,
        };
        // Shift を押している間は、プログラムがマウスを受け取っていても、文字を選ぶ
        if !event.modifiers.shift && terminal.mouse(button, Action::Press, at, &key) {
            self.reported = Some(at);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let unit = match event.click_count {
            1 => Select::Chars,
            2 => Select::Words,
            _ => Select::Lines,
        };
        terminal.select(unit, at, right);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal(cx) else {
            return;
        };
        let (at, right) = self.cell_at(event.position, &terminal);
        let hover = (event.pressed_button.is_none() && opens_links(event.modifiers)).then_some(at);
        self.hover_at(hover, &terminal, cx);
        if self.selecting {
            if event.pressed_button == Some(MouseButton::Left) {
                terminal.select_to(at, right);
                cx.notify();
            } else {
                self.selecting = false;
            }
            return;
        }
        if self.reported == Some(at) || event.modifiers.shift {
            return;
        }
        let button = match event.pressed_button {
            Some(MouseButton::Left) => MouseKey::Left,
            Some(MouseButton::Middle) => MouseKey::Middle,
            Some(MouseButton::Right) => MouseKey::Right,
            _ => MouseKey::None,
        };
        if terminal.mouse(button, Action::Move, at, &mods_key(event.modifiers)) {
            self.reported = Some(at);
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal(cx) else {
            return;
        };
        if std::mem::take(&mut self.opened) && event.button == MouseButton::Left {
            return;
        }
        if self.selecting {
            self.selecting = false;
            cx.notify();
            return;
        }
        let button = match event.button {
            MouseButton::Left => MouseKey::Left,
            MouseButton::Middle => MouseKey::Middle,
            MouseButton::Right => MouseKey::Right,
            _ => return,
        };
        if !event.modifiers.shift {
            let (at, _) = self.cell_at(event.position, &terminal);
            terminal.mouse(button, Action::Release, at, &mods_key(event.modifiers));
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal(cx) else {
            return;
        };
        let delta = event.delta.pixel_delta(self.cell.height).y;
        self.wheel += f32::from(delta) / f32::from(self.cell.height);
        let lines = self.wheel.trunc();
        if lines == 0. {
            return;
        }
        self.wheel -= lines;
        let (at, _) = self.cell_at(event.position, &terminal);
        terminal.wheel(lines as i32, at, &mods_key(event.modifiers));
        cx.notify();
    }

    /// Ctrl（macOS は Cmd）を押しながら乗せているマスを変える。リンクの上なら下線を引き、指の形にする。
    fn hover_at(
        &mut self,
        at: Option<(usize, usize)>,
        terminal: &Terminal,
        cx: &mut Context<Self>,
    ) {
        if self.hover == at {
            return;
        }
        self.hover = at;
        let link = at.and_then(|at| terminal.link_at(at));
        if link != self.link {
            self.link = link;
            cx.notify();
        }
    }

    /// Ctrl（macOS は Cmd）を押した・離した。マウスを動かさなくても、リンクの下線を出し入れする。
    fn modifiers_changed(&mut self, mods: Modifiers, window: &Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal(cx) else {
            return;
        };
        let position = window.mouse_position();
        let inside = self.screen.contains(&position);
        let at = (inside && opens_links(mods)).then(|| self.cell_at(position, &terminal).0);
        self.hover_at(at, &terminal, cx);
    }

    /// カーソルのマスの場所。IME の変換の窓を出す場所になる。
    fn cursor_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
        let terminal = self.terminal(cx)?;
        let (col, row) = terminal.snapshot().input_at?;
        Some(Bounds::new(
            point(
                self.origin.x + self.cell.width * col as f32,
                self.origin.y + self.cell.height * row as f32,
            ),
            self.cell,
        ))
    }
}

fn mods_key(mods: Modifiers) -> Key<'static> {
    Key {
        name: "",
        ctrl: mods.control,
        alt: mods.alt,
        shift: mods.shift,
    }
}

impl EntityInputHandler for TermPane {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.ime.as_ref().map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.ime = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime = None;
        if let Some(terminal) = self.terminal(cx) {
            terminal.clear_selection();
            terminal.input(text);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime = Some(new_text.to_owned()).filter(|text| !text.is_empty());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds(cx)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// 描く前にそろえたもの。
struct Frame {
    snapshot: Snapshot,
    cell: Size<Pixels>,
    origin: Point<Pixels>,
    font: Font,
    font_size: Pixels,
    focused: bool,
    ime: Option<String>,
}

impl TermPane {
    /// 場所の大きさに合わせて桁と行を決め、画面の中身を写し取る。
    fn prepare(
        &mut self,
        terminal: &Terminal,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Frame {
        let theme = cx.theme();
        let font = font(theme.mono_font_family.clone());
        let font_size = theme.font_size * (13. / 16.);
        let text = window.text_system();
        let width = text
            .ch_advance(text.resolve_font(&font), font_size)
            .unwrap_or(font_size * 0.6);
        let height = (font_size * 1.35).round();
        let cell = size(width, height);
        let origin = point(bounds.origin.x + PAD, bounds.origin.y + PAD);
        let cols = (f32::from(bounds.size.width - PAD * 2.) / f32::from(width)).floor();
        let rows = (f32::from(bounds.size.height - PAD * 2.) / f32::from(height)).floor();
        terminal.resize(
            cols.max(2.) as usize,
            rows.max(1.) as usize,
            (f32::from(width) as u16, f32::from(height) as u16),
        );
        self.origin = origin;
        self.cell = cell;
        self.screen = bounds;
        self.scroll.update(bounds, height);
        let mut snapshot = terminal.snapshot();
        // 画面が変わっても下線がリンクに合うように、描くたびに探し直す
        self.link = self.hover.and_then(|at| terminal.link_at(at));
        if let Some(link) = &self.link {
            for &(col, row) in &link.cells {
                if let Some(cell) = snapshot.cells.get_mut(row * snapshot.cols + col) {
                    cell.style |= style::UNDERLINE;
                }
            }
        }
        Frame {
            snapshot,
            cell,
            origin,
            font,
            font_size,
            focused: self.focus.is_focused(window),
            ime: self.ime.clone(),
        }
    }
}

/// 罫線とブロックの文字を、マスいっぱいの四角で描く。Powerline の三角もマスいっぱいに描く。
/// となりのマスとすき間なくつながる。
/// その文字でなければ `false`。
fn paint_lines(c: char, cell: Bounds<Pixels>, color: Hsla, window: &mut Window) -> bool {
    let (x, y) = (cell.origin.x, cell.origin.y);
    let (w, h) = (cell.size.width, cell.size.height);
    if let Some((rects, alpha)) = malakt_term::block_rects(c) {
        for [rx, ry, rw, rh] in rects {
            window.paint_quad(fill(
                Bounds::new(point(x + w * rx, y + h * ry), size(w * rw, h * rh)),
                color.opacity(alpha),
            ));
        }
        return true;
    }
    let light = (f32::from(w) / 8.).round().max(1.);
    if let Some(right) = malakt_term::powerline_round(c) {
        // 平らな辺をマスの端に置き、反対の端までふくらむ半円。平らな辺は となりの色と
        // 縁がぼけて筋が見えるので、となりのマスへ 1 px 重ねる
        let over = gpui_kit::px(1.);
        let edge = if right { x - over } else { x + w + over };
        let radii = point(w + over, h / 2.);
        let mut path = PathBuilder::fill();
        path.move_to(point(edge, y));
        path.arc_to(radii, gpui_kit::px(0.), false, right, point(edge, y + h));
        path.close();
        let Ok(path) = path.build() else {
            return false;
        };
        window.paint_path(path, color);
        return true;
    }
    if let Some((corners, filled)) = malakt_term::powerline_shape(c) {
        let mut path = if filled {
            PathBuilder::fill()
        } else {
            PathBuilder::stroke(gpui_kit::px(light))
        };
        let [first, rest @ ..] = corners.map(|[px, py]| point(x + w * px, y + h * py));
        path.move_to(first);
        for corner in rest {
            path.line_to(corner);
        }
        if filled {
            path.close();
        }
        // 形が作れないことはない（3 点の決まった形）。作れなければ書体の字形で描く
        let Ok(path) = path.build() else {
            return false;
        };
        window.paint_path(path, color);
        return true;
    }
    let Some([left, right, up, down]) = malakt_term::box_arms(c) else {
        return false;
    };
    let thick = |weight: u8| gpui_kit::px(light * f32::from(weight));
    let (cx, cy) = ((x + w / 2.).round(), (y + h / 2.).round());
    // 角で線が欠けないよう、交わる線の太さの半分だけ中心を越えて引く
    let across = thick(up.max(down)) / 2.;
    let along = thick(left.max(right)) / 2.;
    let mut line = |from: Point<Pixels>, to: Point<Pixels>| {
        window.paint_quad(fill(Bounds::from_corners(from, to), color));
    };
    if left > 0 {
        let t = thick(left) / 2.;
        line(point(x, cy - t), point(cx + across, cy + t));
    }
    if right > 0 {
        let t = thick(right) / 2.;
        line(point(cx - across, cy - t), point(x + w, cy + t));
    }
    if up > 0 {
        let t = thick(up) / 2.;
        line(point(cx - t, y), point(cx + t, cy + along));
    }
    if down > 0 {
        let t = thick(down) / 2.;
        line(point(cx - t, cy - along), point(cx + t, y + h));
    }
    true
}

fn paint(frame: &Frame, window: &mut Window, cx: &mut App) {
    let Frame {
        snapshot,
        cell,
        origin,
        ..
    } = frame;
    let at = |col: usize, row: usize| {
        point(
            origin.x + cell.width * col as f32,
            origin.y + cell.height * row as f32,
        )
    };
    let draw =
        |text: &str, fg: u32, look: u8, to: Point<Pixels>, window: &mut Window, cx: &mut App| {
            // Nerd Font のアイコンは、同梱のアイコンの書体で描く。ターミナルの書体が
            // Nerd Font でなくても、Windows Terminal と同じようにプロンプトの記号が出る
            let symbol = text.chars().next().is_some_and(malakt_term::nerd_symbol);
            let mut font = if symbol {
                font(crate::theme::SYMBOL_FONT)
            } else {
                frame.font.clone()
            };
            if look & style::BOLD != 0 && !symbol {
                font = font.bold();
            }
            if look & style::ITALIC != 0 && !symbol {
                font = font.italic();
            }
            let color = hsla(fg);
            let run = TextRun {
                len: text.len(),
                font,
                color,
                background_color: None,
                underline: (look & style::UNDERLINE != 0).then_some(UnderlineStyle {
                    thickness: gpui_kit::px(1.),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: (look & style::STRIKE != 0).then_some(StrikethroughStyle {
                    thickness: gpui_kit::px(1.),
                    color: Some(color),
                }),
            };
            let line = window.text_system().shape_line(
                text.to_owned().into(),
                frame.font_size,
                &[run],
                None,
            );
            // 描けなかった文字は、その場所が空くだけ
            let _ = line.paint(to, cell.height, TextAlign::Left, None, window, cx);
        };
    for row in 0..snapshot.rows {
        for (range, color) in backgrounds(snapshot, row) {
            window.paint_quad(fill(
                Bounds::new(
                    at(range.start, row),
                    size(cell.width * range.len() as f32, cell.height),
                ),
                hsla(color),
            ));
        }
    }
    for row in 0..snapshot.rows {
        for segment in segments(snapshot, row) {
            let mut chars = segment.text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let cell = Bounds::new(at(segment.col, row), *cell);
                if paint_lines(c, cell, hsla(segment.fg), window) {
                    continue;
                }
            }
            draw(
                &segment.text,
                segment.fg,
                segment.style,
                at(segment.col, row),
                window,
                cx,
            );
        }
    }
    if let (Some(ime), Some((col, row))) = (&frame.ime, snapshot.input_at) {
        // 変換中の文字は、入力の場所に、色を敷いて下線を付けて重ねる。
        // カーソルを隠すプログラムの中でも、どこで変換しているかが分かる
        let color = hsla(malakt_term::CURSOR);
        let run = TextRun {
            len: ime.len(),
            font: frame.font.clone(),
            color: hsla(malakt_term::FOREGROUND),
            background_color: None,
            underline: Some(UnderlineStyle {
                thickness: gpui_kit::px(2.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: None,
        };
        let line =
            window
                .text_system()
                .shape_line(ime.clone().into(), frame.font_size, &[run], None);
        let origin = at(col, row);
        window.paint_quad(fill(
            Bounds::new(origin, size(line.width, cell.height)),
            hsla(malakt_term::SELECTION),
        ));
        // 描けなかった文字は、その場所が空くだけ
        let _ = line.paint(origin, cell.height, TextAlign::Left, None, window, cx);
    }
    if let Some((col, row, caret)) = snapshot.cursor {
        let here = snapshot.cells[row * snapshot.cols + col];
        let wide = if here.style & style::WIDE != 0 {
            2.
        } else {
            1.
        };
        let block = Bounds::new(at(col, row), size(cell.width * wide, cell.height));
        let color = hsla(malakt_term::CURSOR);
        if frame.ime.is_some() {
            // 変換中は、下で変換中の文字を描く
        } else if !frame.focused {
            window.paint_quad(outline(block, color, BorderStyle::Solid));
        } else {
            match caret {
                Caret::Block => {
                    window.paint_quad(fill(block, color));
                    if here.c != ' ' {
                        let look = here.style & TEXT_STYLE;
                        let text = String::from(here.c);
                        draw(
                            &text,
                            malakt_term::BACKGROUND,
                            look,
                            block.origin,
                            window,
                            cx,
                        );
                    }
                }
                Caret::Beam => window.paint_quad(fill(
                    Bounds::new(block.origin, size(gpui_kit::px(2.), cell.height)),
                    color,
                )),
                Caret::Underline => window.paint_quad(fill(
                    Bounds::new(
                        point(
                            block.origin.x,
                            block.origin.y + cell.height - gpui_kit::px(2.),
                        ),
                        size(block.size.width, gpui_kit::px(2.)),
                    ),
                    color,
                )),
            }
        }
    }
}

impl Render for TermPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let terms = self.terms.read(cx);
        let exit = terms.active_tab().and_then(|tab| tab.exit);
        let terminal = self.terminal(cx);
        if terms.focus_wanted && terminal.is_some() {
            self.terms.update(cx, |terms, _| terms.focus_wanted = false);
            window.focus(&self.focus, cx);
        }
        // 変換の途中でほかの場所へ移ったら、変換中の文字を残さない（残ると Enter などが届かなくなる）
        if self.ime.is_some() && !self.focus.is_focused(window) {
            self.ime = None;
        }
        let danger = cx.theme().danger;
        let pane = cx.entity();
        let focus = self.focus.clone();
        self.scroll.set_terminal(terminal.clone());
        let scroll = self.scroll.clone();
        div()
            .id("term-screen")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(gpui_kit::rgb(malakt_term::BACKGROUND))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(|this, event, _, cx| this.mouse_move(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| this.mouse_up(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event, _, cx| this.mouse_up(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event, _, cx| this.mouse_up(event, cx)),
            )
            .on_scroll(cx.listener(|this, event, _, cx| this.scroll(event, cx)))
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    this.modifiers_changed(event.modifiers, window, cx);
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    if let Some(terminal) = this.terminal(cx) {
                        this.hover_at(None, &terminal, cx);
                    }
                }
            }))
            .when(self.link.is_some(), |d| d.cursor_pointer())
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style.border_2().border_color(cx.theme().primary)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_paths(paths.paths(), window, cx);
            }))
            .when_some(exit, |d, code| {
                let text = match code {
                    Some(code) => format!("シェルが終了しました（終了コード {code}）"),
                    None => "シェルがシグナルで終了しました".to_owned(),
                };
                d.child(
                    div()
                        .id("term-exit")
                        .flex_none()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(danger)
                        .child(text)
                        .test_support(),
                )
            })
            .when_some(terminal, |d, terminal| {
                d.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(
                            canvas(
                                {
                                    let pane = pane.clone();
                                    move |bounds, window, cx| {
                                        pane.update(cx, |this, cx| {
                                            this.prepare(&terminal, bounds, window, cx)
                                        })
                                    }
                                },
                                move |bounds, frame, window, cx| {
                                    window.handle_input(
                                        &focus,
                                        ElementInputHandler::new(bounds, pane.clone()),
                                        cx,
                                    );
                                    paint(&frame, window, cx);
                                },
                            )
                            .size_full(),
                        )
                        // さかのぼれる行があれば、右にスクロールバーを出す。つかんで動かせる
                        .child(crate::scroll::bar_on_dark("term", &scroll)),
                )
            })
            .test_support()
    }
}

#[cfg(test)]
mod tests {
    use super::{backgrounds, expand_dir, segments};
    use malakt_term::{style, Cell, Snapshot, BACKGROUND, FOREGROUND, SELECTION};

    fn snapshot(cells: &[(char, u32, u32, u8)]) -> Snapshot {
        Snapshot {
            cols: cells.len(),
            rows: 1,
            cells: cells
                .iter()
                .map(|&(c, fg, bg, style)| Cell { c, fg, bg, style })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_row_is_split_where_the_look_changes() {
        let (f, b) = (FOREGROUND, BACKGROUND);
        let s = snapshot(&[
            ('l', f, b, 0),
            ('s', f, b, 0),
            (' ', f, b, 0),
            ('a', 0xff0000, b, 0),
            ('b', 0xff0000, b, style::BOLD),
            ('あ', f, b, style::WIDE),
            (' ', f, b, style::SPACER),
            ('─', f, b, 0),
            ('─', f, b, 0),
            ('x', f, b, 0),
            (' ', f, b, style::UNDERLINE),
        ]);
        let got: Vec<(usize, String, u32, u8)> = segments(&s, 0)
            .into_iter()
            .map(|s| (s.col, s.text, s.fg, s.style))
            .collect();
        assert_eq!(
            got,
            [
                (0, "ls".to_owned(), f, 0),
                (3, "a".to_owned(), 0xff0000, 0),
                (4, "b".to_owned(), 0xff0000, style::BOLD),
                // 全角と罫線は、1 文字ずつマスに置く
                (5, "あ".to_owned(), f, 0),
                (7, "─".to_owned(), f, 0),
                (8, "─".to_owned(), f, 0),
                (9, "x".to_owned(), f, 0),
                // 下線の付いた空白は描く
                (10, " ".to_owned(), f, style::UNDERLINE),
            ]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_utf8_locale_is_given_only_when_none_is_set() {
        use super::default_locale;
        assert!(default_locale(|_| None).is_some_and(|l| l.ends_with("UTF-8")));
        assert!(default_locale(|_| Some(String::new())).is_some());
        let lang = |name: &str| (name == "LANG").then(|| "ja_JP.UTF-8".to_owned());
        assert_eq!(default_locale(lang), None);
        let ctype = |name: &str| (name == "LC_CTYPE").then(|| "ja_JP.eucJP".to_owned());
        assert_eq!(default_locale(ctype), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_app_folder_is_added_to_the_path_once() {
        use super::path_with;
        use std::ffi::OsStr;
        use std::path::Path;
        let dir = Path::new("/Applications/Malakt Mahoz.app/Contents/MacOS");
        assert_eq!(
            path_with(OsStr::new("/usr/bin:/bin"), dir).as_deref(),
            Some("/usr/bin:/bin:/Applications/Malakt Mahoz.app/Contents/MacOS")
        );
        let have = OsStr::new("/usr/bin:/Applications/Malakt Mahoz.app/Contents/MacOS");
        assert_eq!(path_with(have, dir), None);
        // 区切りの文字が入った場所は、PATH に書けない
        assert_eq!(path_with(OsStr::new("/bin"), Path::new("/a:b")), None);
    }

    #[test]
    fn names_are_added_to_wslenv() {
        use super::wsl_env;
        let names = ["MALAKT_MAHOZ_BIN/p".to_owned(), "MALAKT_TASK_ID".to_owned()];
        assert_eq!(wsl_env("", &names), "MALAKT_MAHOZ_BIN/p:MALAKT_TASK_ID");
        assert_eq!(
            wsl_env("FOO/u:", &names),
            "FOO/u:MALAKT_MAHOZ_BIN/p:MALAKT_TASK_ID"
        );
    }

    #[test]
    fn a_tilde_is_the_home_folder() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_dir("~"), home);
        assert_eq!(expand_dir(" ~/work/app "), home.join("work/app"));
        assert_eq!(expand_dir("/srv/app"), std::path::PathBuf::from("/srv/app"));
        assert_eq!(
            expand_dir("~name/app"),
            std::path::PathBuf::from("~name/app")
        );
    }

    #[test]
    fn backgrounds_are_joined_and_selection_wins() {
        let (f, b) = (FOREGROUND, BACKGROUND);
        let s = snapshot(&[
            ('a', f, b, 0),
            ('b', f, 0x112233, 0),
            ('c', f, 0x112233, 0),
            ('d', f, 0x112233, style::SELECTED),
            ('e', f, b, style::SELECTED),
            ('f', f, b, 0),
        ]);
        assert_eq!(backgrounds(&s, 0), [(1..3, 0x112233), (3..5, SELECTION)]);
    }
}
