//! ポモドーロタイマー。最初の画面には出さず、「新しいウィンドウ」から別ウィンドウで開く。
//!
//! タイマーの状態はデータベース（[`malakt_core::pomodoro`]）にあり、画面・AI チャット・MCP の
//! どこから動かしても同じものを見る。ここは、1 秒ごとに時刻を見て段を進めること、段が終わったら
//! ウィンドウの輪を光らせること（通知はデータベースが作り、メインのウィンドウが OS とベルに出す）、
//! 段の始まりに OS で再生中の音楽を操作すること（[`malakt_media`]、標準は何もしない）と、
//! ウィンドウを受け持つ。ウィンドウを閉じてもタイマーは止まらない。

use std::sync::Arc;
use std::time::Instant;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Placement, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::pomodoro::{self, Media, Op, Phase, Run, Settings, Timer};
use malakt_core::{Scope, Task, TaskFilter};
use malakt_media::{Command, Failure};

use crate::clock::{self, px, BarHold, FLASH, RING, TICK};
use crate::db;
use crate::feel;
use crate::press::button;
use crate::render::{app_icon, col, row};
use crate::scroll::Menus;

actions!(pomodoro, [OpenPomodoro, TogglePomodoro, ClosePomodoroPage]);

/// 開いたときの大きさ。
const SIZE: Size<Pixels> = Size {
    width: px(340.),
    height: px(440.),
};
/// タスクを選ぶ一覧に出す数。
const PICK_LIMIT: usize = 100;
/// 位置と大きさ、開いていたかを覚える設定の名前。
const WINDOW_KEY: &str = "pomodoro.window";
const OPEN_KEY: &str = "pomodoro.open";
/// これより前に終わった段では、音楽を操作しない（アプリを閉じていた間に終わった段）。
const MEDIA_LATE_MS: i64 = 60_000;

/// OS で再生中のメディアに操作を送る。テストでは差し替える。
#[derive(Clone)]
pub struct MediaSender(pub Arc<dyn Fn(Command) -> Result<(), Failure> + Send + Sync>);

impl Global for MediaSender {}

/// タイマーの、画面から見た状態。
pub struct Pomodoro {
    pub timer: Timer,
    pub settings: Settings,
    /// 今日の作業の回数と合計（秒）。
    pub today: (usize, i64),
    /// 失敗したこと。ウィンドウに出す。
    pub error: Option<SharedString>,
    /// 知らせた、時刻が来て終わった段の数。
    seen_ended: u64,
    /// 段が終わった時刻。ウィンドウはしばらく輪を光らせる。
    pub flashed: Option<Instant>,
    /// 開いているウィンドウ。
    window: Option<AnyWindowHandle>,
}

pub struct Shared(pub Entity<Pomodoro>);

impl Global for Shared {}

pub fn shared(cx: &App) -> Entity<Pomodoro> {
    cx.global::<Shared>().0.clone()
}

/// 準備する。データベース（[`db::init`]）のあとに 1 回呼ぶ。
pub fn init(cx: &mut App) {
    let db = db::shared(cx);
    let loaded = {
        let store = &db.read(cx).store;
        store
            .pomodoro_timer()
            .and_then(|timer| Ok((timer, store.pomodoro_settings()?)))
    };
    let (timer, settings, error) = match loaded {
        Ok((timer, settings)) => (timer, settings, None),
        Err(e) => (
            Timer::default(),
            Settings::default(),
            Some(format!("ポモドーロの設定を読めませんでした: {e}").into()),
        ),
    };
    let entity = cx.new(|cx| {
        // ほかのウィンドウ・AI・MCP がタイマーを動かしたら、読み直す
        cx.observe(&db, |this: &mut Pomodoro, _, cx| this.refresh(cx))
            .detach();
        Pomodoro {
            seen_ended: timer.ended,
            timer,
            settings,
            today: (0, 0),
            error,
            flashed: None,
            window: None,
        }
    });
    cx.set_global(Shared(entity.clone()));
    if !cx.has_global::<MediaSender>() {
        cx.set_global(MediaSender(Arc::new(malakt_media::send)));
    }
    entity.update(cx, |this, cx| this.refresh(cx));
    cx.bind_keys([
        KeyBinding::new("space", TogglePomodoro, Some("PomodoroTimer")),
        KeyBinding::new("escape", ClosePomodoroPage, Some("Pomodoro")),
    ]);
    cx.on_action(|_: &OpenPomodoro, cx| {
        // メニューやキーから呼ぶときは、ウィンドウを更新している最中なので、終わってから開く
        cx.defer(|cx| {
            if let Err(e) = open(cx) {
                shared(cx).update(cx, |this, cx| {
                    this.fail(format!("ポモドーロのウィンドウを開けませんでした: {e}"), cx)
                });
            }
        })
    });
}

/// 時刻を見続ける。動いている間は 1 秒ごとに段を進め、画面を描き直す。起動したときに 1 回呼ぶ。
pub fn run(cx: &mut App) {
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(TICK).await;
        cx.update(|cx| {
            let this = shared(cx);
            if this.read(cx).timer.running() {
                this.update(cx, |this, cx| this.refresh(cx));
            }
        });
    })
    .detach();
}

/// 前に閉じたときに開いていれば、ポモドーロのウィンドウを開き直す。
pub fn reopen(cx: &mut App) {
    if clock::was_open(OPEN_KEY, cx) {
        if let Err(e) = open(cx) {
            shared(cx).update(cx, |this, cx| {
                this.fail(format!("ポモドーロのウィンドウを開けませんでした: {e}"), cx)
            });
        }
    }
}

/// どの段を始めたかで、音楽に送る操作。
fn media_command(media: Media) -> Option<Command> {
    match media {
        Media::None => None,
        Media::Play => Some(Command::Play),
        Media::Pause => Some(Command::Pause),
    }
}

/// タイマーが `before` から `after` に変わったときに、音楽に送る操作。
/// 作業を始めた（一時停止から戻した）ら作業の設定、作業が終わった・休憩を始めたら休憩の設定。
/// 作業で再生を選んでいて、作業を一時停止・停止したら、音楽も一時停止する。
pub fn media_step(before: &Timer, after: &Timer, settings: &Settings, now: i64) -> Option<Command> {
    let was = before.since.map(|_| before.phase);
    let is = after.since.map(|_| after.phase);
    let work_ended = before.phase == Phase::Work && after.phase.is_break();
    if is == Some(Phase::Work) && was != Some(Phase::Work) {
        return media_command(settings.work_media);
    }
    let break_began = is.is_some_and(Phase::is_break) && !was.is_some_and(Phase::is_break);
    if work_ended || break_began {
        // アプリを閉じていた間に終わった作業では、音楽を操作しない
        let late = after.ended_at.is_some_and(|at| now - at > MEDIA_LATE_MS)
            && after.ended > before.ended
            && !break_began;
        return media_command(settings.break_media).filter(|_| !late);
    }
    if was == Some(Phase::Work) && is.is_none() && settings.work_media == Media::Play {
        return Some(Command::Pause);
    }
    None
}

impl Pomodoro {
    /// データベースのタイマーを読み直す。時刻が来た段は進めて、作業を記録する。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = malakt_core::now_ms();
        let today = pomodoro::day_start(&malakt_core::view::today()).unwrap_or(0);
        let result = db::shared(cx).update(cx, |db, cx| {
            let before = db.store.pomodoro_timer();
            let result = db.store.pomodoro_now(now).and_then(|timer| {
                Ok((
                    timer,
                    db.store.pomodoro_settings()?,
                    db.store.work_total_since(today)?,
                ))
            });
            // 記録が増えたら、タスクの作業時間を出している画面に読み直させる
            if let (Ok(before), Ok((after, ..))) = (&before, &result) {
                if before.ended != after.ended {
                    cx.notify();
                }
            }
            result
        });
        match result {
            Ok((timer, settings, today)) => self.apply(timer, settings, today, now, cx),
            Err(e) => self.fail(format!("ポモドーロのタイマーを読めませんでした: {e}"), cx),
        }
    }

    fn apply(
        &mut self,
        timer: Timer,
        settings: Settings,
        today: (usize, i64),
        now: i64,
        cx: &mut Context<Self>,
    ) {
        if timer.ended > self.seen_ended {
            self.announce();
        }
        self.seen_ended = timer.ended;
        if let Some(command) = media_step(&self.timer, &timer, &settings, now) {
            self.send_media(command, cx);
        }
        self.timer = timer;
        self.settings = settings;
        self.today = today;
        cx.notify();
    }

    /// タイマーを操作する。
    pub fn act(&mut self, op: Op, cx: &mut Context<Self>) {
        let now = malakt_core::now_ms();
        let result = db::shared(cx).update(cx, |db, cx| {
            let result = db.store.pomodoro(op, now);
            cx.notify();
            result
        });
        match result {
            // 読み直しは、データベースの知らせで動く
            Ok(_) => self.refresh(cx),
            Err(e) => self.fail(format!("ポモドーロを操作できませんでした: {e}"), cx),
        }
    }

    /// 始める・一時停止する。
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let op = if self.timer.running() {
            Op::Pause
        } else {
            Op::Start { task: None }
        };
        self.act(op, cx);
    }

    /// 設定を変える。
    pub fn set_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let result = db::shared(cx).update(cx, |db, cx| {
            let result = db.store.set_pomodoro_settings(&settings);
            cx.notify();
            result
        });
        match result {
            Ok(_) => self.refresh(cx),
            Err(e) => self.fail(format!("ポモドーロの設定を変えられませんでした: {e}"), cx),
        }
    }

    pub fn fail(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = Some(text.into());
        cx.notify();
    }

    /// OS で再生中の音楽に、操作を送る。待つ間に画面を止めないよう、裏で送る。
    pub fn send_media(&mut self, command: Command, cx: &mut Context<Self>) {
        let sender = cx.global::<MediaSender>().clone();
        let task = cx
            .background_executor()
            .spawn(async move { (sender.0)(command) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // ウィンドウを閉じたあとでも、次に開いたときに出す
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => crate::now_playing::shared(cx).update(cx, |m, cx| m.read_soon(cx)),
                Err(e) => this.fail(
                    format!("音楽を「{}」にできませんでした: {e}", command.label()),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// 段が終わったら、ウィンドウの輪を光らせる。通知はデータベースが作り
    /// （[`malakt_core::notice::NoticeKind::Pomodoro`]）、メインのウィンドウが OS とベルに出す。
    fn announce(&mut self) {
        self.flashed = Some(Instant::now());
    }
}

/// 開いているポモドーロのウィンドウ。
pub fn window(cx: &App) -> Option<AnyWindowHandle> {
    shared(cx).read(cx).window
}

/// ポモドーロのウィンドウを開く。開いていれば手前に出す。
pub fn open(cx: &mut App) -> Result<AnyWindowHandle> {
    if let Some(handle) = shared(cx).read(cx).window {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return Ok(handle);
        }
    }
    let options =
        clock::window_options("ポモドーロ", WINDOW_KEY, SIZE, size(px(260.), px(320.)), cx);
    let (handle, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| PomodoroView::new(window, cx))
    })?;
    shared(cx).update(cx, |this, _| this.window = Some(handle));
    crate::on_top::opened(handle, crate::on_top::Kind::Pomodoro, None, cx);
    clock::mark_open(OPEN_KEY, cx)?;
    Ok(handle)
}

/// ウィンドウの中で見ているもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Timer,
    /// タスクを選ぶ。
    Tasks,
    Settings,
}

/// ポモドーロのウィンドウの中身。
pub struct PomodoroView {
    pomodoro: Entity<Pomodoro>,
    pub page: Page,
    /// バーの空いているところを押している。動かし始めたらウィンドウを動かす。
    bar_pressed: bool,
    /// タイマーの面のキー（Space）の入力先。
    focus: FocusHandle,
    /// 設定の面のキー（Esc）の入力先。
    page_focus: FocusHandle,
    search: Entity<InputState>,
    /// タスクを選ぶ一覧。
    pub found: Vec<Task>,
    /// ウィンドウの位置と大きさ。閉じるときに覚える。
    bounds: Option<[f32; 4]>,
    _subscriptions: Vec<Subscription>,
}

impl PomodoroView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pomodoro = shared(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("タスクを検索"));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // 開いている間だけ、再生中の曲を読み続ける
        let music = crate::now_playing::shared(cx);
        cx.spawn(async move |this, cx| loop {
            let alive = this.update(cx, |_, cx| {
                crate::now_playing::shared(cx).update(cx, |m, cx| m.read(cx))
            });
            if alive.is_err() {
                break;
            }
            cx.background_executor()
                .timer(crate::now_playing::EVERY)
                .await;
        })
        .detach();
        let subscriptions = vec![
            cx.observe(&pomodoro, |_, _, cx| cx.notify()),
            cx.observe(&music, |_, _, cx| cx.notify()),
            cx.observe(&db::shared(cx), |this, _, cx| {
                if this.page == Page::Tasks {
                    this.find_tasks(cx);
                }
                cx.notify()
            }),
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.find_tasks(cx);
                }
            }),
            cx.observe_window_bounds(window, |this, window, _| {
                this.bounds = Some(clock::bounds_of(window));
            }),
            // 閉じたら、位置と大きさを覚える。終了で閉じるときは、次の起動で開き直す
            cx.on_release(|this, cx| {
                let bounds = this.bounds;
                cx.defer(move |cx| {
                    shared(cx).update(cx, |this, _| this.window = None);
                    if let Err(e) = clock::remember_closed(WINDOW_KEY, OPEN_KEY, bounds, cx) {
                        shared(cx).update(cx, |this, cx| {
                            this.fail(
                                format!("ポモドーロのウィンドウの位置を保存できませんでした: {e}"),
                                cx,
                            )
                        });
                    }
                });
            }),
        ];
        Self {
            pomodoro,
            page: Page::Timer,
            bar_pressed: false,
            focus,
            page_focus: cx.focus_handle(),
            search,
            found: Vec::new(),
            bounds: None,
            _subscriptions: subscriptions,
        }
    }

    /// 見るものを変える。
    pub fn show(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        if page == Page::Tasks {
            self.search
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.find_tasks(cx);
            let search = self.search.clone();
            search.update(cx, |input, cx| input.focus(window, cx));
        } else if page == Page::Settings {
            window.focus(&self.page_focus, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// タスクを選ぶ一覧を、検索の欄に合わせて読み直す。
    fn find_tasks(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_owned();
        let filter = TaskFilter {
            include_descendants: true,
            open_only: true,
            query: (!query.is_empty()).then_some(query),
            ..Default::default()
        };
        match db::shared(cx).read(cx).store.tasks(Scope::All, &filter) {
            Ok(mut tasks) => {
                tasks.truncate(PICK_LIMIT);
                self.found = tasks;
            }
            Err(e) => self.pomodoro.update(cx, |this, cx| {
                this.fail(format!("タスクの一覧を読めませんでした: {e}"), cx)
            }),
        }
        cx.notify();
    }

    fn pick(&mut self, task: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.pomodoro
            .update(cx, |this, cx| this.act(Op::SetTask(task), cx));
        self.show(Page::Timer, window, cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let mut settings = self.pomodoro.read(cx).settings;
        f(&mut settings);
        self.pomodoro
            .update(cx, |this, cx| this.set_settings(settings, cx));
    }

    /// 上の低い 1 行のバー。空いているところを掴むと、ウィンドウを動かせる。
    fn render_bar(&self, unlocked: bool, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let (back, title) = match self.page {
            Page::Timer => (false, "ポモドーロ"),
            Page::Tasks => (true, "タスクを選ぶ"),
            Page::Settings => (true, "ポモドーロの設定"),
        };
        let left = if back {
            clock::back_button("pomodoro-back", cx, |this, window, cx| {
                this.show(Page::Timer, window, cx)
            })
        } else {
            clock::bar_icon("timer")
        };
        let mut right = Vec::new();
        if self.page == Page::Timer && unlocked {
            right.push(
                feel::gloss(
                    button("pomodoro-settings")
                        .icon(app_icon("settings"))
                        .tooltip("設定")
                        .tooltip_placement(Placement::Left)
                        .ghost()
                        .xsmall()
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.show(Page::Settings, window, cx)
                            }),
                        ),
                )
                .into_any_element(),
            );
        }
        clock::bar("pomodoro-bar", left, title.into(), right, window, cx)
    }

    /// 残り時間の輪と、段の名前、残り時間、済んだ作業の数。
    fn render_face(&self, now: i64, reduce_motion: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let p = self.pomodoro.read(cx);
        let timer = &p.timer;
        let color = if timer.phase.is_break() {
            theme.success
        } else {
            theme.primary
        };
        let progress = timer.progress(now);
        let track = theme.muted;
        let flashing = p.flashed.is_some_and(|at| at.elapsed() < FLASH);
        let rounds = p.settings.rounds;
        let done = timer.done.min(rounds);
        let today = p.today;
        let ring = clock::ring(progress, color, track);
        let dots = row()
            .id("pomodoro-rounds")
            .gap_1()
            .children((0..rounds).map(|i| {
                div()
                    .size(px(7.))
                    .rounded_full()
                    .when(i < done, |d| d.bg(color))
                    .when(i >= done, |d| d.border_1().border_color(theme.border))
            }))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(format!(
                    "今日 {} 回・{}",
                    today.0,
                    pomodoro::duration_label(today.1)
                ))
                .build(window, cx)
            });
        let label = match timer.run() {
            Run::Paused => format!("{}（一時停止中）", timer.phase.label()),
            _ => timer.phase.label().to_owned(),
        };
        let face = div()
            .id("pomodoro-face")
            .relative()
            .flex_none()
            .size(RING)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(ring)
            .child(
                div()
                    .id("pomodoro-phase")
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .aria_label(label.clone())
                    .child(label)
                    .test_support(),
            )
            .child(
                div()
                    .id("pomodoro-time")
                    .text_size(px(40.))
                    .line_height(px(44.))
                    .text_color(if timer.run() == Run::Paused {
                        theme.muted_foreground
                    } else {
                        theme.foreground
                    })
                    .aria_label(pomodoro::clock_label(timer.remaining_ms(now)))
                    .child(pomodoro::clock_label(timer.remaining_ms(now)))
                    .test_support(),
            )
            .child(dots);
        // 段が終わったら、輪の外側が 3 回ふくらんで消える
        clock::glow(
            face,
            ("pomodoro-flash", p.seen_ended),
            color,
            flashing && !reduce_motion,
        )
    }

    fn render_timer(
        &self,
        unlocked: bool,
        now: i64,
        reduce_motion: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (running, fresh, task_id) = {
            let timer = &self.pomodoro.read(cx).timer;
            // 最初の作業を始める前は、止めるものがない
            let fresh = timer.run() == Run::Ready && timer.phase == Phase::Work && timer.done == 0;
            (timer.running(), fresh, timer.task_id.clone())
        };
        let task_title = task_id
            .as_deref()
            .and_then(|id| db::shared(cx).read(cx).store.task(id).ok().map(|t| t.title));
        let task = button("pomodoro-task")
            .icon(app_icon("list-todo"))
            // ロック中は、タスクの名前を出さない
            .label(match (&task_title, unlocked) {
                (Some(title), true) => title.clone(),
                _ => "タスクなし".to_owned(),
            })
            .tooltip("タスクを選ぶ")
            .ghost()
            .small()
            .disabled(!unlocked)
            .on_click(cx.listener(|this, _, window, cx| this.show(Page::Tasks, window, cx)));
        let tool = |id: &'static str, icon: &str, tip: &'static str| {
            button(id).icon(app_icon(icon)).tooltip(tip).ghost()
        };
        let controls =
            row()
                .gap_3()
                .items_center()
                .child(feel::gloss_when(
                    !fresh,
                    tool("pomodoro-stop", "square", "停止")
                        .disabled(fresh)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pomodoro.update(cx, |p, cx| p.act(Op::Stop, cx))
                        })),
                ))
                .child(feel::gloss_round(
                    button("pomodoro-toggle")
                        .icon(app_icon(if running { "pause" } else { "play" }))
                        .accessibility_label(if running { "一時停止" } else { "開始" })
                        .tooltip(if running {
                            "一時停止（Space）"
                        } else {
                            "開始（Space）"
                        })
                        .large()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pomodoro.update(cx, |p, cx| p.toggle(cx))
                        })),
                ))
                .child(feel::gloss(
                    tool("pomodoro-skip", "skip-forward", "次の段へ").on_click(cx.listener(
                        |this, _, _, cx| this.pomodoro.update(cx, |p, cx| p.act(Op::Skip, cx)),
                    )),
                ));
        let media = malakt_media::supported().then(|| self.render_music(cx));
        col()
            .key_context("PomodoroTimer")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_3()
            .p_3()
            .child(self.render_face(now, reduce_motion, cx))
            .child(div().max_w_full().child(task))
            .child(controls)
            .children(media)
            .into_any_element()
    }

    /// 再生中の曲（ジャケット・曲名・アーティスト）と、前・再生 / 一時停止・次のボタン。
    fn render_music(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let music = crate::now_playing::shared(cx);
        let (track, error) = {
            let m = music.read(cx);
            (m.track.clone(), m.error.clone())
        };
        let send = |command: Command| {
            cx.listener(move |this: &mut Self, _, _, cx| {
                this.pomodoro.update(cx, |p, cx| p.send_media(command, cx))
            })
        };
        let tool = |id: &'static str, icon: &str, tip: &'static str| {
            button(id)
                .icon(app_icon(icon))
                .tooltip(tip)
                .ghost()
                .xsmall()
        };
        let toggle = match &track {
            Some(t) if t.playing => ("pause", "再生中の音楽を一時停止"),
            Some(_) => ("play", "再生中の音楽を再生"),
            None => ("music", "再生中の音楽を再生 / 一時停止"),
        };
        let buttons = row()
            .flex_none()
            .gap_1()
            .text_color(theme.muted_foreground)
            .child(tool("media-previous", "skip-back", "前の曲").on_click(send(Command::Previous)))
            .child(
                tool("media-toggle", toggle.0, toggle.1)
                    .accessibility_label(toggle.1)
                    .on_click(send(Command::Toggle)),
            )
            .child(tool("media-next", "skip-forward", "次の曲").on_click(send(Command::Next)));
        let Some(track) = track else {
            return col()
                .items_center()
                .gap_1()
                .child(buttons)
                .children(error.map(|text| {
                    div()
                        .id("now-playing-error")
                        .test_support()
                        .aria_label(text.clone())
                        .max_w_full()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(text)
                }))
                .into_any_element();
        };
        let art = px(40.);
        let artwork = match track.artwork.clone() {
            Some(image) => img(image)
                .size(art)
                .rounded(px(4.))
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size(art)
                .flex_none()
                .rounded(px(4.))
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(app_icon("music"))
                .into_any_element(),
        };
        let sub = [track.artist.as_ref(), track.album.as_ref()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        row()
            .id("now-playing")
            .test_support()
            .w_full()
            .max_w(px(320.))
            .gap_2()
            .child(div().flex_none().child(artwork))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id("now-playing-title")
                            .test_support()
                            .aria_label(track.title.clone())
                            .text_sm()
                            .truncate()
                            .child(track.title.clone()),
                    )
                    .when(!sub.is_empty(), |this| {
                        this.child(
                            div()
                                .id("now-playing-artist")
                                .test_support()
                                .aria_label(sub.clone())
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(sub),
                        )
                    }),
            )
            .child(buttons)
            .into_any_element()
    }

    fn render_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let current = self.pomodoro.read(cx).timer.task_id.clone();
        let entry = |id: ElementId, title: SharedString, chosen: bool, muted: bool| {
            row()
                .id(id)
                .w_full()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded(feel::md())
                .cursor_pointer()
                .text_sm()
                .when(muted, |d| d.text_color(theme.muted_foreground))
                .when(chosen, |d| feel::selected(d, theme.accent, cx))
                .hover(|s| s.bg(theme.secondary_hover))
                .aria_label(title.clone())
                .child(div().flex_1().min_w_0().truncate().child(title))
                .when(chosen, |d| d.child(app_icon("check").small()))
        };
        let mut list = col().id("pomodoro-tasks").gap_0p5().child(
            entry(
                "pomodoro-no-task".into(),
                "タスクなし".into(),
                current.is_none(),
                true,
            )
            .on_click(cx.listener(|this, _, window, cx| this.pick(None, window, cx)))
            .test_support(),
        );
        for (i, task) in self.found.iter().enumerate() {
            let id = task.id.clone();
            list = list.child(
                entry(
                    ("pomodoro-pick", i).into(),
                    task.title.clone().into(),
                    current.as_deref() == Some(task.id.as_str()),
                    false,
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.pick(Some(id.clone()), window, cx)),
                )
                .test_support(),
            );
        }
        col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .p_2()
            .child(
                Input::new(&self.search)
                    .id("pomodoro-search")
                    .aria_label("タスクを検索")
                    .small(),
            )
            .child(
                div()
                    .id("pomodoro-tasks-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let settings = self.pomodoro.read(cx).settings;
        let line = |label: &'static str| {
            row()
                .w_full()
                .gap_2()
                .items_center()
                .min_h(px(28.))
                .child(div().flex_1().min_w_0().text_sm().child(label))
        };
        let stepper = |id: &'static str,
                       value: u32,
                       unit: &'static str,
                       min: u32,
                       max: u32,
                       set: fn(&mut Settings, u32),
                       cx: &mut Context<Self>| {
            // 長さは 5 分ごと（5 分より短いときは 1 分ごと）に変える
            let step = |value: u32, up: bool| -> u32 {
                let by = if unit == "回" || value < 5 || (value == 5 && !up) {
                    1
                } else {
                    5
                };
                if up {
                    (value + by).min(max)
                } else {
                    value.saturating_sub(by).max(min)
                }
            };
            let (down, up) = (step(value, false), step(value, true));
            row()
                .gap_1()
                .items_center()
                .child(
                    button(SharedString::from(format!("{id}-down")))
                        .icon(app_icon("minus"))
                        .tooltip("減らす")
                        .ghost()
                        .xsmall()
                        .disabled(value <= min)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.edit(cx, |s| set(s, down))),
                        ),
                )
                .child(
                    div()
                        .id(id)
                        .w(px(52.))
                        .text_center()
                        .text_sm()
                        .child(format!("{value} {unit}"))
                        .test_support(),
                )
                .child(
                    button(SharedString::from(format!("{id}-up")))
                        .icon(app_icon("plus"))
                        .tooltip("増やす")
                        .ghost()
                        .xsmall()
                        .disabled(value >= max)
                        .on_click(cx.listener(move |this, _, _, cx| this.edit(cx, |s| set(s, up)))),
                )
        };
        let (min, max) = (pomodoro::MIN_MINUTES, pomodoro::MAX_MINUTES);
        let media_row = |id: &'static str,
                         current: Media,
                         set: fn(&mut Settings, Media),
                         cx: &mut Context<Self>| {
            row().gap_1().children(Media::ALL.into_iter().map(|m| {
                crate::render::chip((id, m as usize), m.label(), m == current, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.edit(cx, |s| set(s, m))))
                    .test_support()
            }))
        };
        let mut list = col()
            .id("pomodoro-settings-list")
            .gap_2()
            .child(line("作業").child(stepper(
                "pomodoro-work",
                settings.work,
                "分",
                min,
                max,
                |s, v| s.work = v,
                cx,
            )))
            .child(line("休憩").child(stepper(
                "pomodoro-short",
                settings.short_break,
                "分",
                min,
                max,
                |s, v| s.short_break = v,
                cx,
            )))
            .child(line("長い休憩").child(stepper(
                "pomodoro-long",
                settings.long_break,
                "分",
                min,
                max,
                |s, v| s.long_break = v,
                cx,
            )))
            .child(line("長い休憩までの回数").child(stepper(
                "pomodoro-rounds-count",
                settings.rounds,
                "回",
                pomodoro::MIN_ROUNDS,
                pomodoro::MAX_ROUNDS,
                |s, v| s.rounds = v,
                cx,
            )))
            .child(
                line("休憩を自動で始める").child(
                    Switch::new("pomodoro-auto-break")
                        .checked(settings.auto_break)
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            let on = *on;
                            this.edit(cx, |s| s.auto_break = on)
                        })),
                ),
            );
        if malakt_media::supported() {
            list = list
                .child(div().h(gpui_kit::px(1.)).bg(theme.border))
                .child(line("作業を始めたときの音楽"))
                .child(media_row(
                    "pomodoro-work-media",
                    settings.work_media,
                    |s, m| s.work_media = m,
                    cx,
                ))
                .child(line("休憩を始めたときの音楽"))
                .child(media_row(
                    "pomodoro-break-media",
                    settings.break_media,
                    |s, m| s.break_media = m,
                    cx,
                ));
        }
        div()
            .id("pomodoro-settings-scroll")
            .track_focus(&self.page_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .p_3()
            .child(list)
            .into_any_element()
    }
}

impl BarHold for PomodoroView {
    fn bar_pressed(&mut self) -> &mut bool {
        &mut self.bar_pressed
    }
}

impl Render for PomodoroView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let unlocked = db::shared(cx).read(cx).unlocked;
        // ロックしたら、タスクの一覧と設定は閉じる
        if !unlocked && self.page != Page::Timer {
            self.page = Page::Timer;
        }
        let now = malakt_core::now_ms();
        let reduce_motion = cx.reduce_motion();
        let (error, flashing) = {
            let p = self.pomodoro.read(cx);
            (
                p.error.clone(),
                p.flashed.is_some_and(|at| at.elapsed() < FLASH),
            )
        };
        // 光っている間は描き続け、終わったら止める
        if flashing {
            window.request_animation_frame();
        }
        let body = match self.page {
            Page::Timer => self.render_timer(unlocked, now, reduce_motion, cx),
            Page::Tasks => self.render_tasks(cx),
            Page::Settings => self.render_settings(cx),
        };
        let bar = self
            .render_bar(unlocked, window, cx)
            .right_menu(|menu, window, cx| crate::render::window_menu(menu, None, window, cx));
        div()
            .id("pomodoro")
            .key_context("Pomodoro")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &TogglePomodoro, _, cx| {
                this.pomodoro.update(cx, |p, cx| p.toggle(cx))
            }))
            .on_action(cx.listener(|this, _: &ClosePomodoroPage, window, cx| {
                if this.page != Page::Timer {
                    this.show(Page::Timer, window, cx);
                } else if this.pomodoro.read(cx).error.is_some() {
                    this.pomodoro.update(cx, |p, cx| {
                        p.error = None;
                        cx.notify()
                    });
                } else {
                    cx.propagate();
                }
            }))
            .on_action(|_: &crate::shortcuts::ToggleFullscreen, window, _| {
                window.toggle_fullscreen()
            })
            // ここでもマウスの操作を見張る（自動でロックするまでの時間）
            .child(db::input_watcher())
            .child(bar)
            .children(error.map(|error| {
                clock::error_row("pomodoro", error, cx, |this, cx| {
                    this.pomodoro.update(cx, |p, cx| {
                        p.error = None;
                        cx.notify()
                    })
                })
            }))
            .child(body)
            .test_support()
    }
}

#[cfg(test)]
mod tests {
    use super::media_step;
    use malakt_core::pomodoro::{Media, Settings, Timer};
    use malakt_media::Command;

    #[test]
    fn 段の始まりと終わりに送る操作() {
        let s = Settings {
            work_media: Media::Play,
            break_media: Media::Pause,
            ..Settings::default()
        };
        let ready = Timer::new(&s);
        let mut running = ready.clone();
        running.start(0);
        assert_eq!(media_step(&ready, &running, &s, 0), Some(Command::Play));
        // 一時停止・停止で、音楽も止める
        let mut paused = running.clone();
        paused.pause(1000);
        assert_eq!(
            media_step(&running, &paused, &s, 1000),
            Some(Command::Pause)
        );
        // 作業が終わったら、休憩の設定
        let mut ended = running.clone();
        ended.advance(&s, 25 * 60_000);
        assert_eq!(
            media_step(&running, &ended, &s, 25 * 60_000),
            Some(Command::Pause)
        );
        // 閉じていた間に終わった作業では、何もしない
        assert_eq!(media_step(&running, &ended, &s, 3 * 3_600_000), None);
        // 何も選んでいなければ、何も送らない
        let none = Settings::default();
        assert_eq!(media_step(&ready, &running, &none, 0), None);
        assert_eq!(media_step(&running, &paused, &none, 0), None);
        assert_eq!(media_step(&running, &ended, &none, 25 * 60_000), None);
    }
}
