//! 定時タスクの一覧。未所属（どのワークスペースにも入れない）と、ワークスペースごと。
//!
//! 「＋」で「新規定時タスク」を足し、その場でタイトルを書く。行を押すと開き、いつ・AI への指示と、
//! 作るタスクの項目（タスクの詳細と同じもの: URL・状態・優先度・担当と役・開始日・期限・通知・進捗・
//! 予想工数・所要時間の基準・カテゴリ・親タスク・ほかのワークスペース・説明・サブタスク・添付）を
//! その場で変えられる。タイトル・説明・予想工数・AI への指示は Enter かほかの場所を押すと保存し
//! （説明と AI への指示は Shift+Enter で改行）、URL・カテゴリ・サブタスクは Enter で足す。Esc で書きかけを捨てて閉じる。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::estimate::{self, Reading};
use malakt_core::repeat::{self, Rule};
use malakt_core::schedule::{due_days_label, Schedule, SchedulePatch, Template};
use malakt_core::timing::{self, Basis, EndBasis, StartBasis};
use malakt_core::view::Priority;
use malakt_core::{alert, Person, Role, Roles, Status, Store, Task, Workspace};

use super::rules::workspace_tree;
use super::{col, px, row, AiSettings, CONTROL_WIDTH};
use crate::feel::{self, Lit};
use crate::render::{app_icon, remove_x};
use crate::repeat_view::{repeat_menu, Pick};

/// 進捗のメニューに出す値。タスクの詳細と同じ。
const PERCENTS: [u8; 5] = [0, 25, 50, 75, 100];
/// 親タスクのメニューに出すタスクの数。
const MAX_PARENTS: usize = 50;
/// 期限のメニューに出す、作った日から何日後か。
const DUE_DAYS: [i64; 6] = [0, 1, 2, 3, 7, 14];
/// 期限の時刻のメニューに出す時。
const DUE_HOURS: [u32; 7] = [9, 10, 12, 13, 15, 17, 18];
/// 開いた行の、項目名の幅。
const LABEL_WIDTH: Pixels = px(88.);

/// 読めたら値、読めなかったら `problems` に理由を足して `None`。
fn take<T>(problems: &mut Vec<String>, what: &str, r: malakt_core::Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            problems.push(format!("{what}を読めませんでした: {e}"));
            None
        }
    }
}

/// 回の日から何日後かで決める日。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Offset {
    Start,
    Due,
}

/// 項目名と値の 1 行。
fn field(label: &'static str, value: impl IntoElement, cx: &App) -> Div {
    row()
        .gap_2()
        .items_start()
        .child(
            div()
                .w(LABEL_WIDTH)
                .flex_none()
                .pt_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().flex().child(value))
}

/// メニューを開く、値を出したボタンの中身。
fn menu_face(icon: &'static str, text: SharedString, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    row()
        .min_w_0()
        .gap_2()
        .child(app_icon(icon).small().text_color(muted))
        .child(div().min_w_0().truncate().child(text))
        .child(app_icon("chevron-down").small().text_color(muted))
}

/// メニューの 1 行を押したときに、定時タスクを `patch` で変える。
fn on_patch(
    settings: WeakEntity<AiSettings>,
    id: String,
    patch: SchedulePatch,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, _, cx| {
        let patch = patch.clone();
        // 設定を閉じたあとなら、反映する先がない
        let _ = settings.update(cx, |this, cx| this.patch_schedule(&id, patch, cx));
    }
}

/// 開いた定時タスクの、雛形の説明・URL・カテゴリ・サブタスクを書く欄。
pub struct TemplateInputs {
    description: Entity<TextareaState>,
    estimate: Entity<InputState>,
    url: Entity<InputState>,
    category: Entity<InputState>,
    subtask: Entity<InputState>,
}

impl TemplateInputs {
    pub fn new(window: &mut Window, cx: &mut Context<AiSettings>) -> Self {
        TemplateInputs {
            description: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .submit_on_enter(true)
                    .auto_grow(2, 8)
                    .placeholder("ワークスペースの設定")
            }),
            estimate: cx.new(|cx| InputState::new(window, cx).placeholder("1日3時間、90分、1.5h")),
            url: cx.new(|cx| InputState::new(window, cx).placeholder("URL を追加")),
            category: cx.new(|cx| InputState::new(window, cx).placeholder("カテゴリを追加")),
            subtask: cx.new(|cx| InputState::new(window, cx).placeholder("サブタスクを追加")),
        }
    }

    /// 書いたものを保存する・足すきっかけ。
    pub fn subscribe(
        &self,
        window: &mut Window,
        cx: &mut Context<AiSettings>,
    ) -> Vec<Subscription> {
        vec![
            cx.subscribe_in(
                &self.description,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(
                        event,
                        InputEvent::PressEnter { shift: false, .. } | InputEvent::Blur
                    ) {
                        this.save_schedule_description(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &self.estimate,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.save_schedule_estimate(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &self.url,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.add_schedule_urls(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &self.category,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.add_schedule_category(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &self.subtask,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.add_schedule_subtask(window, cx);
                    }
                },
            ),
        ]
    }

    /// 開いた定時タスクの値を入れる。足す欄は空にする。
    fn load(&self, s: &Schedule, window: &mut Window, cx: &mut App) {
        let description = s.task.description.clone().unwrap_or_default();
        self.description
            .update(cx, |i, cx| i.set_value(description, window, cx));
        let estimate = s.task.estimate.map(|e| e.label()).unwrap_or_default();
        self.estimate
            .update(cx, |i, cx| i.set_value(estimate, window, cx));
        for input in [&self.url, &self.category, &self.subtask] {
            input.update(cx, |i, cx| i.set_value("", window, cx));
        }
    }
}

/// 雛形の項目を `f` で変えた書き換え。
fn task_patch(s: &Schedule, f: impl FnOnce(&mut Template)) -> SchedulePatch {
    let mut task = s.task.clone();
    f(&mut task);
    SchedulePatch {
        task: Some(task),
        ..SchedulePatch::default()
    }
}

impl AiSettings {
    /// 定時タスクの一覧で見る場所を変える。`None` は未所属。
    pub fn show_schedules(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.schedules_scope != scope {
            self.schedules_scope = scope;
            self.schedule_open = None;
        }
        self.schedules_error = None;
        cx.notify();
    }

    /// データベースを変える。1 回の操作を、元に戻す 1 つの段にする。失敗は呼んだ側に返す。
    fn try_write_schedules<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Store) -> malakt_core::Result<R>,
    ) -> malakt_core::Result<R> {
        crate::db::shared(cx).update(cx, |db, cx| {
            let result = db.store.undo_mark().and_then(|()| f(&mut db.store));
            let marked = db.store.undo_mark();
            cx.notify();
            result.and_then(|value| marked.map(|()| value))
        })
    }

    /// データベースを変える。失敗したら、一覧の下に理由を出す。
    fn write_schedules<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Store) -> malakt_core::Result<R>,
    ) -> Option<R> {
        let result = self.try_write_schedules(cx, f);
        match result {
            Ok(value) => {
                self.schedules_error = None;
                cx.notify();
                Some(value)
            }
            Err(error) => {
                self.schedules_error = Some(error.to_string().into());
                cx.notify();
                None
            }
        }
    }

    fn schedule(&self, id: &str, cx: &App) -> Option<Schedule> {
        crate::db::shared(cx).read(cx).store.schedule(id).ok()
    }

    /// 「新規定時タスク」を足して開き、タイトルを全部選んで書けるようにする。
    pub fn add_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_schedule(cx);
        let scope = self.schedules_scope.clone();
        let now = malakt_core::view::now();
        let Some(made) =
            self.write_schedules(cx, |s| s.create_schedule(scope.as_deref(), "", None, &now))
        else {
            return;
        };
        self.open_schedule_now(&made, window, cx);
        let input = self.schedule_title.clone();
        // メニューやボタンからフォーカスが戻ったあとに移す
        cx.defer_in(window, move |this, window, cx| {
            if this.schedule_open.is_none() {
                return;
            }
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        });
    }

    fn open_schedule_now(&mut self, s: &Schedule, window: &mut Window, cx: &mut Context<Self>) {
        self.schedule_open = Some(s.id.clone());
        let (title, ai) = (s.title.clone(), s.ai.clone());
        self.schedule_title
            .update(cx, |i, cx| i.set_value(title, window, cx));
        self.schedule_ai
            .update(cx, |i, cx| i.set_value(ai, window, cx));
        self.schedule_inputs.load(s, window, cx);
        cx.notify();
    }

    /// 開いている定時タスク。
    fn open_schedule(&self, cx: &App) -> Option<Schedule> {
        self.schedule_open
            .as_deref()
            .and_then(|id| self.schedule(id, cx))
    }

    /// 開いている定時タスクの雛形を `f` で変える。保存できたら true。
    fn edit_open_template(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Template),
    ) -> bool {
        let Some(s) = self.open_schedule(cx) else {
            return false;
        };
        let patch = task_patch(&s, f);
        let now = malakt_core::view::now();
        self.write_schedules(cx, |store| store.update_schedule(&s.id, &patch, &now))
            .is_some()
    }

    /// 書いた説明を保存する。空にすると、ワークスペースの設定を使う。
    pub(super) fn save_schedule_description(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.open_schedule(cx) else {
            return;
        };
        let text = self
            .schedule_inputs
            .description
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let next = (!text.is_empty()).then_some(text);
        if next == s.task.description {
            return;
        }
        self.edit_open_template(cx, |t| t.description = next);
    }

    /// 書いた URL を足す。空白や改行で区切って、いくつも貼れる。足せなかったときは、書いたまま残す。
    fn add_schedule_urls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.schedule_inputs.url.clone();
        let text = input.read(cx).value().to_string();
        let urls: Vec<String> = malakt_core::weblink::split(&text)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if urls.is_empty() {
            return;
        }
        if self.edit_open_template(cx, |t| t.urls.extend(urls)) {
            input.update(cx, |i, cx| i.set_value("", window, cx));
        }
    }

    /// 書いたカテゴリを足す。ワークスペースの設定を使っていたときは、その中身に足す。
    fn add_schedule_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.schedule_inputs.category.clone();
        let text = input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(s) = self.open_schedule(cx) else {
            return;
        };
        let defaults = match crate::db::shared(cx)
            .read(cx)
            .store
            .draft_for(s.workspace_id.as_deref(), "")
        {
            Ok(draft) => draft.categories,
            Err(error) => {
                self.schedules_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let saved = self.edit_open_template(cx, |t| {
            t.categories.get_or_insert(defaults).push(text);
        });
        if saved {
            input.update(cx, |i, cx| i.set_value("", window, cx));
        }
    }

    /// 書いたサブタスクを足す。
    fn add_schedule_subtask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.schedule_inputs.subtask.clone();
        let text = input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        if self.edit_open_template(cx, |t| t.subtasks.push(text)) {
            input.update(cx, |i, cx| i.set_value("", window, cx));
        }
    }

    /// ファイルを選んで、開いている定時タスクに添付する。
    fn attach_schedule_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.schedule_open.clone() else {
            return;
        };
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("添付".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            // 設定を閉じたあとなら、反映する先がない
            let _ = this.update(cx, |this, cx| this.attach_schedule_paths(&id, &paths, cx));
        })
        .detach();
    }

    /// ファイルを定時タスクに添付する。添付できなかったものは、名前を並べて知らせる。
    pub fn attach_schedule_paths(
        &mut self,
        id: &str,
        paths: &[std::path::PathBuf],
        cx: &mut Context<Self>,
    ) {
        let mut failed = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // 大きすぎるファイルは、読む前に断る
            let bytes = std::fs::metadata(path)
                .map_err(malakt_core::Error::from)
                .and_then(|meta| {
                    if meta.len() > malakt_core::MAX_ATTACHMENT_BYTES {
                        return Err(malakt_core::Error::Invalid(format!(
                            "添付できるのは 1 ファイル {} までです",
                            malakt_core::view::format_bytes(malakt_core::MAX_ATTACHMENT_BYTES)
                        )));
                    }
                    Ok(std::fs::read(path)?)
                });
            let result = bytes.and_then(|bytes| {
                self.try_write_schedules(cx, |s| s.add_schedule_file(id, &name, &bytes))
            });
            if let Err(error) = result {
                failed.push(format!("「{name}」: {error}"));
            }
        }
        self.schedules_error = (!failed.is_empty())
            .then(|| format!("添付できませんでした。{}", failed.join("、")).into());
        cx.notify();
    }

    /// 行を押した。閉じているなら開き、開いているなら保存して閉じる。
    pub fn toggle_schedule(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.schedule_open.as_deref() == Some(id);
        self.save_open_schedule(cx);
        if same {
            self.schedule_open = None;
            cx.notify();
            return;
        }
        if let Some(s) = self.schedule(id, cx) {
            self.open_schedule_now(&s, window, cx);
        }
    }

    fn save_open_schedule(&mut self, cx: &mut Context<Self>) {
        self.save_schedule_title(cx);
        self.save_schedule_ai(cx);
        self.save_schedule_description(cx);
    }

    /// 書いたタイトルを保存する。空にしたときと変えていないときは、今のタイトルのまま。
    pub(super) fn save_schedule_title(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self
            .schedule_open
            .clone()
            .and_then(|id| self.schedule(&id, cx))
        else {
            return;
        };
        let title = self.schedule_title.read(cx).value().trim().to_owned();
        if title.is_empty() || title == s.title {
            return;
        }
        self.patch_schedule(
            &s.id,
            SchedulePatch {
                title: Some(title),
                ..SchedulePatch::default()
            },
            cx,
        );
    }

    /// 書いた予想工数を保存する。空にしたら外す。読めないときは、書いたまま理由を出す。
    fn save_schedule_estimate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self
            .schedule_open
            .clone()
            .and_then(|id| self.schedule(&id, cx))
        else {
            return;
        };
        let text = self.schedule_inputs.estimate.read(cx).value().to_string();
        let value = match estimate::read(&text) {
            Reading::Empty => None,
            Reading::Value(e) => e.non_zero(),
            Reading::Unreadable => {
                self.schedules_error =
                    Some(format!("予想工数「{}」を読めませんでした", text.trim()).into());
                cx.notify();
                return;
            }
        };
        let label = value.map(|e| e.label()).unwrap_or_default();
        self.schedule_inputs
            .estimate
            .update(cx, |i, cx| i.set_value(label, window, cx));
        if value == s.task.estimate {
            return;
        }
        self.patch_template(&s.id, cx, |t| t.estimate = value);
    }

    pub(super) fn save_schedule_ai(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self
            .schedule_open
            .clone()
            .and_then(|id| self.schedule(&id, cx))
        else {
            return;
        };
        let ai = self.schedule_ai.read(cx).value().to_string();
        if ai.trim() == s.ai {
            return;
        }
        self.patch_schedule(
            &s.id,
            SchedulePatch {
                ai: Some(ai),
                ..SchedulePatch::default()
            },
            cx,
        );
    }

    /// Esc。書きかけのタイトルと AI への指示を捨てて、閉じる。
    pub fn cancel_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.schedule_open.take() else {
            return;
        };
        if let Some(s) = self.schedule(&id, cx) {
            self.schedule_inputs.load(&s, window, cx);
            self.schedule_title
                .update(cx, |i, cx| i.set_value(s.title, window, cx));
            self.schedule_ai
                .update(cx, |i, cx| i.set_value(s.ai, window, cx));
        }
        self.schedules_error = None;
        cx.notify();
    }

    fn patch_schedule(&mut self, id: &str, patch: SchedulePatch, cx: &mut Context<Self>) {
        let id = id.to_owned();
        let now = malakt_core::view::now();
        self.write_schedules(cx, |s| s.update_schedule(&id, &patch, &now));
    }

    fn delete_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.schedule_open.as_deref() == Some(id) {
            self.schedule_open = None;
        }
        let id = id.to_owned();
        self.write_schedules(cx, |s| s.delete_schedule(&id));
    }

    /// 定時タスクの一覧。`picker` のときは、上に場所（未所属かワークスペース）を選ぶボタンを出す。
    pub fn schedules_editor(&self, picker: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = &crate::db::shared(cx).read(cx).store;
        let scope = self.schedules_scope.clone();
        let loaded = store.workspaces().and_then(|all| {
            let scope = scope.filter(|id| all.iter().any(|w| &w.id == id));
            let list = store.schedules(scope.as_deref())?;
            // ワークスペースの既定値（担当と期限）。選んでいないときに、何が入るかを出す
            let defaults = store.draft_for(scope.as_deref(), "")?;
            Ok((all, scope, list, store.people()?, defaults))
        });
        let (workspaces, scope, list, people, defaults) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                return div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(format!("定時タスクを読めませんでした: {error}"))
                    .into_any_element();
            }
        };
        let tree = workspace_tree(&workspaces);
        let shown: SharedString = match &scope {
            None => "未所属".into(),
            Some(id) => tree
                .iter()
                .find(|(w, _)| &w.id == id)
                .map(|(w, _)| w.name.clone())
                .unwrap_or_default()
                .into(),
        };
        let settings = cx.entity().downgrade();
        let scope_menu = button("schedules-scope")
            .outline()
            .w(CONTROL_WIDTH)
            .h_8()
            .child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(shown))
                    .child(
                        app_icon("chevron-down")
                            .small()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .pop_menu({
                let scope = scope.clone();
                move |mut menu, _, _| {
                    let choices = std::iter::once((None, "未所属".to_owned(), 0)).chain(
                        tree.iter()
                            .map(|(w, depth)| (Some(w.id.clone()), w.name.clone(), *depth)),
                    );
                    for (id, name, depth) in choices {
                        let settings = settings.clone();
                        let checked = id == scope;
                        // サブワークスペースは、深さの分だけ下げて出す
                        let label = format!("{}{name}", "　".repeat(depth));
                        menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                            move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = settings.update(cx, |this, cx| {
                                    this.save_open_schedule(cx);
                                    this.show_schedules(id.clone(), cx)
                                });
                            },
                        ));
                    }
                    menu
                }
            });
        let add = feel::gloss(
            button("schedule-add")
                .icon(app_icon("plus"))
                .tooltip("定時タスクを追加")
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, window, cx| this.add_schedule(window, cx))),
        );
        let top = row()
            .gap_1()
            .when(picker, |d| d.child(scope_menu))
            .child(div().flex_1())
            .child(add);

        let mut rows = col()
            .id("schedules")
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .overflow_hidden();
        for (ix, s) in list.iter().enumerate() {
            rows = rows.child(self.schedule_row(ix, s, &people, &defaults, cx));
        }
        col()
            .w_full()
            .gap_2()
            .child(top)
            .when(!list.is_empty(), |d| d.child(rows.test_support()))
            .when_some(self.schedules_error.clone(), |d, error| {
                d.child(
                    div()
                        .id("schedules-error")
                        .text_xs()
                        .text_color(theme.danger)
                        .child(error)
                        .test_support(),
                )
            })
            .into_any_element()
    }

    fn schedule_row(
        &self,
        ix: usize,
        s: &Schedule,
        people: &[Person],
        defaults: &malakt_core::TaskDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.schedule_open.as_deref() == Some(s.id.as_str());
        let id = s.id.clone();
        let group = SharedString::from(format!("schedule-{ix}"));
        // タイトルのところ。開いているときは、同じ大きさ・位置のまま入力欄になる
        let title = if open {
            div()
                .flex_1()
                .min_w_0()
                .mx(px(-8.))
                .child(
                    Input::new(&self.schedule_title)
                        .id("schedule-title")
                        .aria_label("定時タスクのタイトル")
                        .text_sm()
                        .bg(theme.background)
                        .border_color(theme.ring)
                        .px_2(),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(!s.enabled, |d| d.text_color(theme.muted_foreground))
                .child(s.title.clone())
                .into_any_element()
        };
        let head = row()
            .id(("schedule", ix))
            .flex_1()
            .min_w_0()
            .gap_2()
            .py_1()
            .cursor_pointer()
            .child(
                div()
                    .id(("schedule-fold", ix))
                    .flex_none()
                    .child(
                        app_icon(if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        })
                        .small()
                        .text_color(theme.muted_foreground),
                    )
                    .test_support(),
            )
            .child(title)
            .when(!open, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(s.rule.label()),
                )
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _: &ClickEvent, window, cx| {
                    // 開いている間にタイトルの欄を押したときは、書き続ける
                    if this.schedule_open.as_deref() == Some(id.as_str())
                        && this
                            .schedule_title
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                    {
                        return;
                    }
                    this.toggle_schedule(&id, window, cx)
                }
            }))
            .test_support();
        let switch = Switch::new(("schedule-on", ix))
            .checked(s.enabled)
            .small()
            .tooltip(if s.enabled { "動かす" } else { "止める" })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, on: &bool, _, cx| {
                    this.patch_schedule(
                        &id,
                        SchedulePatch {
                            enabled: Some(*on),
                            ..SchedulePatch::default()
                        },
                        cx,
                    )
                }
            }));
        let delete = button(("schedule-delete", ix))
            .icon(app_icon("trash"))
            .tooltip("削除")
            .ghost()
            .xsmall()
            // 乗せたときと、開いているときだけ見せる
            .when(!open, |b| {
                b.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.delete_schedule(&id, cx)
            }));
        let line = row()
            .group(group)
            .gap_1()
            .px_3()
            .min_h(px(36.))
            .lit(theme.muted, cx)
            .child(head)
            .child(switch)
            .child(feel::gloss(delete));
        col()
            .when(ix > 0, |d| d.border_t_1().border_color(theme.border))
            .when(open, |d| d.bg(theme.accent.opacity(0.4)))
            .child(line)
            .when(open, |d| {
                d.child(self.schedule_fields(s, people, defaults, cx))
            })
            // Esc は、書きかけを捨てて閉じる。設定のウィンドウは閉じない
            .on_action(
                cx.listener(|this, _: &crate::Dismiss, window, cx| {
                    this.cancel_schedule(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    this.cancel_schedule(window, cx)
                }),
            )
            .into_any_element()
    }

    /// 開いた定時タスクの、いつ・AI への指示と、作るタスクの項目。並びはタスクの詳細と同じ。
    fn schedule_fields(
        &self,
        s: &Schedule,
        people: &[Person],
        defaults: &malakt_core::TaskDraft,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = cx.entity().downgrade();
        let id = s.id.clone();
        let store = &crate::db::shared(cx).read(cx).store;
        let workspace = s.workspace_id.as_deref();
        // 読めなかったものは、一覧の下に理由を出し、その欄は空にする
        let mut problems: Vec<String> = Vec::new();
        let workspaces: Vec<Workspace> =
            take(&mut problems, "ワークスペース", store.workspaces()).unwrap_or_default();
        let files = take(&mut problems, "添付", store.schedule_files(&s.id)).unwrap_or_default();
        let link_defaults: Vec<String> = match workspace {
            Some(w) => take(
                &mut problems,
                "ワークスペースの設定",
                store.task_defaults(w),
            )
            .map(|d| d.link_workspace_ids)
            .unwrap_or_default(),
            None => Vec::new(),
        };
        let standard =
            take(&mut problems, "通知の標準", store.due_alert_standard()).unwrap_or_default();
        let time_standard = take(
            &mut problems,
            "所要時間の基準の標準",
            store.time_basis_standard(),
        )
        .unwrap_or_default();
        // 親にできるタスク: 同じ場所の、終わっていないタスク
        let scope = match workspace {
            Some(w) => malakt_core::Scope::Workspace(w),
            None => malakt_core::Scope::Unfiled,
        };
        let filter = malakt_core::TaskFilter {
            include_descendants: false,
            open_only: true,
            assignee_id: None,
            query: None,
        };
        let mut parents: Vec<Task> =
            take(&mut problems, "タスク", store.tasks(scope, &filter)).unwrap_or_default();
        parents.truncate(MAX_PARENTS);
        let parent: Option<Task> = s.task.parent_id.as_deref().and_then(|p| store.task(p).ok());
        let t = s.task.clone();

        // いつ
        let pick: Pick = std::rc::Rc::new({
            let (settings, id) = (settings.clone(), id.clone());
            move |rule: Option<Rule>, cx: &mut App| {
                let Some(rule) = rule else {
                    return;
                };
                // 設定を閉じたあとなら、反映する先がない
                let _ = settings.update(cx, |this, cx| {
                    this.patch_schedule(
                        &id,
                        SchedulePatch {
                            rule: Some(rule),
                            ..SchedulePatch::default()
                        },
                        cx,
                    )
                });
            }
        });
        let current = s.rule.clone();
        let when = row()
            .gap_2()
            .flex_wrap()
            .child(
                div().flex().child(
                    button("schedule-rule")
                        .accessibility_label(format!("いつ: {}", s.rule.label()))
                        .outline()
                        .small()
                        .child(menu_face("repeat", s.rule.label().into(), cx))
                        .pop_menu(move |menu, window, cx| {
                            repeat_menu(menu, &pick, &current, Some(&current), false, window, cx)
                        }),
                ),
            )
            .when(s.enabled, |d| {
                d.child(
                    div()
                        .id("schedule-next")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(repeat::next_label(&s.next_at))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("次に作る日時")
                                .build(window, cx)
                        }),
                )
            });

        // URL。タスクの詳細と同じチップ
        let mut urls = row().w_full().gap_1p5().flex_wrap().items_center();
        for (ix, url) in t.urls.iter().enumerate() {
            let shown = crate::task_urls::shown(url);
            let tip: SharedString = url.clone().into();
            let remove = url.clone();
            let (settings, id) = (settings.clone(), id.clone());
            urls = urls.child(
                row()
                    .id(("schedule-url", ix))
                    .flex_none()
                    .max_w(px(260.))
                    .gap_1()
                    .items_center()
                    .pl_2()
                    .pr_0p5()
                    .py_0p5()
                    .rounded(feel::md())
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .text_sm()
                    .child(app_icon(shown.icon).small().flex_none())
                    .child(div().min_w_0().truncate().child(shown.label))
                    .child(
                        remove_x(
                            ("schedule-url-remove", ix),
                            "URL を外す",
                            move |_, _, cx| {
                                let remove = remove.clone();
                                let _ = settings.update(cx, |this, cx| {
                                    this.patch_template(&id, cx, |t| {
                                        t.urls.retain(|u| *u != remove)
                                    })
                                });
                            },
                            cx,
                        )
                        .text_color(theme.muted_foreground)
                        .test_support(),
                    )
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .test_support(),
            );
        }
        let urls = urls.child(
            div().w(px(220.)).max_w_full().child(
                Input::new(&self.schedule_inputs.url)
                    .id("schedule-url-input")
                    .aria_label("URL を追加")
                    .small()
                    .bg(theme.background),
            ),
        );

        // 状態
        let status = button("schedule-status")
            .accessibility_label(format!("状態: {}", t.status.label()))
            .outline()
            .small()
            .child(menu_face(
                crate::render::status_icon(t.status),
                t.status.label().into(),
                cx,
            ))
            .pop_menu({
                let (settings, s) = (settings.clone(), s.clone());
                move |mut menu, _, _| {
                    for status in Status::ALL {
                        menu = menu.item(
                            PopupMenuItem::new(status.label())
                                .checked(s.task.status == status)
                                .on_click(on_patch(
                                    settings.clone(),
                                    s.id.clone(),
                                    task_patch(&s, |t| t.status = status),
                                )),
                        );
                    }
                    menu
                }
            });

        // 優先度。決めていなければ、ワークスペースの設定
        let ws_priority = Priority::of(defaults.priority).label();
        let shown_priority: SharedString = match t.priority {
            Some(p) => Priority::of(p).label().into(),
            None => format!("ワークスペースの設定（{ws_priority}）").into(),
        };
        let priority = button("schedule-priority")
            .accessibility_label(format!("優先度: {shown_priority}"))
            .outline()
            .small()
            .child(menu_face("signal-high", shown_priority, cx))
            .pop_menu({
                let (settings, s) = (settings.clone(), s.clone());
                move |mut menu, _, _| {
                    let set = |p: Option<u8>| {
                        on_patch(
                            settings.clone(),
                            s.id.clone(),
                            task_patch(&s, |t| t.priority = p),
                        )
                    };
                    menu = menu
                        .item(
                            PopupMenuItem::new(format!("ワークスペースの設定（{ws_priority}）"))
                                .checked(s.task.priority.is_none())
                                .on_click(set(None)),
                        )
                        .separator();
                    for p in [Priority::High, Priority::Mid, Priority::Low, Priority::None] {
                        menu = menu.item(
                            PopupMenuItem::new(p.label())
                                .checked(s.task.priority.map(Priority::of) == Some(p))
                                .on_click(set(Some(p.value()))),
                        );
                    }
                    menu
                }
            });

        // 担当。選んでいなければ、ワークスペースの既定値
        let name_of = |pid: &str| people.iter().find(|p| p.id == pid).map(|p| p.name.clone());
        let names = |ids: &[String]| {
            ids.iter()
                .filter_map(|pid| name_of(pid))
                .collect::<Vec<_>>()
                .join("、")
        };
        let chosen = names(&s.assignee_ids);
        let shown_people: SharedString = if !chosen.is_empty() {
            chosen.into()
        } else if defaults.assignee_ids.is_empty() {
            "なし".into()
        } else {
            format!("ワークスペースの設定（{}）", names(&defaults.assignee_ids)).into()
        };
        let assignee_menu = button("schedule-assignees")
            .accessibility_label(format!("担当: {shown_people}"))
            .outline()
            .small()
            .child(menu_face("user", shown_people, cx))
            .pop_menu({
                let (settings, id) = (settings.clone(), id.clone());
                let people: Vec<Person> = people.to_vec();
                let ids = s.assignee_ids.clone();
                move |mut menu, _, _| {
                    for person in &people {
                        let on = ids.contains(&person.id);
                        let mut next = ids.clone();
                        if on {
                            next.retain(|p| p != &person.id);
                        } else {
                            next.push(person.id.clone());
                        }
                        menu = menu.item(
                            PopupMenuItem::new(person.name.clone())
                                .checked(on)
                                .on_click(on_patch(
                                    settings.clone(),
                                    id.clone(),
                                    SchedulePatch {
                                        assignee_ids: Some(next),
                                        ..SchedulePatch::default()
                                    },
                                )),
                        );
                    }
                    menu
                }
            });
        // 担当ごとの役（RACI）。タスクの詳細と同じく、名前の横に出す
        let mut assignees = col().gap_1().child(div().flex().child(assignee_menu));
        for (ix, pid) in s.assignee_ids.iter().enumerate() {
            let Some(name) = name_of(pid) else {
                continue;
            };
            let roles = s.roles_of(pid).unwrap_or_default();
            let code = roles.iter().map(Role::code).collect::<Vec<_>>().join("・");
            let role_menu = button(("schedule-roles", ix))
                .accessibility_label(format!("{name} の役: {code}"))
                .ghost()
                .xsmall()
                .child(menu_face("users", code.into(), cx))
                .pop_menu({
                    let (settings, s) = (settings.clone(), s.clone());
                    move |mut menu, _, _| {
                        for role in Role::ALL {
                            let on = roles.contains(role);
                            let next = if on {
                                roles.without(role)
                            } else {
                                roles.with(role)
                            };
                            // 役は 1 つ以上
                            let next = if next.is_empty() { roles } else { next };
                            let patch = task_patch(&s, |t| {
                                t.roles = s
                                    .assignee_ids
                                    .iter()
                                    .enumerate()
                                    .map(|(i, other)| {
                                        let mine = s.roles_of(other).unwrap_or_default();
                                        if i == ix {
                                            next
                                        } else if next.contains(Role::A) && !on {
                                            // A は 1 人だけ。ほかの人の A は外す
                                            let rest = mine.without(Role::A);
                                            if rest.is_empty() {
                                                Roles::R
                                            } else {
                                                rest
                                            }
                                        } else {
                                            mine
                                        }
                                    })
                                    .collect();
                            });
                            menu = menu.item(
                                PopupMenuItem::new(format!("{}（{}）", role.code(), role.label()))
                                    .checked(on)
                                    .on_click(on_patch(settings.clone(), s.id.clone(), patch)),
                            );
                        }
                        menu
                    }
                });
            assignees = assignees.child(
                row()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child(name))
                    .child(role_menu),
            );
        }

        // 開始日・期限。回の日から何日後か
        let start = self.offset_menu(s, Offset::Start, cx);
        let due = self.offset_menu(s, Offset::Due, cx);

        // 通知。期限があるときだけ（タスクの詳細と同じ）
        let has_due = s.due_days.is_some() || defaults.due.is_some();
        let timed = match s.due_days {
            Some(_) => s.due_time.is_some(),
            None => defaults
                .due
                .as_deref()
                .and_then(malakt_core::time_of)
                .is_some(),
        };
        let alerts = has_due.then(|| {
            let (choices, label): (&'static [u32], fn(u32) -> String) = if timed {
                (&alert::TIMED_CHOICES, alert::timed_label)
            } else {
                (&alert::DATE_CHOICES, alert::date_label)
            };
            let (own, base) = if timed {
                (t.alerts_timed.clone(), standard.timed.clone())
            } else {
                (t.alerts_date.clone(), standard.date.clone())
            };
            let shown: SharedString = match &own {
                Some(values) => alert::summary(values, label).into(),
                None => format!("標準（{}）", alert::summary(&base, label)).into(),
            };
            button("schedule-alerts")
                .accessibility_label(format!("通知: {shown}"))
                .outline()
                .small()
                .child(menu_face("bell", shown, cx))
                .pop_menu({
                    let (settings, s) = (settings.clone(), s.clone());
                    move |mut menu, _, _| {
                        let set = |values: Option<Vec<u32>>| {
                            let patch = task_patch(&s, |t| {
                                if timed {
                                    t.alerts_timed = values
                                } else {
                                    t.alerts_date = values
                                }
                            });
                            on_patch(settings.clone(), s.id.clone(), patch)
                        };
                        menu = menu
                            .item(
                                PopupMenuItem::new(format!(
                                    "標準に合わせる（{}）",
                                    alert::summary(&base, label)
                                ))
                                .checked(own.is_none())
                                .on_click(set(None)),
                            )
                            .separator();
                        let chosen = own.clone().unwrap_or_else(|| base.clone());
                        for value in choices.iter().copied() {
                            let on = chosen.contains(&value);
                            let mut next = chosen.clone();
                            if on {
                                next.retain(|v| *v != value);
                            } else {
                                next.push(value);
                            }
                            menu = menu.item(
                                PopupMenuItem::new(label(value))
                                    .checked(on)
                                    .on_click(set(Some(next))),
                            );
                        }
                        menu
                    }
                })
        });

        // 進捗
        let percent = button("schedule-percent")
            .accessibility_label(format!("進捗: {}%", t.percent))
            .outline()
            .small()
            .child(menu_face("gauge", format!("{}%", t.percent).into(), cx))
            .pop_menu({
                let (settings, s) = (settings.clone(), s.clone());
                move |mut menu, _, _| {
                    for value in PERCENTS {
                        menu = menu.item(
                            PopupMenuItem::new(format!("{value}%"))
                                .checked(s.task.percent == value)
                                .on_click(on_patch(
                                    settings.clone(),
                                    s.id.clone(),
                                    task_patch(&s, |t| t.percent = value),
                                )),
                        );
                    }
                    menu
                }
            });

        // 予想工数。書いて Enter かほかの場所を押すと保存する
        let estimate_input = div()
            .id("schedule-estimate")
            .w(CONTROL_WIDTH)
            .child(
                Input::new(&self.schedule_inputs.estimate)
                    .aria_label("予想工数")
                    .small(),
            )
            .test_support();

        // 所要時間の基準。決めていなければ、ワークスペース（なければ全体の標準）
        let inherited = workspace
            .and_then(|w| timing::workspace_basis(&workspaces, w))
            .unwrap_or(time_standard);
        let inherited_text = if workspace.is_some() {
            format!("ワークスペースの設定（{}）", inherited.label())
        } else {
            format!("標準（{}）", inherited.label())
        };
        let shown_basis: SharedString = match t.time_basis {
            Some(b) => b.label().into(),
            None => inherited_text.clone().into(),
        };
        let basis = button("schedule-time-basis")
            .accessibility_label(format!("所要時間の基準: {shown_basis}"))
            .outline()
            .small()
            .child(menu_face("timer", shown_basis, cx))
            .pop_menu({
                let (settings, s) = (settings.clone(), s.clone());
                move |mut menu, _, _| {
                    let now = s.task.time_basis.unwrap_or(inherited);
                    let set = |b: Option<Basis>| {
                        on_patch(
                            settings.clone(),
                            s.id.clone(),
                            task_patch(&s, |t| t.time_basis = b),
                        )
                    };
                    menu = menu.item(
                        PopupMenuItem::new(inherited_text.clone())
                            .checked(s.task.time_basis.is_none())
                            .on_click(set(None)),
                    );
                    menu = menu.separator().label("開始");
                    for start in StartBasis::ALL {
                        menu = menu.item(
                            PopupMenuItem::new(start.label())
                                .checked(s.task.time_basis.is_some() && now.start == start)
                                .on_click(set(Some(Basis::new(start, now.end)))),
                        );
                    }
                    menu = menu.separator().label("完了");
                    for end in EndBasis::ALL {
                        menu = menu.item(
                            PopupMenuItem::new(end.label())
                                .checked(s.task.time_basis.is_some() && now.end == end)
                                .on_click(set(Some(Basis::new(now.start, end)))),
                        );
                    }
                    menu
                }
            });

        // カテゴリ。決めていなければ、ワークスペースの設定
        let mut categories = row().w_full().gap_1p5().flex_wrap().items_center();
        match &t.categories {
            None => {
                let text = if defaults.categories.is_empty() {
                    "ワークスペースの設定（なし）".to_owned()
                } else {
                    format!("ワークスペースの設定（{}）", defaults.categories.join("、"))
                };
                categories = categories.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(text),
                );
            }
            Some(list) => {
                for (ix, c) in list.iter().enumerate() {
                    let value = c.clone();
                    let (settings, id) = (settings.clone(), id.clone());
                    categories = categories.child(
                        row()
                            .id(("schedule-cat", ix))
                            .flex_none()
                            .max_w_full()
                            .gap_0p5()
                            .rounded_full()
                            .bg(theme.secondary)
                            .py_0p5()
                            .pr_0p5()
                            .pl_2p5()
                            .text_sm()
                            .text_color(theme.secondary_foreground)
                            .child(div().min_w_0().truncate().child(c.clone()))
                            .child(
                                remove_x(
                                    ("schedule-cat-remove", ix),
                                    format!("カテゴリ「{c}」を外す"),
                                    move |_, _, cx| {
                                        let value = value.clone();
                                        let _ = settings.update(cx, |this, cx| {
                                            this.patch_template(&id, cx, |t| {
                                                if let Some(list) = &mut t.categories {
                                                    list.retain(|c| *c != value);
                                                }
                                            })
                                        });
                                    },
                                    cx,
                                )
                                .test_support(),
                            )
                            .test_support(),
                    );
                }
                categories = categories.child(feel::gloss(
                    button("schedule-cat-reset")
                        .label("ワークスペースの設定に戻す")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.patch_template(&id, cx, |t| t.categories = None)
                            }
                        })),
                ));
            }
        }
        let categories = categories.child(
            div().w(px(168.)).child(
                Input::new(&self.schedule_inputs.category)
                    .id("schedule-category-input")
                    .aria_label("カテゴリを追加")
                    .small()
                    .bg(theme.background),
            ),
        );

        // 親タスク
        let shown_parent: SharedString = match (&t.parent_id, &parent) {
            (None, _) => "なし".into(),
            (Some(_), Some(p)) => p.title.clone().into(),
            (Some(_), None) => "（消したタスク）".into(),
        };
        let parent_menu = button("schedule-parent")
            .accessibility_label(format!("親タスク: {shown_parent}"))
            .outline()
            .small()
            .child(menu_face("list-tree", shown_parent, cx))
            .pop_menu({
                let (settings, s) = (settings.clone(), s.clone());
                move |mut menu, _, _| {
                    menu = menu.item(
                        PopupMenuItem::new("なし")
                            .checked(s.task.parent_id.is_none())
                            .on_click(on_patch(
                                settings.clone(),
                                s.id.clone(),
                                task_patch(&s, |t| t.parent_id = None),
                            )),
                    );
                    if !parents.is_empty() {
                        menu = menu.separator();
                    }
                    for p in &parents {
                        let pid = p.id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(p.title.clone())
                                .checked(s.task.parent_id.as_deref() == Some(p.id.as_str()))
                                .on_click(on_patch(
                                    settings.clone(),
                                    s.id.clone(),
                                    task_patch(&s, |t| t.parent_id = Some(pid)),
                                )),
                        );
                    }
                    menu
                }
            });

        // ほかのワークスペース。決めていなければ、ワークスペースの設定
        let ws_name = |wid: &str| {
            workspaces
                .iter()
                .find(|w| w.id == wid)
                .map(|w| w.name.clone())
        };
        let ws_names = |ids: &[String]| {
            let list: Vec<String> = ids.iter().filter_map(|w| ws_name(w)).collect();
            if list.is_empty() {
                "なし".to_owned()
            } else {
                list.join("、")
            }
        };
        let shown_links: SharedString = match &t.linked_workspace_ids {
            Some(ids) => ws_names(ids).into(),
            None => format!("ワークスペースの設定（{}）", ws_names(&link_defaults)).into(),
        };
        let links = workspace.map(|own| {
            let tree = super::rules::workspace_tree(&workspaces);
            let own = own.to_owned();
            button("schedule-links")
                .accessibility_label(format!("同期先: {shown_links}"))
                .outline()
                .small()
                .child(menu_face("folders", shown_links, cx))
                .pop_menu({
                    let (settings, s) = (settings.clone(), s.clone());
                    let defaults = link_defaults.clone();
                    let label = format!("ワークスペースの設定（{}）", ws_names(&link_defaults));
                    move |mut menu, _, _| {
                        menu = menu
                            .item(
                                PopupMenuItem::new(label.clone())
                                    .checked(s.task.linked_workspace_ids.is_none())
                                    .on_click(on_patch(
                                        settings.clone(),
                                        s.id.clone(),
                                        task_patch(&s, |t| t.linked_workspace_ids = None),
                                    )),
                            )
                            .separator();
                        let current = s
                            .task
                            .linked_workspace_ids
                            .clone()
                            .unwrap_or_else(|| defaults.clone());
                        for (w, depth) in tree.iter().filter(|(w, _)| w.id != own) {
                            let on = current.contains(&w.id);
                            let mut next = current.clone();
                            if on {
                                next.retain(|x| *x != w.id);
                            } else {
                                next.push(w.id.clone());
                            }
                            menu = menu.item(
                                PopupMenuItem::new(format!("{}{}", "　".repeat(*depth), w.name))
                                    .checked(on)
                                    .on_click(on_patch(
                                        settings.clone(),
                                        s.id.clone(),
                                        task_patch(&s, |t| t.linked_workspace_ids = Some(next)),
                                    )),
                            );
                        }
                        menu
                    }
                })
        });

        // 説明。空ならワークスペースの設定
        let description = div()
            .id("schedule-description")
            .flex_1()
            .min_w_0()
            .child(
                Textarea::new(&self.schedule_inputs.description)
                    .aria_label("説明")
                    .small()
                    .bg(theme.background),
            )
            .test_support();

        // サブタスク
        let mut subtasks = col().w_full().gap_1();
        for (ix, title) in t.subtasks.iter().enumerate() {
            let (settings, id) = (settings.clone(), id.clone());
            subtasks = subtasks.child(
                row()
                    .gap_2()
                    .items_center()
                    .child(
                        app_icon("circle")
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(title.clone()),
                    )
                    .child(
                        remove_x(
                            ("schedule-sub-remove", ix),
                            format!("サブタスク「{title}」を外す"),
                            move |_, _, cx| {
                                let _ = settings.update(cx, |this, cx| {
                                    this.patch_template(&id, cx, |t| {
                                        if ix < t.subtasks.len() {
                                            t.subtasks.remove(ix);
                                        }
                                    })
                                });
                            },
                            cx,
                        )
                        .test_support(),
                    ),
            );
        }
        let subtasks = subtasks.child(
            Input::new(&self.schedule_inputs.subtask)
                .id("schedule-subtask-input")
                .aria_label("サブタスクを追加")
                .small()
                .bg(theme.background),
        );

        // 添付
        let mut attachments = col().w_full().gap_1();
        for (ix, f) in files.iter().enumerate() {
            let file_id = f.id.clone();
            attachments = attachments.child(
                row()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(f.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(malakt_core::view::format_bytes(f.size)),
                    )
                    .child(feel::gloss(
                        button(("schedule-file-remove", ix))
                            .label("外す")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let file_id = file_id.clone();
                                this.write_schedules(cx, |s| s.delete_schedule_file(&file_id));
                            })),
                    )),
            );
        }
        let attachments = attachments.child(
            row().child(feel::gloss(
                button("schedule-attach")
                    .icon(app_icon("paperclip"))
                    .label("ファイルを添付")
                    .ghost()
                    .xsmall()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.attach_schedule_files(window, cx)),
                    ),
            )),
        );

        let ai = div()
            .id("schedule-ai")
            .flex_1()
            .min_w_0()
            .child(
                Textarea::new(&self.schedule_ai)
                    .aria_label("AI への指示")
                    .small()
                    .bg(theme.background),
            )
            .test_support();
        col()
            .id("schedule-fields")
            .gap_2()
            .px_3()
            .pb_3()
            .pt_1()
            .child(field("いつ", when, cx))
            .child(field("URL", urls, cx))
            .child(field("状態", status, cx))
            .child(field("優先度", priority, cx))
            .child(field("担当", assignees, cx))
            .child(field("開始日", start, cx))
            .child(field("期限", due, cx))
            .when_some(alerts, |d, menu| d.child(field("通知", menu, cx)))
            .child(field("進捗", percent, cx))
            .child(field("予想工数", estimate_input, cx))
            .child(field("所要時間", basis, cx))
            .child(field("カテゴリ", categories, cx))
            .child(field("親タスク", parent_menu, cx))
            .when_some(links, |d, menu| d.child(field("同期先", menu, cx)))
            .child(field("説明", description, cx))
            .child(field("サブタスク", subtasks, cx))
            .child(field("添付", attachments, cx))
            .child(field("AI への指示", ai, cx))
            .when(!problems.is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(problems.join("、")),
                )
            })
            .test_support()
    }

    /// 開始日か期限の、回の日から何日後かを選ぶメニュー。
    fn offset_menu(&self, s: &Schedule, which: Offset, cx: &mut Context<Self>) -> AnyElement {
        let settings = cx.entity().downgrade();
        let store = &crate::db::shared(cx).read(cx).store;
        // 期限は、決めていなければワークスペースの設定
        let ws_due = which == Offset::Due
            && store
                .draft_for(s.workspace_id.as_deref(), "")
                .is_ok_and(|d| d.due.is_some());
        let (days, time, label, icon, key) = match which {
            Offset::Start => (
                s.task.start_days,
                s.task.start_time.clone(),
                s.start_label(),
                "calendar-arrow-up",
                "schedule-start",
            ),
            Offset::Due => (
                s.due_days,
                s.due_time.clone(),
                s.due_label(),
                "calendar",
                "schedule-due",
            ),
        };
        let none_label = if ws_due {
            "ワークスペースの設定"
        } else {
            "なし"
        };
        let shown: SharedString = label.unwrap_or_else(|| none_label.to_owned()).into();
        let name = match which {
            Offset::Start => "開始日",
            Offset::Due => "期限",
        };
        let patch = move |s: &Schedule, days: Option<Option<i64>>, time: Option<Option<String>>| {
            match which {
                Offset::Due => SchedulePatch {
                    due_days: days,
                    due_time: time,
                    ..SchedulePatch::default()
                },
                Offset::Start => task_patch(s, |t| {
                    if let Some(days) = days {
                        t.start_days = days;
                    }
                    if let Some(time) = time {
                        t.start_time = time;
                    }
                }),
            }
        };
        div()
            .flex()
            .child(
                button(key)
                    .accessibility_label(format!("{name}: {shown}"))
                    .outline()
                    .small()
                    .child(menu_face(icon, shown, cx))
                    .pop_menu({
                        let s = s.clone();
                        move |mut menu, window, cx| {
                            let set =
                                |p: SchedulePatch| on_patch(settings.clone(), s.id.clone(), p);
                            menu = menu.item(
                                PopupMenuItem::new(none_label)
                                    .checked(days.is_none())
                                    .on_click(set(patch(&s, Some(None), None))),
                            );
                            menu = menu.separator();
                            for n in DUE_DAYS {
                                menu = menu.item(
                                    PopupMenuItem::new(due_days_label(n))
                                        .checked(days == Some(n))
                                        .on_click(set(patch(&s, Some(Some(n)), None))),
                                );
                            }
                            if days.is_none() {
                                return menu;
                            }
                            let (settings, s, time) = (settings.clone(), s.clone(), time.clone());
                            menu.separator()
                                .submenu("時刻", window, cx, move |mut menu, _, _| {
                                    let set = |p: SchedulePatch| {
                                        on_patch(settings.clone(), s.id.clone(), p)
                                    };
                                    menu = menu.item(
                                        PopupMenuItem::new("なし")
                                            .checked(time.is_none())
                                            .on_click(set(patch(&s, None, Some(None)))),
                                    );
                                    for hour in DUE_HOURS {
                                        let value = format!("{hour:02}:00");
                                        menu = menu.item(
                                            PopupMenuItem::new(format!("{hour}:00"))
                                                .checked(time.as_deref() == Some(value.as_str()))
                                                .on_click(set(patch(&s, None, Some(Some(value))))),
                                        );
                                    }
                                    menu
                                })
                        }
                    }),
            )
            .into_any_element()
    }

    /// 定時タスク `id` の雛形を `f` で変える。
    fn patch_template(&mut self, id: &str, cx: &mut Context<Self>, f: impl FnOnce(&mut Template)) {
        let Some(s) = self.schedule(id, cx) else {
            return;
        };
        self.patch_schedule(id, task_patch(&s, f), cx);
    }
}
