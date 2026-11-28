//! 元に戻す・やり直し。
//!
//! タスクやワークスペースの表に付けた一時トリガーが、変更のたびに「それを打ち消す SQL」を
//! 一時表に書く。画面の 1 回の操作が 1 つの段になり、戻すときは段の SQL を逆順に実行する。
//! 実行中の変更もトリガーが記録するので、それがそのまま「やり直し」の段になる。
//!
//! 記録はメモリ上の一時表に置き、ディスクには書かない。アプリを閉じると消える。
//! 段の数と記録の大きさには上限があり、超えた分は古い段から捨てる。

use rusqlite::Connection;

use crate::{Result, Store};

/// 戻せる段の数の上限。
pub const MAX_STEPS: usize = 100;
/// 記録の大きさの上限（バイト）。添付を消した段は、添付の中身も記録に入る。
pub const MAX_BYTES: i64 = 32 * 1024 * 1024;

/// 記録する表。設定・通知・リマインダーの控えは、戻す相手にしない。
const TABLES: &[&str] = &[
    "workspaces",
    "people",
    "person_workspaces",
    "tasks",
    "task_categories",
    "task_urls",
    "task_assignees",
    "comments",
    "changes",
    "blobs",
    "attachments",
    "task_defaults",
    "task_alerts",
    "task_links",
    "tickets",
    "ai_rules",
    "ai_bots",
    "task_repeats",
    "schedules",
    "schedule_files",
    "work_logs",
];

/// 1 回の操作の記録（`undo_log` の行の範囲）。
#[derive(Debug, Clone, Copy)]
struct Step {
    first: i64,
    last: i64,
    bytes: i64,
}

/// 戻せる段と、やり直せる段。
#[derive(Debug, Default)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// ここまでの行は、どれかの段に入れた。
    marked: i64,
    limits: Option<(usize, i64)>,
}

/// 一時表とトリガーを作る。データベースを開くたびに呼ぶ。
pub(crate) fn install(conn: &Connection) -> Result<()> {
    // 一時表はメモリに置く。一時表を作る前に決める
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.execute_batch(
        "CREATE TEMP TABLE undo_log(seq INTEGER PRIMARY KEY AUTOINCREMENT, sql TEXT NOT NULL);",
    )?;
    for table in TABLES {
        let mut columns: Vec<(String, bool)> = Vec::new();
        {
            let mut stmt = conn.prepare(&format!("PRAGMA main.table_info({table})"))?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                columns.push((row.get(1)?, row.get::<_, i64>(5)? > 0));
            }
        }
        let name = |column: &str| format!("\"{column}\"");
        // 行を見つける条件。主キーの値を、記録するときの値で埋める
        let key = |row: &str| {
            columns
                .iter()
                .filter(|(_, primary)| *primary)
                .map(|(c, _)| format!("'{}=' || quote({row}.{})", name(c), name(c)))
                .collect::<Vec<_>>()
                .join(" || ' AND ' || ")
        };
        let names = columns
            .iter()
            .map(|(c, _)| name(c))
            .collect::<Vec<_>>()
            .join(", ");
        let values = columns
            .iter()
            .map(|(c, _)| format!("quote(old.{})", name(c)))
            .collect::<Vec<_>>()
            .join(" || ', ' || ");
        let sets = columns
            .iter()
            .map(|(c, _)| format!("'{}=' || quote(old.{})", name(c), name(c)))
            .collect::<Vec<_>>()
            .join(" || ', ' || ");
        conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER undo_{table}_insert AFTER INSERT ON main.{table} BEGIN
                 INSERT INTO undo_log(sql) VALUES ('DELETE FROM {table} WHERE ' || {new_key});
             END;
             CREATE TEMP TRIGGER undo_{table}_update AFTER UPDATE ON main.{table} BEGIN
                 INSERT INTO undo_log(sql)
                 VALUES ('UPDATE {table} SET ' || {sets} || ' WHERE ' || {new_key});
             END;
             CREATE TEMP TRIGGER undo_{table}_delete BEFORE DELETE ON main.{table} BEGIN
                 INSERT INTO undo_log(sql)
                 VALUES ('INSERT INTO {table}({names}) VALUES (' || {values} || ')');
             END;",
            new_key = key("new"),
        ))?;
    }
    Ok(())
}

impl Store {
    /// 操作の区切り。前の区切りからの変更を 1 つの段にする。操作の前と後に呼ぶ。
    /// 新しい変更があれば、やり直せる段は捨てる。
    pub fn undo_mark(&mut self) -> Result<()> {
        if let Some(step) = self.undo_tail()? {
            self.history.marked = step.last;
            self.history.undo.push(step);
            let dropped = std::mem::take(&mut self.history.redo);
            self.undo_forget(&dropped)?;
            self.undo_trim()?;
        }
        Ok(())
    }

    /// 前の区切りからの変更を、元に戻す相手にしない（アプリが自動でした変更）。
    pub(crate) fn undo_skip(&mut self) -> Result<()> {
        if let Some(step) = self.undo_tail()? {
            self.undo_forget(&[step])?;
            self.history.marked = step.last;
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// いちばん新しい操作を元に戻す。戻すものがなければ `false`。
    pub fn undo(&mut self) -> Result<bool> {
        self.undo_mark()?;
        let Some(step) = self.history.undo.pop() else {
            return Ok(false);
        };
        if let Some(back) = self.undo_run(step)? {
            self.history.redo.push(back);
        }
        Ok(true)
    }

    /// 元に戻した操作をやり直す。やり直すものがなければ `false`。
    pub fn redo(&mut self) -> Result<bool> {
        // 戻したあとに別の変更があれば、やり直せる段はここで捨てられる
        self.undo_mark()?;
        let Some(step) = self.history.redo.pop() else {
            return Ok(false);
        };
        if let Some(back) = self.undo_run(step)? {
            self.history.undo.push(back);
            self.undo_trim()?;
        }
        Ok(true)
    }

    /// 上限を変える（テスト用）。
    pub fn set_undo_limits(&mut self, steps: usize, bytes: i64) {
        self.history.limits = Some((steps, bytes));
    }

    /// 記録の大きさ（バイト）。
    pub fn undo_bytes(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(CAST(sql AS BLOB))), 0) FROM undo_log",
            [],
            |r| r.get(0),
        )?)
    }

    /// まだどの段にも入れていない変更。
    fn undo_tail(&self) -> Result<Option<Step>> {
        let (first, last, bytes): (Option<i64>, Option<i64>, i64) = self.conn.query_row(
            "SELECT MIN(seq), MAX(seq), COALESCE(SUM(LENGTH(CAST(sql AS BLOB))), 0)
             FROM undo_log WHERE seq > ?1",
            [self.history.marked],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(first
            .zip(last)
            .map(|(first, last)| Step { first, last, bytes }))
    }

    /// 段の SQL を逆順に実行して、その段を消す。実行中の変更が、反対向きの段になる。
    /// 失敗したら何も変えずに、その段だけを捨てる。
    fn undo_run(&mut self, step: Step) -> Result<Option<Step>> {
        let run = |conn: &mut Connection| -> Result<()> {
            let tx = conn.transaction()?;
            // 親より先に子を戻すことがあるので、外部キーは最後にまとめて確かめる
            tx.pragma_update(None, "defer_foreign_keys", "ON")?;
            let sql: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT sql FROM undo_log WHERE seq BETWEEN ?1 AND ?2 ORDER BY seq DESC",
                )?;
                let rows = stmt.query_map([step.first, step.last], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            tx.execute(
                "DELETE FROM undo_log WHERE seq BETWEEN ?1 AND ?2",
                [step.first, step.last],
            )?;
            for sql in &sql {
                tx.execute_batch(sql)?;
            }
            tx.commit()?;
            Ok(())
        };
        if let Err(error) = run(&mut self.conn) {
            self.undo_forget(&[step])?;
            return Err(error);
        }
        let back = self.undo_tail()?;
        if let Some(back) = &back {
            self.history.marked = back.last;
        }
        Ok(back)
    }

    fn undo_forget(&self, steps: &[Step]) -> Result<()> {
        for step in steps {
            self.conn.execute(
                "DELETE FROM undo_log WHERE seq BETWEEN ?1 AND ?2",
                [step.first, step.last],
            )?;
        }
        Ok(())
    }

    /// 上限を超えた分を、古い段から捨てる。
    fn undo_trim(&mut self) -> Result<()> {
        let (max_steps, max_bytes) = self.history.limits.unwrap_or((MAX_STEPS, MAX_BYTES));
        let total = |h: &History| h.undo.iter().chain(&h.redo).map(|s| s.bytes).sum::<i64>();
        while !self.history.undo.is_empty()
            && (self.history.undo.len() > max_steps || total(&self.history) > max_bytes)
        {
            let old = self.history.undo.remove(0);
            self.undo_forget(&[old])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Scope, Store, TaskDraft, TaskFilter};

    fn draft(title: &str) -> TaskDraft {
        TaskDraft {
            title: title.into(),
            ..Default::default()
        }
    }

    fn titles(s: &Store) -> Vec<String> {
        let mut titles: Vec<String> = s
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        titles.sort();
        titles
    }

    #[test]
    fn edits_are_undone_and_redone_step_by_step() {
        let mut s = Store::open_in_memory().unwrap();
        let w = s.create_workspace(None, "W").unwrap();
        s.undo_mark().unwrap();
        let a = s.create_task(Some(&w.id), &draft("a")).unwrap();
        s.undo_mark().unwrap();
        let mut d = TaskDraft::from(&a);
        d.title = "b".into();
        d.categories = vec!["x".into()];
        s.update_task(&a.id, &d).unwrap();
        s.undo_mark().unwrap();

        assert!(s.undo().unwrap());
        let back = s.task(&a.id).unwrap();
        assert_eq!(back.title, "a");
        assert!(back.categories.is_empty());
        assert!(s.edits(&a.id).unwrap().is_empty());
        assert!(s.undo().unwrap());
        assert!(titles(&s).is_empty());
        assert!(s.redo().unwrap());
        assert!(s.redo().unwrap());
        assert_eq!(s.task(&a.id).unwrap().title, "b");
        assert_eq!(s.task(&a.id).unwrap().categories, ["x"]);
        assert!(!s.redo().unwrap());

        // 戻したあとに別の変更をすると、やり直せなくなる
        assert!(s.undo().unwrap());
        s.create_task(Some(&w.id), &draft("c")).unwrap();
        assert!(!s.redo().unwrap());
        assert_eq!(titles(&s), ["a", "c"]);
    }

    #[test]
    fn deleting_a_workspace_comes_back_with_everything_in_it() {
        let mut s = Store::open_in_memory().unwrap();
        s.ensure_self().unwrap();
        let me = s.people().unwrap().remove(0);
        let w = s.create_workspace(None, "W").unwrap();
        let child = s.create_workspace(Some(&w.id), "子").unwrap();
        let mut d = draft("親");
        d.assignee_ids = vec![me.id.clone()];
        d.categories = vec!["x".into()];
        let parent = s.create_task(Some(&w.id), &d).unwrap();
        let mut d = draft("子");
        d.parent_id = Some(parent.id.clone());
        let sub = s.create_task(Some(&child.id), &d).unwrap();
        s.add_attachment(&parent.id, None, "a.txt", b"hello")
            .unwrap();
        s.undo_mark().unwrap();

        s.delete_workspace(&w.id).unwrap();
        assert!(titles(&s).is_empty());
        assert!(s.undo().unwrap());
        assert_eq!(s.workspaces().unwrap().len(), 2);
        let back = s.task(&parent.id).unwrap();
        assert_eq!(back.assignee_ids, [me.id.clone()]);
        assert_eq!(back.categories, ["x"]);
        assert_eq!(s.task(&sub.id).unwrap().parent_id, Some(parent.id.clone()));
        let files = s.attachments(&parent.id).unwrap();
        assert_eq!(s.attachment_data(&files[0].id).unwrap(), b"hello");

        // やり直すと、また消える。添付の中身も残らない
        assert!(s.redo().unwrap());
        assert!(s.workspaces().unwrap().is_empty());
        assert_eq!(s.usage().unwrap().attachment_bytes, 0);
    }

    #[test]
    fn history_is_capped_by_steps_and_bytes() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_undo_limits(3, 1024 * 1024);
        for n in 0..10 {
            s.create_task(None, &draft(&format!("t{n}"))).unwrap();
            s.undo_mark().unwrap();
        }
        for _ in 0..3 {
            assert!(s.undo().unwrap());
        }
        assert!(!s.undo().unwrap());
        assert_eq!(titles(&s).len(), 7);

        // 大きすぎる段は、記録ごと捨てる
        let mut s = Store::open_in_memory().unwrap();
        s.set_undo_limits(100, 2000);
        let t = s.create_task(None, &draft("大きい")).unwrap();
        s.undo_mark().unwrap();
        let mut d = TaskDraft::from(&t);
        d.description = "あ".repeat(2000);
        s.update_task(&t.id, &d).unwrap();
        s.undo_mark().unwrap();
        d.description = String::new();
        s.update_task(&t.id, &d).unwrap();
        s.undo_mark().unwrap();
        assert_eq!(s.undo_bytes().unwrap(), 0);
        assert!(!s.undo().unwrap());
    }

    #[test]
    fn 戻せる記録の上限() {
        assert_eq!(super::MAX_STEPS, 100);
        assert_eq!(super::MAX_BYTES, 33_554_432);
    }

    #[test]
    fn 戻せるかとやり直せるかは記録の有無で決まる() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(!s.can_undo());
        assert!(!s.can_redo());
        assert_eq!(s.undo_bytes().unwrap(), 0);
        s.create_task(None, &draft("a")).unwrap();
        s.undo_mark().unwrap();
        assert!(s.can_undo());
        assert!(!s.can_redo());
        assert!(s.undo_bytes().unwrap() > 0);
        assert!(s.undo().unwrap());
        assert!(!s.can_undo());
        assert!(s.can_redo());
        assert!(s.redo().unwrap());
        assert!(s.can_undo());
        assert!(!s.can_redo());
    }

    #[test]
    fn 段数の上限ちょうどまでは残る() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_undo_limits(2, i64::MAX);
        for title in ["a", "b", "c"] {
            s.create_task(None, &draft(title)).unwrap();
            s.undo_mark().unwrap();
        }
        assert!(s.undo().unwrap());
        assert!(s.undo().unwrap());
        assert!(!s.undo().unwrap());
        assert_eq!(titles(&s), ["a"]);
    }
}
