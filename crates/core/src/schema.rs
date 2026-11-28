use rusqlite::Connection;

/// 順番に適用するマイグレーション。追記のみで、既存の要素は書き換えない。
pub(crate) const MIGRATIONS: &[&str] = &[
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
    // 5: ワークスペースの基準日と、タスク追加時の既定値
    r#"
    ALTER TABLE workspaces ADD COLUMN date TEXT;
    CREATE TABLE task_defaults (
        workspace_id TEXT PRIMARY KEY NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        data TEXT NOT NULL
    ) STRICT;
    "#,
    // 6: 1 つのタスクを、ほかのワークスペースにも表示する
    r#"
    CREATE TABLE task_links (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (task_id, workspace_id)
    ) STRICT, WITHOUT ROWID;
    CREATE INDEX task_links_workspace ON task_links(workspace_id);
    "#,
    // 7: 通知
    r#"
    CREATE TABLE notices (
        id TEXT PRIMARY KEY NOT NULL,
        kind TEXT NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        task_title TEXT NOT NULL,
        actor TEXT NOT NULL DEFAULT '',
        comment_id TEXT REFERENCES comments(id) ON DELETE SET NULL,
        excerpt TEXT NOT NULL DEFAULT '',
        status TEXT,
        done_count INTEGER NOT NULL DEFAULT 0,
        total_count INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        read_at INTEGER,
        delivered_at INTEGER,
        dedupe TEXT UNIQUE
    ) STRICT;
    CREATE INDEX notices_created ON notices(created_at);
    CREATE INDEX notices_task ON notices(task_id);
    "#,
    // 8: 完了チケット。secret はハッシュだけを置く
    r#"
    CREATE TABLE tickets (
        id TEXT PRIMARY KEY NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        secret_hash TEXT NOT NULL UNIQUE,
        person_id TEXT REFERENCES people(id) ON DELETE CASCADE,
        whole_task INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        redeemed_at INTEGER,
        comment_id TEXT REFERENCES comments(id) ON DELETE SET NULL
    ) STRICT;
    CREATE INDEX tickets_task ON tickets(task_id);
    "#,
    // 9: ワークスペースのアイコンと色
    r#"
    ALTER TABLE workspaces ADD COLUMN icon TEXT;
    ALTER TABLE workspaces ADD COLUMN color TEXT;
    "#,
    // 10: OS のリマインダーに作ったものの控え。タスクを消したあとにリマインダーを消せるよう、
    //     タスクへの外部キーは付けない
    r#"
    CREATE TABLE reminder_links (
        kind TEXT NOT NULL,
        key TEXT NOT NULL,
        external TEXT NOT NULL,
        hash TEXT NOT NULL,
        done INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (kind, key)
    ) STRICT, WITHOUT ROWID;
    "#,
    // 11: 自分で足したアイコンの画像（小さな PNG）
    r#"
    CREATE TABLE custom_icons (
        name TEXT PRIMARY KEY NOT NULL,
        data BLOB NOT NULL,
        created_at INTEGER NOT NULL
    ) STRICT, WITHOUT ROWID;
    "#,
    // 12: 担当者を使うワークスペース。行がない担当者は、すべてのワークスペースで使う
    r#"
    CREATE TABLE person_workspaces (
        person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
        workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        PRIMARY KEY (person_id, workspace_id)
    ) STRICT, WITHOUT ROWID;
    "#,
    // 13: 期限の通知。タスクごとの、いつ知らせるかの上書きと、通知に出す期限
    r#"
    CREATE TABLE task_alerts (
        task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        timed TEXT,
        date TEXT
    ) STRICT, WITHOUT ROWID;
    ALTER TABLE notices ADD COLUMN due TEXT;
    "#,
    // 14: AI のルール。ワークスペースのないルールはグローバル。
    //     設定に 1 つずつ書いていた「いつも AI に伝えること」と、ワークスペースの「AI に伝えること」を
    //     最初のルールに移す
    r#"
    CREATE TABLE ai_rules (
        id TEXT PRIMARY KEY NOT NULL,
        workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        body TEXT NOT NULL DEFAULT '',
        enabled INTEGER NOT NULL DEFAULT 1,
        apply TEXT NOT NULL DEFAULT 'always',
        position INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX ai_rules_workspace ON ai_rules(workspace_id, position);
    INSERT INTO ai_rules(id, workspace_id, name, body, created_at, updated_at)
        SELECT lower(hex(randomblob(13))), NULL, 'いつも AI に伝えること', trim(value),
               CAST(strftime('%s', 'now') AS INTEGER) * 1000,
               CAST(strftime('%s', 'now') AS INTEGER) * 1000
        FROM settings WHERE key = 'ai.context' AND trim(value) <> '';
    INSERT INTO ai_rules(id, workspace_id, name, body, created_at, updated_at)
        SELECT lower(hex(randomblob(13))), w.id, 'AI に伝えること', trim(s.value),
               CAST(strftime('%s', 'now') AS INTEGER) * 1000,
               CAST(strftime('%s', 'now') AS INTEGER) * 1000
        FROM settings s JOIN workspaces w ON s.key = 'ai.context.ws.' || w.id
        WHERE trim(s.value) <> '';
    DELETE FROM settings WHERE key = 'ai.context' OR key GLOB 'ai.context.ws.*';
    "#,
    // 15: 誰が変えたか（`actor::Actor` の文字列）。前の版の行は空（分からない）
    r#"
    ALTER TABLE tasks ADD COLUMN created_by TEXT NOT NULL DEFAULT '';
    ALTER TABLE tasks ADD COLUMN updated_by TEXT NOT NULL DEFAULT '';
    ALTER TABLE changes ADD COLUMN actor TEXT NOT NULL DEFAULT '';
    ALTER TABLE comments ADD COLUMN actor TEXT NOT NULL DEFAULT '';
    "#,
    // 16: タスクに付けた URL。position は足した順
    r#"
    CREATE TABLE task_urls (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        url TEXT NOT NULL,
        PRIMARY KEY (task_id, position)
    ) STRICT, WITHOUT ROWID;
    "#,
    // 17: 繰り返し（`repeat`）。task_id は今の回。repeat_last は最後に作った回の日、
    //     repeat_ai は回を作ったあと AI に頼むことの待ち行列（どちらも元に戻す相手にしない）
    r#"
    CREATE TABLE task_repeats (
        id TEXT PRIMARY KEY NOT NULL,
        task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
        rule TEXT NOT NULL,
        ai TEXT NOT NULL DEFAULT '',
        at TEXT NOT NULL,
        next_at TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX task_repeats_next ON task_repeats(next_at);
    CREATE TABLE repeat_last (
        repeat_id TEXT PRIMARY KEY NOT NULL,
        at TEXT NOT NULL
    ) STRICT, WITHOUT ROWID;
    CREATE TABLE repeat_ai (
        task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        prompt TEXT NOT NULL,
        failed INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    ) STRICT, WITHOUT ROWID;
    "#,
    // 18: 定時タスク（`schedule`）。workspace_id がなければワークスペースの外に作る。
    //     schedule_last は最後に作った回の日（元に戻す相手にしない）
    r#"
    CREATE TABLE schedules (
        id TEXT PRIMARY KEY NOT NULL,
        workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
        title TEXT NOT NULL,
        rule TEXT NOT NULL,
        ai TEXT NOT NULL DEFAULT '',
        assignees TEXT NOT NULL DEFAULT '[]',
        due_days INTEGER,
        due_time TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        next_at TEXT NOT NULL,
        position INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX schedules_workspace ON schedules(workspace_id, position);
    CREATE INDEX schedules_next ON schedules(next_at);
    CREATE TABLE schedule_last (
        schedule_id TEXT PRIMARY KEY NOT NULL,
        at TEXT NOT NULL
    ) STRICT;
    "#,
    // 19: アクセスキー（`access`）。secret はハッシュだけ、hint は最後の 4 文字。元に戻す相手にしない
    r#"
    CREATE TABLE access_keys (
        id TEXT PRIMARY KEY NOT NULL,
        name TEXT NOT NULL,
        secret_hash TEXT NOT NULL UNIQUE,
        hint TEXT NOT NULL,
        read_only INTEGER NOT NULL DEFAULT 0,
        while_locked INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        used_at INTEGER
    ) STRICT;
    "#,
    // 20: 定時タスクの雛形を、タスクと同じ項目にする。task はタイトル・担当・期限のほかの項目（JSON）。
    //     schedule_files は雛形の添付。中身は添付と同じ blobs を使い、どちらからも指されなくなったら消す
    r#"
    ALTER TABLE schedules ADD COLUMN task TEXT NOT NULL DEFAULT '{}';
    CREATE TABLE schedule_files (
        id TEXT PRIMARY KEY NOT NULL,
        schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        size INTEGER NOT NULL,
        hash TEXT NOT NULL REFERENCES blobs(hash),
        created_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX schedule_files_schedule ON schedule_files(schedule_id, created_at);
    CREATE INDEX schedule_files_hash ON schedule_files(hash);
    DROP TRIGGER attachments_drop_blob;
    CREATE TRIGGER attachments_drop_blob AFTER DELETE ON attachments
    WHEN NOT EXISTS (SELECT 1 FROM attachments WHERE hash = OLD.hash)
     AND NOT EXISTS (SELECT 1 FROM schedule_files WHERE hash = OLD.hash)
    BEGIN
        DELETE FROM blobs WHERE hash = OLD.hash;
    END;
    CREATE TRIGGER schedule_files_drop_blob AFTER DELETE ON schedule_files
    WHEN NOT EXISTS (SELECT 1 FROM attachments WHERE hash = OLD.hash)
     AND NOT EXISTS (SELECT 1 FROM schedule_files WHERE hash = OLD.hash)
    BEGIN
        DELETE FROM blobs WHERE hash = OLD.hash;
    END;
    "#,
    // 21: アクセスキーの使える期間と時間帯。starts / expires は `YYYY-MM-DD` か `YYYY-MM-DD HH:MM`
    //     （その PC の時刻）、windows は時間帯の JSON（`access::Window` の配列）
    r#"
    ALTER TABLE access_keys ADD COLUMN starts TEXT;
    ALTER TABLE access_keys ADD COLUMN expires TEXT;
    ALTER TABLE access_keys ADD COLUMN windows TEXT NOT NULL DEFAULT '[]';
    "#,
    // 22: ポモドーロの作業の記録（`pomodoro`）。task_id がなければタスクを選ばずに測った分。
    //     時刻は UNIX ミリ秒、seconds は一時停止の間を除いた長さ
    r#"
    CREATE TABLE work_logs (
        id TEXT PRIMARY KEY NOT NULL,
        task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
        started_at INTEGER NOT NULL,
        ended_at INTEGER NOT NULL,
        seconds INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX work_logs_task ON work_logs(task_id, ended_at);
    CREATE INDEX work_logs_ended ON work_logs(ended_at);
    "#,
    // 23: CalDAV のサーバーとの同期（`caldav`）。パスワードは OS のキーチェーンに置き、ここには入れない。
    //     カレンダー 1 つをワークスペース 1 つにつなぐ。items はサーバーのタスクとアプリのタスクのつながりで、
    //     ics はサーバーの中身、base は前の同期でそろっていた項目（JSON）。people は担当者の URI と担当者。
    //     その端末だけのものなので、元に戻す相手にも書き出しにも入れない
    r#"
    CREATE TABLE caldav_accounts (
        id TEXT PRIMARY KEY NOT NULL,
        url TEXT NOT NULL,
        username TEXT NOT NULL,
        addresses TEXT NOT NULL DEFAULT '[]',
        created_at INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE caldav_calendars (
        id TEXT PRIMARY KEY NOT NULL,
        account_id TEXT NOT NULL REFERENCES caldav_accounts(id) ON DELETE CASCADE,
        href TEXT NOT NULL,
        name TEXT NOT NULL,
        read_only INTEGER NOT NULL DEFAULT 0,
        workspace_id TEXT,
        token TEXT,
        synced_at INTEGER,
        position INTEGER NOT NULL DEFAULT 0,
        UNIQUE (account_id, href)
    ) STRICT;
    CREATE UNIQUE INDEX caldav_calendars_workspace ON caldav_calendars(workspace_id)
        WHERE workspace_id IS NOT NULL;
    CREATE TABLE caldav_items (
        calendar_id TEXT NOT NULL REFERENCES caldav_calendars(id) ON DELETE CASCADE,
        href TEXT NOT NULL,
        task_id TEXT NOT NULL UNIQUE,
        uid TEXT NOT NULL,
        etag TEXT NOT NULL,
        ics TEXT NOT NULL,
        base TEXT NOT NULL,
        PRIMARY KEY (calendar_id, href)
    ) STRICT;
    CREATE TABLE caldav_people (
        account_id TEXT NOT NULL REFERENCES caldav_accounts(id) ON DELETE CASCADE,
        uri TEXT NOT NULL,
        person_id TEXT NOT NULL,
        PRIMARY KEY (account_id, uri)
    ) STRICT;
    "#,
    // 24: 予想工数（ISO 8601 の期間）と所要時間の基準（`in_process>completed` の形）、進行中にした時刻と
    //     担当が付いた時刻。今までのタスクの 2 つの時刻は、変更履歴から埋める（残っていなければ作った時刻）
    r#"
    ALTER TABLE tasks ADD COLUMN estimate TEXT;
    ALTER TABLE tasks ADD COLUMN time_basis TEXT;
    ALTER TABLE tasks ADD COLUMN started_at INTEGER;
    ALTER TABLE tasks ADD COLUMN assigned_at INTEGER;
    ALTER TABLE workspaces ADD COLUMN time_basis TEXT;
    UPDATE tasks SET started_at = (
        SELECT MIN(c.created_at) FROM changes c
        WHERE c.task_id = tasks.id
          AND ((c.field = 'status' AND c.new_value = 'in_process')
               OR (c.field = 'created' AND json_valid(c.new_value)
                   AND json_extract(c.new_value, '$.status') = 'in_process'))
          AND c.created_at >= COALESCE((
              SELECT MAX(d.created_at) FROM changes d
              WHERE d.task_id = tasks.id AND d.field = 'status' AND d.new_value = 'needs_action'), 0)
    ) WHERE status <> 'needs_action';
    UPDATE tasks SET assigned_at = COALESCE((
        SELECT MAX(c.created_at) FROM changes c
        WHERE c.task_id = tasks.id
          AND ((c.field = 'assignees' AND c.old_value = '' AND c.new_value <> '')
               OR (c.field = 'created' AND json_valid(c.new_value)
                   AND json_extract(c.new_value, '$.assignees') IS NOT NULL))
    ), created_at)
    WHERE EXISTS (SELECT 1 FROM task_assignees a WHERE a.task_id = tasks.id);
    "#,
    // 25: タスクに付かない通知（タイマーとポモドーロの終わり）。task_id を空にでき、文は text に置く
    r#"
    CREATE TABLE notices_new (
        id TEXT PRIMARY KEY NOT NULL,
        kind TEXT NOT NULL,
        task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
        task_title TEXT NOT NULL,
        actor TEXT NOT NULL DEFAULT '',
        comment_id TEXT REFERENCES comments(id) ON DELETE SET NULL,
        excerpt TEXT NOT NULL DEFAULT '',
        status TEXT,
        done_count INTEGER NOT NULL DEFAULT 0,
        total_count INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        read_at INTEGER,
        delivered_at INTEGER,
        dedupe TEXT UNIQUE,
        due TEXT,
        text TEXT NOT NULL DEFAULT ''
    ) STRICT;
    INSERT INTO notices_new(id, kind, task_id, task_title, actor, comment_id, excerpt, status,
                            done_count, total_count, created_at, read_at, delivered_at, dedupe, due)
        SELECT id, kind, task_id, task_title, actor, comment_id, excerpt, status,
               done_count, total_count, created_at, read_at, delivered_at, dedupe, due
        FROM notices;
    DROP TABLE notices;
    ALTER TABLE notices_new RENAME TO notices;
    CREATE INDEX notices_created ON notices(created_at);
    CREATE INDEX notices_task ON notices(task_id);
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

    /// 設定に書いていた「AI に伝えること」は、ルールになって設定から消える。
    #[test]
    fn moves_the_ai_context_into_rules() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &super::MIGRATIONS[..13] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 13).unwrap();
        conn.execute_batch(
            "INSERT INTO workspaces(id, name, created_at, updated_at) VALUES ('w', '仕事', 0, 0);
             INSERT INTO settings(key, value) VALUES ('ai.context', ' 敬語で答える ');
             INSERT INTO settings(key, value) VALUES ('ai.context.ws.w', 'PR は優先度を高');
             INSERT INTO settings(key, value) VALUES ('ai.context.ws.gone', '消えたワークスペース');
             INSERT INTO settings(key, value) VALUES ('ai.provider', 'claude');",
        )
        .unwrap();
        super::migrate(&mut conn).unwrap();
        let rules: Vec<(Option<String>, String, String, i64, String)> = conn
            .prepare(
                "SELECT workspace_id, name, body, enabled, apply FROM ai_rules ORDER BY workspace_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rules,
            vec![
                (
                    None,
                    "いつも AI に伝えること".into(),
                    "敬語で答える".into(),
                    1,
                    "always".into()
                ),
                (
                    Some("w".into()),
                    "AI に伝えること".into(),
                    "PR は優先度を高".into(),
                    1,
                    "always".into()
                ),
            ]
        );
        let left: Vec<String> = conn
            .prepare("SELECT key FROM settings ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(left, vec!["ai.provider".to_owned()]);
    }

    /// 今までのタスクの、進行中にした時刻と担当が付いた時刻を、変更履歴から埋める。
    #[test]
    fn fills_start_and_assign_times_from_history() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &super::MIGRATIONS[..23] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 23).unwrap();
        conn.execute_batch(
            r#"INSERT INTO people(id, name, is_self, created_at, updated_at) VALUES ('p', '佐藤', 0, 0, 0);
             INSERT INTO tasks(id, title, description, status, priority, position, created_at, updated_at)
             VALUES ('done', 'a', '', 'completed', 0, 0, 100, 0),
                    ('again', 'b', '', 'in_process', 0, 1, 100, 0),
                    ('born', 'c', '', 'in_process', 0, 2, 100, 0),
                    ('todo', 'd', '', 'needs_action', 0, 3, 100, 0),
                    ('old', 'e', '', 'completed', 0, 4, 100, 0);
             INSERT INTO task_assignees(task_id, person_id) VALUES ('done', 'p'), ('born', 'p'), ('old', 'p');
             INSERT INTO changes(id, task_id, field, old_value, new_value, created_at) VALUES
               ('1', 'done', 'created', '', '{"status":"needs_action"}', 100),
               ('2', 'done', 'status', 'needs_action', 'in_process', 200),
               ('3', 'done', 'assignees', '', 'p', 250),
               ('4', 'done', 'status', 'in_process', 'completed', 300),
               ('5', 'again', 'status', 'needs_action', 'in_process', 200),
               ('6', 'again', 'status', 'in_process', 'needs_action', 300),
               ('7', 'again', 'status', 'needs_action', 'in_process', 400),
               ('8', 'born', 'created', '', '{"status":"in_process","assignees":"p"}', 100),
               ('9', 'todo', 'status', 'needs_action', 'in_process', 200),
               ('10', 'todo', 'status', 'in_process', 'needs_action', 300);"#,
        )
        .unwrap();
        super::migrate(&mut conn).unwrap();
        let times: Vec<(String, Option<i64>, Option<i64>)> = conn
            .prepare("SELECT id, started_at, assigned_at FROM tasks ORDER BY position")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            times,
            vec![
                ("done".into(), Some(200), Some(250)),
                ("again".into(), Some(400), None),
                ("born".into(), Some(100), Some(100)),
                ("todo".into(), None, None),
                // 変更履歴が残っていなければ、作った時刻を担当が付いた時刻にする
                ("old".into(), None, Some(100)),
            ]
        );
    }

    /// 通知の表を作り直しても、今までの通知はそのまま残り、タスクのない通知も入れられる。
    #[test]
    fn keeps_notices_and_allows_ones_without_a_task() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in &super::MIGRATIONS[..24] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 24).unwrap();
        conn.execute_batch(
            r#"INSERT INTO tasks(id, title, description, status, priority, position, created_at, updated_at)
             VALUES ('t', '請求書', '', 'needs_action', 0, 0, 100, 0);
             INSERT INTO notices(id, kind, task_id, task_title, created_at, read_at, delivered_at, dedupe, due)
             VALUES ('n', 'due', 't', '請求書', 200, NULL, 210, 'due:t:2026-11-28:0', '2026-11-28');"#,
        )
        .unwrap();
        super::migrate(&mut conn).unwrap();
        let kept: (String, Option<String>, Option<i64>, String, String) = conn
            .query_row(
                "SELECT kind, task_id, delivered_at, dedupe, text FROM notices WHERE id = 'n'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            kept,
            (
                "due".into(),
                Some("t".into()),
                Some(210),
                "due:t:2026-11-28:0".into(),
                String::new()
            )
        );
        conn.execute(
            "INSERT INTO notices(id, kind, task_id, task_title, created_at, text) \
             VALUES ('x', 'timer', NULL, '', 300, 'タイマーが終わりました')",
            [],
        )
        .unwrap();
        // タスクを消すと、そのタスクの通知だけが消える
        conn.execute("DELETE FROM tasks WHERE id = 't'", [])
            .unwrap();
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM notices")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, ["x"]);
    }
}
