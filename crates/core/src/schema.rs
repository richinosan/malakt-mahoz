use rusqlite::Connection;

/// 順番に適用するマイグレーション。追記のみで、既存の要素は書き換えない。
const MIGRATIONS: &[&str] = &[r#"
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
    "#];

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
