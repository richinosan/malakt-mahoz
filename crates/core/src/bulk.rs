//! 複数のタスクをまとめて変える操作と、一覧での選択。

use std::collections::HashSet;

use crate::model::*;
use crate::text::normalize_line;
use crate::{Result, Store};

/// 担当者などの一覧の変え方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListMode {
    /// いまの一覧に足す
    #[default]
    Add,
    /// いまの一覧から外す
    Remove,
    /// 一覧をまるごと置き換える
    Set,
}

impl ListMode {
    pub const ALL: [ListMode; 3] = [ListMode::Add, ListMode::Remove, ListMode::Set];

    pub fn label(self) -> &'static str {
        match self {
            ListMode::Add => "追加",
            ListMode::Remove => "外す",
            ListMode::Set => "置き換え",
        }
    }
}

/// まとめて当てる変更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkOp {
    Status(Status),
    Priority(u8),
    /// `None` で期限をなくす。
    Due(Option<String>),
    Assignees {
        mode: ListMode,
        ids: Vec<Id>,
    },
    /// カテゴリは足すか外すかだけ（置き換えはない）。
    Categories {
        add: bool,
        values: Vec<String>,
    },
}

impl BulkOp {
    /// 1 件のタスクをどう変えるか。
    pub fn apply(&self, draft: &mut TaskDraft) {
        match self {
            BulkOp::Status(status) => draft.status = *status,
            BulkOp::Priority(priority) => draft.priority = *priority,
            BulkOp::Due(due) => draft.due = due.clone(),
            BulkOp::Assignees { mode, ids } => match mode {
                ListMode::Add => {
                    for id in ids {
                        if !draft.assignee_ids.contains(id) {
                            draft.assignee_ids.push(id.clone());
                        }
                    }
                }
                ListMode::Remove => draft.assignee_ids.retain(|id| !ids.contains(id)),
                ListMode::Set => draft.assignee_ids = ids.clone(),
            },
            BulkOp::Categories { add, values } => {
                let values: Vec<String> = values
                    .iter()
                    .map(|v| normalize_line(v))
                    .filter(|v| !v.is_empty())
                    .collect();
                if *add {
                    for value in values {
                        if !draft.categories.contains(&value) {
                            draft.categories.push(value);
                        }
                    }
                } else {
                    draft.categories.retain(|c| !values.contains(c));
                }
            }
        }
    }
}

impl Store {
    /// 選んだタスクすべてに同じ変更を当てる。変わったタスクの数を返す。
    /// 途中で失敗したら、そこで止めてエラーを返す。
    pub fn bulk_apply(&mut self, ids: &[Id], op: &BulkOp) -> Result<usize> {
        let mut changed = 0;
        for id in ids {
            let before = self.task(id)?;
            let mut draft = TaskDraft::from(&before);
            op.apply(&mut draft);
            if self.update_task(id, &draft)? != before {
                changed += 1;
            }
        }
        Ok(changed)
    }
}

/// 1 件の選択を反転する。
pub fn toggle(selected: &mut HashSet<Id>, id: &str) {
    if !selected.remove(id) {
        selected.insert(id.to_owned());
    }
}

/// Shift を押しながらの選択。押した行の新しい状態（いまの逆）を、起点の行から
/// 押した行まで（表示中の並びで、両端を含む）に当てる。起点がないか表示中に
/// なければ、押した行だけを反転する。
pub fn toggle_range(selected: &mut HashSet<Id>, visible: &[&Id], anchor: Option<&str>, id: &str) {
    let position = |target: &str| visible.iter().position(|v| v.as_str() == target);
    let (Some(from), Some(to)) = (anchor.and_then(position), position(id)) else {
        toggle(selected, id);
        return;
    };
    let select = !selected.contains(id);
    for target in &visible[from.min(to)..=from.max(to)] {
        if select {
            selected.insert((*target).clone());
        } else {
            selected.remove(target.as_str());
        }
    }
}

/// 見出しのチェックボックス。表示中がすべて選ばれていれば表示中を外し、
/// そうでなければ表示中をすべて選ぶ。
pub fn toggle_all(selected: &mut HashSet<Id>, visible: &[&Id]) {
    if !visible.is_empty() && visible.iter().all(|id| selected.contains(id.as_str())) {
        for id in visible {
            selected.remove(id.as_str());
        }
    } else {
        selected.extend(visible.iter().map(|id| (*id).clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(names: &[&str]) -> Vec<Id> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    fn set(names: &[&str]) -> HashSet<Id> {
        ids(names).into_iter().collect()
    }

    #[test]
    fn operations_change_one_draft() {
        let mut d = TaskDraft {
            title: "t".into(),
            assignee_ids: ids(&["a", "b"]),
            categories: vec!["準備".into()],
            ..Default::default()
        };
        let assignees = |mode, names: &[&str]| BulkOp::Assignees {
            mode,
            ids: ids(names),
        };
        assignees(ListMode::Add, &["b", "c"]).apply(&mut d);
        assert_eq!(d.assignee_ids, ids(&["a", "b", "c"]));
        assignees(ListMode::Remove, &["a", "x"]).apply(&mut d);
        assert_eq!(d.assignee_ids, ids(&["b", "c"]));
        assignees(ListMode::Set, &["z"]).apply(&mut d);
        assert_eq!(d.assignee_ids, ids(&["z"]));
        assignees(ListMode::Set, &[]).apply(&mut d);
        assert!(d.assignee_ids.is_empty());

        let categories = |add, names: &[&str]| BulkOp::Categories {
            add,
            values: names.iter().map(|s| (*s).to_owned()).collect(),
        };
        categories(true, &[" 会場 ", "準備", ""]).apply(&mut d);
        assert_eq!(d.categories, ["準備", "会場"]);
        categories(false, &["準備", "ない"]).apply(&mut d);
        assert_eq!(d.categories, ["会場"]);

        BulkOp::Due(Some("2026-12-24 18:00".into())).apply(&mut d);
        assert_eq!(d.due.as_deref(), Some("2026-12-24 18:00"));
        BulkOp::Due(None).apply(&mut d);
        assert_eq!(d.due, None);
        BulkOp::Priority(1).apply(&mut d);
        BulkOp::Status(Status::InProcess).apply(&mut d);
        assert_eq!((d.priority, d.status), (1, Status::InProcess));
    }

    #[test]
    fn bulk_apply_counts_only_changed_tasks_and_keeps_history() {
        let mut s = Store::open_in_memory().unwrap();
        let sato = s.create_person("佐藤", false).unwrap();
        let mut made = Vec::new();
        for (title, due) in [("a", Some("2026-12-24")), ("b", None), ("c", None)] {
            let draft = TaskDraft {
                title: title.into(),
                due: due.map(str::to_owned),
                ..Default::default()
            };
            made.push(s.create_task(None, &draft).unwrap().id);
        }
        let op = BulkOp::Due(Some("2026-12-24".into()));
        assert_eq!(s.bulk_apply(&made, &op).unwrap(), 2);
        assert_eq!(s.bulk_apply(&made, &op).unwrap(), 0);
        assert_eq!(s.changes(&made[0]).unwrap().len(), 0);
        assert_eq!(s.changes(&made[1]).unwrap().len(), 1);
        let op = BulkOp::Assignees {
            mode: ListMode::Add,
            ids: vec![sato.id.clone()],
        };
        assert_eq!(s.bulk_apply(&made[..2], &op).unwrap(), 2);
        assert_eq!(s.task(&made[1]).unwrap().assignee_ids, [sato.id.clone()]);
        assert!(s.task(&made[2]).unwrap().assignee_ids.is_empty());
        // 正しくない値は、どのタスクも変えずに断る
        let bad = BulkOp::Due(Some("来週".into()));
        assert!(s.bulk_apply(&made, &bad).is_err());
        assert_eq!(s.task(&made[2]).unwrap().due.as_deref(), Some("2026-12-24"));
    }

    #[test]
    fn selection() {
        let all = ids(&["a", "b", "c", "d", "e"]);
        let visible: Vec<&Id> = all.iter().collect();
        let mut sel = HashSet::new();
        toggle(&mut sel, "b");
        assert_eq!(sel, set(&["b"]));
        // 起点から押した行まで、上下どちらでも
        toggle_range(&mut sel, &visible, Some("b"), "d");
        assert_eq!(sel, set(&["b", "c", "d"]));
        toggle_range(&mut sel, &visible, Some("d"), "a");
        assert_eq!(sel, set(&["a", "b", "c", "d"]));
        // 選ばれている行を押すと、範囲を外す
        toggle_range(&mut sel, &visible, Some("a"), "c");
        assert_eq!(sel, set(&["d"]));
        // 起点がない・見えていないときは、その行だけ
        toggle_range(&mut sel, &visible, None, "a");
        toggle_range(&mut sel, &visible, Some("ない"), "e");
        assert_eq!(sel, set(&["a", "d", "e"]));

        toggle_all(&mut sel, &visible);
        assert_eq!(sel.len(), 5);
        toggle_all(&mut sel, &visible);
        assert!(sel.is_empty());
        toggle_all(&mut sel, &[]);
        assert!(sel.is_empty());

        // 見えていないものの選択には触れない
        let mut sel = set(&["a", "c", "x"]);
        toggle_all(&mut sel, &visible[..2]);
        assert_eq!(sel, set(&["a", "b", "c", "x"]));
        toggle_all(&mut sel, &visible[..2]);
        assert_eq!(sel, set(&["c", "x"]));
    }
}
