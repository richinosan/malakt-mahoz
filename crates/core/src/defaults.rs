//! ワークスペースごとの「タスク追加時の既定値」と、ワークスペースの基準日。
//!
//! 既定値は、タスクを足す画面が入力欄に入れてから作る。あとから変えても、
//! すでにあるタスクは変わらない。

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::*;
use crate::store::required_line;
use crate::text::{normalize, normalize_line};
use crate::view::add_days;
use crate::{Error, Result, Store};

/// 追加行で前に選んだ値。
#[derive(serde::Serialize, serde::Deserialize)]
struct QuickChoice {
    status: Status,
    priority: u8,
    assignee_ids: Vec<Id>,
}

fn quick_key(workspace_id: Option<&str>) -> String {
    format!("quick_choice:{}", workspace_id.unwrap_or(""))
}

/// 入力を整えて確かめる。エラーは画面に出す文。
pub fn clean(input: &TaskDefaults) -> Result<TaskDefaults> {
    let unique = |ids: &[Id]| {
        let mut out: Vec<Id> = Vec::new();
        for id in ids {
            if !id.is_empty() && !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    };
    let due = match &input.due {
        DueRule::None => DueRule::None,
        DueRule::Relative { days } => {
            if days.abs() > MAX_DUE_OFFSET_DAYS {
                return Err(Error::Invalid(format!(
                    "基準日からの日数は {MAX_DUE_OFFSET_DAYS} 日以内にしてください"
                )));
            }
            DueRule::Relative { days: *days }
        }
        DueRule::Fixed { date } => {
            let date = date.trim();
            if !is_valid_date(date) {
                return Err(Error::Invalid(
                    "期限の日付を YYYY-MM-DD で入れてください".into(),
                ));
            }
            DueRule::Fixed {
                date: date.to_owned(),
            }
        }
    };
    let due_time = input.due_time.trim();
    let due_time = if due == DueRule::None {
        String::new()
    } else if due_time.is_empty() || is_valid_time(due_time) {
        due_time.to_owned()
    } else {
        return Err(Error::Invalid("期限の時刻を HH:MM で入れてください".into()));
    };
    if input.priority.is_some_and(|p| p > 9) {
        return Err(Error::Invalid("優先度は 0 から 9 です".into()));
    }
    let link_workspace_ids: Vec<Id> = unique(&input.link_workspace_ids)
        .into_iter()
        .filter(|id| *id != input.workspace_id)
        .collect();
    if link_workspace_ids.len() > MAX_DEFAULT_LINKS {
        return Err(Error::Invalid(format!(
            "一緒に表示するワークスペースは {MAX_DEFAULT_LINKS} 件までです"
        )));
    }
    let mut categories: Vec<String> = Vec::new();
    for c in &input.categories {
        let c = normalize_line(c);
        if !c.is_empty() && !categories.contains(&c) {
            categories.push(c);
        }
    }
    Ok(TaskDefaults {
        workspace_id: required_line(&input.workspace_id, "ワークスペース")?,
        assignee_ids: unique(&input.assignee_ids),
        due,
        due_time,
        priority: input.priority,
        categories,
        description: normalize(&input.description).trim().to_owned(),
        link_workspace_ids,
    })
}

/// 保存する。何も入れない設定なら、行ごと消す。
pub(crate) fn save(conn: &Connection, d: &TaskDefaults) -> Result<()> {
    if d.has_any() {
        conn.prepare_cached(
            "INSERT INTO task_defaults(workspace_id, data) VALUES (?1, ?2) \
             ON CONFLICT(workspace_id) DO UPDATE SET data = excluded.data",
        )?
        .execute(params![d.workspace_id, serde_json::to_string(d)?])?;
    } else {
        conn.execute(
            "DELETE FROM task_defaults WHERE workspace_id = ?1",
            params![d.workspace_id],
        )?;
    }
    Ok(())
}

impl Store {
    /// ワークスペースの所要時間の基準を決める。`None` は全体の標準に従う。
    pub fn set_workspace_time_basis(
        &mut self,
        id: &str,
        basis: Option<crate::timing::Basis>,
    ) -> Result<Workspace> {
        let n = self.conn.execute(
            "UPDATE workspaces SET time_basis = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, basis.map(|b| b.code()), now_ms()],
        )?;
        if n == 0 {
            return Err(Error::NotFound(format!("ワークスペース {id}")));
        }
        self.workspace(id)
    }

    /// 全体の標準の、所要時間の基準。
    pub fn time_basis_standard(&self) -> Result<crate::timing::Basis> {
        Ok(self
            .setting(crate::timing::BASIS_SETTING)?
            .as_deref()
            .and_then(crate::timing::Basis::parse)
            .unwrap_or_default())
    }

    /// 全体の標準を変える。
    pub fn set_time_basis_standard(&mut self, basis: crate::timing::Basis) -> Result<()> {
        let code = basis.code();
        self.set_setting(crate::timing::BASIS_SETTING, Some(&code))
    }

    /// ワークスペースの基準日（開催日など）を決める。`None` で消す。
    pub fn set_workspace_date(&mut self, id: &str, date: Option<&str>) -> Result<Workspace> {
        let date = match date.map(str::trim) {
            None | Some("") => None,
            Some(d) if is_valid_date(d) => Some(d),
            Some(d) => {
                return Err(Error::Invalid(format!(
                    "基準日の日付が正しくありません: {d}"
                )))
            }
        };
        let n = self.conn.execute(
            "UPDATE workspaces SET date = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, date, now_ms()],
        )?;
        if n == 0 {
            return Err(Error::NotFound(format!("ワークスペース {id}")));
        }
        self.workspace(id)
    }

    /// そのワークスペースか、いちばん近い親の基準日。
    pub fn workspace_date(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .workspace_path(id)?
            .into_iter()
            .rev()
            .find_map(|w| w.date))
    }

    /// 既定値。なければ空。いなくなった担当者や、消えたワークスペースは除いて返す。
    pub fn task_defaults(&self, workspace_id: &str) -> Result<TaskDefaults> {
        self.workspace(workspace_id)?;
        let data: Option<String> = self
            .conn
            .prepare_cached("SELECT data FROM task_defaults WHERE workspace_id = ?1")?
            .query_row(params![workspace_id], |r| r.get(0))
            .optional()?;
        let mut defaults: TaskDefaults = match data {
            Some(data) => serde_json::from_str(&data)?,
            None => TaskDefaults::default(),
        };
        defaults.workspace_id = workspace_id.to_owned();
        let people = self.people()?;
        defaults
            .assignee_ids
            .retain(|id| people.iter().any(|p| p.id == *id));
        let workspaces = self.workspaces()?;
        defaults
            .link_workspace_ids
            .retain(|id| id != workspace_id && workspaces.iter().any(|w| w.id == *id));
        Ok(defaults)
    }

    pub fn set_task_defaults(&mut self, defaults: &TaskDefaults) -> Result<TaskDefaults> {
        let cleaned = clean(defaults)?;
        self.workspace(&cleaned.workspace_id)?;
        let people = self.people()?;
        if let Some(id) = cleaned
            .assignee_ids
            .iter()
            .find(|id| !people.iter().any(|p| p.id == **id))
        {
            return Err(Error::NotFound(format!("担当者 {id}")));
        }
        for id in &cleaned.link_workspace_ids {
            self.workspace(id)?;
        }
        save(&self.conn, &cleaned)?;
        // 既定値を決め直したら、追加行で前に選んだ値よりそちらを使う
        self.set_setting(&quick_key(Some(&cleaned.workspace_id)), None)?;
        self.task_defaults(&cleaned.workspace_id)
    }

    /// 既定値を入れた、新しいタスクの下書き。ワークスペースの外なら何も入れない。
    pub fn draft_for(&self, workspace_id: Option<&str>, title: &str) -> Result<TaskDraft> {
        let mut draft = TaskDraft {
            title: title.to_owned(),
            ..Default::default()
        };
        let Some(workspace_id) = workspace_id else {
            return Ok(draft);
        };
        let defaults = self.task_defaults(workspace_id)?;
        let date = match &defaults.due {
            DueRule::None => None,
            DueRule::Fixed { date } => Some(date.clone()),
            DueRule::Relative { days } => self
                .workspace_date(workspace_id)?
                .map(|base| add_days(&base, i64::from(*days))),
        };
        draft.due = date.map(|date| {
            if defaults.due_time.is_empty() {
                date
            } else {
                format!("{date} {}", defaults.due_time)
            }
        });
        draft.assignee_ids = defaults.assignee_ids;
        draft.priority = defaults.priority.unwrap_or(draft.priority);
        draft.categories = defaults.categories;
        draft.description = defaults.description;
        Ok(draft)
    }

    /// 一覧の追加行に最初に入れる値。前にその行で選んだ状態・優先度・担当があればそれを、
    /// なければワークスペースの既定値を使う。担当がどれにもなければ自分にする。
    pub fn quick_draft(&self, workspace_id: Option<&str>) -> Result<TaskDraft> {
        let mut draft = self.draft_for(workspace_id, "")?;
        let people = self.people()?;
        let remembered = self
            .setting(&quick_key(workspace_id))?
            .and_then(|json| serde_json::from_str::<QuickChoice>(&json).ok());
        if let Some(choice) = remembered {
            draft.status = choice.status;
            draft.priority = choice.priority;
            draft.assignee_ids = choice
                .assignee_ids
                .into_iter()
                .filter(|id| people.iter().any(|p| &p.id == id))
                .collect();
        } else if draft.assignee_ids.is_empty() {
            if let Some(me) = people.iter().find(|p| p.is_self) {
                draft.assignee_ids.push(me.id.clone());
            }
        }
        Ok(draft)
    }

    /// 追加行で選んだ状態・優先度・担当を、ワークスペースごとに覚える。
    pub fn remember_quick(&mut self, workspace_id: Option<&str>, draft: &TaskDraft) -> Result<()> {
        let choice = QuickChoice {
            status: draft.status,
            priority: draft.priority,
            assignee_ids: draft.assignee_ids.clone(),
        };
        let json = serde_json::to_string(&choice)?;
        self.set_setting(&quick_key(workspace_id), Some(&json))
    }

    pub(crate) fn all_task_defaults(&self) -> Result<Vec<TaskDefaults>> {
        let mut stmt = self
            .conn
            .prepare("SELECT workspace_id, data FROM task_defaults ORDER BY workspace_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (workspace_id, data) = row?;
            let mut d: TaskDefaults = serde_json::from_str(&data)?;
            d.workspace_id = workspace_id;
            out.push(d);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::model::*;
    use crate::{Error, Store};

    fn setup() -> (Store, Workspace) {
        let mut s = Store::open_in_memory().unwrap();
        let ws = s.create_workspace(None, "冬のイベント").unwrap();
        (s, ws)
    }

    #[test]
    fn empty_by_default_and_nothing_is_filled() {
        let (s, ws) = setup();
        let d = s.task_defaults(&ws.id).unwrap();
        assert!(!d.has_any());
        assert_eq!(d.workspace_id, ws.id);
        let draft = s.draft_for(Some(&ws.id), "会場を探す").unwrap();
        assert_eq!(
            draft,
            TaskDraft {
                title: "会場を探す".into(),
                ..Default::default()
            }
        );
        assert_eq!(s.draft_for(None, "メモ").unwrap().title, "メモ");
        assert!(matches!(s.task_defaults("ない"), Err(Error::NotFound(_))));
    }

    #[test]
    fn defaults_fill_a_new_task() {
        let (mut s, ws) = setup();
        let sato = s.create_person("佐藤", false).unwrap();
        let saved = s
            .set_task_defaults(&TaskDefaults {
                workspace_id: ws.id.clone(),
                assignee_ids: vec![sato.id.clone(), sato.id.clone()],
                due: DueRule::Fixed {
                    date: " 2026-12-20 ".into(),
                },
                due_time: "18:00".into(),
                priority: Some(1),
                categories: vec![" 準備 ".into(), "準備".into(), "".into()],
                description: "\r\n## 目的\r\n\r\n## 手順\r\n".into(),
                link_workspace_ids: vec![ws.id.clone()],
            })
            .unwrap();
        assert_eq!(saved.assignee_ids, [sato.id.clone()]);
        assert_eq!(saved.categories, ["準備"]);
        assert_eq!(saved.description, "## 目的\n\n## 手順");
        // 自分自身は「一緒に表示する先」にならない
        assert!(saved.link_workspace_ids.is_empty());
        let draft = s.draft_for(Some(&ws.id), "会場を探す").unwrap();
        assert_eq!(draft.due.as_deref(), Some("2026-12-20 18:00"));
        assert_eq!(draft.priority, 1);
        let task = s.create_task(Some(&ws.id), &draft).unwrap();
        assert_eq!(task.assignee_ids, [sato.id.clone()]);
        assert_eq!(task.categories, ["準備"]);
        assert_eq!(task.description, "## 目的\n\n## 手順");
        assert_eq!(task.status, Status::NeedsAction);
        // 既定値を変えても、すでにあるタスクは変わらない
        s.set_task_defaults(&TaskDefaults {
            workspace_id: ws.id.clone(),
            ..Default::default()
        })
        .unwrap();
        assert!(!s.task_defaults(&ws.id).unwrap().has_any());
        assert_eq!(s.task(&task.id).unwrap().priority, 1);
    }

    #[test]
    fn relative_due_follows_the_base_date_of_the_nearest_workspace() {
        let (mut s, ws) = setup();
        let child = s.create_workspace(Some(&ws.id), "会場").unwrap();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: child.id.clone(),
            due: DueRule::Relative { days: -7 },
            ..Default::default()
        })
        .unwrap();
        // 基準日がまだなければ、期限は入れない
        assert_eq!(s.draft_for(Some(&child.id), "下見").unwrap().due, None);
        s.set_workspace_date(&ws.id, Some("2027-01-03")).unwrap();
        assert_eq!(
            s.draft_for(Some(&child.id), "下見").unwrap().due.as_deref(),
            Some("2026-12-27")
        );
        // 自分の基準日があれば、そちらを使う
        s.set_workspace_date(&child.id, Some("2026-03-01")).unwrap();
        assert_eq!(
            s.draft_for(Some(&child.id), "下見").unwrap().due.as_deref(),
            Some("2026-02-22")
        );
        s.set_workspace_date(&child.id, None).unwrap();
        assert_eq!(s.workspace(&child.id).unwrap().date, None);
        assert!(matches!(
            s.set_workspace_date(&ws.id, Some("2026-02-30")),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.set_workspace_date("ない", None),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let (mut s, ws) = setup();
        let with = |f: &dyn Fn(&mut TaskDefaults)| {
            let mut d = TaskDefaults {
                workspace_id: ws.id.clone(),
                ..Default::default()
            };
            f(&mut d);
            d
        };
        let bad = [
            with(&|d| d.due = DueRule::Relative { days: 3651 }),
            with(&|d| d.due = DueRule::Relative { days: -3651 }),
            with(&|d| {
                d.due = DueRule::Fixed {
                    date: "来週".into(),
                }
            }),
            with(&|d| {
                d.due = DueRule::Relative { days: 0 };
                d.due_time = "25:00".into();
            }),
            with(&|d| d.priority = Some(10)),
            with(&|d| d.link_workspace_ids = (0..21).map(|i| format!("w{i}")).collect()),
        ];
        for d in &bad {
            assert!(
                matches!(s.set_task_defaults(d), Err(Error::Invalid(_))),
                "{d:?}"
            );
        }
        assert!(matches!(
            s.set_task_defaults(&with(&|d| d.assignee_ids = vec!["ない".into()])),
            Err(Error::NotFound(_))
        ));
        // 上限ちょうどは通る。期限なしなら時刻は捨てる
        let ok = s
            .set_task_defaults(&with(&|d| {
                d.due = DueRule::Relative { days: -3650 };
            }))
            .unwrap();
        assert_eq!(ok.due, DueRule::Relative { days: -3650 });
        let none = super::clean(&with(&|d| d.due_time = "09:00".into())).unwrap();
        assert_eq!(none.due_time, "");
    }

    #[test]
    fn removed_people_and_workspaces_drop_out() {
        let (mut s, ws) = setup();
        let other = s.create_workspace(None, "広報").unwrap();
        let sato = s.create_person("佐藤", false).unwrap();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: ws.id.clone(),
            assignee_ids: vec![sato.id.clone()],
            link_workspace_ids: vec![other.id.clone()],
            priority: Some(5),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            s.task_defaults(&ws.id).unwrap().link_workspace_ids,
            [other.id.clone()]
        );
        s.delete_person(&sato.id).unwrap();
        s.delete_workspace(&other.id).unwrap();
        let d = s.task_defaults(&ws.id).unwrap();
        assert!(d.assignee_ids.is_empty() && d.link_workspace_ids.is_empty());
        assert_eq!(d.priority, Some(5));
        // ワークスペースを消すと、既定値も消える
        s.delete_workspace(&ws.id).unwrap();
        assert!(s.all_task_defaults().unwrap().is_empty());
    }

    #[test]
    fn export_and_import_keep_dates_and_defaults() {
        let (mut s, ws) = setup();
        s.set_workspace_date(&ws.id, Some("2027-01-03")).unwrap();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: ws.id.clone(),
            due: DueRule::Relative { days: -3 },
            due_time: "12:00".into(),
            ..Default::default()
        })
        .unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&s, &mut zip).unwrap();
        let mut other = Store::open_in_memory().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut other, zip).unwrap();
        assert_eq!(
            other.workspace(&ws.id).unwrap().date.as_deref(),
            Some("2027-01-03")
        );
        assert_eq!(
            other.task_defaults(&ws.id).unwrap(),
            s.task_defaults(&ws.id).unwrap()
        );
        assert_eq!(
            other.draft_for(Some(&ws.id), "x").unwrap().due.as_deref(),
            Some("2026-12-31 12:00")
        );
    }

    #[test]
    fn quick_draft_starts_with_me_and_remembers_the_choice() {
        let mut store = Store::open_in_memory().unwrap();
        store.ensure_self().unwrap();
        let me = store.people().unwrap().remove(0);
        let sato = store.create_person("佐藤", false).unwrap();
        let ws = store.create_workspace(None, "仕事").unwrap();

        // 何も決めていなければ、担当は自分
        let draft = store.quick_draft(Some(&ws.id)).unwrap();
        assert_eq!(draft.assignee_ids, vec![me.id.clone()]);
        assert_eq!(
            store.quick_draft(None).unwrap().assignee_ids,
            vec![me.id.clone()]
        );

        // 選んだ値は、そのワークスペースでだけ残る。担当なしも選んだ値として残る
        let mut chosen = draft.clone();
        chosen.status = Status::InProcess;
        chosen.priority = 1;
        chosen.assignee_ids = vec![sato.id.clone()];
        store.remember_quick(Some(&ws.id), &chosen).unwrap();
        let draft = store.quick_draft(Some(&ws.id)).unwrap();
        assert_eq!(draft.status, Status::InProcess);
        assert_eq!(draft.priority, 1);
        assert_eq!(draft.assignee_ids, vec![sato.id.clone()]);
        assert_eq!(
            store.quick_draft(None).unwrap().assignee_ids,
            vec![me.id.clone()]
        );
        chosen.assignee_ids.clear();
        store.remember_quick(None, &chosen).unwrap();
        assert!(store.quick_draft(None).unwrap().assignee_ids.is_empty());

        // 消した担当者は入れない
        store.delete_person(&sato.id).unwrap();
        assert!(store
            .quick_draft(Some(&ws.id))
            .unwrap()
            .assignee_ids
            .is_empty());

        // 既定値を決め直したら、そちらを使う
        store
            .set_task_defaults(&TaskDefaults {
                workspace_id: ws.id.clone(),
                assignee_ids: vec![me.id.clone()],
                priority: Some(5),
                ..Default::default()
            })
            .unwrap();
        let draft = store.quick_draft(Some(&ws.id)).unwrap();
        assert_eq!(draft.assignee_ids, vec![me.id.clone()]);
        assert_eq!(draft.priority, 5);
        assert_eq!(draft.status, Status::NeedsAction);

        // ワークスペースを消すと、覚えた値も消える
        store.remember_quick(Some(&ws.id), &chosen).unwrap();
        store.delete_workspace(&ws.id).unwrap();
        assert_eq!(
            store.setting(&super::quick_key(Some(&ws.id))).unwrap(),
            None
        );
    }

    #[test]
    fn 優先度は9まで入れられる() {
        use super::clean;
        let base = TaskDefaults {
            workspace_id: "w".into(),
            ..Default::default()
        };
        let ok = clean(&TaskDefaults {
            priority: Some(9),
            ..base.clone()
        })
        .unwrap();
        assert_eq!(ok.priority, Some(9));
        assert!(clean(&TaskDefaults {
            priority: Some(10),
            ..base
        })
        .is_err());
    }

    #[test]
    fn 一緒に表示するワークスペースは上限ちょうどまで選べる() {
        use super::clean;
        let links = |n: usize| TaskDefaults {
            workspace_id: "w".into(),
            link_workspace_ids: (0..n).map(|i| format!("l{i}")).collect(),
            ..Default::default()
        };
        assert_eq!(
            clean(&links(MAX_DEFAULT_LINKS))
                .unwrap()
                .link_workspace_ids
                .len(),
            MAX_DEFAULT_LINKS
        );
        assert!(clean(&links(MAX_DEFAULT_LINKS + 1)).is_err());
    }
}
