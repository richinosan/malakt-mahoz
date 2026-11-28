//! 開始日と期限を選ぶカレンダー。値を押すとその場にパネルが開く。
//!
//! 上から、日付と時刻の自由入力（打つと下に解釈を出す）、よく使う日のチップ、月のカレンダー、
//! 時刻のチップの順に並ぶ。どれを使っても同じ値を書き換え、カレンダーの日やチップは押すとすぐ保存する
//! （開いたまま、続けて時刻を選べる）。選んでいる日・時刻をもう 1 度押すと閉じる。
//! 打った文字は Enter と、欄を離れたときに保存する。Esc と外を押すと閉じる。
//!
//! カレンダーには、祝日（日曜と同じ赤）・土曜（青）・今日（輪）・選んでいる日（塗り）・前後の月（薄く）を出し、
//! ワークスペースの基準日に下線、開始日〜期限の間に帯を引く。

use std::cell::Cell;
use std::rc::Rc;

use chrono::{Datelike, Duration, NaiveDate};
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::calendar::{self, Hint};
use malakt_core::{view, when, Id};

use crate::feel::{self, Lit, Sunk};
use crate::press::button;
use crate::render::{app_icon, col, px, row};
use crate::state::{MainView, When};

actions!(
    date_pick,
    [
        DayLeft,
        DayRight,
        DayUp,
        DayDown,
        WeekStart,
        WeekEnd,
        MonthPrev,
        MonthNext,
        YearPrev,
        YearNext,
        PickFocused
    ]
);

/// カレンダーの中のキー。一覧の ↑↓ などより先に受ける。
pub fn bind(cx: &mut App) {
    let at = Some("DateGrid");
    cx.bind_keys([
        KeyBinding::new("left", DayLeft, at),
        KeyBinding::new("right", DayRight, at),
        KeyBinding::new("up", DayUp, at),
        KeyBinding::new("down", DayDown, at),
        KeyBinding::new("home", WeekStart, at),
        KeyBinding::new("end", WeekEnd, at),
        KeyBinding::new("pageup", MonthPrev, at),
        KeyBinding::new("pagedown", MonthNext, at),
        KeyBinding::new("shift-pageup", YearPrev, at),
        KeyBinding::new("shift-pagedown", YearNext, at),
        KeyBinding::new("enter", PickFocused, at),
        KeyBinding::new("space", PickFocused, at),
    ]);
}

/// パネルの幅（ベースの日付のポップオーバーと同じ）。
const PANEL_WIDTH: f32 = 340.;

/// 日曜・祝日と土曜の文字の色（ベースの `--calendar-sunday` / `--calendar-saturday`）。
fn sunday_color(cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() {
        rgb(0xf58f9c).into()
    } else {
        rgb(0xbe3348).into()
    }
}

fn saturday_color(cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() {
        rgb(0x8cc6f0).into()
    } else {
        rgb(0x2b67a3).into()
    }
}

/// どの日付を選んでいるか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateTarget {
    /// タスクの開始日か期限。`list` なら一覧のセル、そうでなければ詳細
    Task { id: Id, which: When, list: bool },
    /// 一覧の追加行の期限
    Quick,
    /// まとめて変える期限（ダイアログの中にそのまま出す）
    Bulk,
    /// ワークスペースの設定の、日付を指定した期限
    DefaultDue,
    /// ワークスペースの基準日（時刻はない）
    BaseDate,
}

impl DateTarget {
    /// その場に開くパネルか（まとめて変えるダイアログの中のものは、ダイアログと一緒に閉じる）。
    pub fn popover(&self) -> bool {
        *self != DateTarget::Bulk
    }

    /// 時刻も選べるか。
    fn timed(&self) -> bool {
        *self != DateTarget::BaseDate
    }

    /// 要素の名前（テストで押す所）。
    fn key(&self) -> &'static str {
        match self {
            DateTarget::Task {
                which: When::Start, ..
            } => "start",
            DateTarget::Task { .. } | DateTarget::Quick | DateTarget::Bulk => "due",
            DateTarget::DefaultDue => "def-due",
            DateTarget::BaseDate => "ws-date",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            DateTarget::Task {
                which: When::Start, ..
            } => "開始日",
            DateTarget::BaseDate => "基準日",
            _ => "期限",
        }
    }
}

/// 開いているカレンダー。
pub struct DatePick {
    pub target: DateTarget,
    /// 出している月
    pub month: (i32, u32),
    /// キーで動かす日
    pub focused: NaiveDate,
    /// 月を選ぶ一覧を開いている
    pub months: bool,
    /// 値のボタンの場所。外を押したかを見るときに、値のボタンは外に入れない（押すと閉じる）
    pub anchor: Rc<Cell<Bounds<Pixels>>>,
    /// 開いた回数。出てくる動きを毎回はじめから描く
    pub opens: usize,
    /// キーで日を動かしている（動かしている日に輪を出す）
    pub keyed: bool,
    /// 入力欄に入れた値。欄の文字がこれと違えば書きかけ
    filled: Option<String>,
}

impl MainView {
    /// カレンダーの入力欄（日付と時刻）の、Enter と欄を離れたときの動き。`new` で 1 度だけ呼ぶ。
    pub(crate) fn subscribe_date_pick(
        inputs: [(&Entity<InputState>, bool); 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        inputs
            .into_iter()
            .map(|(input, time)| {
                cx.subscribe_in(
                    input,
                    window,
                    move |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::PressEnter { .. } => this.date_text_enter(time, window, cx),
                        InputEvent::Blur => {
                            if this.date_pick.is_some() {
                                this.apply_date_text(time, true, window, cx);
                            }
                        }
                        InputEvent::Change => {
                            if !time {
                                this.follow_date_text(cx);
                            }
                            cx.notify();
                        }
                        _ => {}
                    },
                )
            })
            .collect()
    }

    /// いまの値（`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`）。
    pub(crate) fn date_value(&self, target: &DateTarget, cx: &App) -> Option<String> {
        match target {
            DateTarget::Task { id, which, .. } => {
                let task = self
                    .detail
                    .as_ref()
                    .map(|d| &d.task)
                    .filter(|t| &t.id == id)
                    .or_else(|| self.tasks.iter().find(|t| &t.id == id))?;
                match which {
                    When::Start => task.start.clone(),
                    When::Due => task.due.clone(),
                }
            }
            DateTarget::Quick => self.quick.due.clone(),
            DateTarget::Bulk => self.bulk_due.clone(),
            DateTarget::DefaultDue => {
                let date = self.inputs.def_date.read(cx).value().trim().to_owned();
                let date = when::resolve(&date, &view::now(), None);
                malakt_core::is_valid_date(&date).then(|| {
                    let time = self.inputs.def_time.read(cx).value().trim().to_owned();
                    if malakt_core::model::is_valid_time(&time) {
                        format!("{date} {time}")
                    } else {
                        date
                    }
                })
            }
            DateTarget::BaseDate => {
                let date = self.inputs.ws_date.read(cx).value().trim().to_owned();
                let date = when::resolve(&date, &view::now(), None);
                malakt_core::is_valid_date(&date).then_some(date)
            }
        }
    }

    /// 値を変える。タスクならすぐ保存する。保存できなかったときは `false`（エラーは画面に出る）。
    fn set_date_value(
        &mut self,
        target: &DateTarget,
        value: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.date_value(target, cx) == value {
            return true;
        }
        match target {
            DateTarget::Task { id, which, .. } => {
                let which = *which;
                self.error = None;
                self.edit_task_by_id(id, cx, |d| match which {
                    When::Start => d.start = value,
                    When::Due => d.due = value,
                });
                if self.error.is_some() {
                    return false;
                }
                self.reload(cx);
            }
            DateTarget::Quick => {
                self.quick.due = value;
                self.quick_touched = true;
            }
            DateTarget::Bulk => self.bulk_due = value,
            DateTarget::DefaultDue => {
                let date = value.as_deref().map(malakt_core::date_of).unwrap_or("");
                let time = value
                    .as_deref()
                    .and_then(malakt_core::time_of)
                    .unwrap_or("");
                let (date, time) = (date.to_owned(), time.to_owned());
                self.inputs
                    .def_date
                    .update(cx, |s, cx| s.set_value(date, window, cx));
                self.inputs
                    .def_time
                    .update(cx, |s, cx| s.set_value(time, window, cx));
            }
            DateTarget::BaseDate => {
                let date = value.unwrap_or_default();
                self.inputs
                    .ws_date
                    .update(cx, |s, cx| s.set_value(date, window, cx));
            }
        }
        cx.notify();
        true
    }

    /// 日付の入力欄に出す形（2026/11/30）。
    fn date_text_of(value: Option<&str>) -> String {
        value
            .and_then(calendar::parse_date)
            .map(calendar::slash)
            .unwrap_or_default()
    }

    /// 入力欄を、いまの値に合わせる。
    fn fill_date_texts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = &self.date_pick else { return };
        let value = self.date_value(&pick.target, cx);
        if let Some(pick) = &mut self.date_pick {
            pick.filled = value.clone();
        }
        let date = Self::date_text_of(value.as_deref());
        let time = value
            .as_deref()
            .and_then(malakt_core::time_of)
            .unwrap_or("")
            .to_owned();
        self.inputs
            .pick_date
            .update(cx, |s, cx| s.set_value(date, window, cx));
        self.inputs
            .pick_time
            .update(cx, |s, cx| s.set_value(time, window, cx));
    }

    /// AI やほかのウィンドウが値を変えたら、書きかけでない欄を合わせる。
    pub(crate) fn sync_date_texts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = &self.date_pick else { return };
        if self.date_value(&pick.target, cx) == pick.filled
            || self.date_text_dirty(false, cx)
            || self.date_text_dirty(true, cx)
        {
            return;
        }
        self.fill_date_texts(window, cx);
    }

    /// 値を押した。開いていれば閉じ、閉じていれば開く。
    pub(crate) fn toggle_date_pick(
        &mut self,
        target: DateTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.date_pick.as_ref().is_some_and(|p| p.target == target) {
            self.close_date_pick(window, cx);
            return;
        }
        self.open_date_pick(target, window, cx);
    }

    /// カレンダーを開く。ほかのカレンダーが開いていれば、書きかけを保存して閉じてから。
    pub(crate) fn open_date_pick(
        &mut self,
        target: DateTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.date_pick.is_some() {
            self.close_date_pick(window, cx);
        }
        let today = calendar::parse_date(&self.today).unwrap_or_default();
        let shown = self
            .date_value(&target, cx)
            .as_deref()
            .and_then(calendar::parse_date)
            .unwrap_or(today);
        self.date_opens += 1;
        let popover = target.popover();
        self.date_pick = Some(DatePick {
            target,
            month: (shown.year(), shown.month()),
            focused: shown,
            months: false,
            anchor: Rc::default(),
            opens: self.date_opens,
            keyed: false,
            filled: None,
        });
        self.fill_date_texts(window, cx);
        if popover {
            // 開いた直後は日付の欄に書ける。値のボタンからフォーカスが戻るのに負けないよう、後で移す
            let input = self.inputs.pick_date.clone();
            cx.defer_in(window, move |_, window, cx| {
                input.update(cx, |s, cx| s.focus(window, cx));
            });
        }
        cx.notify();
    }

    /// 書きかけの文字を保存して閉じる。
    pub(crate) fn close_date_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.date_pick.is_none() {
            return;
        }
        self.apply_date_text(false, true, window, cx);
        self.apply_date_text(true, true, window, cx);
        self.date_pick = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// 閉じる（Esc）。選んだものはもう保存してあるので、書きかけの文字だけを捨てる。
    pub(crate) fn dismiss_date_pick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.date_pick.as_ref().map(|p| p.target.clone()) else {
            return false;
        };
        // 置いてあった画面（ダイアログ・詳細）がもうないなら、閉じたことにして Esc を次へ回す
        let shown = match &target {
            DateTarget::Task {
                id, list: false, ..
            } => self.detail.as_ref().is_some_and(|d| &d.task.id == id),
            DateTarget::DefaultDue | DateTarget::BaseDate => {
                matches!(self.modal, crate::state::Modal::WorkspaceSettings(_))
            }
            _ => true,
        };
        if !shown {
            self.date_pick = None;
            return false;
        }
        if !target.popover() {
            return false;
        }
        self.date_pick = None;
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    /// 入力欄の文字が、いまの値と違う（書きかけ）か。
    fn date_text_dirty(&self, time: bool, cx: &App) -> bool {
        let Some(pick) = &self.date_pick else {
            return false;
        };
        let value = pick.filled.clone();
        if time {
            let text = self.inputs.pick_time.read(cx).value().trim().to_owned();
            text != value
                .as_deref()
                .and_then(malakt_core::time_of)
                .unwrap_or("")
        } else {
            let text = self.inputs.pick_date.read(cx).value().trim().to_owned();
            text != Self::date_text_of(value.as_deref())
        }
    }

    /// 入力欄の文字を値にする。読めなければ `false`（値は変えない）。
    /// `revert` なら、読めなかった文字を捨てていまの値に戻す（欄を離れたとき）。
    fn apply_date_text(
        &mut self,
        time: bool,
        revert: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.date_pick.as_ref().map(|p| p.target.clone()) else {
            return false;
        };
        if !self.date_text_dirty(time, cx) {
            return true;
        }
        let current = self.date_value(&target, cx);
        let now = view::now();
        let next = if time {
            let text = self.inputs.pick_time.read(cx).value().to_string();
            let date = current.as_deref().map(malakt_core::date_of);
            match (date, calendar::time_hint(&text, &now, date.is_some())) {
                (Some(date), (Some(time), _)) => Some(Some(format!("{date} {time}"))),
                (Some(date), (None, Hint::Empty(_))) => Some(Some(date.to_owned())),
                _ => None,
            }
        } else {
            let text = self.inputs.pick_date.read(cx).value().to_string();
            match calendar::date_hint(&text, &now, current.is_some()) {
                (_, Hint::Empty(_)) => Some(None),
                (Some(value), _) => Some(Some(if malakt_core::time_of(&value).is_some() {
                    value
                } else {
                    view::with_time_of(&value, current.as_deref())
                })),
                (None, _) => None,
            }
        };
        let next = next.map(|v| {
            if target.timed() {
                v
            } else {
                v.map(|v| malakt_core::date_of(&v).to_owned())
            }
        });
        let ok = match next {
            Some(value) => self.set_date_value(&target, value, window, cx),
            None => false,
        };
        if ok || revert {
            self.fill_date_texts(window, cx);
        }
        let shown = self
            .date_value(&target, cx)
            .as_deref()
            .and_then(calendar::parse_date);
        if let (Some(date), Some(pick)) = (shown, &mut self.date_pick) {
            pick.focused = date;
            pick.month = (date.year(), date.month());
        }
        ok
    }

    /// 入力欄で Enter。読めたら閉じる（まとめて変えるダイアログでは、変更する）。
    fn date_text_enter(&mut self, time: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.date_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        if !self.apply_date_text(time, false, window, cx) {
            cx.notify();
            return;
        }
        if target.popover() {
            self.close_date_pick(window, cx);
        } else {
            self.submit_bulk_due(false, cx);
        }
    }

    /// 打っている日付のある月をカレンダーに出す。
    fn follow_date_text(&mut self, cx: &mut Context<Self>) {
        let text = self.inputs.pick_date.read(cx).value().to_string();
        if let (Some(value), _) = calendar::date_hint(&text, &view::now(), false) {
            if let (Some(date), Some(pick)) = (calendar::parse_date(&value), &mut self.date_pick) {
                pick.month = (date.year(), date.month());
            }
        }
    }

    /// カレンダーの日かチップを押した。選んでいる日をもう 1 度押したら閉じる。
    pub(crate) fn pick_day(
        &mut self,
        date: NaiveDate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.date_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        let current = self.date_value(&target, cx);
        let ymd = calendar::ymd(date);
        if target.popover() && current.as_deref().map(malakt_core::date_of) == Some(ymd.as_str()) {
            self.close_date_pick(window, cx);
            return;
        }
        let value = if target.timed() {
            view::with_time_of(&ymd, current.as_deref())
        } else {
            ymd
        };
        if let Some(pick) = &mut self.date_pick {
            pick.focused = date;
            pick.month = (date.year(), date.month());
        }
        self.set_date_value(&target, Some(value), window, cx);
        self.fill_date_texts(window, cx);
    }

    /// 時刻のチップを押した。`None` は終日。選んでいる時刻をもう 1 度押したら閉じる。
    fn pick_time(&mut self, time: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.date_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        let Some(current) = self.date_value(&target, cx) else {
            return;
        };
        if target.popover() && malakt_core::time_of(&current) == time {
            self.close_date_pick(window, cx);
            return;
        }
        let date = malakt_core::date_of(&current);
        let value = match time {
            Some(time) => format!("{date} {time}"),
            None => date.to_owned(),
        };
        self.set_date_value(&target, Some(value), window, cx);
        self.fill_date_texts(window, cx);
    }

    /// 「クリア」。日付をなくして閉じる。
    fn clear_date_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.date_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        if self.set_date_value(&target, None, window, cx) {
            self.date_pick = None;
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// 出す月を動かす。
    fn move_month(&mut self, step: i32, cx: &mut Context<Self>) {
        if let Some(pick) = &mut self.date_pick {
            pick.month = calendar::shift_month(pick.month.0, pick.month.1, step);
            pick.months = false;
            cx.notify();
        }
    }

    /// キーで日を動かす。見えていない月なら、その月を出す。
    fn move_focus(&mut self, f: impl FnOnce(NaiveDate) -> NaiveDate, cx: &mut Context<Self>) {
        if let Some(pick) = &mut self.date_pick {
            let date = f(pick.focused);
            pick.focused = date;
            pick.keyed = true;
            pick.month = (date.year(), date.month());
            cx.notify();
        }
    }

    /// 基準日（ワークスペースの日付）。チップと下線に使う。
    fn date_base(&self, target: &DateTarget, cx: &App) -> Option<NaiveDate> {
        let workspace = match target {
            DateTarget::Task { id, .. } => self
                .detail
                .as_ref()
                .map(|d| &d.task)
                .filter(|t| &t.id == id)
                .or_else(|| self.tasks.iter().find(|t| &t.id == id))
                .and_then(|t| t.workspace_id.clone()),
            DateTarget::Quick => self.current_workspace().map(str::to_owned),
            DateTarget::DefaultDue => {
                return self
                    .date_value(&DateTarget::BaseDate, cx)
                    .as_deref()
                    .and_then(calendar::parse_date)
            }
            DateTarget::Bulk | DateTarget::BaseDate => None,
        }?;
        self.workspaces
            .iter()
            .find(|w| w.id == workspace)
            .and_then(|w| w.date.as_deref())
            .and_then(calendar::parse_date)
    }

    /// 期間のもう一方の端（期限なら開始日、開始日なら期限）と、どちらの端か（`true` は始まり）。
    fn date_other_end(&self, target: &DateTarget, cx: &App) -> Option<(NaiveDate, bool)> {
        let DateTarget::Task { id, which, list } = target else {
            return None;
        };
        let other = match which {
            When::Start => When::Due,
            When::Due => When::Start,
        };
        let value = self.date_value(
            &DateTarget::Task {
                id: id.clone(),
                which: other,
                list: *list,
            },
            cx,
        )?;
        Some((calendar::parse_date(&value)?, other == When::Start))
    }

    /// 値のボタンに出す文字（2026/11/30 14:00。なければ「未設定」）。
    pub(crate) fn date_shown(&self, target: &DateTarget, cx: &App) -> Div {
        match self.date_value(target, cx) {
            Some(value) => div()
                .text_sm()
                .child(view::format_date(&value, &self.today)),
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("未設定"),
        }
    }

    /// 値のボタン。押すとその場にカレンダーが開く。開いている間は、ボタンの下にパネルを出す。
    pub(crate) fn date_trigger(
        &self,
        id: impl Into<ElementId>,
        target: DateTarget,
        shown: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.date_pick.as_ref().filter(|p| p.target == target);
        let anchor = open.map(|p| p.anchor.clone());
        let label = target.label();
        let theme = cx.theme().clone();
        let trigger = button(id)
            .ghost()
            .small()
            .h_auto()
            .min_h_6()
            .max_w_full()
            .py_0p5()
            .justify_start()
            .tooltip(format!("{label}を変更"))
            .when(open.is_some(), |b| b.bg(theme.accent.opacity(0.4)))
            .child(
                row()
                    .id("value")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(shown.min_w_0().truncate())
                    .test_support(),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.toggle_date_pick(target.clone(), window, cx)
            }));
        div()
            .relative()
            .flex()
            .max_w_full()
            .child(trigger)
            .when_some(anchor, |d, anchor| {
                d.child(
                    canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                        .absolute()
                        .inset_0(),
                )
                .child(
                    div().absolute().top_full().left_0().child(
                        deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.))
                                .child(div().pt_1().child(self.date_popover(cx))),
                        )
                        .with_priority(2),
                    ),
                )
            })
    }

    /// その場に開くパネル。外を押すと、書きかけを保存して閉じる。
    fn date_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pick) = &self.date_pick else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let anchor = pick.anchor.clone();
        let opens = pick.opens;
        let motion = self.motion;
        let panel = col()
            .id("date-pick")
            .occlude()
            .w(px(PANEL_WIDTH))
            .p_2()
            .rounded(feel::lg())
            .border_1()
            .border_color(feel::rim(cx))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_lg()
            .on_mouse_down_out(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // 値のボタンを押したときは、ボタンのほうで閉じる
                    if anchor.get().contains(&event.position) {
                        return;
                    }
                    this.close_date_pick(window, cx);
                }),
            )
            .child(self.date_panel(cx))
            .child(
                row()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(feel::gloss(
                        button("date-clear")
                            .label("クリア")
                            .outline()
                            .small()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.clear_date_pick(window, cx)),
                            ),
                    )),
            )
            .test_support();
        if !motion {
            return panel.into_any_element();
        }
        // すっと下りて出る
        panel
            .with_animation(
                ("date-pick-in", opens),
                Animation::new(std::time::Duration::from_millis(160)).with_easing(ease_out_quint()),
                |d, t| d.opacity(t).mt(px(-6.) * (1. - t)),
            )
            .into_any_element()
    }

    /// まとめて変えるダイアログに、そのまま置くカレンダー。
    pub(crate) fn date_inline(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.date_pick.is_none() {
            return div().into_any_element();
        }
        self.date_panel(cx)
    }

    /// パネルの中身。入力欄・よく使う日・カレンダー・時刻のチップ。
    fn date_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pick) = &self.date_pick else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let target = pick.target.clone();
        let label = target.label();
        let now = view::now();
        let today = calendar::parse_date(&self.today).unwrap_or_default();
        let value = self.date_value(&target, cx);
        let selected = value.as_deref().and_then(calendar::parse_date);
        let date_text = self.inputs.pick_date.read(cx).value().to_string();
        let (typed, date_hint) = calendar::date_hint(&date_text, &now, value.is_some());
        let preview = typed
            .as_deref()
            .and_then(calendar::parse_date)
            .filter(|d| Some(*d) != selected);
        let time_text = self.inputs.pick_time.read(cx).value().to_string();
        let (_, time_hint) = calendar::time_hint(&time_text, &now, selected.is_some());
        let base = self.date_base(&target, cx);
        let other = self.date_other_end(&target, cx);
        let key = target.key();

        let field = |caption: &'static str, input: Div, hint: &Hint| {
            col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.muted_foreground)
                        .child(caption),
                )
                .child(input)
                .child(hint_line(hint, &theme))
        };
        let inputs = row()
            .gap_2()
            .items_start()
            .child(
                field(
                    "日付",
                    div().child(
                        Input::new(&self.inputs.pick_date)
                            .id("pick-date-input")
                            .aria_label(format!("{label}の日付"))
                            .small(),
                    ),
                    &date_hint,
                )
                .flex_1()
                .min_w_0(),
            )
            .when(target.timed(), |d| {
                d.child(
                    field(
                        "時刻（任意）",
                        div().when(selected.is_none(), |d| d.opacity(0.5)).child(
                            Input::new(&self.inputs.pick_time)
                                .id("pick-time-input")
                                .aria_label(format!("{label}の時刻（任意）"))
                                .disabled(selected.is_none())
                                .small(),
                        ),
                        &time_hint,
                    )
                    .w(px(128.))
                    .flex_none(),
                )
            });

        let mut chips = row().gap_1p5().flex_wrap();
        for (ix, (text, date)) in calendar::quick_dates(today, base).into_iter().enumerate() {
            chips = chips.child(
                pill(("date-chip", ix), text, Some(date) == selected, false, cx)
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(calendar::with_weekday(date))
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.pick_day(date, window, cx)
                    }))
                    .test_support(),
            );
        }

        let content = col()
            .id(SharedString::from(format!("{key}-pick")))
            .gap_3()
            .child(inputs)
            .child(chips)
            .child(self.date_calendar(pick, selected, preview, base, other, cx))
            .when(target.timed(), |d| {
                d.child(self.time_chips(value.as_deref(), cx))
            })
            .test_support();
        content.into_any_element()
    }

    /// 月のカレンダー（見出し・曜日・6 週・祝日・凡例）。
    fn date_calendar(
        &self,
        pick: &DatePick,
        selected: Option<NaiveDate>,
        preview: Option<NaiveDate>,
        base: Option<NaiveDate>,
        other: Option<(NaiveDate, bool)>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let (sunday, saturday) = (sunday_color(cx), saturday_color(cx));
        let today = calendar::parse_date(&self.today).unwrap_or_default();
        let (year, month) = pick.month;
        let grid_focused = pick.keyed;
        // 帯の両端。もう一方の端と、いま選んでいる日
        let band = match (other, selected) {
            (Some((end, true)), Some(sel)) if end < sel => Some((end, sel)),
            (Some((end, false)), Some(sel)) if sel < end => Some((sel, end)),
            _ => None,
        };

        let nav_button = |id: &'static str, icon: &'static str, tip: &'static str| {
            button(id)
                .icon(app_icon(icon))
                .outline()
                .small()
                .tooltip(tip)
        };
        let header = row()
            .gap_1()
            .child(
                div()
                    .id("date-month")
                    .flex()
                    .items_center()
                    .h_8()
                    .pl_2()
                    .pr_1p5()
                    .rounded(feel::md())
                    .cursor_pointer()
                    .lit(theme.accent.opacity(0.6), cx)
                    .sunk(theme.primary.opacity(0.4), cx)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .child(calendar::month_title(year, month)),
                    )
                    .child(
                        app_icon("chevron-down")
                            .small()
                            .ml_0p5()
                            .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        let scroll = this.month_scroll.clone();
                        if let Some(pick) = &mut this.date_pick {
                            pick.months = !pick.months;
                            if pick.months {
                                let choices = calendar::month_choices(
                                    calendar::parse_date(&this.today).unwrap_or_default(),
                                    pick.month,
                                );
                                if let Some(ix) = choices.iter().position(|m| *m == pick.month) {
                                    scroll.scroll_to_item(ix.saturating_sub(3));
                                }
                            }
                        }
                        cx.notify();
                    }))
                    .test_support(),
            )
            .child(
                row()
                    .ml_auto()
                    .gap_1()
                    .child(feel::gloss(
                        button("date-today")
                            .label("今日")
                            .outline()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(pick) = &mut this.date_pick {
                                    pick.month = (today.year(), today.month());
                                    pick.focused = today;
                                    pick.months = false;
                                }
                                cx.notify();
                            })),
                    ))
                    .child(feel::gloss(
                        nav_button("date-prev", "chevron-left", "前の月")
                            .on_click(cx.listener(|this, _, _, cx| this.move_month(-1, cx))),
                    ))
                    .child(feel::gloss(
                        nav_button("date-next", "chevron-right", "次の月")
                            .on_click(cx.listener(|this, _, _, cx| this.move_month(1, cx))),
                    )),
            );

        let mut weekdays = row();
        for (i, name) in calendar::WEEKDAYS.iter().enumerate() {
            weekdays = weekdays.child(
                div()
                    .flex_1()
                    .py_1()
                    .text_center()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(match i {
                        0 => sunday,
                        6 => saturday,
                        _ => theme.muted_foreground,
                    })
                    .child(*name),
            );
        }

        let mut grid =
            col()
                .id("date-grid")
                .key_context("DateGrid")
                .track_focus(&self.date_grid)
                .on_action(cx.listener(|this, _: &DayLeft, _, cx| {
                    this.move_focus(|d| d - Duration::days(1), cx)
                }))
                .on_action(cx.listener(|this, _: &DayRight, _, cx| {
                    this.move_focus(|d| d + Duration::days(1), cx)
                }))
                .on_action(cx.listener(|this, _: &DayUp, _, cx| {
                    this.move_focus(|d| d - Duration::days(7), cx)
                }))
                .on_action(cx.listener(|this, _: &DayDown, _, cx| {
                    this.move_focus(|d| d + Duration::days(7), cx)
                }))
                .on_action(cx.listener(|this, _: &WeekStart, _, cx| {
                    this.move_focus(|d| d - Duration::days(calendar::weekday(d) as i64), cx)
                }))
                .on_action(cx.listener(|this, _: &WeekEnd, _, cx| {
                    this.move_focus(|d| d + Duration::days(6 - calendar::weekday(d) as i64), cx)
                }))
                .on_action(cx.listener(|this, _: &MonthPrev, _, cx| {
                    this.move_focus(|d| calendar::shift_months(d, -1), cx)
                }))
                .on_action(cx.listener(|this, _: &MonthNext, _, cx| {
                    this.move_focus(|d| calendar::shift_months(d, 1), cx)
                }))
                .on_action(cx.listener(|this, _: &YearPrev, _, cx| {
                    this.move_focus(|d| calendar::shift_months(d, -12), cx)
                }))
                .on_action(cx.listener(|this, _: &YearNext, _, cx| {
                    this.move_focus(|d| calendar::shift_months(d, 12), cx)
                }))
                .on_action(cx.listener(|this, _: &PickFocused, window, cx| {
                    if let Some(date) = this.date_pick.as_ref().map(|p| p.focused) {
                        this.pick_day(date, window, cx);
                    }
                }))
                .child(weekdays);
        let days = calendar::month_grid(year, month);
        for week in days.chunks(7) {
            let mut line = row();
            for &date in week {
                let holiday = calendar::holiday_name(date);
                let weekday = calendar::weekday(date);
                let in_month = date.month() == month && date.year() == year;
                let is_selected = Some(date) == selected;
                let is_other = other.is_some_and(|(o, _)| o == date);
                let in_band = band.is_some_and(|(s, e)| s <= date && date <= e);
                let in_base = base == Some(date);
                let red = weekday == 0 || holiday.is_some();
                let ink = if is_selected {
                    theme.primary_foreground
                } else if red {
                    sunday
                } else if weekday == 6 {
                    saturday
                } else {
                    theme.foreground
                };
                let mut day = div()
                    .id(("date-day", date.num_days_from_ce() as usize))
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h_8()
                    .w_full()
                    .rounded(feel::md())
                    .text_sm()
                    .cursor_pointer()
                    .text_color(ink)
                    .when(!in_month && !is_selected, |d| d.opacity(0.45))
                    .when(is_other || is_selected, |d| d.font_weight(FontWeight::BOLD))
                    .when(is_selected, |d| d.bg(theme.primary))
                    .when(!is_selected, |d| d.lit(theme.foreground.opacity(0.1), cx))
                    .when(date == today, |d| {
                        d.border_2().border_color(if is_selected {
                            theme.primary_foreground.opacity(0.8)
                        } else {
                            theme.foreground.opacity(0.6)
                        })
                    })
                    .when(preview == Some(date), |d| {
                        d.border_1().border_dashed().border_color(theme.primary)
                    })
                    .when(grid_focused && pick.focused == date, |d| {
                        d.border_2().border_color(theme.ring)
                    })
                    .child(date.day().to_string())
                    .when(holiday.is_some(), |d| {
                        d.child(
                            div()
                                .absolute()
                                .top_1()
                                .right(px(6.))
                                .size_1p5()
                                .rounded_full()
                                .bg(if is_selected {
                                    theme.primary_foreground
                                } else {
                                    sunday
                                }),
                        )
                    })
                    .when(in_base, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_2()
                                .right_2()
                                .bottom_1()
                                .h_0p5()
                                .rounded_full()
                                .bg(if is_selected {
                                    theme.primary_foreground
                                } else {
                                    theme.primary
                                }),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.date_grid.focus(window, cx);
                        this.pick_day(date, window, cx)
                    }));
                if let Some(name) = holiday {
                    day = day.tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(name).build(window, cx)
                    });
                }
                let start = band.is_some_and(|(s, _)| s == date) || weekday == 0;
                let end = band.is_some_and(|(_, e)| e == date) || weekday == 6;
                let radius = feel::md();
                let cell = div()
                    .flex_1()
                    .p(gpui_kit::px(1.))
                    .when(in_band, |d| {
                        d.bg(theme.accent.opacity(if is_other { 1. } else { 0.5 }))
                            .when(start, |d| d.rounded_l(radius))
                            .when(end, |d| d.rounded_r(radius))
                    })
                    .when(!in_band && is_other, |d| d.bg(theme.accent).rounded(radius))
                    .child(day.test_support());
                line = line.child(cell);
            }
            grid = grid.child(line);
        }

        let holidays = calendar::month_holidays(year, month);
        let holiday_list = if holidays.is_empty() {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("この月の祝日はありません")
        } else {
            let mut list = row().flex_wrap().gap_x_3().gap_y_0p5().text_xs();
            for (date, name) in holidays {
                list = list.child(
                    row()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .text_color(sunday)
                                .child(format!(
                                    "{}日({})",
                                    date.day(),
                                    calendar::WEEKDAYS[calendar::weekday(date)]
                                )),
                        )
                        .child(name),
                );
            }
            list
        };
        let range_label = other.map(|_| "開始日〜期限");
        let legend = (base.is_some() || range_label.is_some()).then(|| {
            row()
                .flex_wrap()
                .gap_x_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .when(base.is_some(), |d| {
                    d.child(
                        row()
                            .gap_1()
                            .child(div().h_0p5().w_3().rounded_full().bg(theme.primary))
                            .child("基準日"),
                    )
                })
                .when_some(range_label, |d, text| {
                    d.child(
                        row()
                            .gap_1()
                            .child(div().h(px(10.)).w_3().rounded_sm().bg(theme.accent))
                            .child(text),
                    )
                })
        });

        col()
            .relative()
            .child(header)
            .child(grid.mt_1())
            .child(holiday_list.mt_1p5())
            .children(legend.map(|l| l.mt_1()))
            .when(pick.months, |d| d.child(self.month_list(pick, cx)))
    }

    /// 月の見出しを押すと出る、月の一覧。
    fn month_list(&self, pick: &DatePick, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let today = calendar::parse_date(&self.today).unwrap_or_default();
        let mut list = col()
            .id("date-months")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.month_scroll)
            .p_1();
        for (ix, (year, month)) in calendar::month_choices(today, pick.month)
            .into_iter()
            .enumerate()
        {
            let current = (year, month) == pick.month;
            list = list.child(
                div()
                    .id(("date-month-item", ix))
                    .px_2()
                    .py_1()
                    .rounded(feel::md())
                    .text_sm()
                    .cursor_pointer()
                    .when(current, |d| {
                        d.bg(theme.accent)
                            .text_color(theme.accent_foreground)
                            .font_weight(FontWeight::BOLD)
                    })
                    .lit(theme.accent.opacity(0.6), cx)
                    .child(calendar::month_title(year, month))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(pick) = &mut this.date_pick {
                            pick.month = (year, month);
                            pick.months = false;
                        }
                        cx.notify();
                    }))
                    .test_support(),
            );
        }
        div()
            .absolute()
            .top(px(36.))
            .left_0()
            .w(px(180.))
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .occlude()
            .h(px(240.))
            .child(list)
            .vertical_scrollbar(&self.month_scroll)
    }

    /// 時刻のチップ（9:00・12:00・18:00・23:59・終日）。日付がまだなければ押せない。
    fn time_chips(&self, value: Option<&str>, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let has_date = value.is_some();
        let time = value.and_then(malakt_core::time_of);
        let mut line = row()
            .gap_1p5()
            .flex_wrap()
            .pt_2p5()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .mr_0p5()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.muted_foreground)
                    .child("時刻"),
            );
        let chips = calendar::TIME_CHIPS.iter().map(|t| Some(*t)).chain([None]);
        for (ix, chip) in chips.enumerate() {
            let text: SharedString = match chip {
                Some(t) => t.trim_start_matches('0').to_owned().into(),
                None => "終日".into(),
            };
            let active = has_date && time == chip;
            line = line.child(
                pill(("time-chip", ix), text, active, !has_date, cx)
                    .when(has_date, |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.pick_time(chip, window, cx)
                        }))
                    })
                    .test_support(),
            );
        }
        line
    }
}

/// 入力欄の下の、解釈の一行。
fn hint_line(hint: &Hint, theme: &gpui_kit::component::Theme) -> Div {
    let line = row().flex_wrap().gap_x_1().min_h_4().text_xs();
    match hint {
        Hint::Empty(text) => line.text_color(theme.muted_foreground).child(*text),
        Hint::Invalid => line
            .font_weight(FontWeight::BOLD)
            .text_color(theme.danger)
            .child("解釈できません"),
        Hint::Ok(parts) => line
            .text_color(theme.foreground)
            .children(parts.iter().map(|p| div().flex_none().child(p.clone()))),
    }
}

/// 丸いチップ。選んでいるものは金の縁と淡い面、太字。
pub(crate) fn pill(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    active: bool,
    disabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .flex()
        .items_center()
        .min_h_7()
        .px_2()
        .py_1()
        .rounded_full()
        .border_1()
        .text_xs()
        .flex_none()
        .when(disabled, |d| d.opacity(0.5))
        .when(!disabled, |d| d.cursor_pointer())
        .when(active, |d| {
            d.border_color(theme.primary)
                .bg(theme.accent)
                .text_color(theme.accent_foreground)
                .font_weight(FontWeight::BOLD)
        })
        .when(!active, |d| {
            d.border_color(theme.border)
                .bg(theme.background)
                .when(!disabled, |d| d.lit(theme.accent, cx))
        })
        .when(!disabled, |d| d.sunk(theme.primary.opacity(0.4), cx))
        .child(text.into())
}
