use rusqlite::Connection;

/// 順番に適用するマイグレーション。追記のみで、既存の要素は書き換えない。
const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE workspaces (
        id TEXT PRIMARY KEY NOT NULL,
        parent_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        position INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX workspaces_parent ON workspaces(parent_id, position);

    CREATE TABLE people (
        id TEXT PRIMARY KEY NOT NULL,
        name TEXT NOT NULL,
        is_self INTEGER NOT NULL DEFAULT 0,
        note TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;

    CREATE TABLE tasks (
        id TEXT PRIMARY KEY NOT NULL,
        workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
        title TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL,
        priority INTEGER NOT NULL DEFAULT 0,
        start TEXT,
        due TEXT,
        percent INTEGER NOT NULL DEFAULT 0,
        parent_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
        position INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        completed_at INTEGER
    ) STRICT;
    CREATE INDEX tasks_workspace ON tasks(workspace_id, position);
    CREATE INDEX tasks_due ON tasks(due) WHERE due IS NOT NULL;
    CREATE INDEX tasks_parent ON tasks(parent_id) WHERE parent_id IS NOT NULL;

    CREATE TABLE task_categories (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        category TEXT NOT NULL,
        PRIMARY KEY (task_id, category)
    ) STRICT, WITHOUT ROWID;
    CREATE INDEX task_categories_category ON task_categories(category);

    CREATE TABLE task_assignees (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
        PRIMARY KEY (task_id, person_id)
    ) STRICT, WITHOUT ROWID;
    CREATE INDEX task_assignees_person ON task_assignees(person_id);

    CREATE TABLE comments (
        id TEXT PRIMARY KEY NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        author_id TEXT REFERENCES people(id) ON DELETE SET NULL,
        body TEXT NOT NULL,
        pinned INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX comments_task ON comments(task_id, created_at);

    CREATE TABLE changes (
        id TEXT PRIMARY KEY NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        field TEXT NOT NULL,
        old_value TEXT NOT NULL,
        new_value TEXT NOT NULL,
        created_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX changes_task ON changes(task_id, created_at);

    CREATE TABLE settings (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    ) STRICT, WITHOUT ROWID;
    "#,
    // 2: 担当者ごとの役（RACI）と、担当者ごとの完了
    r#"
    ALTER TABLE task_assignees ADD COLUMN roles TEXT NOT NULL DEFAULT 'R';
    ALTER TABLE task_assignees ADD COLUMN completed_at INTEGER;
    "#,
    // 3: 状態つきのコメント、レビューコメントのスレッド、メンション
    r#"
    ALTER TABLE comments ADD COLUMN status TEXT;
    ALTER TABLE comments ADD COLUMN review INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE comments ADD COLUMN anchor TEXT;
    ALTER TABLE comments ADD COLUMN reply_to TEXT REFERENCES comments(id) ON DELETE CASCADE;
    ALTER TABLE comments ADD COLUMN resolved_at INTEGER;
    ALTER TABLE comments ADD COLUMN mentions TEXT NOT NULL DEFAULT '';
    CREATE INDEX comments_reply ON comments(reply_to);
    "#,
    // 4: 添付ファイル。中身は内容のハッシュごとに 1 つだけ持ち、
    //    どの添付からも指されなくなったら消す
    r#"
    CREATE TABLE blobs (
        hash TEXT PRIMARY KEY NOT NULL,
        size INTEGER NOT NULL,
        compressed INTEGER NOT NULL,
        data BLOB NOT NULL
    ) STRICT;
    CREATE TABLE attachments (
        id TEXT PRIMARY KEY NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        author_id TEXT REFERENCES people(id) ON DELETE SET NULL,
        name TEXT NOT NULL,
        size INTEGER NOT NULL,
        hash TEXT NOT NULL REFERENCES blobs(hash),
        created_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX attachments_task ON attachments(task_id, created_at);
    CREATE INDEX attachments_hash ON attachments(hash);
    CREATE TRIGGER attachments_drop_blob AFTER DELETE ON attachments
    WHEN NOT EXISTS (SELECT 1 FROM attachments WHERE hash = OLD.hash)
    BEGIN
        DELETE FROM blobs WHERE hash = OLD.hash;
    END;
    "#,
];

pub(crate) fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: usize =
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn version() -> usize {
    MIGRATIONS.len()
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// 最初の版のデータベースを開くと、担当者は全員 R、未完了として読める。
    #[test]
    fn upgrades_assignees_from_the_first_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(super::MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(
            "INSERT INTO people(id, name, is_self, created_at, updated_at) VALUES ('p', '佐藤', 0, 0, 0);
             INSERT INTO tasks(id, title, description, status, priority, position, created_at, updated_at)
             VALUES ('t', '下見', '', 'needs_action', 0, 0, 0, 0);
             INSERT INTO task_assignees(task_id, person_id) VALUES ('t', 'p');",
        )
        .unwrap();
        super::migrate(&mut conn).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version as usize, super::MIGRATIONS.len());
        let (roles, done): (String, Option<i64>) = conn
            .query_row(
                "SELECT roles, completed_at FROM task_assignees WHERE task_id = 't'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((roles.as_str(), done), ("R", None));
        // もう一度開いても何もしない
        super::migrate(&mut conn).unwrap();
    }
}
