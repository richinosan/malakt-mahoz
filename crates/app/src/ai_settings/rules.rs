//! AI のルールの一覧。グローバル（すべてのワークスペース）と、ワークスペースごと。
//!
//! 「＋」で「新規ルール」を足し、その場で名前を書く。行を押すと開き、名前と本文をその場で書ける。
//! 名前は Enter かほかの場所を押すと、本文はほかの場所を押すと保存し、Esc で書きかけを捨てて閉じる。

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::rules::{AiRule, Apply, RulePatch};
use malakt_core::{Store, Workspace};

use super::{col, px, row, AiSettings, CONTROL_WIDTH};
use crate::feel::{self, Lit};
use crate::render::app_icon;
use crate::scroll::Menus;

/// ワークスペースを、左のメニューと同じ親子の順に並べる。深さも返す。
fn workspace_tree(all: &[Workspace]) -> Vec<(Workspace, usize)> {
    fn walk(
        all: &[Workspace],
        parent: Option<&str>,
        depth: usize,
        out: &mut Vec<(Workspace, usize)>,
    ) {
        let mut children: Vec<&Workspace> = all
            .iter()
            .filter(|w| w.parent_id.as_deref() == parent)
            .collect();
        children.sort_by_key(|w| w.position);
        for w in children {
            out.push((w.clone(), depth));
            walk(all, Some(&w.id), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(all, None, 0, &mut out);
    out
}

impl AiSettings {
    /// ルールの一覧で見る場所を変える。`None` はグローバル。
    pub fn show_rules(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.rules_scope != scope {
            self.rules_scope = scope;
            self.rule_open = None;
        }
        self.rules_error = None;
        cx.notify();
    }

    /// データベースを変える。1 回の操作を、元に戻す 1 つの段にする。
    fn write_rules<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Store) -> malakt_core::Result<R>,
    ) -> Option<R> {
        let result = crate::db::shared(cx).update(cx, |db, cx| {
            let result = db.store.undo_mark().and_then(|()| f(&mut db.store));
            let marked = db.store.undo_mark();
            cx.notify();
            result.and_then(|value| marked.map(|()| value))
        });
        match result {
            Ok(value) => {
                self.rules_error = None;
                cx.notify();
                Some(value)
            }
            Err(error) => {
                self.rules_error = Some(error.to_string().into());
                cx.notify();
                None
            }
        }
    }

    fn rule(&self, id: &str, cx: &App) -> Option<AiRule> {
        crate::db::shared(cx).read(cx).store.ai_rule(id).ok()
    }

    /// 「新規ルール」を足して開き、名前を全部選んで書けるようにする。
    pub fn add_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_rule(cx);
        let scope = self.rules_scope.clone();
        let Some(rule) = self.write_rules(cx, |s| s.create_ai_rule(scope.as_deref(), "", ""))
        else {
            return;
        };
        self.open_rule_now(&rule, window, cx);
        let input = self.rule_name.clone();
        // メニューやボタンからフォーカスが戻ったあとに移す
        cx.defer_in(window, move |this, window, cx| {
            if this.rule_open.is_none() {
                return;
            }
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        });
    }

    fn open_rule_now(&mut self, rule: &AiRule, window: &mut Window, cx: &mut Context<Self>) {
        self.rule_open = Some(rule.id.clone());
        let (name, body) = (rule.name.clone(), rule.body.clone());
        self.rule_name
            .update(cx, |s, cx| s.set_value(name, window, cx));
        self.rule_body
            .update(cx, |s, cx| s.set_value(body, window, cx));
        cx.notify();
    }

    /// 行を押した。閉じているなら開き、開いているなら保存して閉じる。
    pub fn toggle_rule(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.rule_open.as_deref() == Some(id);
        self.save_open_rule(cx);
        if same {
            self.rule_open = None;
            cx.notify();
            return;
        }
        if let Some(rule) = self.rule(id, cx) {
            self.open_rule_now(&rule, window, cx);
        }
    }

    /// 開いているルールの、書いた名前と本文を保存する。
    fn save_open_rule(&mut self, cx: &mut Context<Self>) {
        self.save_rule_name(cx);
        self.save_rule_body(cx);
    }

    /// 書いた名前を保存する。空にしたときと変えていないときは、今の名前のまま。
    pub(super) fn save_rule_name(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.rule_open.clone() else {
            return;
        };
        let Some(rule) = self.rule(&id, cx) else {
            return;
        };
        let name = self.rule_name.read(cx).value().trim().to_owned();
        if name.is_empty() || name == rule.name {
            return;
        }
        self.write_rules(cx, |s| {
            s.update_ai_rule(
                &id,
                &RulePatch {
                    name: Some(name),
                    ..RulePatch::default()
                },
            )
        });
    }

    pub(super) fn save_rule_body(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.rule_open.clone() else {
            return;
        };
        let Some(rule) = self.rule(&id, cx) else {
            return;
        };
        let body = self.rule_body.read(cx).value().to_string();
        if body.trim() == rule.body {
            return;
        }
        self.write_rules(cx, |s| {
            s.update_ai_rule(
                &id,
                &RulePatch {
                    body: Some(body),
                    ..RulePatch::default()
                },
            )
        });
    }

    /// Esc。書きかけの名前と本文を捨てて、ルールを閉じる。
    pub fn cancel_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.rule_open.take() else {
            return;
        };
        if let Some(rule) = self.rule(&id, cx) {
            self.rule_name
                .update(cx, |s, cx| s.set_value(rule.name, window, cx));
            self.rule_body
                .update(cx, |s, cx| s.set_value(rule.body, window, cx));
        }
        self.rules_error = None;
        cx.notify();
    }

    fn set_rule(&mut self, id: &str, patch: RulePatch, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.write_rules(cx, |s| s.update_ai_rule(&id, &patch));
    }

    fn delete_rule(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.rule_open.as_deref() == Some(id) {
            self.rule_open = None;
        }
        let id = id.to_owned();
        self.write_rules(cx, |s| s.delete_ai_rule(&id));
    }

    /// ルールの一覧。`picker` のときは、上に場所（グローバルかワークスペース）を選ぶボタンを出す。
    pub fn rules_editor(&self, picker: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = &crate::db::shared(cx).read(cx).store;
        let scope = self.rules_scope.clone();
        let loaded = store
            .workspaces()
            .and_then(|all| Ok((all, store.ai_rules(scope.as_deref())?)));
        let (workspaces, rules) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                return div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(format!("AI のルールを読めませんでした: {error}"))
                    .into_any_element();
            }
        };
        // 消えたワークスペースを見ていたら、グローバルに戻して出す
        let scope = scope.filter(|id| workspaces.iter().any(|w| &w.id == id));
        let tree = workspace_tree(&workspaces);
        let shown: SharedString = match &scope {
            None => "すべてのワークスペース".into(),
            Some(id) => tree
                .iter()
                .find(|(w, _)| &w.id == id)
                .map(|(w, _)| w.name.clone())
                .unwrap_or_default()
                .into(),
        };
        let settings = cx.entity().downgrade();
        let scope_menu = Button::new("ai-rules-scope")
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
                    let choices = std::iter::once((None, "すべてのワークスペース".to_owned(), 0))
                        .chain(
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
                                    this.save_open_rule(cx);
                                    this.show_rules(id.clone(), cx)
                                });
                            },
                        ));
                    }
                    menu
                }
            });
        let add = feel::gloss(
            Button::new("ai-rule-add")
                .icon(app_icon("plus"))
                .tooltip("ルールを追加")
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, window, cx| this.add_rule(window, cx))),
        );
        let top = row()
            .gap_1()
            .when(picker, |d| d.child(scope_menu))
            .child(div().flex_1())
            .child(add);

        let mut list = col()
            .id("ai-rules")
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .overflow_hidden();
        for (ix, rule) in rules.iter().enumerate() {
            list = list.child(self.rule_row(ix, rule, cx));
        }
        col()
            .w_full()
            .gap_2()
            .child(top)
            .when(!rules.is_empty(), |d| d.child(list.test_support()))
            .when_some(self.rules_error.clone(), |d, error| {
                d.child(
                    div()
                        .id("ai-rules-error")
                        .text_xs()
                        .text_color(theme.danger)
                        .child(error)
                        .test_support(),
                )
            })
            .into_any_element()
    }

    fn rule_row(&self, ix: usize, rule: &AiRule, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.rule_open.as_deref() == Some(rule.id.as_str());
        let id = rule.id.clone();
        let group = SharedString::from(format!("ai-rule-{ix}"));
        // 名前のところ。開いているときは、同じ大きさ・位置のまま入力欄になる
        let name = if open {
            div()
                .flex_1()
                .min_w_0()
                .mx(px(-8.))
                .child(
                    Input::new(&self.rule_name)
                        .id("ai-rule-name")
                        .aria_label("ルールの名前")
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
                .when(!rule.enabled, |d| d.text_color(theme.muted_foreground))
                .child(rule.name.clone())
                .into_any_element()
        };
        let head = row()
            .id(("ai-rule", ix))
            .flex_1()
            .min_w_0()
            .gap_2()
            .py_1()
            .cursor_pointer()
            .child(
                div()
                    .id(("ai-rule-fold", ix))
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
            .child(name)
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _: &ClickEvent, window, cx| {
                    // 開いている間に名前の欄を押したときは、書き続ける
                    if this.rule_open.as_deref() == Some(id.as_str())
                        && this.rule_name.read(cx).focus_handle(cx).is_focused(window)
                    {
                        return;
                    }
                    this.toggle_rule(&id, window, cx)
                }
            }))
            .test_support();
        let settings = cx.entity().downgrade();
        let current = rule.apply;
        let apply = Button::new(("ai-rule-apply", ix))
            .ghost()
            .xsmall()
            .label(current.label())
            .dropdown_caret(true)
            .tooltip("使う場面")
            .pop_menu({
                let id = id.clone();
                move |mut menu, _, _| {
                    for choice in Apply::ALL {
                        let settings = settings.clone();
                        let id = id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(choice.label())
                                .checked(choice == current)
                                .on_click(move |_, _, cx| {
                                    // 設定を閉じたあとなら、反映する先がない
                                    let _ = settings.update(cx, |this, cx| {
                                        this.set_rule(
                                            &id,
                                            RulePatch {
                                                apply: Some(choice),
                                                ..RulePatch::default()
                                            },
                                            cx,
                                        )
                                    });
                                }),
                        );
                    }
                    menu
                }
            });
        let switch = Switch::new(("ai-rule-on", ix))
            .checked(rule.enabled)
            .small()
            .tooltip(if rule.enabled {
                "使う"
            } else {
                "使わない"
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, on: &bool, _, cx| {
                    this.set_rule(
                        &id,
                        RulePatch {
                            enabled: Some(*on),
                            ..RulePatch::default()
                        },
                        cx,
                    )
                }
            }));
        let delete = Button::new(("ai-rule-delete", ix))
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
                move |this, _, _, cx| this.delete_rule(&id, cx)
            }));
        let line = row()
            .group(group)
            .gap_1()
            .px_3()
            .min_h(px(36.))
            .lit(theme.muted, cx)
            .child(head)
            .child(apply)
            .child(switch)
            .child(feel::gloss(delete));
        col()
            .when(ix > 0, |d| d.border_t_1().border_color(theme.border))
            .when(open, |d| d.bg(theme.accent.opacity(0.4)))
            .child(line)
            .when(open, |d| {
                d.child(
                    div()
                        .id("ai-rule-body")
                        .px_3()
                        .pb_3()
                        .child(
                            Textarea::new(&self.rule_body)
                                .h(px(120.))
                                .bg(theme.background),
                        )
                        .test_support(),
                )
            })
            // Esc は、書きかけを捨ててルールを閉じる。設定のウィンドウは閉じない
            .on_action(
                cx.listener(|this, _: &crate::Dismiss, window, cx| this.cancel_rule(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    this.cancel_rule(window, cx)
                }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::workspace_tree;
    use malakt_core::Store;

    #[test]
    fn workspaces_are_listed_parent_first() {
        let mut store = Store::open_in_memory().unwrap();
        let work = store.create_workspace(None, "仕事").unwrap();
        store.create_workspace(None, "家").unwrap();
        store.create_workspace(Some(&work.id), "チーム").unwrap();
        let tree: Vec<(String, usize)> = workspace_tree(&store.workspaces().unwrap())
            .into_iter()
            .map(|(w, depth)| (w.name, depth))
            .collect();
        assert_eq!(
            tree,
            vec![
                ("仕事".to_owned(), 0),
                ("チーム".to_owned(), 1),
                ("家".to_owned(), 0)
            ]
        );
    }
}
