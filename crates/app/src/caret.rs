//! 入力欄の ↑↓ を、OS のふつうの動きに合わせる。
//!
//! いちばん上の行（1 行だけの欄ではその行）で ↑ を押すと文の先頭へ、いちばん下の行で ↓ を
//! 押すと文の末尾へ動く。Shift を押していれば、そこまで選ぶ。gpui の入力欄はこのとき何も
//! しないので、キーを押す前と後でカーソルが動かなかったときだけ、ここで先頭・末尾へ動かす。
//! 折り返した行も、入力欄が実際に動かしたかどうかで見るので、見た目の行のとおりになる。
//!
//! アプリの中で作るすべての入力欄（[`InputState`] と [`TextareaState`]）に、作ったときに付く。
//! ↑↓ をほかの用途に使ったとき（シェルの入力行の履歴、ペインのリサイズ）は、[`cancel`] を呼ぶ。

use std::ops::Range;

use gpui_kit::component::input::{InputState, MoveDown, MoveUp, TextareaState};
use gpui_kit::*;

use gpui_kit::base::actions::{SelectDown, SelectUp};

/// 入力欄。1 行の欄と、複数行の欄。
#[derive(Clone)]
enum Field {
    Line(WeakEntity<InputState>),
    Area(WeakEntity<TextareaState>),
}

/// 同じ処理を、1 行の欄と複数行の欄のどちらにも書く。
macro_rules! on_field {
    ($field:expr, $state:ident => $body:expr) => {
        match $field {
            Field::Line(weak) => weak.upgrade().map(|$state| $body),
            Field::Area(weak) => weak.upgrade().map(|$state| $body),
        }
    };
}

impl Field {
    fn alive(&self) -> bool {
        on_field!(self, _state => ()).is_some()
    }

    fn focused(&self, window: &Window, cx: &App) -> bool {
        on_field!(self, state => state.read(cx).focus_handle(cx).is_focused(window))
            .unwrap_or(false)
    }

    fn snapshot(&self, cx: &App) -> Option<Caret> {
        on_field!(self, state => {
            let state = state.read(cx);
            Caret {
                range: state.selected_range(),
                reversed: state.is_selection_reversed(),
                text: state.value(),
            }
        })
    }

    fn select(&self, range: Range<usize>, reversed: bool, cx: &mut App) {
        on_field!(self, state => state.update(cx, |state, cx| {
            state.set_selected_range_with_direction(range, reversed, cx);
            cx.notify();
        }));
    }
}

/// キーを押す前の、カーソルと選んでいる範囲と文字。
#[derive(Clone, PartialEq)]
struct Caret {
    range: Range<usize>,
    reversed: bool,
    text: SharedString,
}

impl Caret {
    /// 選び始めた側。選んでいなければカーソルの位置。
    fn anchor(&self) -> usize {
        if self.reversed {
            self.range.end
        } else {
            self.range.start
        }
    }
}

/// どちらへ動かすか。
#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    Up,
    Down,
    SelectUp,
    SelectDown,
}

impl Step {
    fn of(action: &dyn Action) -> Option<Self> {
        let any = action.as_any();
        if any.is::<MoveUp>() {
            Some(Step::Up)
        } else if any.is::<MoveDown>() {
            Some(Step::Down)
        } else if any.is::<SelectUp>() {
            Some(Step::SelectUp)
        } else if any.is::<SelectDown>() {
            Some(Step::SelectDown)
        } else {
            None
        }
    }

    fn of_key(key: &Keystroke) -> Option<Self> {
        let m = &key.modifiers;
        if m.control || m.alt || m.platform || m.function {
            return None;
        }
        match (key.key.as_str(), m.shift) {
            ("up", false) => Some(Step::Up),
            ("down", false) => Some(Step::Down),
            ("up", true) => Some(Step::SelectUp),
            ("down", true) => Some(Step::SelectDown),
            _ => None,
        }
    }

    /// 動かなかったときに選ぶ範囲と、カーソルが範囲の先頭にあるか。
    fn target(self, before: &Caret, len: usize) -> (Range<usize>, bool) {
        match self {
            Step::Up => (0..0, false),
            Step::Down => (len..len, false),
            Step::SelectUp => (0..before.anchor(), true),
            Step::SelectDown => (before.anchor()..len, false),
        }
    }
}

#[derive(Default)]
struct Fields {
    all: Vec<Field>,
    /// 押したキーを処理する前の、フォーカスがある入力欄の様子。
    pending: Option<(Field, Caret)>,
}

impl Global for Fields {}

/// アプリの入力欄に付ける。起動のときに 1 回だけ呼ぶ。
pub fn init(cx: &mut App) {
    cx.set_global(Fields::default());
    cx.observe_new(|_: &mut InputState, _, cx: &mut Context<InputState>| {
        add(Field::Line(cx.weak_entity()), cx);
    })
    .detach();
    cx.observe_new(
        |_: &mut TextareaState, _, cx: &mut Context<TextareaState>| {
            add(Field::Area(cx.weak_entity()), cx);
        },
    )
    .detach();
    cx.intercept_keystrokes(|_, window, cx| {
        let focused = cx
            .global::<Fields>()
            .all
            .iter()
            .find(|field| field.focused(window, cx))
            .cloned();
        let pending = focused.and_then(|field| field.snapshot(cx).map(|caret| (field, caret)));
        cx.global_mut::<Fields>().pending = pending;
    })
    .detach();
    cx.observe_keystrokes(|event, window, cx| {
        let Some((field, before)) = cx.global_mut::<Fields>().pending.take() else {
            return;
        };
        // 1 行の欄は ↑↓ を受けずに外へ回すので、そのときはキーで見る
        let step = match event.action.as_deref() {
            Some(action) => Step::of(action),
            None => Step::of_key(&event.keystroke),
        };
        let Some(step) = step else {
            return;
        };
        // 入力欄が自分で動かした（または、ほかの欄へ移った）なら、そのまま
        if !field.focused(window, cx) || field.snapshot(cx).as_ref() != Some(&before) {
            return;
        }
        let (range, reversed) = step.target(&before, before.text.len());
        field.select(range, reversed, cx);
    })
    .detach();
}

fn add(field: Field, cx: &mut App) {
    if cx.has_global::<Fields>() {
        let fields = cx.global_mut::<Fields>();
        fields.all.retain(Field::alive);
        fields.all.push(field);
    }
}

/// 今押したキーを、ほかの用途に使った。入力欄のカーソルは先頭・末尾へ動かさない。
pub fn cancel(cx: &mut App) {
    if cx.has_global::<Fields>() {
        cx.global_mut::<Fields>().pending = None;
    }
}
