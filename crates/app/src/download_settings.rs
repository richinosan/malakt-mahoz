//! 設定の「ダウンロード」の行（保存先、保存先を毎回確認する、日付のフォルダ）。
//! 決まりそのものは [`crate::download_rules`]、覚えるのは [`crate::downloads::Downloads::set_rules`]。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::download_rules::{self, Rules, DEFAULT_PATTERN};
use crate::feel;
use crate::render::{app_icon, col, row};

/// 設定の「ダウンロード」の行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadRow {
    /// 保存先のフォルダ
    Folder,
    /// 保存先を毎回確認する
    Ask,
    /// 日付のフォルダ
    Subfolder,
}

impl DownloadRow {
    /// 名前の下に、変えるものを置く行か。
    pub fn wide(self) -> bool {
        self == DownloadRow::Subfolder
    }
}

pub struct DownloadSettings {
    /// 日付のフォルダの形式の欄。
    pattern: Entity<InputState>,
    /// 決まりを変えられなかった。変えた行の下に出す。
    error: Option<(DownloadRow, String)>,
    _subscriptions: Vec<Subscription>,
}

impl DownloadSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let downloads = crate::downloads::shared(cx);
        let shown = downloads
            .read(cx)
            .rules
            .subfolder
            .clone()
            .unwrap_or_else(|| DEFAULT_PATTERN.to_owned());
        let pattern = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder(DEFAULT_PATTERN);
            input.set_value(shown, window, cx);
            input
        });
        let subscriptions = vec![
            cx.observe(&downloads, |_, _, cx| cx.notify()),
            cx.subscribe(&pattern, |this, input, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let rules = crate::downloads::shared(cx).read(cx).rules.clone();
                // 日付のフォルダを使っていないときは、書くだけで決まりは変えない
                if rules.subfolder.is_none() {
                    return;
                }
                let text = input.read(cx).value().trim().to_string();
                this.apply(
                    DownloadRow::Subfolder,
                    Rules {
                        subfolder: Some(text),
                        ..rules
                    },
                    cx,
                );
            }),
        ];
        Self {
            pattern,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 決まりを変える。変えられなければ、その行に何がおかしいかを出す。
    fn apply(&mut self, at: DownloadRow, rules: Rules, cx: &mut Context<Self>) {
        let saved =
            crate::downloads::shared(cx).update(cx, |downloads, cx| downloads.set_rules(rules, cx));
        self.error = saved.err().map(|e| (at, e));
        cx.notify();
    }

    /// 保存先のフォルダを選ぶ。
    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("保存先にする".into()),
        });
        cx.spawn(async move |this, cx| {
            let folder = match chosen.await {
                Ok(Ok(Some(mut paths))) if !paths.is_empty() => paths.remove(0),
                // キャンセルした
                Ok(Ok(_)) => return,
                Ok(Err(e)) => {
                    // 設定を閉じたあとなら、出す先がない
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some((
                            DownloadRow::Folder,
                            format!("保存先のフォルダを選べませんでした: {e}"),
                        ));
                        cx.notify();
                    });
                    return;
                }
                Err(_) => return,
            };
            // 設定を閉じたあとなら、変える先がない
            let _ = this.update(cx, |this, cx| {
                let rules = crate::downloads::shared(cx).read(cx).rules.clone();
                this.apply(
                    DownloadRow::Folder,
                    Rules {
                        folder: Some(folder),
                        ..rules
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// 行の右（`wide` なら下）に置く、変えるもの。
    pub fn control(&mut self, at: DownloadRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rules = crate::downloads::shared(cx).read(cx).rules.clone();
        let error = self
            .error
            .as_ref()
            .filter(|(row, _)| *row == at)
            .map(|(_, e)| e.clone());
        let error_line = |id: &'static str| {
            error.clone().map(|e| {
                div()
                    .id(id)
                    .text_xs()
                    .text_color(theme.danger)
                    .child(e)
                    .test_support()
            })
        };
        match at {
            DownloadRow::Folder => {
                let shown = crate::downloads::root_dir(cx).display().to_string();
                col()
                    .items_end()
                    .gap_1()
                    .child(
                        row()
                            .gap_1()
                            .child(feel::gloss(
                                button("download-folder")
                                    .outline()
                                    .small()
                                    .max_w(px(320.))
                                    .tooltip(shown.clone())
                                    .child(
                                        row()
                                            .min_w_0()
                                            .gap_2()
                                            .child(app_icon("folder").small())
                                            .child(div().min_w_0().truncate().child(shown)),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                            ))
                            .child(
                                button("download-folder-reset")
                                    .icon(app_icon("rotate-ccw"))
                                    .ghost()
                                    .small()
                                    .tooltip("標準（OS のダウンロードのフォルダ）に戻す")
                                    // 場所は取っておき、標準のままのときは見せない
                                    .when(rules.folder.is_none(), |b| b.invisible().disabled(true))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let rules =
                                            crate::downloads::shared(cx).read(cx).rules.clone();
                                        this.apply(
                                            DownloadRow::Folder,
                                            Rules {
                                                folder: None,
                                                ..rules
                                            },
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .children(error_line("download-folder-error"))
                    .into_any_element()
            }
            DownloadRow::Ask => Switch::new("download-ask")
                .checked(rules.ask)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    let rules = crate::downloads::shared(cx).read(cx).rules.clone();
                    this.apply(DownloadRow::Ask, Rules { ask: *on, ..rules }, cx);
                }))
                .into_any_element(),
            DownloadRow::Subfolder => {
                let on = rules.subfolder.is_some();
                let preview = rules.subfolder.as_deref().map(|pattern| {
                    match download_rules::subfolder(pattern, download_rules::today()) {
                        Ok(path) => crate::downloads::root_dir(cx)
                            .join(path)
                            .display()
                            .to_string(),
                        Err(e) => e,
                    }
                });
                col()
                    .gap_2()
                    .child(
                        row()
                            .gap_3()
                            .child(Switch::new("download-subfolder").checked(on).on_click(
                                cx.listener(|this, on: &bool, _, cx| {
                                    let rules = crate::downloads::shared(cx).read(cx).rules.clone();
                                    let subfolder = on.then(|| {
                                        let text = this.pattern.read(cx).value().trim().to_string();
                                        if text.is_empty() {
                                            DEFAULT_PATTERN.to_owned()
                                        } else {
                                            text
                                        }
                                    });
                                    this.apply(
                                        DownloadRow::Subfolder,
                                        Rules { subfolder, ..rules },
                                        cx,
                                    );
                                }),
                            ))
                            .child(
                                div()
                                    .id("download-pattern")
                                    .w(px(220.))
                                    .child(
                                        Input::new(&self.pattern)
                                            .small()
                                            .disabled(!on)
                                            .aria_label("日付のフォルダの形式"),
                                    )
                                    .test_support(),
                            ),
                    )
                    .when_some(preview.filter(|_| error.is_none()), |d, preview| {
                        d.child(
                            div()
                                .id("download-pattern-preview")
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(preview)
                                .test_support(),
                        )
                    })
                    .children(error_line("download-pattern-error"))
                    .into_any_element()
            }
        }
    }
}
