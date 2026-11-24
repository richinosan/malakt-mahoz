//! SQLite への保存と読み出し。

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::*;
use crate::text::{normalize, normalize_line};
use crate::{schema, Error, Result};

/// 1 つのデータベースへの接続。
///
/// 書き込みはすべてトランザクションで行う。複数ウィンドウからは 1 つの `Store` を共有する。
pub struct Store {
    conn: Connection,
}

const TASK_COLUMNS: &str = "t.id, t.workspace_id, t.title, t.description, t.status, t.priority, \
     t.due, t.position, t.created_at, t.updated_at, t.completed_at, t.start, t.percent, t.parent_id";

impl Store {
    /// ファイルを開く。なければ作る。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        // WAL は読み書きが互いを止めず、fsync の回数も少ない。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// メモリ上に作る（テスト用）。
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.set_prepared_statement_cache_capacity(64);
        schema::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    // ---- ワークスペース ----

    pub fn create_workspace(&mut self, parent_id: Option<&str>, name: &str) -> Result<Workspace> {
        let name = required_line(name, "ワークスペース名")?;
        if let Some(p) = parent_id {
            self.workspace(p)?;
        }
        let now = now_ms();
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM workspaces WHERE parent_id IS ?1",
            params![parent_id],
            |r| r.get(0),
        )?;
        let ws = Workspace {
            id: new_id(),
            parent_id: parent_id.map(str::to_owned),
            name,
            position,
            created_at: now,
            updated_at: now,
        };
        insert_workspace(&self.conn, &ws)?;
        Ok(ws)
    }

    pub fn workspace(&self, id: &str) -> Result<Workspace> {
        self.conn
            .prepare_cached(
                "SELECT id, parent_id, name, position, created_at, updated_at \
                 FROM workspaces WHERE id = ?1",
            )?
            .query_row(params![id], workspace_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("ワークスペース {id}")))
    }

    /// すべてのワークスペース。親ごとに `position` 順。
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, parent_id, name, position, created_at, updated_at \
             FROM workspaces ORDER BY parent_id, position, id",
        )?;
        let rows = stmt.query_map([], workspace_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_workspace(&mut self, id: &str, name: &str) -> Result<()> {
        let name = required_line(name, "ワークスペース名")?;
        let n = self.conn.execute(
            "UPDATE workspaces SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, name, now_ms()],
        )?;
        found(n, "ワークスペース", id)
    }

    /// 別の親の下（`None` なら最上位）の末尾に移す。自分や子孫の中には移せない。
    pub fn move_workspace(&mut self, id: &str, new_parent_id: Option<&str>) -> Result<()> {
        self.workspace(id)?;
        if let Some(p) = new_parent_id {
            self.workspace(p)?;
            if self.workspace_subtree_ids(id)?.iter().any(|d| d == p) {
                return Err(Error::Cycle);
            }
        }
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM workspaces \
             WHERE parent_id IS ?1 AND id <> ?2",
            params![new_parent_id, id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "UPDATE workspaces SET parent_id = ?2, position = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, new_parent_id, position, now_ms()],
        )?;
        Ok(())
    }

    /// 子孫のワークスペースと、その中のタスクもすべて消す。
    pub fn delete_workspace(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
        found(n, "ワークスペース", id)
    }

    /// 自分自身と、すべての子孫の ID。
    pub fn workspace_subtree_ids(&self, id: &str) -> Result<Vec<Id>> {
        let mut stmt = self.conn.prepare_cached(
            "WITH RECURSIVE sub(id) AS ( \
                 SELECT id FROM workspaces WHERE id = ?1 \
                 UNION ALL \
                 SELECT w.id FROM workspaces w JOIN sub ON w.parent_id = sub.id) \
             SELECT id FROM sub",
        )?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 最上位からそのワークスペースまでの道筋（パンくず用）。
    pub fn workspace_path(&self, id: &str) -> Result<Vec<Workspace>> {
        let mut stmt = self.conn.prepare_cached(
            "WITH RECURSIVE up(id, parent_id, name, position, created_at, updated_at, depth) AS ( \
                 SELECT id, parent_id, name, position, created_at, updated_at, 0 \
                 FROM workspaces WHERE id = ?1 \
                 UNION ALL \
                 SELECT w.id, w.parent_id, w.name, w.position, w.created_at, w.updated_at, \
                        up.depth + 1 \
                 FROM workspaces w JOIN up ON w.id = up.parent_id) \
             SELECT id, parent_id, name, position, created_at, updated_at \
             FROM up ORDER BY depth DESC",
        )?;
        let rows = stmt.query_map(params![id], workspace_from_row)?;
        let path: Vec<Workspace> = rows.collect::<rusqlite::Result<_>>()?;
        if path.is_empty() {
            return Err(Error::NotFound(format!("ワークスペース {id}")));
        }
        Ok(path)
    }

    /// ワークスペースごとの、未完了のタスクの数（子孫は含めない）。
    pub fn open_task_counts(&self) -> Result<HashMap<Id, u32>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT workspace_id, COUNT(*) FROM tasks \
             WHERE status IN ('needs_action', 'in_process') AND workspace_id IS NOT NULL \
             GROUP BY workspace_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- 担当者 ----

    pub fn create_person(&mut self, name: &str, is_self: bool) -> Result<Person> {
        let name = required_line(name, "名前")?;
        let now = now_ms();
        let person = Person {
            id: new_id(),
            name,
            is_self,
            note: String::new(),
            created_at: now,
            updated_at: now,
        };
        let tx = self.conn.transaction()?;
        if is_self {
            tx.execute("UPDATE people SET is_self = 0 WHERE is_self = 1", [])?;
        }
        insert_person(&tx, &person)?;
        tx.commit()?;
        Ok(person)
    }

    /// 自分が先頭、あとは名前順。
    pub fn people(&self) -> Result<Vec<Person>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name, is_self, note, created_at, updated_at \
             FROM people ORDER BY is_self DESC, name, id",
        )?;
        let rows = stmt.query_map([], person_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn update_person(&mut self, id: &str, name: &str, note: &str) -> Result<()> {
        let name = required_line(name, "名前")?;
        let n = self.conn.execute(
            "UPDATE people SET name = ?2, note = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, name, normalize(note), now_ms()],
        )?;
        found(n, "担当者", id)
    }

    /// 担当から外れるだけで、タスクは残る。
    pub fn delete_person(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM people WHERE id = ?1", params![id])?;
        found(n, "担当者", id)
    }

    // ---- タスク ----

    /// `workspace_id` が `None` なら、どのワークスペースにも入れずに作る。
    pub fn create_task(&mut self, workspace_id: Option<&str>, draft: &TaskDraft) -> Result<Task> {
        let draft = clean_draft(draft)?;
        if let Some(w) = workspace_id {
            self.workspace(w)?;
        }
        if let Some(parent_id) = draft.parent_id.as_deref() {
            self.task(parent_id)?;
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE workspace_id IS ?1",
            params![workspace_id],
            |r| r.get(0),
        )?;
        let task = Task {
            id: new_id(),
            workspace_id: workspace_id.map(str::to_owned),
            title: draft.title,
            description: draft.description,
            status: draft.status,
            priority: draft.priority,
            start: draft.start,
            due: draft.due,
            percent: draft.percent,
            parent_id: draft.parent_id,
            assignee_ids: draft.assignee_ids,
            categories: draft.categories,
            position,
            created_at: now,
            updated_at: now,
            completed_at: (draft.status == Status::Completed).then_some(now),
        };
        insert_task(&tx, &task)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn task(&self, id: &str) -> Result<Task> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks t WHERE t.id = ?1");
        let mut task = self
            .conn
            .prepare_cached(&sql)?
            .query_row(params![id], task_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("タスク {id}")))?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT person_id FROM task_assignees WHERE task_id = ?1 ORDER BY person_id",
        )?;
        task.assignee_ids = stmt
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT category FROM task_categories WHERE task_id = ?1 ORDER BY category",
        )?;
        task.categories = stmt
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(task)
    }

    /// サブタスク。`position` 順。
    pub fn subtasks(&self, parent_id: &str) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks t WHERE t.parent_id = ?1 ORDER BY t.position, t.id"
        );
        let mut tasks: Vec<Task> = self
            .conn
            .prepare_cached(&sql)?
            .query_map(params![parent_id], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        self.fill_links(&mut tasks)?;
        Ok(tasks)
    }

    /// 未完了で、期限が `until` 以前（期限切れを含む）のタスク。全ワークスペースから、期限の近い順。
    pub fn due_tasks(&self, until: &str, assignee_id: Option<&str>) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks t \
             WHERE t.due IS NOT NULL AND t.due <= ?1 \
               AND t.status IN ('needs_action', 'in_process') \
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM task_assignees a \
                    WHERE a.task_id = t.id AND a.person_id = ?2)) \
             ORDER BY t.due, t.priority = 0, t.priority, t.id"
        );
        let mut tasks: Vec<Task> = self
            .conn
            .prepare_cached(&sql)?
            .query_map(params![until, assignee_id], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        self.fill_links(&mut tasks)?;
        Ok(tasks)
    }

    /// 使われているカテゴリ。名前順。
    pub fn categories(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT category FROM task_categories ORDER BY category")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// タスクの一覧。`position` 順。
    pub fn tasks<'a>(&self, scope: impl Into<Scope<'a>>, filter: &TaskFilter) -> Result<Vec<Task>> {
        let scope = scope.into();
        let workspace_id = match scope {
            Scope::Workspace(id) => Some(id),
            _ => None,
        };
        let descendants = filter.include_descendants && workspace_id.is_some();
        let mut sql = String::with_capacity(512);
        if descendants {
            sql.push_str(
                "WITH RECURSIVE sub(id) AS ( \
                     SELECT id FROM workspaces WHERE id = ?1 \
                     UNION ALL \
                     SELECT w.id FROM workspaces w JOIN sub ON w.parent_id = sub.id) ",
            );
        }
        sql.push_str("SELECT ");
        sql.push_str(TASK_COLUMNS);
        sql.push_str(" FROM tasks t WHERE ");
        sql.push_str(match scope {
            Scope::All => "?1 IS NULL",
            Scope::Unfiled => "?1 IS NULL AND t.workspace_id IS NULL",
            Scope::Workspace(_) if descendants => "t.workspace_id IN (SELECT id FROM sub)",
            Scope::Workspace(_) => "t.workspace_id = ?1",
        });
        if filter.open_only {
            sql.push_str(" AND t.status IN ('needs_action', 'in_process')");
        }
        // 使わない条件も番号を固定して、SQL の種類を減らしキャッシュを効かせる。
        sql.push_str(
            " AND (?2 IS NULL OR EXISTS (SELECT 1 FROM task_assignees a \
                   WHERE a.task_id = t.id AND a.person_id = ?2)) \
              AND (?3 IS NULL OR instr(lower(t.title), ?3) > 0 \
                   OR instr(lower(t.description), ?3) > 0) \
              ORDER BY t.workspace_id, t.position, t.id",
        );
        let query = filter
            .query
            .as_deref()
            .map(|q| normalize_line(q).to_lowercase())
            .filter(|q| !q.is_empty());
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut tasks: Vec<Task> = stmt
            .query_map(
                params![workspace_id, filter.assignee_id, query],
                task_from_row,
            )?
            .collect::<rusqlite::Result<_>>()?;
        self.fill_links(&mut tasks)?;
        Ok(tasks)
    }

    /// 担当者とカテゴリをまとめて埋める（タスクごとに問い合わせない）。
    fn fill_links(&self, tasks: &mut [Task]) -> Result<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        let index: HashMap<&str, usize> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();
        let mut found: Vec<(usize, Id)> = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT task_id, person_id FROM task_assignees ORDER BY task_id, person_id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let task_id = row.get_ref(0)?.as_str().map_err(rusqlite::Error::from)?;
                if let Some(&i) = index.get(task_id) {
                    found.push((i, row.get(1)?));
                }
            }
        }
        let mut categories: Vec<(usize, String)> = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT task_id, category FROM task_categories ORDER BY task_id, category",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let task_id = row.get_ref(0)?.as_str().map_err(rusqlite::Error::from)?;
                if let Some(&i) = index.get(task_id) {
                    categories.push((i, row.get(1)?));
                }
            }
        }
        drop(index);
        for (i, person_id) in found {
            tasks[i].assignee_ids.push(person_id);
        }
        for (i, category) in categories {
            tasks[i].categories.push(category);
        }
        Ok(())
    }

    /// 内容を更新する。変わった項目は変更履歴に残す。
    pub fn update_task(&mut self, id: &str, draft: &TaskDraft) -> Result<Task> {
        let draft = clean_draft(draft)?;
        let old = self.task(id)?;
        if draft.parent_id != old.parent_id {
            self.check_parent(id, draft.parent_id.as_deref())?;
        }
        let now = now_ms();
        let completed_at = match (old.status, draft.status) {
            (_, s) if s != Status::Completed => None,
            (Status::Completed, _) => old.completed_at,
            _ => Some(now),
        };
        let tx = self.conn.transaction()?;
        let mut changed = false;
        let mut record = |field: &str, a: String, b: String| -> Result<()> {
            if a != b {
                changed = true;
                insert_change(&tx, id, field, &a, &b, now)?;
            }
            Ok(())
        };
        record("title", old.title.clone(), draft.title.clone())?;
        record(
            "description",
            old.description.clone(),
            draft.description.clone(),
        )?;
        record(
            "status",
            old.status.as_str().into(),
            draft.status.as_str().into(),
        )?;
        record(
            "priority",
            old.priority.to_string(),
            draft.priority.to_string(),
        )?;
        record(
            "due",
            old.due.clone().unwrap_or_default(),
            draft.due.clone().unwrap_or_default(),
        )?;
        record(
            "assignees",
            old.assignee_ids.join(","),
            draft.assignee_ids.join(","),
        )?;
        record(
            "start",
            old.start.clone().unwrap_or_default(),
            draft.start.clone().unwrap_or_default(),
        )?;
        record(
            "percent",
            old.percent.to_string(),
            draft.percent.to_string(),
        )?;
        record(
            "parent",
            old.parent_id.clone().unwrap_or_default(),
            draft.parent_id.clone().unwrap_or_default(),
        )?;
        record(
            "categories",
            old.categories.join(","),
            draft.categories.join(","),
        )?;
        if !changed {
            drop(tx);
            return Ok(old);
        }
        tx.execute(
            "UPDATE tasks SET title = ?2, description = ?3, status = ?4, priority = ?5, \
             due = ?6, updated_at = ?7, completed_at = ?8, start = ?9, percent = ?10, \
             parent_id = ?11 WHERE id = ?1",
            params![
                id,
                draft.title,
                draft.description,
                draft.status.as_str(),
                draft.priority,
                draft.due,
                now,
                completed_at,
                draft.start,
                draft.percent,
                draft.parent_id
            ],
        )?;
        if old.assignee_ids != draft.assignee_ids {
            tx.execute("DELETE FROM task_assignees WHERE task_id = ?1", params![id])?;
            insert_assignees(&tx, id, &draft.assignee_ids)?;
        }
        if old.categories != draft.categories {
            tx.execute(
                "DELETE FROM task_categories WHERE task_id = ?1",
                params![id],
            )?;
            insert_categories(&tx, id, &draft.categories)?;
        }
        tx.commit()?;
        Ok(Task {
            title: draft.title,
            description: draft.description,
            status: draft.status,
            priority: draft.priority,
            start: draft.start,
            due: draft.due,
            percent: draft.percent,
            parent_id: draft.parent_id,
            assignee_ids: draft.assignee_ids,
            categories: draft.categories,
            updated_at: now,
            completed_at,
            ..old
        })
    }

    /// 親タスクにできるか。自分自身や、自分の子孫は親にできない。
    fn check_parent(&self, id: &str, parent_id: Option<&str>) -> Result<()> {
        let Some(parent_id) = parent_id else {
            return Ok(());
        };
        self.task(parent_id)?;
        let inside: bool = self.conn.query_row(
            "WITH RECURSIVE sub(id) AS ( \
                 SELECT ?1 \
                 UNION \
                 SELECT t.id FROM tasks t JOIN sub ON t.parent_id = sub.id) \
             SELECT EXISTS(SELECT 1 FROM sub WHERE id = ?2)",
            params![id, parent_id],
            |r| r.get(0),
        )?;
        if inside {
            return Err(Error::Invalid(
                "自分自身やサブタスクを親タスクにはできません".into(),
            ));
        }
        Ok(())
    }

    pub fn set_task_status(&mut self, id: &str, status: Status) -> Result<Task> {
        let mut draft = TaskDraft::from(&self.task(id)?);
        draft.status = status;
        self.update_task(id, &draft)
    }

    /// 別のワークスペースの末尾に移す。`None` ならどこにも入れない状態に戻す。
    pub fn move_task(&mut self, id: &str, workspace_id: Option<&str>) -> Result<()> {
        let old = self.task(id)?;
        if old.workspace_id.as_deref() == workspace_id {
            return Ok(());
        }
        if let Some(w) = workspace_id {
            self.workspace(w)?;
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE workspace_id IS ?1",
            params![workspace_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE tasks SET workspace_id = ?2, position = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, workspace_id, position, now],
        )?;
        insert_change(
            &tx,
            id,
            "workspace",
            old.workspace_id.as_deref().unwrap_or_default(),
            workspace_id.unwrap_or_default(),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_task(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        found(n, "タスク", id)
    }

    pub fn changes(&self, task_id: &str) -> Result<Vec<Change>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, task_id, field, old_value, new_value, created_at \
             FROM changes WHERE task_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![task_id], change_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- コメント ----

    pub fn add_comment(
        &mut self,
        task_id: &str,
        author_id: Option<&str>,
        body: &str,
    ) -> Result<Comment> {
        let body = normalize(body).trim().to_owned();
        if body.is_empty() {
            return Err(Error::Invalid("コメントが空です".into()));
        }
        self.task(task_id)?;
        let now = now_ms();
        let comment = Comment {
            id: new_id(),
            task_id: task_id.to_owned(),
            author_id: author_id.map(str::to_owned),
            body,
            pinned: false,
            created_at: now,
            updated_at: now,
        };
        insert_comment(&self.conn, &comment)?;
        Ok(comment)
    }

    /// ピン留めが先、あとは古い順。
    pub fn comments(&self, task_id: &str) -> Result<Vec<Comment>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, task_id, author_id, body, pinned, created_at, updated_at \
             FROM comments WHERE task_id = ?1 ORDER BY pinned DESC, created_at, id",
        )?;
        let rows = stmt.query_map(params![task_id], comment_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_comment_pinned(&mut self, id: &str, pinned: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE comments SET pinned = ?2 WHERE id = ?1",
            params![id, pinned],
        )?;
        found(n, "コメント", id)
    }

    pub fn delete_comment(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM comments WHERE id = ?1", params![id])?;
        found(n, "コメント", id)
    }

    // ---- 設定 ----

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
            .query_row(params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, v],
            )?,
            None => self
                .conn
                .execute("DELETE FROM settings WHERE key = ?1", params![key])?,
        };
        Ok(())
    }

    // ---- エクスポート / インポート用 ----

    pub(crate) fn dump(&self) -> Result<crate::archive::Data> {
        let workspaces = self.workspaces()?;
        let people = self.people()?;
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks t ORDER BY t.id");
        let mut tasks: Vec<Task> = self
            .conn
            .prepare(&sql)?
            .query_map([], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        self.fill_links(&mut tasks)?;
        let comments = self
            .conn
            .prepare(
                "SELECT id, task_id, author_id, body, pinned, created_at, updated_at \
                 FROM comments ORDER BY id",
            )?
            .query_map([], comment_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        let changes = self
            .conn
            .prepare(
                "SELECT id, task_id, field, old_value, new_value, created_at \
                 FROM changes ORDER BY id",
            )?
            .query_map([], change_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(crate::archive::Data {
            workspaces,
            people,
            tasks,
            comments,
            changes,
        })
    }

    /// ID が同じものは上書き、なければ追加する。全体を 1 つのトランザクションで行う。
    pub(crate) fn merge(&mut self, data: &crate::archive::Data) -> Result<()> {
        let tx = self.conn.transaction()?;
        // 親より先に子が来ても入るように、検査をコミットまで遅らせる。
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        let had_self: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM people WHERE is_self = 1)",
            [],
            |r| r.get(0),
        )?;
        for w in &data.workspaces {
            insert_workspace(&tx, w)?;
        }
        for p in &data.people {
            // 「自分」は 1 人だけ。すでにいれば、取り込む側は普通の担当者にする。
            let mut p = p.clone();
            if had_self {
                let same: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM people WHERE id = ?1 AND is_self = 1)",
                    params![p.id],
                    |r| r.get(0),
                )?;
                p.is_self = same;
            }
            insert_person(&tx, &p)?;
        }
        for t in &data.tasks {
            tx.execute(
                "DELETE FROM task_assignees WHERE task_id = ?1",
                params![t.id],
            )?;
            tx.execute(
                "DELETE FROM task_categories WHERE task_id = ?1",
                params![t.id],
            )?;
            insert_task(&tx, t)?;
        }
        for c in &data.comments {
            insert_comment(&tx, c)?;
        }
        for c in &data.changes {
            tx.prepare_cached(
                "INSERT OR REPLACE INTO changes(id, task_id, field, old_value, new_value, \
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                c.id,
                c.task_id,
                c.field,
                c.old_value,
                c.new_value,
                c.created_at
            ])?;
        }
        // 取り込んだ結果、親子が輪になっていないか確かめる。
        let reachable: i64 = tx.query_row(
            "WITH RECURSIVE down(id) AS ( \
                 SELECT id FROM workspaces WHERE parent_id IS NULL \
                 UNION ALL \
                 SELECT w.id FROM workspaces w JOIN down ON w.parent_id = down.id) \
             SELECT COUNT(*) FROM down",
            [],
            |r| r.get(0),
        )?;
        let total: i64 = tx.query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))?;
        if reachable != total {
            return Err(Error::Cycle);
        }
        tx.commit().map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Error::Invalid("存在しないデータを参照しています".into())
            }
            e => Error::Db(e),
        })
    }
}

fn required_line(input: &str, what: &str) -> Result<String> {
    let s = normalize_line(input);
    if s.is_empty() {
        return Err(Error::Invalid(format!("{what}が空です")));
    }
    Ok(s)
}

fn found(affected: usize, what: &str, id: &str) -> Result<()> {
    if affected == 0 {
        return Err(Error::NotFound(format!("{what} {id}")));
    }
    Ok(())
}

fn clean_draft(draft: &TaskDraft) -> Result<TaskDraft> {
    let title = required_line(&draft.title, "タイトル")?;
    if draft.priority > 9 {
        return Err(Error::Invalid("優先度は 0 から 9 です".into()));
    }
    let due = match draft.due.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(d) if is_valid_date(d) => Some(d.to_owned()),
        Some(d) => return Err(Error::Invalid(format!("期限の日付が正しくありません: {d}"))),
    };
    let start = match draft.start.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(d) if is_valid_date(d) => Some(d.to_owned()),
        Some(d) => {
            return Err(Error::Invalid(format!(
                "開始日の日付が正しくありません: {d}"
            )))
        }
    };
    if draft.percent > 100 {
        return Err(Error::Invalid("進捗は 0 から 100 です".into()));
    }
    let mut assignee_ids = draft.assignee_ids.clone();
    assignee_ids.sort_unstable();
    assignee_ids.dedup();
    let mut categories: Vec<String> = draft
        .categories
        .iter()
        .map(|c| normalize_line(c))
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort_unstable();
    categories.dedup();
    Ok(TaskDraft {
        title,
        description: normalize(&draft.description),
        status: draft.status,
        priority: draft.priority,
        start,
        due,
        // 完了にすると進捗は 100% になる
        percent: if draft.status == Status::Completed {
            100
        } else {
            draft.percent
        },
        parent_id: draft.parent_id.clone().filter(|p| !p.is_empty()),
        assignee_ids,
        categories,
    })
}

fn insert_workspace(conn: &Connection, w: &Workspace) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO workspaces(id, parent_id, name, position, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(id) DO UPDATE SET parent_id = excluded.parent_id, name = excluded.name, \
         position = excluded.position, updated_at = excluded.updated_at",
    )?
    .execute(params![
        w.id,
        w.parent_id,
        w.name,
        w.position,
        w.created_at,
        w.updated_at
    ])
}

fn insert_person(conn: &Connection, p: &Person) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO people(id, name, is_self, note, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, is_self = excluded.is_self, \
         note = excluded.note, updated_at = excluded.updated_at",
    )?
    .execute(params![
        p.id,
        p.name,
        p.is_self,
        p.note,
        p.created_at,
        p.updated_at
    ])
}

fn insert_task(conn: &Connection, t: &Task) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO tasks(id, workspace_id, title, description, status, priority, due, \
         position, created_at, updated_at, completed_at, start, percent, parent_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(id) DO UPDATE SET workspace_id = excluded.workspace_id, \
         title = excluded.title, description = excluded.description, status = excluded.status, \
         priority = excluded.priority, due = excluded.due, position = excluded.position, \
         updated_at = excluded.updated_at, completed_at = excluded.completed_at, \
         start = excluded.start, percent = excluded.percent, parent_id = excluded.parent_id",
    )?
    .execute(params![
        t.id,
        t.workspace_id,
        t.title,
        t.description,
        t.status.as_str(),
        t.priority,
        t.due,
        t.position,
        t.created_at,
        t.updated_at,
        t.completed_at,
        t.start,
        t.percent,
        t.parent_id
    ])?;
    insert_assignees(conn, &t.id, &t.assignee_ids)?;
    insert_categories(conn, &t.id, &t.categories)
}

fn insert_categories(
    conn: &Connection,
    task_id: &str,
    categories: &[String],
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO task_categories(task_id, category) VALUES (?1, ?2)",
    )?;
    for category in categories {
        stmt.execute(params![task_id, category])?;
    }
    Ok(())
}

fn insert_assignees(conn: &Connection, task_id: &str, ids: &[Id]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO task_assignees(task_id, person_id) VALUES (?1, ?2)",
    )?;
    for person_id in ids {
        stmt.execute(params![task_id, person_id])?;
    }
    Ok(())
}

fn insert_comment(conn: &Connection, c: &Comment) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO comments(id, task_id, author_id, body, pinned, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(id) DO UPDATE SET body = excluded.body, pinned = excluded.pinned, \
         author_id = excluded.author_id, updated_at = excluded.updated_at",
    )?
    .execute(params![
        c.id,
        c.task_id,
        c.author_id,
        c.body,
        c.pinned,
        c.created_at,
        c.updated_at
    ])
}

fn insert_change(
    conn: &Connection,
    task_id: &str,
    field: &str,
    old: &str,
    new: &str,
    now: i64,
) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO changes(id, task_id, field, old_value, new_value, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![new_id(), task_id, field, old, new, now])
}

fn workspace_from_row(r: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        position: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

fn person_from_row(r: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: r.get(0)?,
        name: r.get(1)?,
        is_self: r.get(2)?,
        note: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

fn task_from_row(r: &Row<'_>) -> rusqlite::Result<Task> {
    let status: String = r.get(4)?;
    Ok(Task {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        status: Status::parse(&status).unwrap_or(Status::NeedsAction),
        priority: r.get(5)?,
        due: r.get(6)?,
        start: r.get(11)?,
        percent: r.get(12)?,
        parent_id: r.get(13)?,
        assignee_ids: Vec::new(),
        categories: Vec::new(),
        position: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        completed_at: r.get(10)?,
    })
}

fn comment_from_row(r: &Row<'_>) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: r.get(0)?,
        task_id: r.get(1)?,
        author_id: r.get(2)?,
        body: r.get(3)?,
        pinned: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

fn change_from_row(r: &Row<'_>) -> rusqlite::Result<Change> {
    Ok(Change {
        id: r.get(0)?,
        task_id: r.get(1)?,
        field: r.get(2)?,
        old_value: r.get(3)?,
        new_value: r.get(4)?,
        created_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn draft(title: &str) -> TaskDraft {
        TaskDraft {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn migration_sets_user_version() {
        let s = store();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, schema::version());
    }

    #[test]
    fn workspaces_nest_and_keep_order() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(None, "B").unwrap();
        let a1 = s.create_workspace(Some(&a.id), "A-1").unwrap();
        let a2 = s.create_workspace(Some(&a.id), "A-2").unwrap();
        assert_eq!((a.position, b.position), (0, 1));
        assert_eq!((a1.position, a2.position), (0, 1));
        assert_eq!(a1.parent_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(s.workspaces().unwrap().len(), 4);
        let path = s.workspace_path(&a2.id).unwrap();
        assert_eq!(
            path.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            ["A", "A-2"]
        );
    }

    #[test]
    fn workspace_name_is_required_and_normalized() {
        let mut s = store();
        assert!(matches!(
            s.create_workspace(None, "  \n "),
            Err(Error::Invalid(_))
        ));
        let w = s.create_workspace(None, " か\u{3099}いじょう ").unwrap();
        assert_eq!(w.name, "がいじょう");
        assert!(matches!(
            s.rename_workspace(&w.id, ""),
            Err(Error::Invalid(_))
        ));
        s.rename_workspace(&w.id, "会場").unwrap();
        assert_eq!(s.workspace(&w.id).unwrap().name, "会場");
        assert!(matches!(
            s.rename_workspace("nope", "x"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn unknown_parent_is_rejected() {
        let mut s = store();
        assert!(matches!(
            s.create_workspace(Some("nope"), "x"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn workspace_cannot_move_into_itself_or_descendant() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(Some(&a.id), "B").unwrap();
        let c = s.create_workspace(Some(&b.id), "C").unwrap();
        assert!(matches!(
            s.move_workspace(&a.id, Some(&a.id)),
            Err(Error::Cycle)
        ));
        assert!(matches!(
            s.move_workspace(&a.id, Some(&c.id)),
            Err(Error::Cycle)
        ));
        assert!(matches!(
            s.move_workspace(&a.id, Some("nope")),
            Err(Error::NotFound(_))
        ));
        s.move_workspace(&c.id, None).unwrap();
        let c = s.workspace(&c.id).unwrap();
        assert_eq!(c.parent_id, None);
        assert_eq!(c.position, 1);
        s.move_workspace(&c.id, Some(&a.id)).unwrap();
        let c = s.workspace(&c.id).unwrap();
        assert_eq!(c.parent_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(c.position, 1);
        // 同じ親に移し直しても、自分の分で番号が増えない
        s.move_workspace(&c.id, Some(&a.id)).unwrap();
        assert_eq!(s.workspace(&c.id).unwrap().position, 1);
    }

    #[test]
    fn deleting_workspace_removes_descendants_and_tasks() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(Some(&a.id), "B").unwrap();
        let keep = s.create_workspace(None, "keep").unwrap();
        let t = s.create_task(Some(&b.id), &draft("t")).unwrap();
        let k = s.create_task(Some(&keep.id), &draft("k")).unwrap();
        s.delete_workspace(&a.id).unwrap();
        assert!(matches!(s.workspace(&b.id), Err(Error::NotFound(_))));
        assert!(matches!(s.task(&t.id), Err(Error::NotFound(_))));
        assert!(s.task(&k.id).is_ok());
        assert!(matches!(s.delete_workspace(&a.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn subtree_and_path() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(Some(&a.id), "B").unwrap();
        let c = s.create_workspace(Some(&b.id), "C").unwrap();
        let other = s.create_workspace(None, "other").unwrap();
        let mut ids = s.workspace_subtree_ids(&a.id).unwrap();
        ids.sort();
        let mut expect = vec![a.id.clone(), b.id.clone(), c.id.clone()];
        expect.sort();
        assert_eq!(ids, expect);
        assert_eq!(s.workspace_subtree_ids(&other.id).unwrap(), [other.id]);
        assert!(matches!(s.workspace_path("nope"), Err(Error::NotFound(_))));
        assert_eq!(s.workspace_path(&c.id).unwrap().len(), 3);
    }

    #[test]
    fn only_one_self() {
        let mut s = store();
        let me = s.create_person("自分", true).unwrap();
        let guest = s.create_person("外部の田中さん", false).unwrap();
        let me2 = s.create_person("新しい自分", true).unwrap();
        let people = s.people().unwrap();
        assert_eq!(people.len(), 3);
        assert_eq!(people[0].id, me2.id);
        assert!(people[0].is_self);
        assert_eq!(people.iter().filter(|p| p.is_self).count(), 1);
        assert!(people.iter().any(|p| p.id == me.id && !p.is_self));
        s.update_person(&guest.id, "田中", "受付担当\r\n午前のみ")
            .unwrap();
        let g = s
            .people()
            .unwrap()
            .into_iter()
            .find(|p| p.id == guest.id)
            .unwrap();
        assert_eq!(
            (g.name.as_str(), g.note.as_str()),
            ("田中", "受付担当\n午前のみ")
        );
        assert!(matches!(
            s.create_person(" ", false),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.update_person("nope", "x", ""),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            s.update_person(&guest.id, "", ""),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn task_create_validates() {
        let mut s = store();
        let w = s.create_workspace(None, "W").unwrap();
        assert!(matches!(
            s.create_task(Some(&w.id), &draft(" ")),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.create_task(Some("nope"), &draft("x")),
            Err(Error::NotFound(_))
        ));
        let mut d = draft("x");
        d.priority = 10;
        assert!(matches!(
            s.create_task(Some(&w.id), &d),
            Err(Error::Invalid(_))
        ));
        d.priority = 9;
        d.due = Some("2026-02-30".into());
        assert!(matches!(
            s.create_task(Some(&w.id), &d),
            Err(Error::Invalid(_))
        ));
        d.due = Some(" ".into());
        let t = s.create_task(Some(&w.id), &d).unwrap();
        assert_eq!(t.due, None);
        assert_eq!(t.priority, 9);
        assert_eq!(t.completed_at, None);
        d.due = Some(" 2026-12-01 ".into());
        d.status = Status::Completed;
        let t2 = s.create_task(Some(&w.id), &d).unwrap();
        assert_eq!(t2.due.as_deref(), Some("2026-12-01"));
        assert_eq!(t2.position, 1);
        assert_eq!(t2.completed_at, Some(t2.created_at));
        assert_eq!(s.task(&t2.id).unwrap(), t2);
    }

    #[test]
    fn task_assignees_are_sorted_deduped_and_loaded() {
        let mut s = store();
        let w = s.create_workspace(None, "W").unwrap();
        let p1 = s.create_person("a", true).unwrap();
        let p2 = s.create_person("b", false).unwrap();
        let mut d = draft("x");
        d.assignee_ids = vec![p2.id.clone(), p1.id.clone(), p2.id.clone()];
        let t = s.create_task(Some(&w.id), &d).unwrap();
        let mut expect = vec![p1.id.clone(), p2.id.clone()];
        expect.sort();
        assert_eq!(t.assignee_ids, expect);
        assert_eq!(s.task(&t.id).unwrap().assignee_ids, expect);
        let other = s.create_task(Some(&w.id), &draft("y")).unwrap();
        let list = s.tasks(&w.id, &TaskFilter::default()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].assignee_ids, expect);
        assert!(list[1].assignee_ids.is_empty());
        assert_eq!(list[1].id, other.id);
        // 担当者を消すと担当から外れるだけ
        s.delete_person(&p2.id).unwrap();
        assert_eq!(s.task(&t.id).unwrap().assignee_ids, [p1.id.clone()]);
        assert!(matches!(s.delete_person(&p2.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn task_filters() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(Some(&a.id), "B").unwrap();
        let z = s.create_workspace(None, "Z").unwrap();
        let me = s.create_person("me", true).unwrap();
        let t1 = s.create_task(Some(&a.id), &draft("会場の予約")).unwrap();
        let mut d = draft("Print Flyers");
        d.assignee_ids = vec![me.id.clone()];
        d.description = "A4 で 100 部".into();
        let t2 = s.create_task(Some(&b.id), &d).unwrap();
        let t3 = s.create_task(Some(&a.id), &draft("done")).unwrap();
        s.set_task_status(&t3.id, Status::Completed).unwrap();
        let t4 = s.create_task(Some(&a.id), &draft("cancel")).unwrap();
        s.set_task_status(&t4.id, Status::Cancelled).unwrap();
        s.create_task(Some(&z.id), &draft("別の場所")).unwrap();

        let ids = |s: &Store, ws: &str, f: TaskFilter| -> Vec<Id> {
            s.tasks(ws, &f).unwrap().into_iter().map(|t| t.id).collect()
        };
        assert_eq!(
            ids(&s, &a.id, TaskFilter::default()),
            [t1.id.clone(), t3.id.clone(), t4.id.clone()]
        );
        let all = ids(
            &s,
            &a.id,
            TaskFilter {
                include_descendants: true,
                ..Default::default()
            },
        );
        assert_eq!(all.len(), 4);
        assert!(all.contains(&t2.id));
        assert_eq!(
            ids(
                &s,
                &a.id,
                TaskFilter {
                    open_only: true,
                    ..Default::default()
                }
            ),
            [t1.id.clone()]
        );
        assert_eq!(
            ids(
                &s,
                &a.id,
                TaskFilter {
                    include_descendants: true,
                    assignee_id: Some(me.id.clone()),
                    ..Default::default()
                }
            ),
            [t2.id.clone()]
        );
        // 大文字小文字を区別しない。説明も探す。空の検索語は絞り込まない。
        for (q, n) in [
            ("flyers", 1),
            ("100 部", 1),
            ("会場", 1),
            ("  ", 4),
            ("zzz", 0),
        ] {
            let got = ids(
                &s,
                &a.id,
                TaskFilter {
                    include_descendants: true,
                    query: Some(q.into()),
                    ..Default::default()
                },
            );
            assert_eq!(got.len(), n, "query {q:?}");
        }
        let counts = s.open_task_counts().unwrap();
        assert_eq!(counts.get(&a.id), Some(&1));
        assert_eq!(counts.get(&b.id), Some(&1));
        assert_eq!(counts.get(&z.id), Some(&1));
    }

    #[test]
    fn update_records_history_and_completion() {
        let mut s = store();
        let w = s.create_workspace(None, "W").unwrap();
        let p = s.create_person("p", false).unwrap();
        let t = s.create_task(Some(&w.id), &draft("old")).unwrap();

        // 何も変えなければ履歴も更新時刻も変わらない
        let same = s.update_task(&t.id, &TaskDraft::from(&t)).unwrap();
        assert_eq!(same, t);
        assert!(s.changes(&t.id).unwrap().is_empty());

        let d = TaskDraft {
            title: "new".into(),
            description: "desc".into(),
            status: Status::Completed,
            priority: 3,
            due: Some("2026-12-24".into()),
            assignee_ids: vec![p.id.clone()],
            ..Default::default()
        };
        let u = s.update_task(&t.id, &d).unwrap();
        assert_eq!(u, s.task(&t.id).unwrap());
        assert_eq!(u.title, "new");
        assert!(u.completed_at.is_some());
        assert_eq!(u.created_at, t.created_at);
        let mut fields: Vec<String> = s
            .changes(&t.id)
            .unwrap()
            .into_iter()
            .map(|c| c.field)
            .collect();
        fields.sort();
        assert_eq!(
            fields,
            [
                "assignees",
                "description",
                "due",
                "percent",
                "priority",
                "status",
                "title"
            ]
        );
        let ch = s.changes(&t.id).unwrap();
        let title = ch.iter().find(|c| c.field == "title").unwrap();
        assert_eq!(
            (title.old_value.as_str(), title.new_value.as_str()),
            ("old", "new")
        );
        let status = ch.iter().find(|c| c.field == "status").unwrap();
        assert_eq!(
            (status.old_value.as_str(), status.new_value.as_str()),
            ("needs_action", "completed")
        );

        // 完了のまま別の項目を変えても、完了時刻は保たれる
        let mut d2 = d.clone();
        d2.title = "newer".into();
        let u2 = s.update_task(&t.id, &d2).unwrap();
        assert_eq!(u2.completed_at, u.completed_at);
        assert_eq!(s.task(&t.id).unwrap().assignee_ids, [p.id.clone()]);

        // 完了を取り消すと完了時刻は消える
        let u3 = s.set_task_status(&t.id, Status::InProcess).unwrap();
        assert_eq!(u3.completed_at, None);
        assert_eq!(s.task(&t.id).unwrap().completed_at, None);

        // 担当を外す
        let mut d4 = TaskDraft::from(&u3);
        d4.assignee_ids.clear();
        s.update_task(&t.id, &d4).unwrap();
        assert!(s.task(&t.id).unwrap().assignee_ids.is_empty());

        assert!(matches!(s.update_task("nope", &d), Err(Error::NotFound(_))));
    }

    #[test]
    fn move_and_delete_task() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(None, "B").unwrap();
        s.create_task(Some(&b.id), &draft("first")).unwrap();
        let t = s.create_task(Some(&a.id), &draft("t")).unwrap();
        s.move_task(&t.id, Some(&a.id)).unwrap();
        assert!(s.changes(&t.id).unwrap().is_empty());
        assert!(matches!(
            s.move_task(&t.id, Some("nope")),
            Err(Error::NotFound(_))
        ));
        s.move_task(&t.id, Some(&b.id)).unwrap();
        let moved = s.task(&t.id).unwrap();
        assert_eq!(moved.workspace_id.as_deref(), Some(b.id.as_str()));
        assert_eq!(moved.position, 1);
        let ch = s.changes(&t.id).unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].field, "workspace");
        assert_eq!(
            (ch[0].old_value.clone(), ch[0].new_value.clone()),
            (a.id, b.id)
        );
        s.delete_task(&t.id).unwrap();
        assert!(matches!(s.delete_task(&t.id), Err(Error::NotFound(_))));
        assert!(s.changes(&t.id).unwrap().is_empty());
    }

    #[test]
    fn comments_pin_and_delete() {
        let mut s = store();
        let w = s.create_workspace(None, "W").unwrap();
        let p = s.create_person("p", true).unwrap();
        let t = s.create_task(Some(&w.id), &draft("t")).unwrap();
        assert!(matches!(
            s.add_comment(&t.id, None, " \r\n"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.add_comment("nope", None, "x"),
            Err(Error::NotFound(_))
        ));
        let c1 = s
            .add_comment(&t.id, Some(&p.id), " 1行目\r\n2行目 ")
            .unwrap();
        assert_eq!(c1.body, "1行目\n2行目");
        let c2 = s.add_comment(&t.id, None, "second").unwrap();
        s.set_comment_pinned(&c2.id, true).unwrap();
        let list = s.comments(&t.id).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, c2.id);
        assert!(list[0].pinned);
        assert!(!list[1].pinned);
        assert_eq!(list[1].author_id.as_deref(), Some(p.id.as_str()));
        s.set_comment_pinned(&c2.id, false).unwrap();
        assert!(!s.comments(&t.id).unwrap().iter().any(|c| c.pinned));
        // 書いた人を消してもコメントは残る
        s.delete_person(&p.id).unwrap();
        let list = s.comments(&t.id).unwrap();
        assert_eq!(list.iter().find(|c| c.id == c1.id).unwrap().author_id, None);
        s.delete_comment(&c1.id).unwrap();
        assert_eq!(s.comments(&t.id).unwrap().len(), 1);
        assert!(matches!(s.delete_comment(&c1.id), Err(Error::NotFound(_))));
        assert!(matches!(
            s.set_comment_pinned("nope", true),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn settings_round_trip() {
        let mut s = store();
        assert_eq!(s.setting("k").unwrap(), None);
        s.set_setting("k", Some("v1")).unwrap();
        s.set_setting("k", Some("v2")).unwrap();
        assert_eq!(s.setting("k").unwrap().as_deref(), Some("v2"));
        s.set_setting("k", None).unwrap();
        assert_eq!(s.setting("k").unwrap(), None);
    }

    #[test]
    fn data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        let id = {
            let mut s = Store::open(&path).unwrap();
            s.create_workspace(None, "残る").unwrap().id
        };
        let s = Store::open(&path).unwrap();
        assert_eq!(s.workspace(&id).unwrap().name, "残る");
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    fn draft(title: &str) -> TaskDraft {
        TaskDraft {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn subtasks_and_parent_rules() {
        let mut s = Store::open_in_memory().unwrap();
        let w = s.create_workspace(None, "W").unwrap();
        let parent = s.create_task(Some(&w.id), &draft("親")).unwrap();
        let mut d = draft("子");
        d.parent_id = Some(parent.id.clone());
        let child = s.create_task(Some(&w.id), &d).unwrap();
        let mut d2 = draft("孫");
        d2.parent_id = Some(child.id.clone());
        let grand = s.create_task(Some(&w.id), &d2).unwrap();
        assert_eq!(
            s.subtasks(&parent.id)
                .unwrap()
                .iter()
                .map(|t| &t.id)
                .collect::<Vec<_>>(),
            [&child.id]
        );
        assert!(s.subtasks(&grand.id).unwrap().is_empty());

        let mut bad = draft("x");
        bad.parent_id = Some("nope".into());
        assert!(matches!(
            s.create_task(Some(&w.id), &bad),
            Err(Error::NotFound(_))
        ));

        // 自分自身・子・孫は親にできない
        for target in [&parent.id, &child.id, &grand.id] {
            let mut d = TaskDraft::from(&s.task(&parent.id).unwrap());
            d.parent_id = Some(target.clone());
            assert!(
                matches!(s.update_task(&parent.id, &d), Err(Error::Invalid(_))),
                "{target}"
            );
        }
        // 孫を親の直下に付け替えるのはできる
        let mut d = TaskDraft::from(&s.task(&grand.id).unwrap());
        d.parent_id = Some(parent.id.clone());
        s.update_task(&grand.id, &d).unwrap();
        assert_eq!(s.subtasks(&parent.id).unwrap().len(), 2);
        assert_eq!(s.changes(&grand.id).unwrap()[0].field, "parent");
        // 空文字の親は「なし」
        d.parent_id = Some(String::new());
        assert_eq!(s.update_task(&grand.id, &d).unwrap().parent_id, None);
        // 親を消すとサブタスクは残り、親なしになる
        s.delete_task(&parent.id).unwrap();
        assert_eq!(s.task(&child.id).unwrap().parent_id, None);
    }

    #[test]
    fn start_percent_and_categories() {
        let mut s = Store::open_in_memory().unwrap();
        let w = s.create_workspace(None, "W").unwrap();
        let mut d = draft("t");
        d.percent = 101;
        assert!(matches!(
            s.create_task(Some(&w.id), &d),
            Err(Error::Invalid(_))
        ));
        d.percent = 100;
        d.start = Some("2026-13-01".into());
        assert!(matches!(
            s.create_task(Some(&w.id), &d),
            Err(Error::Invalid(_))
        ));
        d.start = Some("2026-12-01".into());
        d.percent = 40;
        d.categories = vec![" 設営 ".into(), "広報".into(), "設営".into(), " ".into()];
        let t = s.create_task(Some(&w.id), &d).unwrap();
        assert_eq!(t.start.as_deref(), Some("2026-12-01"));
        assert_eq!(t.percent, 40);
        assert_eq!(t.categories, ["広報", "設営"]);
        assert_eq!(s.task(&t.id).unwrap(), t);
        assert_eq!(s.tasks(&w.id, &TaskFilter::default()).unwrap()[0], t);
        assert_eq!(s.categories().unwrap(), ["広報", "設営"]);

        // 完了にすると進捗は 100%。戻しても数字はそのまま
        let done = s.set_task_status(&t.id, Status::Completed).unwrap();
        assert_eq!(done.percent, 100);
        assert_eq!(
            s.set_task_status(&t.id, Status::InProcess).unwrap().percent,
            100
        );

        let mut d = TaskDraft::from(&s.task(&t.id).unwrap());
        d.categories = vec!["会計".into()];
        d.start = None;
        d.percent = 0;
        let u = s.update_task(&t.id, &d).unwrap();
        assert_eq!(u, s.task(&t.id).unwrap());
        assert_eq!(u.categories, ["会計"]);
        assert_eq!((u.start.clone(), u.percent), (None, 0));
        assert_eq!(s.categories().unwrap(), ["会計"]);
        let fields: Vec<String> = s
            .changes(&t.id)
            .unwrap()
            .into_iter()
            .map(|c| c.field)
            .collect();
        assert!(fields.contains(&"categories".to_string()));
        assert!(fields.contains(&"start".to_string()));
    }

    #[test]
    fn due_tasks_across_workspaces() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(Some(&a.id), "B").unwrap();
        let me = s.create_person("me", true).unwrap();
        let mk = |s: &mut Store, ws: &str, title: &str, due: Option<&str>, prio: u8, mine: bool| {
            let mut d = draft(title);
            d.due = due.map(str::to_owned);
            d.priority = prio;
            if mine {
                d.assignee_ids = vec![me.id.clone()];
            }
            s.create_task(Some(ws), &d).unwrap()
        };
        let overdue = mk(&mut s, &a.id, "overdue", Some("2026-11-01"), 0, false);
        let soon_low = mk(&mut s, &b.id, "soon-low", Some("2026-12-01"), 9, true);
        let soon_high = mk(&mut s, &b.id, "soon-high", Some("2026-12-01"), 1, false);
        let soon_none = mk(&mut s, &b.id, "soon-none", Some("2026-12-01"), 0, false);
        let edge = mk(&mut s, &a.id, "edge", Some("2026-12-24"), 0, false);
        mk(&mut s, &a.id, "later", Some("2026-12-25"), 0, true);
        mk(&mut s, &a.id, "no due", None, 0, true);
        let done = mk(&mut s, &a.id, "done", Some("2026-11-02"), 0, true);
        s.set_task_status(&done.id, Status::Completed).unwrap();

        let ids: Vec<Id> = s
            .due_tasks("2026-12-24", None)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        // 期限順。同じ日なら優先度の高い順で、優先度なしは最後
        assert_eq!(
            ids,
            [
                overdue.id,
                soon_high.id,
                soon_low.id.clone(),
                soon_none.id,
                edge.id
            ]
        );
        let mine = s.due_tasks("2026-12-24", Some(&me.id)).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, soon_low.id);
        assert_eq!(mine[0].assignee_ids, [me.id.clone()]);
    }
}

#[cfg(test)]
mod unfiled_tests {
    use super::*;

    /// ワークスペースも担当者も作らずに、そのまま使える。
    #[test]
    fn works_with_nothing_set_up() {
        let mut s = Store::open_in_memory().unwrap();
        let d = TaskDraft {
            title: "牛乳を買う".into(),
            ..Default::default()
        };
        let t = s.create_task(None, &d).unwrap();
        assert_eq!(t.workspace_id, None);
        let t2 = s
            .create_task(
                None,
                &TaskDraft {
                    title: "二つ目".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!((t.position, t2.position), (0, 1));
        assert_eq!(
            s.tasks(Scope::Unfiled, &TaskFilter::default())
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            s.tasks(Scope::All, &TaskFilter::default()).unwrap().len(),
            2
        );
        s.add_comment(&t.id, None, "メモ").unwrap();
        s.set_task_status(&t.id, Status::Completed).unwrap();
        assert!(s.open_task_counts().unwrap().is_empty());

        // あとからワークスペースに入れる。戻すこともできる
        let w = s.create_workspace(None, "買い物").unwrap();
        s.move_task(&t.id, Some(&w.id)).unwrap();
        assert_eq!(
            s.task(&t.id).unwrap().workspace_id.as_deref(),
            Some(w.id.as_str())
        );
        let unfiled = s.tasks(Scope::Unfiled, &TaskFilter::default()).unwrap();
        assert_eq!(unfiled.iter().map(|t| &t.id).collect::<Vec<_>>(), [&t2.id]);
        assert_eq!(s.tasks(&w.id, &TaskFilter::default()).unwrap().len(), 1);
        assert_eq!(
            s.tasks(Scope::All, &TaskFilter::default()).unwrap().len(),
            2
        );
        // 「すべて」と「未所属」では、子孫を含める指定は無視される
        let f = TaskFilter {
            include_descendants: true,
            open_only: true,
            ..Default::default()
        };
        assert_eq!(s.tasks(Scope::All, &f).unwrap().len(), 1);
        assert_eq!(s.tasks(Scope::Unfiled, &f).unwrap().len(), 1);
        s.move_task(&t.id, None).unwrap();
        assert_eq!(s.task(&t.id).unwrap().workspace_id, None);
        assert_eq!(s.task(&t.id).unwrap().position, 2);
        let ch = s.changes(&t.id).unwrap();
        let last = ch.iter().rfind(|c| c.field == "workspace").unwrap();
        assert_eq!(
            (last.old_value.as_str(), last.new_value.as_str()),
            (w.id.as_str(), "")
        );
        // ワークスペースを消しても、未所属のタスクは残る
        s.delete_workspace(&w.id).unwrap();
        assert_eq!(
            s.tasks(Scope::All, &TaskFilter::default()).unwrap().len(),
            2
        );
    }
}
