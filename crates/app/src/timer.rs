//! 単純なタイマー（カウントダウンとストップウォッチ）。最初の画面には出さず、「新しいウィンドウ」から
//! 別ウィンドウで開く。
//!
//! 状態はデータベース（[`malakt_core::timer`]）にあり、画面・AI bot・MCP のどこから動かしても
//! 同じものを見る。ここは、1 秒ごとに時刻を見ること、終わったらウィンドウの輪を光らせること
//! （通知はデータベースが作り、メインのウィンドウが OS とベルに出す）と、ウィンドウを受け持つ。
//! ウィンドウを閉じてもタイマーは止まらない。部品はポモドーロと同じもの（[`crate::clock`]）。

use std::time::Instant;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Placement, Selectable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::timer::{self, Mode, Op, Run, Timer};

use crate::clock::{self, px, BarHold, FLASH, RING, TICK};
use crate::db;
use crate::feel::{self, Lit};
use crate::press::button;
use crate::render::{app_icon, col, row};
use crate::scroll::Menus;

actions!(timer, [OpenTimer, ToggleTimer, CloseTimerEdit]);

/// キーの一覧に出す、タイマーのキー。
pub fn help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Space", "開始 / 一時停止"),
        ("Esc", "時間の編集をキャンセル"),
    ]
}

/// 開いたときの大きさ。
const SIZE: Size<Pixels> = Size {
    width: px(320.),
    height: px(420.),
};
/// 位置と大きさ、開いていたかを覚える設定の名前。
const WINDOW_KEY: &str = "timer.window";
const OPEN_KEY: &str = "timer.open";
/// すぐ選べる長さ（秒）。
pub const PRESETS: [i64; 5] = [60, 180, 300, 600, 1800];
/// 時間の欄の大きさ。読むときと書き直すときで同じ。
const TIME_WIDTH: Pixels = px(176.);
const TIME_HEIGHT: Pixels = px(52.);
/// すぐ選べる長さの列の高さ。出していないときも空けておく。
const PRESETS_HEIGHT: Pixels = px(28.);
/// 「足す」ボタンで足す長さ（秒）。
const ADD_SECS: i64 = 60;

/// タイマーの、画面から見た状態。
pub struct TimerState {
    pub timer: Timer,
    /// 失敗したこと。ウィンドウに出す。
    pub error: Option<SharedString>,
    /// 光らせた、時刻が来て終わったカウントダウンの数。
    seen_ended: u64,
    /// 終わった時刻。ウィンドウはしばらく輪を光らせる。
    pub flashed: Option<Instant>,
    /// 開いているウィンドウ。
    window: Option<AnyWindowHandle>,
}

pub struct Shared(pub Entity<TimerState>);

impl Global for Shared {}

pub fn shared(cx: &App) -> Entity<TimerState> {
    cx.global::<Shared>().0.clone()
}

/// 準備する。データベース（[`db::init`]）のあとに 1 回呼ぶ。
pub fn init(cx: &mut App) {
    let db = db::shared(cx);
    let (timer, error) = match db.read(cx).store.timer_state() {
        Ok(timer) => (timer, None),
        Err(e) => (
            Timer::default(),
            Some(format!("タイマーを読めませんでした: {e}").into()),
        ),
    };
    let entity = cx.new(|cx| {
        // ほかのウィンドウ・AI・MCP がタイマーを動かしたら、読み直す
        cx.observe(&db, |this: &mut TimerState, _, cx| this.refresh(cx))
            .detach();
        TimerState {
            seen_ended: timer.ended,
            timer,
            error,
            flashed: None,
            window: None,
        }
    });
    cx.set_global(Shared(entity.clone()));
    entity.update(cx, |this, cx| this.refresh(cx));
    cx.bind_keys([
        KeyBinding::new("space", ToggleTimer, Some("TimerFace")),
        KeyBinding::new("escape", CloseTimerEdit, Some("TimerWindow")),
    ]);
    cx.on_action(|_: &OpenTimer, cx| {
        // メニューやキーから呼ぶときは、ウィンドウを更新している最中なので、終わってから開く
        cx.defer(|cx| {
            if let Err(e) = open(cx) {
                shared(cx).update(cx, |this, cx| {
                    this.fail(format!("タイマーのウィンドウを開けませんでした: {e}"), cx)
                });
            }
        })
    });
}

/// 時刻を見続ける。動いている間は 1 秒ごとに読み直し、画面を描き直す。起動したときに 1 回呼ぶ。
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

/// 前に閉じたときに開いていれば、タイマーのウィンドウを開き直す。
pub fn reopen(cx: &mut App) {
    if clock::was_open(OPEN_KEY, cx) {
        if let Err(e) = open(cx) {
            shared(cx).update(cx, |this, cx| {
                this.fail(format!("タイマーのウィンドウを開けませんでした: {e}"), cx)
            });
        }
    }
}

impl TimerState {
    /// データベースのタイマーを読み直す。時刻が来たカウントダウンは止まり、通知ができる。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = malakt_core::now_ms();
        let result = db::shared(cx).update(cx, |db, cx| {
            let ended = db.store.timer_state().map(|t| t.ended);
            let result = db.store.timer_now(now);
            // 終わって通知ができたら、メインのウィンドウに OS とベルへ出させる
            if let (Ok(before), Ok(after)) = (&ended, &result) {
                if *before != after.ended {
                    cx.notify();
                }
            }
            result
        });
        match result {
            Ok(timer) => self.apply(timer, cx),
            Err(e) => self.fail(format!("タイマーを読めませんでした: {e}"), cx),
        }
    }

    fn apply(&mut self, timer: Timer, cx: &mut Context<Self>) {
        if timer.ended > self.seen_ended {
            self.flashed = Some(Instant::now());
        }
        self.seen_ended = timer.ended;
        self.timer = timer;
        cx.notify();
    }

    /// タイマーを操作する。
    pub fn act(&mut self, op: Op, cx: &mut Context<Self>) {
        let now = malakt_core::now_ms();
        let result = db::shared(cx).update(cx, |db, cx| {
            let result = db.store.timer(op, now);
            cx.notify();
            result
        });
        match result {
            Ok(timer) => self.apply(timer, cx),
            Err(e) => self.fail(format!("タイマーを操作できませんでした: {e}"), cx),
        }
    }

    /// 始める・一時停止する。
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let op = if self.timer.running() {
            Op::Pause
        } else {
            Op::Start
        };
        self.act(op, cx);
    }

    pub fn fail(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = Some(text.into());
        cx.notify();
    }
}

/// 開いているタイマーのウィンドウ。
pub fn window(cx: &App) -> Option<AnyWindowHandle> {
    shared(cx).read(cx).window
}

/// タイマーのウィンドウを開く。開いていれば手前に出す。
pub fn open(cx: &mut App) -> Result<AnyWindowHandle> {
    if let Some(handle) = shared(cx).read(cx).window {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return Ok(handle);
        }
    }
    let options = clock::window_options("タイマー", WINDOW_KEY, SIZE, size(px(240.), px(300.)), cx);
    let (handle, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| TimerView::new(window, cx))
    })?;
    shared(cx).update(cx, |this, _| this.window = Some(handle));
    crate::on_top::opened(handle, crate::on_top::Kind::Timer, None, cx);
    clock::mark_open(OPEN_KEY, cx)?;
    Ok(handle)
}

/// タイマーのウィンドウの中身。
pub struct TimerView {
    state: Entity<TimerState>,
    /// バーの空いているところを押している。動かし始めたらウィンドウを動かす。
    bar_pressed: bool,
    /// キー（Space）の入力先。
    focus: FocusHandle,
    /// 長さをその場で書き直している。
    pub editing: bool,
    input: Entity<InputState>,
    /// ウィンドウの位置と大きさ。閉じるときに覚える。
    bounds: Option<[f32; 4]>,
    _subscriptions: Vec<Subscription>,
}

impl TimerView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = shared(cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("5:00"));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.commit(window, cx),
                    // ほかの場所を押したら保存する
                    InputEvent::Blur if this.editing => this.commit(window, cx),
                    _ => {}
                },
            ),
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
                                format!("タイマーのウィンドウの位置を保存できませんでした: {e}"),
                                cx,
                            )
                        });
                    }
                });
            }),
        ];
        Self {
            state,
            bar_pressed: false,
            focus,
            editing: false,
            input,
            bounds: None,
            _subscriptions: subscriptions,
        }
    }

    fn act(&mut self, op: Op, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.act(op, cx));
    }

    /// 長さをその場で書き直し始める。
    pub fn start_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let length = self.state.read(cx).timer.length;
        let text = malakt_core::pomodoro::clock_label(length * 1000);
        self.editing = true;
        let input = self.input.clone();
        input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// 書いた長さにする。読めなければエラーを出して、書いたまま残す。
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        match timer::parse_length(&text) {
            Some(secs) => {
                self.editing = false;
                window.focus(&self.focus, cx);
                if secs != self.state.read(cx).timer.length
                    || self.state.read(cx).timer.run() != Run::Ready
                {
                    self.act(Op::SetLength(secs), cx);
                }
            }
            None => {
                self.editing = false;
                window.focus(&self.focus, cx);
                self.state.update(cx, |s, cx| {
                    s.fail(format!("時間を読めませんでした: {}", text.trim()), cx)
                });
            }
        }
        cx.notify();
    }

    fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn render_bar(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let mode = self.state.read(cx).timer.mode;
        let modes = Mode::ALL.map(|m| {
            let (id, icon) = match m {
                Mode::Countdown => ("timer-countdown", "hourglass"),
                Mode::Stopwatch => ("timer-stopwatch", "timer"),
            };
            feel::gloss(
                button(id)
                    .icon(app_icon(icon))
                    .tooltip(m.label())
                    .tooltip_placement(Placement::Bottom)
                    .ghost()
                    .xsmall()
                    .selected(m == mode)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.cancel_editing(window, cx);
                        this.act(Op::SetMode(m), cx)
                    })),
            )
        });
        clock::bar(
            "timer-bar",
            clock::bar_icon("alarm-clock"),
            "タイマー".into(),
            modes.into(),
            window,
            cx,
        )
    }

    /// 輪と、今の様子、時間。カウントダウンが止まっている間は、時間を押すと書き直せる。
    fn render_face(&self, now: i64, reduce_motion: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.state.read(cx);
        let t = &s.timer;
        let color = if t.run() == Run::Done {
            theme.success
        } else {
            theme.primary
        };
        let flashing = s.flashed.is_some_and(|at| at.elapsed() < FLASH);
        let seen = s.seen_ended;
        let editable = t.mode == Mode::Countdown && !t.running();
        let status = match (t.mode, t.run()) {
            (_, Run::Paused) => "一時停止中".to_owned(),
            (_, Run::Done) => "終わりました".to_owned(),
            (Mode::Countdown, _) => timer::length_label(t.length),
            (Mode::Stopwatch, _) => Mode::Stopwatch.label().to_owned(),
        };
        let label = t.label(now);
        let dim = matches!(t.run(), Run::Paused);
        let time_size = px(40.);
        let time = if self.editing {
            // その場で書き直す。文字の大きさも位置も、読むときと同じ
            div()
                .w(TIME_WIDTH)
                .text_size(time_size)
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.commit(window, cx)))
                .child(
                    Input::new(&self.input)
                        .id("timer-input")
                        .aria_label("時間")
                        // `Input::h` は複数行の入力欄のもの。1 行の高さは最小の高さで広げる
                        .min_h(TIME_HEIGHT)
                        .text_size(time_size)
                        .line_height(px(44.))
                        .text_center()
                        .bg(theme.background)
                        .border_color(theme.ring)
                        .px_2(),
                )
                .into_any_element()
        } else {
            div()
                .id("timer-time")
                .w(TIME_WIDTH)
                .h(TIME_HEIGHT)
                .flex()
                .items_center()
                .justify_center()
                .px_2()
                .rounded(feel::md())
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .text_size(time_size)
                .line_height(px(44.))
                .text_color(if dim {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .aria_label(label.clone())
                .child(label)
                .when(editable, |d| {
                    d.cursor_pointer()
                        .lit(theme.muted, cx)
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("時間を変える")
                                .build(window, cx)
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.start_editing(window, cx)
                        }))
                })
                .test_support()
                .into_any_element()
        };
        let face = div()
            .id("timer-face")
            .relative()
            .flex_none()
            .size(RING)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(clock::ring(t.progress(now), color, theme.muted))
            .child(
                div()
                    .id("timer-status")
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .aria_label(status.clone())
                    .child(status)
                    .test_support(),
            )
            .child(time);
        // 終わったら、輪の外側が 3 回ふくらんで消える
        clock::glow(
            face,
            ("timer-flash", seen),
            color,
            flashing && !reduce_motion,
        )
    }

    fn render_body(&self, now: i64, reduce_motion: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.state.read(cx).timer.clone();
        let running = t.running();
        let countdown = t.mode == Mode::Countdown;
        // 書き直している間も出したままにする。出さないときも場所は空けておき、
        // 時間とボタンの位置を動かさない（押そうとしたボタンがずれない）
        let presets = (countdown && !running).then(|| {
            let ready = t.run() == Run::Ready;
            row().id("timer-presets").gap_1().justify_center().children(
                PRESETS.iter().enumerate().map(|(i, &secs)| {
                    crate::render::chip(
                        ("timer-preset", i),
                        timer::length_label(secs),
                        ready && t.length == secs,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.act(Op::SetLength(secs), cx)))
                    .test_support()
                }),
            )
        });
        let slot = || div().w(px(40.)).flex().justify_center();
        let tool = |id: &'static str, icon: &str, tip: &'static str| {
            button(id).icon(app_icon(icon)).tooltip(tip).ghost()
        };
        let can_reset = t.run() != Run::Ready;
        let controls = row()
            .gap_3()
            .items_center()
            .child(
                slot().child(feel::gloss_when(
                    can_reset,
                    tool("timer-reset", "rotate-ccw", "リセット")
                        .disabled(!can_reset)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_editing(window, cx);
                            this.act(Op::Reset, cx)
                        })),
                )),
            )
            .child(feel::gloss_round(
                button("timer-toggle")
                    .icon(app_icon(if running { "pause" } else { "play" }))
                    .accessibility_label(if running { "一時停止" } else { "開始" })
                    .tooltip(if running {
                        "一時停止（Space）"
                    } else {
                        "開始（Space）"
                    })
                    .large()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.cancel_editing(window, cx);
                        this.state.update(cx, |s, cx| s.toggle(cx))
                    })),
            ))
            .child(slot().when(countdown, |d| {
                d.child(feel::gloss(tool("timer-add", "plus", "1 分足す").on_click(
                    cx.listener(|this, _, window, cx| {
                        this.cancel_editing(window, cx);
                        this.act(Op::Add(ADD_SECS), cx)
                    }),
                )))
            }));
        col()
            .key_context("TimerFace")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_3()
            .p_3()
            .child(self.render_face(now, reduce_motion, cx))
            .child(
                div()
                    .h(PRESETS_HEIGHT)
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(presets),
            )
            .child(controls)
            .into_any_element()
    }
}

impl BarHold for TimerView {
    fn bar_pressed(&mut self) -> &mut bool {
        &mut self.bar_pressed
    }
}

impl Render for TimerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = malakt_core::now_ms();
        let reduce_motion = cx.reduce_motion();
        let (error, flashing) = {
            let s = self.state.read(cx);
            (
                s.error.clone(),
                s.flashed.is_some_and(|at| at.elapsed() < FLASH),
            )
        };
        // 光っている間は描き続け、終わったら止める
        if flashing {
            window.request_animation_frame();
        }
        let body = self.render_body(now, reduce_motion, cx);
        let bar = self
            .render_bar(window, cx)
            .right_menu(|menu, window, cx| crate::render::window_menu(menu, None, window, cx));
        div()
            .id("timer")
            .key_context("TimerWindow")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &ToggleTimer, _, cx| {
                this.state.update(cx, |s, cx| s.toggle(cx))
            }))
            .on_action(cx.listener(|this, _: &CloseTimerEdit, window, cx| {
                if this.editing {
                    this.cancel_editing(window, cx);
                } else if this.state.read(cx).error.is_some() {
                    this.state.update(cx, |s, cx| {
                        s.error = None;
                        cx.notify();
                    });
                } else {
                    cx.propagate();
                }
            }))
            .on_action(|_: &crate::shortcuts::ToggleFullscreen, window, _| {
                window.toggle_fullscreen()
            })
            .on_action(|_: &crate::shortcuts::ShowKeys, window, cx| {
                crate::keys_help::toggle(window, cx)
            })
            // ここでもマウスの操作を見張る（自動でロックするまでの時間）
            .child(db::input_watcher())
            .child(bar)
            .children(error.map(|error| {
                clock::error_row("timer", error, cx, |this, cx| {
                    this.state.update(cx, |s, cx| {
                        s.error = None;
                        cx.notify();
                    })
                })
            }))
            .child(body)
            .children(crate::keys_help::overlay(window, cx))
            .test_support()
    }
}
