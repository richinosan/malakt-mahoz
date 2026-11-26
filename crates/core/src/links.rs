//! 1 つのタスクを、複数のワークスペースに表示する。
//!
//! タスクは 1 つのワークスペースに「入って」いて、ほかのワークスペースにも
//! 「表示」できる。どこから開いても同じタスクで、内容は 1 つ。

use rusqlite::{params, Connection};

use crate::model::*;
use crate::store::{insert_change, touch, trim_changes};
use crate::{Error, Result, Store};

pub(crate) fn insert(
    conn: &Connection,
    task_id: &str,
    workspace_id: &str,
    at: i64,
) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO task_links(task_id, workspace_id, created_at) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![task_id, workspace_id, at])
}

/// ワークスペース（と子孫）を消す前に、そこに入っているタスクのうち、
/// 外のワークスペースにも表示しているものを、そちらへ移しておく。
pub(crate) fn rehome_before_delete(conn: &Connection, workspace_id: &str) -> Result<()> {
    const SUB: &str = "WITH RECURSIVE sub(id) AS ( \
         SELECT id FROM workspaces WHERE id = ?1 \
         UNION ALL \
         SELECT w.id FROM workspaces w JOIN sub ON w.parent_id = sub.id) ";
    let moves: Vec<(Id, Id)> = conn
        .prepare(&format!(
            "{SUB} SELECT t.id, ( \
                 SELECT l.workspace_id FROM task_links l \
                 WHERE l.task_id = t.id AND l.workspace_id NOT IN (SELECT id FROM sub) \
                 ORDER BY l.created_at, l.workspace_id LIMIT 1) AS home \
             FROM tasks t WHERE t.workspace_id IN (SELECT id FROM sub)"
        ))?
        .query_map(params![workspace_id], |r| {
            Ok((r.get::<_, Id>(0)?, r.get::<_, Option<Id>>(1)?))
        })?
        .filter_map(|row| match row {
            Ok((task, Some(home))) => Some(Ok((task, home))),
            Ok((_, None)) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<_>>()?;
    for (task_id, home) in moves {
        conn.execute(
            "UPDATE tasks SET workspace_id = ?2 WHERE id = ?1",
            params![task_id, home],
        )?;
        conn.execute(
            "DELETE FROM task_links WHERE task_id = ?1 AND workspace_id = ?2",
            params![task_id, home],
        )?;
    }
    Ok(())
}

impl Store {
    /// タスクを、ほかのワークスペースにも表示する。すでに出ていれば何もしない。
    /// どこにも入っていないタスクは、そのワークスペースに入れる。
    pub fn link_task(&mut self, id: &str, workspace_id: &str) -> Result<Task> {
        let task = self.task(id)?;
        self.workspace(workspace_id)?;
        if task.workspace_id.is_none() {
            self.move_task(id, Some(workspace_id))?;
            return self.task(id);
        }
        if task.is_in(workspace_id) {
            return Ok(task);
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        insert(&tx, id, workspace_id, now)?;
        touch(&tx, id, now, &self.actor)?;
        insert_change(&tx, id, "link", "", workspace_id, now, &self.actor)?;
        trim_changes(&tx, id)?;
        tx.commit()?;
        self.task(id)
    }

    /// そのワークスペースに出すのをやめる。入っている場所から外すなら、
    /// 次の表示先に入れ直す。最後の 1 つは外せない（消すときはタスクを削除する）。
    pub fn unlink_task(&mut self, id: &str, workspace_id: &str) -> Result<Task> {
        let task = self.task(id)?;
        if !task.is_in(workspace_id) {
            return Ok(task);
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        if task.workspace_id.as_deref() == Some(workspace_id) {
            let Some(next) = task.linked_workspace_ids.first() else {
                return Err(Error::Invalid(
                    "最後の 1 つは外せません。消すときはタスクを削除してください".into(),
                ));
            };
            tx.execute(
                "UPDATE tasks SET workspace_id = ?2 WHERE id = ?1",
                params![id, next],
            )?;
            tx.execute(
                "DELETE FROM task_links WHERE task_id = ?1 AND workspace_id = ?2",
                params![id, next],
            )?;
        } else {
            tx.execute(
                "DELETE FROM task_links WHERE task_id = ?1 AND workspace_id = ?2",
                params![id, workspace_id],
            )?;
        }
        touch(&tx, id, now, &self.actor)?;
        insert_change(&tx, id, "link", workspace_id, "", now, &self.actor)?;
        trim_changes(&tx, id)?;
        tx.commit()?;
        self.task(id)
    }

    /// ワークスペースの既定値を入れてタスクを作る。既定の表示先にも出す。
    pub fn add_task_in(&mut self, workspace_id: Option<&str>, title: &str) -> Result<Task> {
        let draft = self.draft_for(workspace_id, title)?;
        self.create_task_shown(workspace_id, &draft)
    }

    /// 下書きのとおりにタスクを作り、ワークスペースの既定の表示先にも出す。
    pub fn create_task_shown(
        &mut self,
        workspace_id: Option<&str>,
        draft: &TaskDraft,
    ) -> Result<Task> {
        let links = match workspace_id {
            Some(w) => self.task_defaults(w)?.link_workspace_ids,
            None => Vec::new(),
        };
        let task = self.create_task(workspace_id, draft)?;
        if links.is_empty() {
            return Ok(task);
        }
        let tx = self.conn.transaction()?;
        for link in &links {
            insert(&tx, &task.id, link, task.created_at)?;
        }
        tx.commit()?;
        self.task(&task.id)
    }
}

#[cfg(test)]
mod tests {
    use crate::model::*;
    use crate::{Error, Store};

    fn setup() -> (Store, Workspace, Workspace, Task) {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_workspace(None, "冬のイベント").unwrap();
        let b = s.create_workspace(None, "広報").unwrap();
        let task = s
            .create_task(
                Some(&a.id),
                &TaskDraft {
                    title: "告知を出す".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        (s, a, b, task)
    }

    fn titles(s: &Store, ws: &str) -> Vec<String> {
        s.tasks(ws, &TaskFilter::default())
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
    }

    #[test]
    fn a_linked_task_shows_in_both_workspaces_and_is_one_task() {
        let (mut s, a, b, task) = setup();
        let linked = s.link_task(&task.id, &b.id).unwrap();
        assert_eq!(linked.workspace_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(linked.linked_workspace_ids, std::slice::from_ref(&b.id));
        assert!(linked.is_in(&a.id) && linked.is_in(&b.id));
        assert_eq!(titles(&s, &a.id), ["告知を出す"]);
        assert_eq!(titles(&s, &b.id), ["告知を出す"]);
        // 全体の一覧では 1 件のまま
        assert_eq!(
            s.tasks(Scope::All, &TaskFilter::default()).unwrap().len(),
            1
        );
        let counts = s.open_task_counts().unwrap();
        assert_eq!((counts.get(&a.id), counts.get(&b.id)), (Some(&1), Some(&1)));
        // どちらから完了にしても、両方で完了になる
        s.set_task_status(&task.id, Status::Completed).unwrap();
        assert!(s.open_task_counts().unwrap().is_empty());
        // 二重に足しても増えない
        s.link_task(&task.id, &b.id).unwrap();
        s.link_task(&task.id, &a.id).unwrap();
        assert_eq!(s.task(&task.id).unwrap().linked_workspace_ids.len(), 1);
        assert!(matches!(
            s.link_task(&task.id, "ない"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn descendants_view_finds_links_too() {
        let (mut s, a, b, task) = setup();
        let child = s.create_workspace(Some(&b.id), "SNS").unwrap();
        s.link_task(&task.id, &child.id).unwrap();
        assert!(titles(&s, &b.id).is_empty());
        let filter = TaskFilter {
            include_descendants: true,
            ..Default::default()
        };
        assert_eq!(s.tasks(b.id.as_str(), &filter).unwrap().len(), 1);
        assert_eq!(s.tasks(a.id.as_str(), &filter).unwrap().len(), 1);
    }

    #[test]
    fn unlinking_keeps_the_task_somewhere() {
        let (mut s, a, b, task) = setup();
        // 最後の 1 つは外せない
        assert!(matches!(
            s.unlink_task(&task.id, &a.id),
            Err(Error::Invalid(_))
        ));
        s.link_task(&task.id, &b.id).unwrap();
        // 入っている場所から外すと、表示先に入れ直す
        let t = s.unlink_task(&task.id, &a.id).unwrap();
        assert_eq!(t.workspace_id.as_deref(), Some(b.id.as_str()));
        assert!(t.linked_workspace_ids.is_empty());
        assert!(titles(&s, &a.id).is_empty());
        s.link_task(&task.id, &a.id).unwrap();
        let t = s.unlink_task(&task.id, &a.id).unwrap();
        assert_eq!(t.workspace_id.as_deref(), Some(b.id.as_str()));
        assert!(t.linked_workspace_ids.is_empty());
        // 出ていない場所を外しても何も起きない
        assert_eq!(s.unlink_task(&task.id, &a.id).unwrap(), t);
        let fields: Vec<String> = s
            .edits(&task.id)
            .unwrap()
            .into_iter()
            .map(|c| c.field)
            .collect();
        assert_eq!(fields.iter().filter(|f| *f == "link").count(), 4);
    }

    #[test]
    fn moving_and_unfiled_tasks() {
        let (mut s, a, b, task) = setup();
        s.link_task(&task.id, &b.id).unwrap();
        // 表示先へ移すと、表示は外れて 1 か所になる
        s.move_task(&task.id, Some(&b.id)).unwrap();
        let t = s.task(&task.id).unwrap();
        assert_eq!(t.workspace_id.as_deref(), Some(b.id.as_str()));
        assert!(t.linked_workspace_ids.is_empty());
        // どこにも入れない状態に戻すと、表示先も外れる
        s.link_task(&task.id, &a.id).unwrap();
        s.move_task(&task.id, None).unwrap();
        let t = s.task(&task.id).unwrap();
        assert!(t.workspace_id.is_none() && t.linked_workspace_ids.is_empty());
        // どこにも入っていないタスクを表示すると、そこに入る
        let t = s.link_task(&task.id, &a.id).unwrap();
        assert_eq!(t.workspace_id.as_deref(), Some(a.id.as_str()));
        assert!(t.linked_workspace_ids.is_empty());
    }

    #[test]
    fn deleting_a_workspace_keeps_tasks_shown_elsewhere() {
        let (mut s, a, b, task) = setup();
        let child = s.create_workspace(Some(&a.id), "会場").unwrap();
        let inside = s
            .create_task(
                Some(&child.id),
                &TaskDraft {
                    title: "下見".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let only_here = s
            .create_task(
                Some(&a.id),
                &TaskDraft {
                    title: "片付け".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        s.link_task(&task.id, &b.id).unwrap();
        // 消す範囲の中だけに出ているものは、一緒に消える
        s.link_task(&inside.id, &a.id).unwrap();
        s.delete_workspace(&a.id).unwrap();
        let t = s.task(&task.id).unwrap();
        assert_eq!(t.workspace_id.as_deref(), Some(b.id.as_str()));
        assert!(t.linked_workspace_ids.is_empty());
        assert!(s.task(&inside.id).is_err());
        assert!(s.task(&only_here.id).is_err());
        // 表示先のほうを消すと、表示だけが外れる
        let c = s.create_workspace(None, "物販").unwrap();
        s.link_task(&task.id, &c.id).unwrap();
        s.delete_workspace(&c.id).unwrap();
        assert!(s.task(&task.id).unwrap().linked_workspace_ids.is_empty());
    }

    #[test]
    fn default_links_apply_to_new_tasks() {
        let (mut s, a, b, _) = setup();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: a.id.clone(),
            link_workspace_ids: vec![b.id.clone()],
            priority: Some(1),
            ..Default::default()
        })
        .unwrap();
        let t = s.add_task_in(Some(&a.id), "チラシを刷る").unwrap();
        assert_eq!(t.linked_workspace_ids, std::slice::from_ref(&b.id));
        assert_eq!(t.priority, 1);
        assert!(titles(&s, &b.id).contains(&"チラシを刷る".to_owned()));
        let plain = s.add_task_in(None, "メモ").unwrap();
        assert!(plain.workspace_id.is_none() && plain.linked_workspace_ids.is_empty());
    }

    #[test]
    fn export_and_import_keep_links() {
        let (mut s, _, b, task) = setup();
        s.link_task(&task.id, &b.id).unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&s, &mut zip).unwrap();
        let mut other = Store::open_in_memory().unwrap();
        for _ in 0..2 {
            zip.set_position(0);
            crate::archive::import(&mut other, &mut zip).unwrap();
        }
        assert_eq!(
            other.task(&task.id).unwrap().linked_workspace_ids,
            std::slice::from_ref(&b.id)
        );
        assert_eq!(titles(&other, &b.id), ["告知を出す"]);
    }
}
