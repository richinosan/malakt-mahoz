//! SQLite への保存と読み出し。

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::actor::Actor;
use crate::model::*;
use crate::text::{normalize, normalize_line};
use crate::{schema, Error, Result};

/// 1 つのデータベースへの接続。
///
/// 書き込みはすべてトランザクションで行う。複数ウィンドウからは 1 つの `Store` を共有する。
pub struct Store {
    pub(crate) conn: Connection,
    /// 元に戻す・やり直しの段（`undo`）。
    pub(crate) history: crate::undo::History,
    /// これからの変更を誰がするか。作成者・最終更新者・変更履歴・コメントに残す。
    pub(crate) actor: Actor,
}

/// ディスクの使用量（バイト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// データベースのファイルの大きさ。
    pub database_bytes: u64,
    /// そのうち、消したデータの跡で、整理すると返せる分。
    pub reclaimable_bytes: u64,
    /// そのうち、添付ファイルの中身（圧縮後、重複なし）。
    pub attachment_bytes: u64,
}

const AUTO_VACUUM_INCREMENTAL: i64 = 2;
/// WAL のファイルをこの大きさまで切り詰める。
const WAL_SIZE_LIMIT: i64 = 4 * 1024 * 1024;

impl Drop for Store {
    /// 閉じるときに、消した分の領域を返し、WAL を空にする。
    fn drop(&mut self) {
        let _ = self.compact();
    }
}

const TASK_COLUMNS: &str = "t.id, t.workspace_id, t.title, t.description, t.status, t.priority, \
     t.due, t.position, t.created_at, t.updated_at, t.completed_at, t.start, t.percent, t.parent_id, \
     t.created_by, t.updated_by";

/// 自動で作る「自分」の名前。あとから変えられる。
const SELF_NAME: &str = "自分";

impl Store {
    /// ファイルを開く。なければ作る。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        // 消した分の領域をファイルから返せるようにする。途中から変えるには作り直しが要る
        let auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
        if auto_vacuum != AUTO_VACUUM_INCREMENTAL {
            conn.pragma_update(None, "auto_vacuum", AUTO_VACUUM_INCREMENTAL)?;
            conn.execute_batch("VACUUM")?;
        }
        // WAL は読み書きが互いを止めず、fsync の回数も少ない。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // WAL のファイルが大きくなったまま残らないようにする
        conn.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT)?;
        Self::init(conn)
    }

    /// ほかのプロセス（コマンドなど）がデータベースを書き換えるたびに変わる番号。
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// ディスクの使用量。
    pub fn usage(&self) -> Result<Usage> {
        let get = |name: &str| -> rusqlite::Result<u64> {
            self.conn
                .query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0))
                .map(|n| n as u64)
        };
        let page = get("page_size")?;
        Ok(Usage {
            database_bytes: get("page_count")? * page,
            reclaimable_bytes: get("freelist_count")? * page,
            attachment_bytes: self.blob_bytes()?,
        })
    }

    /// 使っていない領域をファイルから返し、WAL を空にする。
    pub fn compact(&mut self) -> Result<Usage> {
        {
            // 1 行進めるごとに 1 ページ返るので、最後まで進める
            let mut stmt = self.conn.prepare("PRAGMA incremental_vacuum")?;
            let mut rows = stmt.query([])?;
            while rows.next()?.is_some() {}
        }
        // メモリ上のデータベースなど、WAL でないときは何も起きない
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .optional()?;
        self.usage()
    }

    /// メモリ上に作る（テスト用）。
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.set_prepared_statement_cache_capacity(64);
        schema::migrate(&mut conn)?;
        crate::undo::install(&conn)?;
        let mut store = Self {
            conn,
            history: Default::default(),
            actor: Actor::Me,
        };
        store.purge_custom_icons()?;
        Ok(store)
    }

    // ---- 誰が変えるか ----

    /// これからの変更を誰がするか。最初は自分。前の値を返す。
    pub fn set_actor(&mut self, actor: Actor) -> Actor {
        std::mem::replace(&mut self.actor, actor)
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// `f` の間だけ、変更を `actor` がしたものとして残す。
    pub fn acting<R>(&mut self, actor: Actor, f: impl FnOnce(&mut Self) -> R) -> R {
        let before = self.set_actor(actor);
        let result = f(self);
        self.actor = before;
        result
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
            date: None,
            icon: None,
            color: None,
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
                "SELECT id, parent_id, name, position, created_at, updated_at, date, icon, color \
                 FROM workspaces WHERE id = ?1",
            )?
            .query_row(params![id], workspace_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("ワークスペース {id}")))
    }

    /// すべてのワークスペース。親ごとに `position` 順。
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, parent_id, name, position, created_at, updated_at, date, icon, color \
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

    /// 親 `parent_id`（`None` なら最上位）の下の、`index` 番目に置く。
    /// `index` は自分を除いた兄弟の中での位置で、多すぎるときは末尾になる。自分や子孫の中には移せない。
    pub fn place_workspace(
        &mut self,
        id: &str,
        parent_id: Option<&str>,
        index: usize,
    ) -> Result<()> {
        self.workspace(id)?;
        if let Some(p) = parent_id {
            self.workspace(p)?;
            if self.workspace_subtree_ids(id)?.iter().any(|d| d == p) {
                return Err(Error::Cycle);
            }
        }
        let tx = self.conn.transaction()?;
        let mut siblings: Vec<Id> = {
            let mut stmt = tx.prepare_cached(
                "SELECT id FROM workspaces WHERE parent_id IS ?1 AND id <> ?2 \
                 ORDER BY position, id",
            )?;
            let rows = stmt.query_map(params![parent_id, id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        siblings.insert(index.min(siblings.len()), id.to_owned());
        let now = now_ms();
        for (position, sibling) in siblings.iter().enumerate() {
            if sibling == id {
                tx.execute(
                    "UPDATE workspaces SET parent_id = ?2, position = ?3, updated_at = ?4 \
                     WHERE id = ?1",
                    params![id, parent_id, position as i64, now],
                )?;
            } else {
                // 番号が変わらない兄弟は書き直さない（元に戻す履歴を増やさない）
                tx.execute(
                    "UPDATE workspaces SET position = ?2 WHERE id = ?1 AND position <> ?2",
                    params![sibling, position as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 子孫のワークスペースと、その中のタスクもすべて消す。
    /// ほかのワークスペースにも表示しているタスクは消さず、そちらに残す。
    pub fn delete_workspace(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        crate::links::rehome_before_delete(&tx, id)?;
        // 消すワークスペースの中でだけ使っていた担当者も消す。残すと、すべてのワークスペースに出てしまう
        tx.execute(
            "WITH RECURSIVE sub(id) AS ( \
                 SELECT id FROM workspaces WHERE id = ?1 \
                 UNION ALL \
                 SELECT w.id FROM workspaces w JOIN sub ON w.parent_id = sub.id) \
             DELETE FROM people \
             WHERE id IN (SELECT person_id FROM person_workspaces) \
               AND NOT EXISTS (SELECT 1 FROM person_workspaces pw \
                               WHERE pw.person_id = people.id \
                                 AND pw.workspace_id NOT IN (SELECT id FROM sub))",
            params![id],
        )?;
        // 追加行で選んだ値も、消すワークスペースの分は残さない
        tx.execute(
            "WITH RECURSIVE sub(id) AS ( \
                 SELECT id FROM workspaces WHERE id = ?1 \
                 UNION ALL \
                 SELECT w.id FROM workspaces w JOIN sub ON w.parent_id = sub.id) \
             DELETE FROM settings WHERE key IN (SELECT 'quick_choice:' || id FROM sub)",
            params![id],
        )?;
        let n = tx.execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
        found(n, "ワークスペース", id)?;
        tx.commit()?;
        Ok(())
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

    /// そのワークスペースのいちばん上の親。
    pub fn workspace_root(&self, id: &str) -> Result<Id> {
        Ok(self.workspace_path(id)?.swap_remove(0).id)
    }

    /// 最上位からそのワークスペースまでの道筋（パンくず用）。
    pub fn workspace_path(&self, id: &str) -> Result<Vec<Workspace>> {
        let mut stmt = self.conn.prepare_cached(
            "WITH RECURSIVE up(id, parent_id, name, position, created_at, updated_at, date, icon, color, depth) AS ( \
                 SELECT id, parent_id, name, position, created_at, updated_at, date, icon, color, 0 \
                 FROM workspaces WHERE id = ?1 \
                 UNION ALL \
                 SELECT w.id, w.parent_id, w.name, w.position, w.created_at, w.updated_at, \
                        w.date, w.icon, w.color, up.depth + 1 \
                 FROM workspaces w JOIN up ON w.id = up.parent_id) \
             SELECT id, parent_id, name, position, created_at, updated_at, date, icon, color \
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
            "SELECT place, COUNT(*) FROM ( \
                 SELECT workspace_id AS place FROM tasks \
                 WHERE status IN ('needs_action', 'in_process') AND workspace_id IS NOT NULL \
                 UNION ALL \
                 SELECT l.workspace_id FROM task_links l JOIN tasks t ON t.id = l.task_id \
                 WHERE t.status IN ('needs_action', 'in_process')) \
             GROUP BY place",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// ワークスペースのアイコンと色を決める。`None` で既定に戻す。
    pub fn set_workspace_icon(
        &mut self,
        id: &str,
        icon: Option<&str>,
        color: Option<&str>,
    ) -> Result<Workspace> {
        let icon = icon.map(str::trim).filter(|i| !i.is_empty());
        if let Some(icon) = icon {
            let ok = icon.len() <= 40
                && icon
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !ok {
                return Err(Error::Invalid(format!(
                    "アイコンの名前が正しくありません: {icon}"
                )));
            }
            if icon.starts_with(crate::icon::CUSTOM_PREFIX)
                && crate::icon::get(&self.conn, icon)?.is_none()
            {
                return Err(Error::Invalid(format!(
                    "アイコンの画像がありません: {icon}"
                )));
            }
        }
        let color = color.map(str::trim).filter(|c| !c.is_empty());
        if let Some(color) = color {
            if !crate::model::WORKSPACE_COLORS.contains(&color) {
                return Err(Error::Invalid(format!("色が正しくありません: {color}")));
            }
        }
        let n = self.conn.execute(
            "UPDATE workspaces SET icon = ?2, color = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, icon, color, now_ms()],
        )?;
        found(n, "ワークスペース", id)?;
        self.workspace(id)
    }

    // ---- 担当者 ----

    /// 「自分」がまだいなければ作る。自分はいつでも担当に選べる。
    pub fn ensure_self(&mut self) -> Result<()> {
        let has_self: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM people WHERE is_self = 1)",
            [],
            |r| r.get(0),
        )?;
        if !has_self {
            self.create_person(SELF_NAME, true)?;
        }
        Ok(())
    }

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
            workspace_ids: Vec::new(),
        };
        let tx = self.conn.transaction()?;
        if is_self {
            tx.execute("UPDATE people SET is_self = 0 WHERE is_self = 1", [])?;
        }
        insert_person(&tx, &person)?;
        tx.commit()?;
        Ok(person)
    }

    /// そのワークスペースのいちばん上の親と、そのサブワークスペースで使う担当者を作る。
    pub fn create_local_person(&mut self, name: &str, workspace_id: &str) -> Result<Person> {
        let name = required_line(name, "名前")?;
        let root = self.workspace_root(workspace_id)?;
        let now = now_ms();
        let person = Person {
            id: new_id(),
            name,
            is_self: false,
            note: String::new(),
            created_at: now,
            updated_at: now,
            workspace_ids: vec![root],
        };
        let tx = self.conn.transaction()?;
        insert_person(&tx, &person)?;
        put_person_workspaces(&tx, &person)?;
        tx.commit()?;
        Ok(person)
    }

    /// 担当者を使うワークスペースを決める。空なら、すべてのワークスペースで使う。
    /// 渡したワークスペースは、いちばん上の親に直して持つ。
    pub fn set_person_workspaces(&mut self, id: &str, workspace_ids: &[Id]) -> Result<()> {
        let mut roots = Vec::new();
        for w in workspace_ids {
            let root = self.workspace_root(w)?;
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let tx = self.conn.transaction()?;
        let is_self: Option<bool> = tx
            .query_row(
                "SELECT is_self FROM people WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        match is_self {
            None => return Err(Error::NotFound(format!("担当者 {id}"))),
            Some(true) if !roots.is_empty() => {
                return Err(Error::Invalid(
                    "「自分」は、すべてのワークスペースで使います".into(),
                ));
            }
            Some(_) => {}
        }
        tx.execute(
            "DELETE FROM person_workspaces WHERE person_id = ?1",
            params![id],
        )?;
        for root in &roots {
            tx.execute(
                "INSERT INTO person_workspaces(person_id, workspace_id) VALUES (?1, ?2)",
                params![id, root],
            )?;
        }
        tx.execute(
            "UPDATE people SET updated_at = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 担当者を、そのワークスペース（のいちばん上の親）でも使えるようにする。
    /// すべてのワークスペースで使う担当者は、そのまま。
    pub fn share_person(&mut self, id: &str, workspace_id: &str) -> Result<()> {
        let root = self.workspace_root(workspace_id)?;
        let mut ids: Vec<Id> = self
            .conn
            .prepare_cached("SELECT workspace_id FROM person_workspaces WHERE person_id = ?1")?
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if ids.is_empty() || ids.contains(&root) {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM people WHERE id = ?1)",
                params![id],
                |r| r.get(0),
            )?;
            return found(usize::from(exists), "担当者", id);
        }
        ids.push(root);
        self.set_person_workspaces(id, &ids)
    }

    /// 自分が先頭、あとは名前順。
    pub fn people(&self) -> Result<Vec<Person>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name, is_self, note, created_at, updated_at \
             FROM people ORDER BY is_self DESC, name, id",
        )?;
        let rows = stmt.query_map([], person_from_row)?;
        let mut people: Vec<Person> = rows.collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT person_id, workspace_id FROM person_workspaces ORDER BY workspace_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (person, workspace): (Id, Id) = (row.get(0)?, row.get(1)?);
            if let Some(p) = people.iter_mut().find(|p| p.id == person) {
                p.workspace_ids.push(workspace);
            }
        }
        Ok(people)
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
            assignee_roles: vec![Roles::R; draft.assignee_ids.len()],
            assignee_done: vec![
                (draft.status == Status::Completed).then_some(now);
                draft.assignee_ids.len()
            ],
            assignee_ids: draft.assignee_ids,
            categories: draft.categories,
            urls: draft.urls,
            linked_workspace_ids: Vec::new(),
            position,
            created_at: now,
            updated_at: now,
            completed_at: (draft.status == Status::Completed).then_some(now),
            created_by: self.actor.clone(),
            updated_by: self.actor.clone(),
        };
        insert_task(&tx, &task)?;
        record_created(&tx, &task, &self.actor)?;
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
            "SELECT person_id, roles, completed_at FROM task_assignees \
             WHERE task_id = ?1 ORDER BY person_id",
        )?;
        let mut rows = stmt.query(params![id])?;
        while let Some(row) = rows.next()? {
            task.assignee_ids.push(row.get(0)?);
            task.assignee_roles
                .push(Roles::parse(row.get_ref(1)?.as_str().unwrap_or_default()));
            task.assignee_done.push(row.get(2)?);
        }
        drop(rows);
        let mut stmt = self.conn.prepare_cached(
            "SELECT category FROM task_categories WHERE task_id = ?1 ORDER BY category",
        )?;
        task.categories = stmt
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare_cached("SELECT url FROM task_urls WHERE task_id = ?1 ORDER BY position")?;
        task.urls = stmt
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT workspace_id FROM task_links WHERE task_id = ?1 \
             ORDER BY created_at, workspace_id",
        )?;
        task.linked_workspace_ids = stmt
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
             WHERE t.due IS NOT NULL AND substr(t.due, 1, 10) <= ?1 \
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
            Scope::Workspace(_) if descendants => {
                "(t.workspace_id IN (SELECT id FROM sub) OR EXISTS ( \
                 SELECT 1 FROM task_links l WHERE l.task_id = t.id \
                 AND l.workspace_id IN (SELECT id FROM sub)))"
            }
            Scope::Workspace(_) => {
                "(t.workspace_id = ?1 OR EXISTS (SELECT 1 FROM task_links l \
                 WHERE l.task_id = t.id AND l.workspace_id = ?1))"
            }
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

    /// 担当者・カテゴリ・URL をまとめて埋める（タスクごとに問い合わせない）。
    fn fill_links(&self, tasks: &mut [Task]) -> Result<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        let index: HashMap<&str, usize> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();
        let mut found: Vec<(usize, Id, Roles, Option<i64>)> = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT task_id, person_id, roles, completed_at FROM task_assignees \
                 ORDER BY task_id, person_id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let task_id = row.get_ref(0)?.as_str().map_err(rusqlite::Error::from)?;
                if let Some(&i) = index.get(task_id) {
                    let roles = Roles::parse(row.get_ref(2)?.as_str().unwrap_or_default());
                    found.push((i, row.get(1)?, roles, row.get(3)?));
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
        let mut urls: Vec<(usize, String)> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT task_id, url FROM task_urls ORDER BY task_id, position")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let task_id = row.get_ref(0)?.as_str().map_err(rusqlite::Error::from)?;
                if let Some(&i) = index.get(task_id) {
                    urls.push((i, row.get(1)?));
                }
            }
        }
        let mut links: Vec<(usize, Id)> = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT task_id, workspace_id FROM task_links \
                 ORDER BY task_id, created_at, workspace_id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let task_id = row.get_ref(0)?.as_str().map_err(rusqlite::Error::from)?;
                if let Some(&i) = index.get(task_id) {
                    links.push((i, row.get(1)?));
                }
            }
        }
        drop(index);
        for (i, workspace_id) in links {
            tasks[i].linked_workspace_ids.push(workspace_id);
        }
        for (i, person_id, roles, done) in found {
            tasks[i].assignee_ids.push(person_id);
            tasks[i].assignee_roles.push(roles);
            tasks[i].assignee_done.push(done);
        }
        for (i, category) in categories {
            tasks[i].categories.push(category);
        }
        for (i, url) in urls {
            tasks[i].urls.push(url);
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
        let actor = self.actor.clone();
        let tx = self.conn.transaction()?;
        let mut changed = false;
        let mut record = |field: &str, a: String, b: String| -> Result<()> {
            if a != b {
                changed = true;
                insert_change(&tx, id, field, &a, &b, now, &actor)?;
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
        record("urls", old.urls.join("\n"), draft.urls.join("\n"))?;
        if !changed {
            drop(tx);
            return Ok(old);
        }
        tx.execute(
            "UPDATE tasks SET title = ?2, description = ?3, status = ?4, priority = ?5, \
             due = ?6, updated_at = ?7, completed_at = ?8, start = ?9, percent = ?10, \
             parent_id = ?11, updated_by = ?12 WHERE id = ?1",
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
                draft.parent_id,
                actor
            ],
        )?;
        if old.assignee_ids != draft.assignee_ids {
            // 残る人の役と完了は変えない。足した人は R から始まる
            for person_id in &old.assignee_ids {
                if !draft.assignee_ids.contains(person_id) {
                    tx.execute(
                        "DELETE FROM task_assignees WHERE task_id = ?1 AND person_id = ?2",
                        params![id, person_id],
                    )?;
                }
            }
            insert_assignees(&tx, id, &draft.assignee_ids, &[], &[])?;
        }
        // タスクごと完了にしたら全員の分を完了にし、完了から戻したら全員の分を取り消す
        match (old.status, draft.status) {
            (Status::Completed, s) if s != Status::Completed => {
                tx.execute(
                    "UPDATE task_assignees SET completed_at = NULL WHERE task_id = ?1",
                    params![id],
                )?;
            }
            (_, Status::Completed) => {
                tx.execute(
                    "UPDATE task_assignees SET completed_at = ?2 \
                     WHERE task_id = ?1 AND completed_at IS NULL",
                    params![id, now],
                )?;
            }
            _ => {}
        }
        if old.categories != draft.categories {
            tx.execute(
                "DELETE FROM task_categories WHERE task_id = ?1",
                params![id],
            )?;
            insert_categories(&tx, id, &draft.categories)?;
        }
        if old.urls != draft.urls {
            tx.execute("DELETE FROM task_urls WHERE task_id = ?1", params![id])?;
            insert_urls(&tx, id, &draft.urls)?;
        }
        trim_changes(&tx, id)?;
        tx.commit()?;
        self.task(id)
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

    /// 担当者の役を変える。
    ///
    /// 役は 1 つ以上。A（説明責任者）は 1 つのタスクに 1 人までなので、
    /// A を付けるとほかの人の A は外れる（役がなくなる人は R に戻る）。
    pub fn set_assignee_roles(&mut self, id: &str, person_id: &str, roles: Roles) -> Result<Task> {
        let old = self.task(id)?;
        let Some(current) = old.roles_of(person_id) else {
            return Err(Error::Invalid(
                "担当者ではない人の役は変えられません".into(),
            ));
        };
        if roles.is_empty() {
            return Err(Error::Invalid("役は 1 つ以上必要です".into()));
        }
        if roles == current {
            return Ok(old);
        }
        let now = now_ms();
        let actor = self.actor.clone();
        let tx = self.conn.transaction()?;
        let write = |person: &str, before: Roles, after: Roles| -> Result<()> {
            tx.prepare_cached(
                "UPDATE task_assignees SET roles = ?3 WHERE task_id = ?1 AND person_id = ?2",
            )?
            .execute(params![id, person, after.to_string()])?;
            insert_change(
                &tx,
                id,
                "assignee_roles",
                &format!("{person}={before}"),
                &format!("{person}={after}"),
                now,
                &actor,
            )?;
            Ok(())
        };
        if roles.contains(Role::A) {
            for other in old.assignee_ids.iter().filter(|p| *p != person_id) {
                let before = old.roles_of(other).unwrap_or_default();
                if before.contains(Role::A) {
                    let after = before.without(Role::A);
                    write(
                        other,
                        before,
                        if after.is_empty() { Roles::R } else { after },
                    )?;
                }
            }
        }
        write(person_id, current, roles)?;
        touch(&tx, id, now, &actor)?;
        trim_changes(&tx, id)?;
        tx.commit()?;
        self.task(id)
    }

    /// 担当者の「自分の分」を完了にする・取り消す。
    ///
    /// 全員の分がそろうとタスクが完了になる。完了したタスクで誰かの分を
    /// 取り消すと、タスクは進行中に戻る。
    pub fn set_assignee_done(&mut self, id: &str, person_id: &str, done: bool) -> Result<Task> {
        let old = self.task(id)?;
        if old.roles_of(person_id).is_none() {
            return Err(Error::Invalid(
                "担当者ではない人の分は完了にできません".into(),
            ));
        }
        if old.is_done_by(person_id) == done {
            return Ok(old);
        }
        let (was_done, total) = old.done_count();
        if done && was_done + 1 == total {
            // 最後の 1 人。タスクごと完了にする（全員の分も完了になる）
            return self.set_task_status(id, Status::Completed);
        }
        if !done && old.status == Status::Completed {
            // タスクを戻すと全員の分が取り消されるので、ほかの人の分を戻す
            let keep: Vec<(Id, i64)> = old
                .assignee_ids
                .iter()
                .zip(&old.assignee_done)
                .filter(|(p, _)| *p != person_id)
                .filter_map(|(p, at)| at.map(|at| (p.clone(), at)))
                .collect();
            self.set_task_status(id, Status::InProcess)?;
            let tx = self.conn.transaction()?;
            for (person, at) in &keep {
                tx.execute(
                    "UPDATE task_assignees SET completed_at = ?3 \
                     WHERE task_id = ?1 AND person_id = ?2",
                    params![id, person, at],
                )?;
            }
            insert_change(
                &tx,
                id,
                "assignee_done",
                person_id,
                "",
                now_ms(),
                &self.actor,
            )?;
            tx.commit()?;
            return self.task(id);
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE task_assignees SET completed_at = ?3 WHERE task_id = ?1 AND person_id = ?2",
            params![id, person_id, done.then_some(now)],
        )?;
        let (before, after) = if done {
            ("", person_id)
        } else {
            (person_id, "")
        };
        insert_change(&tx, id, "assignee_done", before, after, now, &self.actor)?;
        touch(&tx, id, now, &self.actor)?;
        trim_changes(&tx, id)?;
        tx.commit()?;
        self.task(id)
    }

    /// タスクを渡した順に並べ直す。手動の順は、この順で決まる。
    pub fn set_task_order(&mut self, ids: &[Id]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE tasks SET position = ?2 WHERE id = ?1",
                params![id, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
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
            "UPDATE tasks SET workspace_id = ?2, position = ?3, updated_at = ?4, updated_by = ?5 \
             WHERE id = ?1",
            params![id, workspace_id, position, now, self.actor],
        )?;
        // 移した先にすでに表示していたなら、二重にならないよう表示のほうを外す。
        // どこにも入れない状態に戻すなら、表示先もすべて外す
        tx.execute(
            "DELETE FROM task_links WHERE task_id = ?1 AND (?2 IS NULL OR workspace_id = ?2)",
            params![id, workspace_id],
        )?;
        insert_change(
            &tx,
            id,
            "workspace",
            old.workspace_id.as_deref().unwrap_or_default(),
            workspace_id.unwrap_or_default(),
            now,
            &self.actor,
        )?;
        trim_changes(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_task(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        found(n, "タスク", id)
    }

    /// 変更履歴。古い順（同じ時刻なら残した順）。
    pub fn changes(&self, task_id: &str) -> Result<Vec<Change>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, task_id, field, old_value, new_value, created_at, actor \
             FROM changes WHERE task_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map(params![task_id], change_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 作成の記録を除いた変更履歴（テスト用）。
    #[cfg(test)]
    pub(crate) fn edits(&self, task_id: &str) -> Result<Vec<Change>> {
        let mut changes = self.changes(task_id)?;
        changes.retain(|c| c.field != CREATED_FIELD);
        Ok(changes)
    }

    // ---- コメント ----

    pub fn add_comment(
        &mut self,
        task_id: &str,
        author_id: Option<&str>,
        body: &str,
    ) -> Result<Comment> {
        self.add_comment_with(
            task_id,
            author_id,
            &NewComment {
                body: body.to_owned(),
                ..Default::default()
            },
        )
    }

    /// コメントを書く。状態の変更、レビューコメント、返信もここから。
    ///
    /// - `status` があれば、同時にタスクの状態を変える（完了なら完了報告）
    /// - `review` か `anchor` があれば、返信と解決ができるスレッドのルートになる
    /// - `reply_to` はスレッドのルートだけを指せる。解決済みのスレッドに返信すると未解決に戻る
    pub fn add_comment_with(
        &mut self,
        task_id: &str,
        author_id: Option<&str>,
        new: &NewComment,
    ) -> Result<Comment> {
        let body = normalize(&new.body).trim().to_owned();
        if body.is_empty() {
            return Err(Error::Invalid("コメントが空です".into()));
        }
        let review = new.review || new.anchor.is_some();
        let kinds = usize::from(review)
            + usize::from(new.reply_to.is_some())
            + usize::from(new.status.is_some());
        if kinds > 1 {
            return Err(Error::Invalid(
                "レビューコメント、返信、状態の変更は同時に指定できません".into(),
            ));
        }
        let anchor = match &new.anchor {
            Some(a) => {
                let exact = normalize(&a.exact);
                if exact.trim().is_empty() {
                    return Err(Error::Invalid("選択範囲が空です".into()));
                }
                Some(Anchor {
                    exact,
                    prefix: normalize(&a.prefix),
                    suffix: normalize(&a.suffix),
                })
            }
            None => None,
        };
        self.task(task_id)?;
        if let Some(root_id) = new.reply_to.as_deref() {
            let root = self.comment(root_id)?;
            if root.task_id != task_id || !root.is_thread_root() {
                return Err(Error::Invalid(
                    "返信できるのは、同じタスクのレビューコメントだけです".into(),
                ));
            }
        }
        if let Some(status) = new.status {
            self.set_task_status(task_id, status)?;
        }
        let now = now_ms();
        let comment = Comment {
            id: new_id(),
            task_id: task_id.to_owned(),
            author_id: author_id.map(str::to_owned),
            mentions: crate::mention::find(&body, &self.people()?),
            body,
            pinned: false,
            created_at: now,
            updated_at: now,
            status: new.status,
            review,
            anchor,
            reply_to: new.reply_to.clone(),
            resolved_at: None,
            actor: self.actor.clone(),
        };
        let tx = self.conn.transaction()?;
        insert_comment(&tx, &comment)?;
        if let Some(root_id) = &comment.reply_to {
            tx.execute(
                "UPDATE comments SET resolved_at = NULL WHERE id = ?1",
                params![root_id],
            )?;
        }
        tx.commit()?;
        Ok(comment)
    }

    pub fn comment(&self, id: &str) -> Result<Comment> {
        let sql = format!("SELECT {COMMENT_COLUMNS} FROM comments WHERE id = ?1");
        self.conn
            .prepare_cached(&sql)?
            .query_row(params![id], comment_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("コメント {id}")))
    }

    /// ピン留めが先、あとは古い順。
    pub fn comments(&self, task_id: &str) -> Result<Vec<Comment>> {
        let sql = format!(
            "SELECT {COMMENT_COLUMNS} FROM comments WHERE task_id = ?1 \
             ORDER BY pinned DESC, created_at, id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![task_id], comment_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `since` かそれより後に書かれた・書き直されたコメントの、ID とタスクの ID と時刻。古い順。
    pub fn comments_changed_since(&self, since: i64) -> Result<Vec<(Id, Id, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, task_id, updated_at FROM comments WHERE updated_at >= ?1 \
             ORDER BY updated_at",
        )?;
        let rows = stmt.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// いちばん新しいコメントの時刻。コメントがなければ 0。
    pub fn latest_comment_time(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(updated_at), 0) FROM comments",
            [],
            |r| r.get(0),
        )?)
    }

    /// 本文を書き直す。メンションも読み直す。
    pub fn update_comment(&mut self, id: &str, body: &str) -> Result<Comment> {
        let body = normalize(body).trim().to_owned();
        if body.is_empty() {
            return Err(Error::Invalid("コメントが空です".into()));
        }
        let old = self.comment(id)?;
        if old.body == body {
            return Ok(old);
        }
        let mentions = crate::mention::find(&body, &self.people()?);
        self.conn.execute(
            "UPDATE comments SET body = ?2, mentions = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, body, mentions.join(","), now_ms()],
        )?;
        self.comment(id)
    }

    /// ピン留めする・外す。1 つのタスクに 10 件まで。
    /// 完了報告は最初から目立つ場所に出るので、留める対象にしない。
    pub fn set_comment_pinned(&mut self, id: &str, pinned: bool) -> Result<()> {
        let comment = self.comment(id)?;
        if comment.pinned == pinned {
            return Ok(());
        }
        if pinned {
            if comment.is_completion_report() {
                return Err(Error::Invalid("完了報告はピン留めできません".into()));
            }
            let count: usize = self.conn.query_row(
                "SELECT COUNT(*) FROM comments WHERE task_id = ?1 AND pinned = 1",
                params![comment.task_id],
                |r| r.get(0),
            )?;
            if count >= MAX_PINNED_COMMENTS {
                return Err(Error::Invalid(format!(
                    "ピン留めできるのは {MAX_PINNED_COMMENTS} 件までです"
                )));
            }
        }
        self.conn.execute(
            "UPDATE comments SET pinned = ?2 WHERE id = ?1",
            params![id, pinned],
        )?;
        Ok(())
    }

    /// レビューコメントのスレッドを解決済みにする・再開する。何度呼んでも同じ。
    pub fn set_comment_resolved(&mut self, id: &str, resolved: bool) -> Result<Comment> {
        let comment = self.comment(id)?;
        if !comment.is_thread_root() {
            return Err(Error::Invalid(
                "解決済みにできるのはレビューコメントだけです".into(),
            ));
        }
        if comment.resolved_at.is_some() == resolved {
            return Ok(comment);
        }
        self.conn.execute(
            "UPDATE comments SET resolved_at = ?2 WHERE id = ?1",
            params![id, resolved.then(now_ms)],
        )?;
        self.comment(id)
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
            .prepare(&format!(
                "SELECT {COMMENT_COLUMNS} FROM comments ORDER BY id"
            ))?
            .query_map([], comment_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        let changes = self
            .conn
            .prepare(
                "SELECT id, task_id, field, old_value, new_value, created_at, actor \
                 FROM changes ORDER BY created_at, rowid",
            )?
            .query_map([], change_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        // 使っている画像だけを書き出す
        let mut icons = Vec::new();
        for w in &workspaces {
            let Some(icon) = w.icon.as_deref().filter(|i| crate::icon::is_custom(i)) else {
                continue;
            };
            if icons.iter().any(|(name, _)| name == icon) {
                continue;
            }
            if let Some(png) = crate::icon::get(&self.conn, icon)? {
                icons.push((icon.to_owned(), png));
            }
        }
        Ok(crate::archive::Data {
            icons,
            workspaces,
            people,
            tasks,
            comments,
            changes,
            attachments: self.all_attachments()?,
            task_defaults: self.all_task_defaults()?,
            task_alerts: self.all_task_alerts()?,
            ai_rules: self.all_ai_rules()?,
        })
    }

    /// ID が同じものは上書き、なければ追加する。全体を 1 つのトランザクションで行う。
    /// 添付の中身は、まだ持っていないものだけ `fetch` にハッシュを渡して受け取る。
    pub(crate) fn merge(
        &mut self,
        data: &crate::archive::Data,
        fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        // 親より先に子が来ても入るように、検査をコミットまで遅らせる。
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        // 自動で作ったままの「自分」しかいないところへ、自分のいるデータを取り込むなら復元。
        // 取り込む側の「自分」を使い、手つかずのほうは消す。
        if let Some(theirs) = data.people.iter().find(|p| p.is_self) {
            tx.execute(
                "DELETE FROM people WHERE is_self = 1 AND id <> ?1 AND name = ?2 \
                 AND note = '' AND created_at = updated_at \
                 AND (SELECT COUNT(*) FROM people) = 1 \
                 AND NOT EXISTS(SELECT 1 FROM task_assignees WHERE person_id = people.id) \
                 AND NOT EXISTS(SELECT 1 FROM comments WHERE author_id = people.id) \
                 AND NOT EXISTS(SELECT 1 FROM attachments WHERE author_id = people.id) \
                 AND NOT EXISTS(SELECT 1 FROM tickets WHERE person_id = people.id)",
                params![theirs.id, SELF_NAME],
            )?;
        }
        let had_self: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM people WHERE is_self = 1)",
            [],
            |r| r.get(0),
        )?;
        for (name, png) in &data.icons {
            crate::icon::put(&tx, name, png)?;
        }
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
            put_person_workspaces(&tx, &p)?;
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
            tx.execute("DELETE FROM task_links WHERE task_id = ?1", params![t.id])?;
            tx.execute("DELETE FROM task_urls WHERE task_id = ?1", params![t.id])?;
            insert_task(&tx, t)?;
        }
        for c in &data.comments {
            insert_comment(&tx, c)?;
        }
        for c in &data.changes {
            tx.prepare_cached(
                "INSERT OR REPLACE INTO changes(id, task_id, field, old_value, new_value, \
                 created_at, actor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![
                c.id,
                c.task_id,
                c.field,
                c.old_value,
                c.new_value,
                c.created_at,
                c.actor
            ])?;
        }
        for d in &data.task_defaults {
            crate::defaults::save(&tx, d)?;
        }
        for a in &data.task_alerts {
            crate::alert::save(&tx, a)?;
        }
        for r in &data.ai_rules {
            crate::rules::save(&tx, r)?;
        }
        for a in &data.attachments {
            let known: bool = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)")?
                .query_row(params![a.hash], |r| r.get(0))?;
            if !known {
                let bytes = fetch(&a.hash)?;
                if bytes.len() as u64 != a.size || crate::attachment::hash_of(&bytes) != a.hash {
                    return Err(Error::Invalid(format!(
                        "添付「{}」の中身が壊れています",
                        a.name
                    )));
                }
                crate::attachment::put_blob(&tx, &a.hash, &bytes)?;
            }
            crate::attachment::insert(&tx, a)?;
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

pub(crate) fn required_line(input: &str, what: &str) -> Result<String> {
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
        Some(d) if is_valid_when(d) => Some(d.to_owned()),
        Some(d) => return Err(Error::Invalid(format!("期限が正しくありません: {d}"))),
    };
    let start = match draft.start.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(d) if is_valid_when(d) => Some(d.to_owned()),
        Some(d) => return Err(Error::Invalid(format!("開始日が正しくありません: {d}"))),
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
    let urls = crate::weblink::clean_all(&draft.urls).map_err(Error::Invalid)?;
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
        urls,
    })
}

fn insert_workspace(conn: &Connection, w: &Workspace) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO workspaces(id, parent_id, name, position, created_at, updated_at, date, icon, color) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(id) DO UPDATE SET parent_id = excluded.parent_id, name = excluded.name, \
         position = excluded.position, updated_at = excluded.updated_at, date = excluded.date, \
         icon = excluded.icon, color = excluded.color",
    )?
    .execute(params![
        w.id,
        w.parent_id,
        w.name,
        w.position,
        w.created_at,
        w.updated_at,
        w.date,
        w.icon,
        w.color
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

/// 担当者を使うワークスペースを、`p` のとおりに置き換える。
fn put_person_workspaces(conn: &Connection, p: &Person) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM person_workspaces WHERE person_id = ?1",
        params![p.id],
    )?;
    for w in &p.workspace_ids {
        conn.execute(
            "INSERT OR IGNORE INTO person_workspaces(person_id, workspace_id) VALUES (?1, ?2)",
            params![p.id, w],
        )?;
    }
    Ok(())
}

fn insert_task(conn: &Connection, t: &Task) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO tasks(id, workspace_id, title, description, status, priority, due, \
         position, created_at, updated_at, completed_at, start, percent, parent_id, \
         created_by, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
         ON CONFLICT(id) DO UPDATE SET workspace_id = excluded.workspace_id, \
         title = excluded.title, description = excluded.description, status = excluded.status, \
         priority = excluded.priority, due = excluded.due, position = excluded.position, \
         updated_at = excluded.updated_at, completed_at = excluded.completed_at, \
         start = excluded.start, percent = excluded.percent, parent_id = excluded.parent_id, \
         created_by = excluded.created_by, updated_by = excluded.updated_by",
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
        t.parent_id,
        t.created_by,
        t.updated_by
    ])?;
    insert_assignees(
        conn,
        &t.id,
        &t.assignee_ids,
        &t.assignee_roles,
        &t.assignee_done,
    )?;
    for workspace_id in &t.linked_workspace_ids {
        if t.workspace_id.as_ref() != Some(workspace_id) {
            crate::links::insert(conn, &t.id, workspace_id, t.updated_at)?;
        }
    }
    insert_categories(conn, &t.id, &t.categories)?;
    insert_urls(conn, &t.id, &t.urls)
}

fn insert_urls(conn: &Connection, task_id: &str, urls: &[String]) -> rusqlite::Result<()> {
    let mut stmt =
        conn.prepare_cached("INSERT INTO task_urls(task_id, position, url) VALUES (?1, ?2, ?3)")?;
    for (position, url) in urls.iter().enumerate() {
        stmt.execute(params![task_id, position as i64, url])?;
    }
    Ok(())
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

/// 担当者を足す。すでにいる人はそのままにする。役の指定がない人は R。
fn insert_assignees(
    conn: &Connection,
    task_id: &str,
    ids: &[Id],
    roles: &[Roles],
    done: &[Option<i64>],
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO task_assignees(task_id, person_id, roles, completed_at) \
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (i, person_id) in ids.iter().enumerate() {
        let roles = roles.get(i).copied().unwrap_or_default();
        stmt.execute(params![
            task_id,
            person_id,
            roles.to_string(),
            done.get(i).copied().flatten()
        ])?;
    }
    Ok(())
}

fn insert_comment(conn: &Connection, c: &Comment) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO comments(id, task_id, author_id, body, pinned, created_at, updated_at, \
         status, review, anchor, reply_to, resolved_at, mentions, actor) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(id) DO UPDATE SET body = excluded.body, pinned = excluded.pinned, \
         author_id = excluded.author_id, updated_at = excluded.updated_at, \
         status = excluded.status, review = excluded.review, anchor = excluded.anchor, \
         reply_to = excluded.reply_to, resolved_at = excluded.resolved_at, \
         mentions = excluded.mentions, actor = excluded.actor",
    )?
    .execute(params![
        c.id,
        c.task_id,
        c.author_id,
        c.body,
        c.pinned,
        c.created_at,
        c.updated_at,
        c.status.map(Status::as_str),
        c.review,
        c.anchor
            .as_ref()
            .map(|a| serde_json::to_string(a).expect("文字列だけの構造体")),
        c.reply_to,
        c.resolved_at,
        c.mentions.join(","),
        c.actor
    ])
}

pub(crate) fn insert_change(
    conn: &Connection,
    task_id: &str,
    field: &str,
    old: &str,
    new: &str,
    now: i64,
    actor: &Actor,
) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO changes(id, task_id, field, old_value, new_value, created_at, actor) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![new_id(), task_id, field, old, new, now, actor])
}

/// 中身を変えた時刻と人を残す。
pub(crate) fn touch(
    conn: &Connection,
    task_id: &str,
    now: i64,
    actor: &Actor,
) -> rusqlite::Result<usize> {
    conn.prepare_cached("UPDATE tasks SET updated_at = ?2, updated_by = ?3 WHERE id = ?1")?
        .execute(params![task_id, now, actor])
}

/// 作ったことを変更履歴に残す。作ったときの値（空でないもの）は、項目の名前と値の JSON にして
/// 同じ行に入れる（[`crate::history::created_values`] で読む）。
fn record_created(conn: &Connection, t: &Task, actor: &Actor) -> rusqlite::Result<()> {
    let number = |n: u8| if n == 0 { String::new() } else { n.to_string() };
    let fields = [
        ("title", t.title.clone()),
        ("description", t.description.clone()),
        ("status", t.status.as_str().to_owned()),
        ("priority", number(t.priority)),
        ("start", t.start.clone().unwrap_or_default()),
        ("due", t.due.clone().unwrap_or_default()),
        ("percent", number(t.percent)),
        ("parent", t.parent_id.clone().unwrap_or_default()),
        ("assignees", t.assignee_ids.join(",")),
        ("categories", t.categories.join(",")),
        ("urls", t.urls.join("\n")),
        ("workspace", t.workspace_id.clone().unwrap_or_default()),
    ];
    let values: serde_json::Map<String, serde_json::Value> = fields
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.to_owned(), serde_json::Value::String(v)))
        .collect();
    let json = serde_json::Value::Object(values).to_string();
    insert_change(conn, &t.id, CREATED_FIELD, "", &json, t.created_at, actor)?;
    Ok(())
}

/// 変更履歴が [`MAX_CHANGES_PER_TASK`] 件か [`MAX_CHANGE_BYTES_PER_TASK`] を超えたら、
/// 古いものから消す。作成の記録（誰がいつ作ったか）は残す。
pub(crate) fn trim_changes(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    let (count, bytes): (i64, i64) = conn
        .prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(old_value AS BLOB)) \
             + LENGTH(CAST(new_value AS BLOB))), 0) FROM changes WHERE task_id = ?1",
        )?
        .query_row(params![task_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if count as usize <= MAX_CHANGES_PER_TASK && bytes <= MAX_CHANGE_BYTES_PER_TASK {
        return Ok(());
    }
    // 新しいものから数えて、件数と大きさの両方に収まるところまで残す
    let rows: Vec<(String, i64)> = conn
        .prepare_cached(
            "SELECT id, LENGTH(CAST(old_value AS BLOB)) + LENGTH(CAST(new_value AS BLOB)) \
             FROM changes WHERE task_id = ?1 AND field <> ?2 \
             ORDER BY created_at DESC, rowid DESC",
        )?
        .query_map(params![task_id, CREATED_FIELD], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let (mut kept, mut size) = (0usize, 0i64);
    let mut stmt = conn.prepare_cached("DELETE FROM changes WHERE id = ?1")?;
    for (id, len) in rows {
        // 作成の記録の分も、上限に数える。いちばん新しい変更は、大きくても残す
        if kept == 0 || (kept + 1 < MAX_CHANGES_PER_TASK && size + len <= MAX_CHANGE_BYTES_PER_TASK)
        {
            kept += 1;
            size += len;
        } else {
            stmt.execute(params![id])?;
        }
    }
    Ok(())
}

fn workspace_from_row(r: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        position: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        date: r.get(6)?,
        icon: r.get(7)?,
        color: r.get(8)?,
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
        workspace_ids: Vec::new(),
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
        assignee_roles: Vec::new(),
        assignee_done: Vec::new(),
        categories: Vec::new(),
        urls: Vec::new(),
        linked_workspace_ids: Vec::new(),
        position: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        completed_at: r.get(10)?,
        created_by: r.get(14)?,
        updated_by: r.get(15)?,
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
        status: r.get_ref(7)?.as_str_or_null()?.and_then(Status::parse),
        review: r.get(8)?,
        anchor: r
            .get_ref(9)?
            .as_str_or_null()?
            .and_then(|json| serde_json::from_str(json).ok()),
        reply_to: r.get(10)?,
        resolved_at: r.get(11)?,
        mentions: r
            .get_ref(12)?
            .as_str()?
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
        actor: r.get(13)?,
    })
}

const COMMENT_COLUMNS: &str = "id, task_id, author_id, body, pinned, created_at, updated_at, \
     status, review, anchor, reply_to, resolved_at, mentions, actor";

fn change_from_row(r: &Row<'_>) -> rusqlite::Result<Change> {
    Ok(Change {
        id: r.get(0)?,
        task_id: r.get(1)?,
        field: r.get(2)?,
        old_value: r.get(3)?,
        new_value: r.get(4)?,
        created_at: r.get(5)?,
        actor: r.get(6)?,
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

    fn order(s: &Store, parent: Option<&str>) -> Vec<String> {
        let mut list: Vec<Workspace> = s
            .workspaces()
            .unwrap()
            .into_iter()
            .filter(|w| w.parent_id.as_deref() == parent)
            .collect();
        list.sort_by_key(|w| w.position);
        list.into_iter().map(|w| w.name).collect()
    }

    #[test]
    fn place_workspace_reorders_and_nests() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(None, "B").unwrap();
        let c = s.create_workspace(None, "C").unwrap();
        // 同じ親の中で前へ
        s.place_workspace(&c.id, None, 0).unwrap();
        assert_eq!(order(&s, None), ["C", "A", "B"]);
        // 同じ親の中で後ろへ。番号は自分を除いた位置
        s.place_workspace(&c.id, None, 2).unwrap();
        assert_eq!(order(&s, None), ["A", "B", "C"]);
        // 多すぎる番号は末尾
        s.place_workspace(&a.id, None, 99).unwrap();
        assert_eq!(order(&s, None), ["B", "C", "A"]);
        // サブワークスペースにする（子孫ごと移る）
        let a1 = s.create_workspace(Some(&a.id), "A1").unwrap();
        s.place_workspace(&a.id, Some(&b.id), 0).unwrap();
        assert_eq!(order(&s, None), ["B", "C"]);
        assert_eq!(order(&s, Some(&b.id)), ["A"]);
        assert_eq!(
            s.workspace(&a1.id).unwrap().parent_id.as_deref(),
            Some(a.id.as_str())
        );
        // 兄弟の間に入れる
        let d = s.create_workspace(None, "D").unwrap();
        s.place_workspace(&d.id, Some(&b.id), 0).unwrap();
        assert_eq!(order(&s, Some(&b.id)), ["D", "A"]);
        // 外して最上位の先頭へ
        s.place_workspace(&a.id, None, 0).unwrap();
        assert_eq!(order(&s, None), ["A", "B", "C"]);
        assert_eq!(order(&s, Some(&b.id)), ["D"]);
    }

    #[test]
    fn place_workspace_refuses_cycles_and_unknown_ids() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(Some(&a.id), "B").unwrap();
        assert!(matches!(
            s.place_workspace(&a.id, Some(&a.id), 0),
            Err(Error::Cycle)
        ));
        assert!(matches!(
            s.place_workspace(&a.id, Some(&b.id), 0),
            Err(Error::Cycle)
        ));
        assert!(matches!(
            s.place_workspace("nope", None, 0),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            s.place_workspace(&b.id, Some("nope"), 0),
            Err(Error::NotFound(_))
        ));
        // 断ったときは何も変わらない
        assert_eq!(
            s.workspace(&b.id).unwrap().parent_id.as_deref(),
            Some(a.id.as_str())
        );
    }

    #[test]
    fn place_workspace_undoes_in_one_step() {
        let mut s = store();
        let a = s.create_workspace(None, "A").unwrap();
        let b = s.create_workspace(None, "B").unwrap();
        s.undo_mark().unwrap();
        s.place_workspace(&b.id, Some(&a.id), 0).unwrap();
        s.undo_mark().unwrap();
        s.undo().unwrap();
        assert_eq!(order(&s, None), ["A", "B"]);
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
    fn local_people_belong_to_the_top_workspace() {
        let mut s = store();
        s.ensure_self().unwrap();
        let a = s.create_workspace(None, "A").unwrap();
        let sub = s.create_workspace(Some(&a.id), "A-1").unwrap();
        let b = s.create_workspace(None, "B").unwrap();
        // サブワークスペースで作っても、いちばん上の親のものになる
        let sato = s.create_local_person("佐藤", &sub.id).unwrap();
        assert_eq!(sato.workspace_ids, [a.id.clone()]);
        let get = |s: &Store, id: &str| {
            s.people()
                .unwrap()
                .into_iter()
                .find(|p| p.id == id)
                .unwrap()
        };
        assert_eq!(get(&s, &sato.id), sato);
        assert!(!sato.is_global());

        // ほかのワークスペースでも使えるようにする。2 回しても同じ
        s.share_person(&sato.id, &b.id).unwrap();
        s.share_person(&sato.id, &b.id).unwrap();
        let mut both = vec![a.id.clone(), b.id.clone()];
        both.sort();
        assert_eq!(get(&s, &sato.id).workspace_ids, both);
        // すべてのワークスペースで使う担当者は、そのまま
        let me = s.people().unwrap().remove(0);
        s.share_person(&me.id, &b.id).unwrap();
        assert!(get(&s, &me.id).is_global());
        assert!(matches!(
            s.share_person("nope", &b.id),
            Err(Error::NotFound(_))
        ));
        // 「自分」は、ワークスペースを決められない
        assert!(matches!(
            s.set_person_workspaces(&me.id, std::slice::from_ref(&b.id)),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.set_person_workspaces("nope", &[]),
            Err(Error::NotFound(_))
        ));

        // 片方のワークスペースを消しても、もう片方で使っているので残る
        s.delete_workspace(&b.id).unwrap();
        assert_eq!(get(&s, &sato.id).workspace_ids, [a.id.clone()]);
        // 使っているワークスペースがなくなると、担当者も消える。「自分」は残る
        s.undo_mark().unwrap();
        s.delete_workspace(&a.id).unwrap();
        let left = s.people().unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].is_self);
        // 元に戻すと、担当者もワークスペースとの対応も戻る
        assert!(s.undo().unwrap());
        assert_eq!(get(&s, &sato.id).workspace_ids, [a.id.clone()]);

        // 空にすると、すべてのワークスペースで使う
        s.set_person_workspaces(&sato.id, &[]).unwrap();
        assert!(get(&s, &sato.id).is_global());
        s.set_person_workspaces(&sato.id, std::slice::from_ref(&sub.id))
            .unwrap();
        assert_eq!(get(&s, &sato.id).workspace_ids, [a.id]);
    }

    #[test]
    fn local_people_survive_export_and_import() {
        let mut s = store();
        s.ensure_self().unwrap();
        let a = s.create_workspace(None, "A").unwrap();
        let sato = s.create_local_person("佐藤", &a.id).unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&s, &mut zip).unwrap();
        let mut other = store();
        crate::archive::import(&mut other, &mut zip).unwrap();
        assert_eq!(other.people().unwrap(), s.people().unwrap());
        // 取り込み直すと、書き出したときの対応に戻る
        other.set_person_workspaces(&sato.id, &[]).unwrap();
        zip.set_position(0);
        crate::archive::import(&mut other, &mut zip).unwrap();
        assert_eq!(other.people().unwrap(), s.people().unwrap());
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
        assert!(s.edits(&t.id).unwrap().is_empty());

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
            .edits(&t.id)
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
        let ch = s.edits(&t.id).unwrap();
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
        assert!(s.edits(&t.id).unwrap().is_empty());
        assert!(matches!(
            s.move_task(&t.id, Some("nope")),
            Err(Error::NotFound(_))
        ));
        s.move_task(&t.id, Some(&b.id)).unwrap();
        let moved = s.task(&t.id).unwrap();
        assert_eq!(moved.workspace_id.as_deref(), Some(b.id.as_str()));
        assert_eq!(moved.position, 1);
        let ch = s.edits(&t.id).unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].field, "workspace");
        assert_eq!(
            (ch[0].old_value.clone(), ch[0].new_value.clone()),
            (a.id, b.id)
        );
        s.delete_task(&t.id).unwrap();
        assert!(matches!(s.delete_task(&t.id), Err(Error::NotFound(_))));
        assert!(s.edits(&t.id).unwrap().is_empty());
    }

    #[test]
    fn changed_comments_are_found_by_time() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(None, &draft("記事")).unwrap();
        assert_eq!(s.latest_comment_time().unwrap(), 0);
        let a = s.add_comment(&t.id, None, "書いた").unwrap();
        assert_eq!(s.latest_comment_time().unwrap(), a.updated_at);
        assert_eq!(
            s.comments_changed_since(a.updated_at).unwrap(),
            vec![(a.id.clone(), t.id.clone(), a.updated_at)]
        );
        assert!(s
            .comments_changed_since(a.updated_at + 1)
            .unwrap()
            .is_empty());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let edited = s.update_comment(&a.id, "書き直した").unwrap();
        assert_eq!(
            s.comments_changed_since(a.updated_at + 1).unwrap(),
            vec![(a.id, t.id, edited.updated_at)]
        );
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
        assert_eq!(s.edits(&grand.id).unwrap()[0].field, "parent");
        // 空文字の親は「なし」
        d.parent_id = Some(String::new());
        assert_eq!(s.update_task(&grand.id, &d).unwrap().parent_id, None);
        // 親を消すとサブタスクは残り、親なしになる
        s.delete_task(&parent.id).unwrap();
        assert_eq!(s.task(&child.id).unwrap().parent_id, None);
    }

    #[test]
    fn urls_keep_their_order_and_are_recorded() {
        let mut s = Store::open_in_memory().unwrap();
        let w = s.create_workspace(None, "W").unwrap();
        let mut d = draft("t");
        d.urls = vec![
            "https://b.example.jp/x".into(),
            " github.com/acme/app/pull/7 ".into(),
            "https://b.example.jp/x".into(),
            "".into(),
        ];
        let t = s.create_task(Some(&w.id), &d).unwrap();
        assert_eq!(
            t.urls,
            [
                "https://b.example.jp/x",
                "https://github.com/acme/app/pull/7"
            ]
        );
        assert_eq!(s.task(&t.id).unwrap(), t);
        assert_eq!(s.tasks(&w.id, &TaskFilter::default()).unwrap()[0], t);

        // 足す・外す・並べ替えは変更履歴に残る
        let mut d = TaskDraft::from(&t);
        d.urls.insert(0, "https://a.example.jp/".into());
        d.urls.retain(|u| !u.contains("b.example"));
        let u = s.update_task(&t.id, &d).unwrap();
        assert_eq!(
            u.urls,
            [
                "https://a.example.jp/",
                "https://github.com/acme/app/pull/7"
            ]
        );
        let edit = &s.edits(&t.id).unwrap()[0];
        assert_eq!(edit.field, "urls");
        assert_eq!(
            edit.old_value,
            "https://b.example.jp/x\nhttps://github.com/acme/app/pull/7"
        );

        // URL でないものがあれば、何も変えずに失敗する
        let mut bad = TaskDraft::from(&u);
        bad.urls.push("メモ".into());
        assert!(matches!(
            s.update_task(&t.id, &bad),
            Err(Error::Invalid(m)) if m.contains("「メモ」")
        ));
        assert_eq!(s.task(&t.id).unwrap().urls, u.urls);

        // 元に戻せる。タスクを消すと URL も消える
        s.undo_mark().unwrap();
        let mut d = TaskDraft::from(&u);
        d.urls.clear();
        s.update_task(&t.id, &d).unwrap();
        s.undo_mark().unwrap();
        assert!(s.undo().unwrap());
        assert_eq!(s.task(&t.id).unwrap().urls, u.urls);
        s.delete_task(&t.id).unwrap();
        let left: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM task_urls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
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
            .edits(&t.id)
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
        let ch = s.edits(&t.id).unwrap();
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

#[cfg(test)]
mod raci_tests {
    use super::Store;
    use crate::model::*;
    use crate::Error;

    /// 担当者 3 人のタスクを用意する。
    fn setup() -> (Store, Task, [Id; 3]) {
        let mut s = Store::open_in_memory().unwrap();
        let ids = ["あ", "い", "う"].map(|name| s.create_person(name, false).unwrap().id);
        let task = s
            .create_task(
                None,
                &TaskDraft {
                    title: "会場を決める".into(),
                    assignee_ids: ids.to_vec(),
                    ..Default::default()
                },
            )
            .unwrap();
        (s, task, ids)
    }

    #[test]
    fn new_assignees_start_as_r() {
        let (s, task, ids) = setup();
        assert_eq!(task.assignee_roles, [Roles::R; 3]);
        let loaded = s.task(&task.id).unwrap();
        assert_eq!(loaded, task);
        assert_eq!(loaded.roles_of(&ids[0]), Some(Roles::R));
        assert_eq!(loaded.roles_of("いない人"), None);
        let listed = s.tasks(Scope::All, &TaskFilter::default()).unwrap();
        assert_eq!(listed[0], task);
    }

    #[test]
    fn accountable_is_one_person_per_task() {
        let (mut s, task, ids) = setup();
        let ra = Roles::of(&[Role::R, Role::A]);
        let t = s.set_assignee_roles(&task.id, &ids[0], ra).unwrap();
        assert_eq!(t.roles_of(&ids[0]), Some(ra));
        // A だけの人から A が移ると、その人は R に戻る
        let only_a = Roles::of(&[Role::A]);
        let t = s.set_assignee_roles(&task.id, &ids[1], only_a).unwrap();
        assert_eq!(t.roles_of(&ids[0]), Some(Roles::R));
        assert_eq!(t.roles_of(&ids[1]), Some(only_a));
        let t = s
            .set_assignee_roles(&task.id, &ids[2], Roles::of(&[Role::A, Role::I]))
            .unwrap();
        assert_eq!(t.roles_of(&ids[1]), Some(Roles::R));
        assert_eq!(t.roles_of(&ids[2]).unwrap().to_string(), "A,I");
        assert_eq!(
            t.assignee_roles
                .iter()
                .filter(|r| r.contains(Role::A))
                .count(),
            1
        );
        // A 以外は何人でも持てる
        let c = Roles::of(&[Role::C]);
        s.set_assignee_roles(&task.id, &ids[0], c).unwrap();
        let t = s.set_assignee_roles(&task.id, &ids[1], c).unwrap();
        assert_eq!(t.assignee_roles[..2], [c, c]);
    }

    #[test]
    fn roles_need_at_least_one_and_an_assignee() {
        let (mut s, task, ids) = setup();
        assert!(matches!(
            s.set_assignee_roles(&task.id, &ids[0], Roles::NONE),
            Err(Error::Invalid(_))
        ));
        let outsider = s.create_person("え", false).unwrap();
        assert!(matches!(
            s.set_assignee_roles(&task.id, &outsider.id, Roles::R),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.set_assignee_done(&task.id, &outsider.id, true),
            Err(Error::Invalid(_))
        ));
        assert_eq!(s.task(&task.id).unwrap(), task);
    }

    #[test]
    fn role_changes_are_recorded() {
        let (mut s, task, ids) = setup();
        s.set_assignee_roles(&task.id, &ids[0], Roles::of(&[Role::A]))
            .unwrap();
        // 同じ役をもう一度付けても履歴は増えない
        s.set_assignee_roles(&task.id, &ids[0], Roles::of(&[Role::A]))
            .unwrap();
        s.set_assignee_roles(&task.id, &ids[1], Roles::of(&[Role::A, Role::C]))
            .unwrap();
        let changes: Vec<(String, String)> = s
            .edits(&task.id)
            .unwrap()
            .into_iter()
            .filter(|c| c.field == "assignee_roles")
            .map(|c| (c.old_value, c.new_value))
            .collect();
        assert_eq!(
            changes,
            [
                (format!("{}=R", ids[0]), format!("{}=A", ids[0])),
                (format!("{}=A", ids[0]), format!("{}=R", ids[0])),
                (format!("{}=R", ids[1]), format!("{}=A,C", ids[1])),
            ]
        );
    }

    #[test]
    fn roles_survive_adding_and_removing_other_assignees() {
        let (mut s, task, ids) = setup();
        let ci = Roles::of(&[Role::C, Role::I]);
        s.set_assignee_roles(&task.id, &ids[0], ci).unwrap();
        s.set_assignee_done(&task.id, &ids[0], true).unwrap();
        let mut draft = TaskDraft::from(&s.task(&task.id).unwrap());
        draft.assignee_ids = vec![ids[0].clone(), ids[2].clone()];
        let t = s.update_task(&task.id, &draft).unwrap();
        assert_eq!(t.assignee_ids, [ids[0].clone(), ids[2].clone()]);
        assert_eq!(t.assignee_roles, [ci, Roles::R]);
        assert!(t.is_done_by(&ids[0]) && !t.is_done_by(&ids[2]));
        // 外して足し直した人は R から
        draft.assignee_ids = vec![ids[2].clone()];
        s.update_task(&task.id, &draft).unwrap();
        draft.assignee_ids = vec![ids[0].clone(), ids[2].clone()];
        let t = s.update_task(&task.id, &draft).unwrap();
        assert_eq!(t.roles_of(&ids[0]), Some(Roles::R));
        assert!(!t.is_done_by(&ids[0]));
    }

    #[test]
    fn task_completes_when_everyone_is_done() {
        let (mut s, task, ids) = setup();
        let t = s.set_assignee_done(&task.id, &ids[0], true).unwrap();
        assert_eq!((t.status, t.done_count()), (Status::NeedsAction, (1, 3)));
        let t = s.set_assignee_done(&task.id, &ids[1], true).unwrap();
        assert_eq!((t.status, t.done_count()), (Status::NeedsAction, (2, 3)));
        // 役に関わらず全員を数える
        s.set_assignee_roles(&task.id, &ids[2], Roles::of(&[Role::I]))
            .unwrap();
        let t = s.set_assignee_done(&task.id, &ids[2], true).unwrap();
        assert_eq!((t.status, t.done_count()), (Status::Completed, (3, 3)));
        assert_eq!(t.percent, 100);
        assert!(t.completed_at.is_some());
    }

    #[test]
    fn undoing_one_person_reopens_the_task() {
        let (mut s, task, ids) = setup();
        for id in &ids {
            s.set_assignee_done(&task.id, id, true).unwrap();
        }
        let t = s.set_assignee_done(&task.id, &ids[1], false).unwrap();
        assert_eq!((t.status, t.done_count()), (Status::InProcess, (2, 3)));
        assert!(t.is_done_by(&ids[0]) && !t.is_done_by(&ids[1]) && t.is_done_by(&ids[2]));
        assert_eq!(t.completed_at, None);
        // 取り消しの取り消しは何もしない
        assert_eq!(s.set_assignee_done(&task.id, &ids[1], false).unwrap(), t);
    }

    #[test]
    fn completing_the_whole_task_completes_everyone() {
        let (mut s, task, ids) = setup();
        s.set_assignee_done(&task.id, &ids[0], true).unwrap();
        let first = s.task(&task.id).unwrap().assignee_done[0];
        let t = s.set_task_status(&task.id, Status::Completed).unwrap();
        assert_eq!(t.done_count(), (3, 3));
        // 先に完了していた人の時刻は変えない
        assert_eq!(t.assignee_done[0], first);
        // タスクを戻すと全員の分が取り消される
        let t = s.set_task_status(&task.id, Status::NeedsAction).unwrap();
        assert_eq!(t.done_count(), (0, 3));
        // 中止では誰の分も完了にしない
        let t = s.set_task_status(&task.id, Status::Cancelled).unwrap();
        assert_eq!(t.done_count(), (0, 3));
    }

    #[test]
    fn a_single_assignee_completes_the_task() {
        let mut s = Store::open_in_memory().unwrap();
        let me = s.create_person("自分", true).unwrap();
        let task = s
            .create_task(
                None,
                &TaskDraft {
                    title: "牛乳を買う".into(),
                    assignee_ids: vec![me.id.clone()],
                    ..Default::default()
                },
            )
            .unwrap();
        let t = s.set_assignee_done(&task.id, &me.id, true).unwrap();
        assert_eq!((t.status, t.done_count()), (Status::Completed, (1, 1)));
        let t = s.set_assignee_done(&task.id, &me.id, false).unwrap();
        assert_eq!((t.status, t.done_count()), (Status::InProcess, (0, 1)));
    }

    #[test]
    fn done_changes_are_recorded() {
        let (mut s, task, ids) = setup();
        s.set_assignee_done(&task.id, &ids[0], true).unwrap();
        s.set_assignee_done(&task.id, &ids[0], false).unwrap();
        let changes: Vec<(String, String)> = s
            .edits(&task.id)
            .unwrap()
            .into_iter()
            .filter(|c| c.field == "assignee_done")
            .map(|c| (c.old_value, c.new_value))
            .collect();
        assert_eq!(
            changes,
            [
                (String::new(), ids[0].clone()),
                (ids[0].clone(), String::new())
            ]
        );
    }

    #[test]
    fn roles_and_done_round_trip_through_dump_and_merge() {
        let (mut s, task, ids) = setup();
        s.set_assignee_roles(&task.id, &ids[0], Roles::of(&[Role::A, Role::C]))
            .unwrap();
        s.set_assignee_done(&task.id, &ids[1], true).unwrap();
        let expect = s.task(&task.id).unwrap();
        let mut other = Store::open_in_memory().unwrap();
        other
            .merge(&s.dump().unwrap(), &mut |hash| s.blob(hash))
            .unwrap();
        assert_eq!(other.task(&task.id).unwrap(), expect);
    }
}

#[cfg(test)]
mod comment_tests {
    use super::Store;
    use crate::model::*;
    use crate::Error;

    fn setup() -> (Store, Task) {
        let mut s = Store::open_in_memory().unwrap();
        let task = s
            .create_task(
                None,
                &TaskDraft {
                    title: "記事を書く".into(),
                    description: "導入を書く。本文を書く。".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        (s, task)
    }

    fn review(body: &str) -> NewComment {
        NewComment {
            body: body.into(),
            review: true,
            ..Default::default()
        }
    }

    fn reply(root: &Comment, body: &str) -> NewComment {
        NewComment {
            body: body.into(),
            reply_to: Some(root.id.clone()),
            ..Default::default()
        }
    }

    #[test]
    fn status_comment_changes_the_task_and_is_a_completion_report() {
        let (mut s, task) = setup();
        let c = s
            .add_comment_with(
                &task.id,
                None,
                &NewComment {
                    body: "公開しました".into(),
                    status: Some(Status::Completed),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(c.is_completion_report());
        assert_eq!(s.task(&task.id).unwrap().status, Status::Completed);
        assert_eq!(s.comments(&task.id).unwrap(), [c.clone()]);
        // 完了報告はピン留めできない
        assert!(matches!(
            s.set_comment_pinned(&c.id, true),
            Err(Error::Invalid(_))
        ));
        // 状態つきでも、完了でなければ完了報告ではない
        let c = s
            .add_comment_with(
                &task.id,
                None,
                &NewComment {
                    body: "やり直します".into(),
                    status: Some(Status::InProcess),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!c.is_completion_report());
        assert_eq!(s.task(&task.id).unwrap().status, Status::InProcess);
        s.set_comment_pinned(&c.id, true).unwrap();
    }

    #[test]
    fn empty_status_comment_changes_nothing() {
        let (mut s, task) = setup();
        let result = s.add_comment_with(
            &task.id,
            None,
            &NewComment {
                body: "  \n".into(),
                status: Some(Status::Completed),
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(Error::Invalid(_))));
        assert_eq!(s.task(&task.id).unwrap().status, Status::NeedsAction);
    }

    #[test]
    fn pins_are_limited_per_task() {
        let (mut s, task) = setup();
        let ids: Vec<Id> = (0..=MAX_PINNED_COMMENTS)
            .map(|i| {
                s.add_comment(&task.id, None, &format!("メモ {i}"))
                    .unwrap()
                    .id
            })
            .collect();
        for id in &ids[..MAX_PINNED_COMMENTS] {
            s.set_comment_pinned(id, true).unwrap();
        }
        // すでに留めてあるものをもう一度留めても数えない
        s.set_comment_pinned(&ids[0], true).unwrap();
        assert!(matches!(
            s.set_comment_pinned(&ids[MAX_PINNED_COMMENTS], true),
            Err(Error::Invalid(_))
        ));
        // 1 つ外せば留められる
        s.set_comment_pinned(&ids[0], false).unwrap();
        s.set_comment_pinned(&ids[MAX_PINNED_COMMENTS], true)
            .unwrap();
        let pinned = s
            .comments(&task.id)
            .unwrap()
            .iter()
            .filter(|c| c.pinned)
            .count();
        assert_eq!(pinned, MAX_PINNED_COMMENTS);
        // 別のタスクの数は別
        let other = s
            .create_task(
                None,
                &TaskDraft {
                    title: "別".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let c = s.add_comment(&other.id, None, "メモ").unwrap();
        s.set_comment_pinned(&c.id, true).unwrap();
    }

    #[test]
    fn editing_updates_body_time_and_mentions() {
        let (mut s, task) = setup();
        let sato = s.create_person("佐藤", false).unwrap();
        let tanaka = s.create_person("田中", false).unwrap();
        let c = s
            .add_comment(&task.id, None, "@佐藤 確認お願いします")
            .unwrap();
        assert_eq!(c.mentions, [sato.id.clone()]);
        assert!(!c.is_edited());
        // 同じ本文なら何も変わらない
        assert_eq!(
            s.update_comment(&c.id, "@佐藤 確認お願いします\n").unwrap(),
            c
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let edited = s.update_comment(&c.id, "@田中 に変更します").unwrap();
        assert_eq!(edited.body, "@田中 に変更します");
        assert_eq!(edited.mentions, [tanaka.id.clone()]);
        assert!(edited.is_edited());
        assert_eq!(edited.created_at, c.created_at);
        assert!(matches!(
            s.update_comment(&c.id, " "),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.update_comment("ない", "本文"),
            Err(Error::NotFound(_))
        ));
        assert_eq!(s.comments(&task.id).unwrap(), [edited]);
    }

    #[test]
    fn review_threads_reply_resolve_and_reopen() {
        let (mut s, task) = setup();
        let root = s
            .add_comment_with(&task.id, None, &review("全体に見出しがほしい"))
            .unwrap();
        assert!(root.is_thread_root());
        let first = s
            .add_comment_with(&task.id, None, &reply(&root, "足します"))
            .unwrap();
        assert_eq!(first.reply_to.as_deref(), Some(root.id.as_str()));
        assert!(!first.is_thread_root());

        let resolved = s.set_comment_resolved(&root.id, true).unwrap();
        assert!(resolved.resolved_at.is_some());
        // 何度呼んでも同じ
        assert_eq!(s.set_comment_resolved(&root.id, true).unwrap(), resolved);
        // 解決済みのスレッドに返信すると未解決に戻る
        s.add_comment_with(&task.id, None, &reply(&root, "まだでした"))
            .unwrap();
        assert_eq!(s.comment(&root.id).unwrap().resolved_at, None);
        s.set_comment_resolved(&root.id, true).unwrap();
        let reopened = s.set_comment_resolved(&root.id, false).unwrap();
        assert_eq!(reopened.resolved_at, None);

        // ルートを消すと返信も消える
        s.delete_comment(&root.id).unwrap();
        assert!(s.comments(&task.id).unwrap().is_empty());
    }

    #[test]
    fn anchored_review_keeps_the_selection() {
        let (mut s, task) = setup();
        let c = s
            .add_comment_with(
                &task.id,
                None,
                &NewComment {
                    body: "ここを詳しく".into(),
                    anchor: Some(Anchor {
                        exact: "本文を書く".into(),
                        prefix: "導入を書く。".into(),
                        suffix: "。".into(),
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        // 選択範囲があればレビューコメントになる
        assert!(c.review && c.is_thread_root());
        let loaded = s.comment(&c.id).unwrap();
        assert_eq!(loaded, c);
        assert_eq!(loaded.anchor.unwrap().exact, "本文を書く");
        let empty = NewComment {
            body: "x".into(),
            anchor: Some(Anchor::default()),
            ..Default::default()
        };
        assert!(matches!(
            s.add_comment_with(&task.id, None, &empty),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn thread_rules_are_enforced() {
        let (mut s, task) = setup();
        let plain = s.add_comment(&task.id, None, "ふつうのコメント").unwrap();
        let root = s
            .add_comment_with(&task.id, None, &review("レビュー"))
            .unwrap();
        let child = s
            .add_comment_with(&task.id, None, &reply(&root, "返信"))
            .unwrap();
        let invalid = |s: &mut Store, task_id: &str, new: &NewComment| {
            assert!(matches!(
                s.add_comment_with(task_id, None, new),
                Err(Error::Invalid(_))
            ));
        };
        // ふつうのコメントや返信には返信できない（スレッドは 1 段）
        invalid(&mut s, &task.id, &reply(&plain, "x"));
        invalid(&mut s, &task.id, &reply(&child, "x"));
        // 別のタスクのスレッドには返信できない
        let other = s
            .create_task(
                None,
                &TaskDraft {
                    title: "別".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        invalid(&mut s, &other.id, &reply(&root, "x"));
        // レビュー、返信、状態の変更は同時に指定できない
        let mut both = reply(&root, "x");
        both.review = true;
        invalid(&mut s, &task.id, &both);
        let mut with_status = review("x");
        with_status.status = Some(Status::Completed);
        invalid(&mut s, &task.id, &with_status);
        assert_eq!(s.task(&task.id).unwrap().status, Status::NeedsAction);
        // 解決済みにできるのはルートだけ
        for id in [&plain.id, &child.id] {
            assert!(matches!(
                s.set_comment_resolved(id, true),
                Err(Error::Invalid(_))
            ));
        }
        assert_eq!(s.comments(&task.id).unwrap().len(), 3);
    }

    #[test]
    fn threads_and_mentions_round_trip_through_dump_and_merge() {
        let (mut s, task) = setup();
        let sato = s.create_person("佐藤", false).unwrap();
        let anchored = NewComment {
            body: "@佐藤 ここ".into(),
            anchor: Some(Anchor {
                exact: "導入".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let root = s
            .add_comment_with(&task.id, Some(&sato.id), &anchored)
            .unwrap();
        s.add_comment_with(&task.id, None, &reply(&root, "直しました"))
            .unwrap();
        s.set_comment_resolved(&root.id, true).unwrap();
        s.add_comment_with(
            &task.id,
            None,
            &NewComment {
                body: "完了".into(),
                status: Some(Status::Completed),
                ..Default::default()
            },
        )
        .unwrap();
        let expect = s.comments(&task.id).unwrap();
        assert_eq!(expect[0].mentions, [sato.id.clone()]);
        let mut other = Store::open_in_memory().unwrap();
        other
            .merge(&s.dump().unwrap(), &mut |hash| s.blob(hash))
            .unwrap();
        assert_eq!(other.comments(&task.id).unwrap(), expect);
    }
}

#[cfg(test)]
mod disk_tests {
    use super::Store;
    use crate::model::*;

    fn size(path: &std::path::Path) -> u64 {
        std::fs::metadata(path).map_or(0, |m| m.len())
    }

    #[test]
    fn deleted_data_gives_the_space_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let wal = dir.path().join("data.sqlite3-wal");
        let mut s = Store::open(&path).unwrap();
        let ids: Vec<Id> = (0..200)
            .map(|i| {
                let draft = TaskDraft {
                    title: format!("タスク {i}"),
                    description: "長い説明。".repeat(400),
                    ..Default::default()
                };
                s.create_task(None, &draft).unwrap().id
            })
            .collect();
        let full = s.compact().unwrap();
        assert_eq!(full.reclaimable_bytes, 0);
        assert_eq!(size(&path), full.database_bytes);
        assert!(full.database_bytes > 1_000_000);

        for id in &ids {
            s.delete_task(id).unwrap();
        }
        // 消した直後は、跡が返せる領域として残る
        assert!(s.usage().unwrap().reclaimable_bytes > 500_000);
        let after = s.compact().unwrap();
        assert_eq!(after.reclaimable_bytes, 0);
        assert!(after.database_bytes < full.database_bytes / 4);
        assert_eq!(size(&path), after.database_bytes);
        assert_eq!(size(&wal), 0);
    }

    #[test]
    fn closing_compacts_and_empties_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let full = {
            let mut s = Store::open(&path).unwrap();
            let ids: Vec<Id> = (0..100)
                .map(|i| {
                    let draft = TaskDraft {
                        title: format!("タスク {i}"),
                        description: "長い説明。".repeat(800),
                        ..Default::default()
                    };
                    s.create_task(None, &draft).unwrap().id
                })
                .collect();
            let full = s.compact().unwrap().database_bytes;
            for id in &ids {
                s.delete_task(id).unwrap();
            }
            full
        };
        assert!(size(&path) < full / 4);
        assert_eq!(size(&dir.path().join("data.sqlite3-wal")), 0);
        // 開き直しても中身は読める
        let s = Store::open(&path).unwrap();
        assert!(s
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
            .is_empty());
        assert_eq!(s.usage().unwrap().reclaimable_bytes, 0);
    }

    #[test]
    fn existing_databases_are_converted_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite3");
        // 領域を返さない設定で作られた、古いデータベース
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA auto_vacuum = NONE; CREATE TABLE keep(x); INSERT INTO keep VALUES (1);",
            )
            .unwrap();
        }
        drop(Store::open(&path).unwrap());
        let conn = rusqlite::Connection::open(&path).unwrap();
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, super::AUTO_VACUUM_INCREMENTAL);
        let kept: i64 = conn
            .query_row("SELECT x FROM keep", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[test]
    fn in_memory_store_reports_usage() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(s.usage().unwrap().database_bytes > 0);
        s.compact().unwrap();
    }

    #[test]
    fn workspace_icon_and_color() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = store.create_workspace(None, "旅行").unwrap();
        assert_eq!((ws.icon, ws.color), (None, None));
        let ws = store
            .set_workspace_icon(&ws.id, Some("plane"), Some("blue"))
            .unwrap();
        assert_eq!(ws.icon.as_deref(), Some("plane"));
        assert_eq!(ws.color.as_deref(), Some("blue"));
        assert_eq!(store.workspaces().unwrap()[0], ws);
        // 正しくない値は入れない
        assert!(store
            .set_workspace_icon(&ws.id, Some("../x"), None)
            .is_err());
        assert!(store
            .set_workspace_icon(&ws.id, Some("plane"), Some("#fff"))
            .is_err());
        assert!(store.set_workspace_icon("ない", None, None).is_err());
        assert_eq!(store.workspace(&ws.id).unwrap(), ws);
        // 既定に戻す
        let ws = store.set_workspace_icon(&ws.id, None, Some(" ")).unwrap();
        assert_eq!((ws.icon, ws.color), (None, None));
    }

    #[test]
    fn self_is_made_once() {
        let mut store = Store::open_in_memory().unwrap();
        store.ensure_self().unwrap();
        store.ensure_self().unwrap();
        let people = store.people().unwrap();
        assert_eq!(people.len(), 1);
        assert!(people[0].is_self);
        assert_eq!(people[0].name, "自分");
        // すでに自分がいれば足さない
        let mut store = Store::open_in_memory().unwrap();
        let me = store.create_person("田中", true).unwrap();
        store.ensure_self().unwrap();
        assert_eq!(store.people().unwrap(), [me]);
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;
    use crate::history;

    fn draft(title: &str) -> TaskDraft {
        TaskDraft {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn creation_and_updates_remember_who() {
        let mut s = Store::open_in_memory().unwrap();
        let w = s.create_workspace(None, "W").unwrap();
        let t = s.create_task(Some(&w.id), &draft("下見")).unwrap();
        assert_eq!((&t.created_by, &t.updated_by), (&Actor::Me, &Actor::Me));
        // 作成の記録が 1 行、作ったときの値つきで残る
        let ch = s.changes(&t.id).unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].field, CREATED_FIELD);
        let entries = history::entries(&ch);
        assert!(entries[0].created);
        let fields: Vec<&str> = entries[0]
            .changes
            .iter()
            .map(|c| c.field.as_str())
            .collect();
        assert!(fields.contains(&"title") && fields.contains(&"workspace"));

        let ai = Actor::ai("Claude", Some("GitHub の定期チェック"));
        let mut d = TaskDraft::from(&t);
        d.title = "会場の下見".into();
        let u = s.acting(ai.clone(), |s| s.update_task(&t.id, &d)).unwrap();
        assert_eq!((&u.created_by, &u.updated_by), (&Actor::Me, &ai));
        assert_eq!(s.actor(), &Actor::Me, "終わったら自分に戻る");
        let last = s.changes(&t.id).unwrap().pop().unwrap();
        assert_eq!((last.field.as_str(), &last.actor), ("title", &ai));

        // 何も変えなければ、最後に変えた人もそのまま
        let agent = Actor::agent("claude-code");
        s.set_actor(agent.clone());
        let same = s.update_task(&t.id, &d).unwrap();
        assert_eq!(same.updated_by, ai);
        // 移す・表示する・役を変える・担当の分を完了にするのも、変えた人が残る
        let p = s.create_person("佐藤", false).unwrap();
        let q = s.create_person("鈴木", false).unwrap();
        d.assignee_ids = vec![p.id.clone(), q.id.clone()];
        s.update_task(&t.id, &d).unwrap();
        let other = s.create_workspace(None, "X").unwrap();
        s.link_task(&t.id, &other.id).unwrap();
        s.set_assignee_roles(&t.id, &p.id, Roles::parse("A"))
            .unwrap();
        s.set_assignee_done(&t.id, &p.id, true).unwrap();
        s.move_task(&t.id, Some(&other.id)).unwrap();
        let task = s.task(&t.id).unwrap();
        assert_eq!(task.updated_by, agent);
        let fields: Vec<(String, Actor)> = s
            .changes(&t.id)
            .unwrap()
            .into_iter()
            .skip(2)
            .map(|c| (c.field, c.actor))
            .collect();
        assert_eq!(
            fields,
            [
                "assignees",
                "link",
                "assignee_roles",
                "assignee_done",
                "workspace"
            ]
            .map(|f| (f.to_owned(), agent.clone()))
        );
        // コメントにも、書いた操作をしたものが残る
        let c = s.add_comment(&t.id, None, "メモ").unwrap();
        assert_eq!(c.actor, agent);
        assert_eq!(s.comment(&c.id).unwrap().actor, agent);
    }

    #[test]
    fn a_ticket_report_is_by_the_reporter() {
        let mut s = Store::open_in_memory().unwrap();
        let p = s.create_person("佐藤", false).unwrap();
        let q = s.create_person("鈴木", false).unwrap();
        let mut d = draft("告知");
        d.assignee_ids = vec![p.id.clone(), q.id.clone()];
        let t = s.create_task(None, &d).unwrap();
        let issued = s.issue_ticket(&t.id, Some(&p.id)).unwrap();
        let comment = s.redeem_ticket(&issued.secret, "公開しました").unwrap();
        let reporter = Actor::Ticket(Some(p.id.clone()));
        assert_eq!(comment.actor, reporter);
        let last = s.changes(&t.id).unwrap().pop().unwrap();
        assert_eq!(
            (last.field.as_str(), &last.actor),
            ("assignee_done", &reporter)
        );
        assert_eq!(s.task(&t.id).unwrap().updated_by, reporter);
        assert_eq!(s.actor(), &Actor::Me);
    }

    #[test]
    fn history_is_trimmed_per_task_but_keeps_the_creation() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(None, &draft("t")).unwrap();
        let mut d = TaskDraft::from(&t);
        for n in 0..MAX_CHANGES_PER_TASK + 20 {
            d.title = format!("t{n}");
            s.update_task(&t.id, &d).unwrap();
        }
        let ch = s.changes(&t.id).unwrap();
        assert_eq!(ch.len(), MAX_CHANGES_PER_TASK);
        assert_eq!(ch[0].field, CREATED_FIELD, "作成の記録は残る");
        assert_eq!(
            ch.last().unwrap().new_value,
            format!("t{}", MAX_CHANGES_PER_TASK + 19)
        );
        assert_eq!(ch[1].old_value, "t20", "古いものから消える");

        // 大きな説明を何度も書き換えても、大きさの上限で収まる
        let big = "あ".repeat(100_000);
        for n in 0..10 {
            d.description = format!("{n}{big}");
            s.update_task(&t.id, &d).unwrap();
        }
        let bytes: i64 = s
            .changes(&t.id)
            .unwrap()
            .iter()
            .map(|c| (c.old_value.len() + c.new_value.len()) as i64)
            .sum();
        assert!(bytes <= MAX_CHANGE_BYTES_PER_TASK, "{bytes}");
        assert_eq!(s.changes(&t.id).unwrap()[0].field, CREATED_FIELD);
    }

    #[test]
    fn who_survives_export_and_import() {
        let mut s = Store::open_in_memory().unwrap();
        let ai = Actor::ai("Claude", None);
        let t = s
            .acting(ai.clone(), |s| s.create_task(None, &draft("t")))
            .unwrap();
        let mut d = TaskDraft::from(&t);
        d.title = "u".into();
        s.acting(Actor::Reminders, |s| s.update_task(&t.id, &d))
            .unwrap();
        s.acting(Actor::agent("codex"), |s| s.add_comment(&t.id, None, "x"))
            .unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&s, &mut zip).unwrap();
        let mut other = Store::open_in_memory().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut other, zip).unwrap();
        let back = other.task(&t.id).unwrap();
        assert_eq!((back.created_by, back.updated_by), (ai, Actor::Reminders));
        assert_eq!(other.changes(&t.id).unwrap(), s.changes(&t.id).unwrap());
        assert_eq!(
            other.comments(&t.id).unwrap()[0].actor,
            Actor::agent("codex")
        );
    }

    /// 前の版のデータベースの行は、誰がしたか分からないものとして読める。
    #[test]
    fn rows_before_the_upgrade_are_unknown() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let tx = conn.transaction().unwrap();
        for sql in &crate::schema::MIGRATIONS[..14] {
            tx.execute_batch(sql).unwrap();
        }
        tx.pragma_update(None, "user_version", 14).unwrap();
        tx.execute_batch(
            "INSERT INTO tasks(id, title, description, status, priority, position, created_at, updated_at)
             VALUES ('t', '下見', '', 'needs_action', 0, 0, 0, 0);
             INSERT INTO changes(id, task_id, field, old_value, new_value, created_at)
             VALUES ('c', 't', 'title', 'a', '下見', 1);",
        )
        .unwrap();
        tx.commit().unwrap();
        let s = Store::init(conn).unwrap();
        let t = s.task("t").unwrap();
        assert_eq!(
            (t.created_by, t.updated_by),
            (Actor::Unknown, Actor::Unknown)
        );
        let ch = s.changes("t").unwrap();
        assert_eq!(ch[0].actor, Actor::Unknown);
        assert!(history::misses_older(&history::entries(&ch)));
    }
}
