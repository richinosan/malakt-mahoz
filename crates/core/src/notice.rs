//! 通知。
//!
//! 自分の操作では作らない。作るのは、時間で起きること（期限が今日）と、
//! 自分以外から届いた変更（完了チケットでの報告、取り込んだデータでの割り当て）だけ。
//! 画面の通知一覧に出し、OS の通知にも 1 回だけ出す。

use rusqlite::{params, Connection, Row};

use crate::model::*;
use crate::{Result, Store};

/// 通知の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// 担当になった
    Assigned,
    /// コメントが付いた
    Commented,
    /// 状態が変わった（担当者が自分の分だけ完了にした場合も含む）
    StatusChanged,
    /// 期限が今日
    DueToday,
    /// メンションされた
    Mentioned,
}

impl NoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeKind::Assigned => "assigned",
            NoticeKind::Commented => "commented",
            NoticeKind::StatusChanged => "status_changed",
            NoticeKind::DueToday => "due_today",
            NoticeKind::Mentioned => "mentioned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "assigned" => NoticeKind::Assigned,
            "commented" => NoticeKind::Commented,
            "status_changed" => NoticeKind::StatusChanged,
            "due_today" => NoticeKind::DueToday,
            "mentioned" => NoticeKind::Mentioned,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: Id,
    pub kind: NoticeKind,
    pub task_id: Id,
    /// 通知を作った時点のタイトル。
    pub task_title: String,
    /// 操作をした人の、その時点の名前。人の操作でなければ空。
    pub actor: String,
    pub comment_id: Option<Id>,
    /// コメントの本文の先頭（最大 200 文字）。
    pub excerpt: String,
    /// 変更後の状態。
    pub status: Option<Status>,
    /// 担当者が自分の分だけを完了にしたときの、完了している人数と担当者の数。それ以外は 0。
    pub done_count: u32,
    pub total_count: u32,
    pub created_at: i64,
    /// 既読にした時刻。未読なら `None`。
    pub read_at: Option<i64>,
}

impl Notice {
    pub fn is_unread(&self) -> bool {
        self.read_at.is_none()
    }

    /// 一覧と OS の通知に出す 1 行。
    pub fn headline(&self) -> String {
        let title = &self.task_title;
        let who = if self.actor.is_empty() {
            "だれか".to_owned()
        } else {
            format!("{} さん", self.actor)
        };
        match self.kind {
            NoticeKind::DueToday => format!("「{title}」の期限は今日です"),
            NoticeKind::Assigned if self.actor.is_empty() => {
                format!("「{title}」の担当になりました")
            }
            NoticeKind::Assigned => format!("{who}が「{title}」の担当にあなたを入れました"),
            NoticeKind::Commented => format!("{who}が「{title}」にコメントしました"),
            NoticeKind::Mentioned => format!("{who}が「{title}」であなたにメンションしました"),
            NoticeKind::StatusChanged => match self.status {
                Some(status) => {
                    format!("{who}が「{title}」を「{}」にしました", status.label())
                }
                None => format!(
                    "{who}が「{title}」の自分の分を完了にしました（{}/{} 人）",
                    self.done_count, self.total_count
                ),
            },
        }
    }
}

/// 新しく作る通知。
#[derive(Debug, Clone, Default)]
pub(crate) struct NewNotice<'a> {
    pub task_id: &'a str,
    pub actor: &'a str,
    pub comment_id: Option<&'a str>,
    pub excerpt: &'a str,
    pub status: Option<Status>,
    pub done_count: u32,
    pub total_count: u32,
    /// 同じ鍵の通知は 1 回しか作らない。
    pub dedupe: Option<String>,
}

/// 既読の通知を残す日数。
pub const KEEP_READ_DAYS: i64 = 90;
/// 残す通知の数。超えたら古いものから消す。
pub const MAX_NOTICES: usize = 500;
/// 抜粋の長さ（文字）。
pub const EXCERPT_CHARS: usize = 200;

const COLUMNS: &str = "id, kind, task_id, task_title, actor, comment_id, excerpt, status, \
                       done_count, total_count, created_at, read_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Notice> {
    let kind: String = r.get(1)?;
    let status: Option<String> = r.get(7)?;
    Ok(Notice {
        id: r.get(0)?,
        // 知らない種類は、いちばん無難な「コメント」として出す
        kind: NoticeKind::parse(&kind).unwrap_or(NoticeKind::Commented),
        task_id: r.get(2)?,
        task_title: r.get(3)?,
        actor: r.get(4)?,
        comment_id: r.get(5)?,
        excerpt: r.get(6)?,
        status: status.as_deref().and_then(Status::parse),
        done_count: r.get(8)?,
        total_count: r.get(9)?,
        created_at: r.get(10)?,
        read_at: r.get(11)?,
    })
}

/// 通知を作る。同じ `dedupe` の通知がすでにあれば作らない。作ったら `true`。
pub(crate) fn insert(conn: &Connection, kind: NoticeKind, new: &NewNotice<'_>) -> Result<bool> {
    let title: String = conn
        .prepare_cached("SELECT title FROM tasks WHERE id = ?1")?
        .query_row(params![new.task_id], |r| r.get(0))?;
    let excerpt: String = new.excerpt.chars().take(EXCERPT_CHARS).collect();
    let n = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO notices(id, kind, task_id, task_title, actor, comment_id, \
             excerpt, status, done_count, total_count, created_at, dedupe) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?
        .execute(params![
            new_id(),
            kind.as_str(),
            new.task_id,
            title,
            new.actor,
            new.comment_id,
            excerpt,
            new.status.map(Status::as_str),
            new.done_count,
            new.total_count,
            now_ms(),
            new.dedupe
        ])?;
    Ok(n > 0)
}

impl Store {
    /// 新しい順。
    pub fn notices(&self, limit: usize) -> Result<Vec<Notice>> {
        let sql =
            format!("SELECT {COLUMNS} FROM notices ORDER BY created_at DESC, id DESC LIMIT ?1");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![limit as i64], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn unread_notice_count(&self) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM notices WHERE read_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn mark_notice_read(&mut self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE notices SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL",
            params![id, now_ms()],
        )?;
        Ok(())
    }

    /// すべて既読にする。既読にした数を返す。
    pub fn mark_all_notices_read(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE notices SET read_at = ?1 WHERE read_at IS NULL",
            params![now_ms()],
        )?)
    }

    /// 期限が今日の、未完了のタスクの通知を作る。同じタスク・同じ日には 1 回だけ。
    /// 対象は、自分が担当のタスクと、担当者のいないタスク。作った数を返す。
    /// ついでに、古い通知を片付ける。
    pub fn sweep_due_today(&mut self, today: &str) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let ids: Vec<Id> = tx
            .prepare_cached(
                "SELECT t.id FROM tasks t \
                 WHERE substr(t.due, 1, 10) = ?1 \
                   AND t.status IN ('needs_action', 'in_process') \
                   AND (NOT EXISTS (SELECT 1 FROM task_assignees a WHERE a.task_id = t.id) \
                        OR EXISTS (SELECT 1 FROM task_assignees a JOIN people p \
                                   ON p.id = a.person_id \
                                   WHERE a.task_id = t.id AND p.is_self = 1)) \
                 ORDER BY t.due, t.id",
            )?
            .query_map(params![today], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut made = 0;
        for id in &ids {
            let new = NewNotice {
                task_id: id,
                dedupe: Some(format!("due:{id}:{today}")),
                ..Default::default()
            };
            made += usize::from(insert(&tx, NoticeKind::DueToday, &new)?);
        }
        // ディスクを使い続けないよう、既読の古いものと、多すぎる分を消す
        tx.execute(
            "DELETE FROM notices WHERE read_at IS NOT NULL AND read_at < ?1",
            params![now_ms() - KEEP_READ_DAYS * 24 * 60 * 60 * 1000],
        )?;
        tx.execute(
            "DELETE FROM notices WHERE id IN ( \
                 SELECT id FROM notices ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?1)",
            params![MAX_NOTICES as i64],
        )?;
        tx.commit()?;
        Ok(made)
    }

    /// まだ OS の通知に出していないもの（古い順）。
    pub fn undelivered_notices(&self) -> Result<Vec<Notice>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM notices WHERE delivered_at IS NULL AND read_at IS NULL \
             ORDER BY created_at, id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// OS の通知に出したと記録する。
    pub fn mark_notices_delivered(&mut self, ids: &[Id]) -> Result<()> {
        let now = now_ms();
        let mut stmt = self
            .conn
            .prepare_cached("UPDATE notices SET delivered_at = ?2 WHERE id = ?1")?;
        for id in ids {
            stmt.execute(params![id, now])?;
        }
        Ok(())
    }

    /// 自分が担当になっている、未完了のタスク。
    pub(crate) fn open_tasks_of_self(&self) -> Result<Vec<Id>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.task_id FROM task_assignees a \
             JOIN people p ON p.id = a.person_id AND p.is_self = 1 \
             JOIN tasks t ON t.id = a.task_id \
             WHERE t.status IN ('needs_action', 'in_process') ORDER BY a.task_id",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 取り込みで新しく自分の担当になったタスクを知らせる。`before` は取り込む前の担当。
    pub(crate) fn notify_newly_assigned(&mut self, before: &[Id]) -> Result<usize> {
        let after = self.open_tasks_of_self()?;
        let mut made = 0;
        for id in after.iter().filter(|id| !before.contains(id)) {
            let new = NewNotice {
                task_id: id,
                dedupe: Some(format!("assigned:{id}")),
                ..Default::default()
            };
            made += usize::from(insert(&self.conn, NoticeKind::Assigned, &new)?);
        }
        Ok(made)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Store, Person, Person) {
        let mut s = Store::open_in_memory().unwrap();
        let me = s.create_person("自分", true).unwrap();
        let sato = s.create_person("佐藤", false).unwrap();
        (s, me, sato)
    }

    fn task(s: &mut Store, title: &str, due: Option<&str>, assignees: &[&Person]) -> Task {
        let draft = TaskDraft {
            title: title.into(),
            due: due.map(str::to_owned),
            assignee_ids: assignees.iter().map(|p| p.id.clone()).collect(),
            ..Default::default()
        };
        s.create_task(None, &draft).unwrap()
    }

    #[test]
    fn due_today_is_notified_once_per_task_and_day() {
        let (mut s, me, sato) = setup();
        let mine = task(&mut s, "自分の", Some("2026-12-24"), &[&me, &sato]);
        let timed = task(&mut s, "時刻つき", Some("2026-12-24 18:00"), &[]);
        task(&mut s, "ほかの人の", Some("2026-12-24"), &[&sato]);
        task(&mut s, "明日", Some("2026-12-25"), &[&me]);
        let done = task(&mut s, "済み", Some("2026-12-24"), &[&me]);
        s.set_task_status(&done.id, Status::Completed).unwrap();

        assert_eq!(s.sweep_due_today("2026-12-24").unwrap(), 2);
        assert_eq!(s.sweep_due_today("2026-12-24").unwrap(), 0);
        let notices = s.notices(10).unwrap();
        let mut ids: Vec<&str> = notices.iter().map(|n| n.task_id.as_str()).collect();
        ids.sort_unstable();
        let mut expected = [mine.id.as_str(), timed.id.as_str()];
        expected.sort_unstable();
        assert_eq!(ids, expected);
        assert!(notices
            .iter()
            .all(|n| n.kind == NoticeKind::DueToday && n.is_unread()));
        assert_eq!(s.unread_notice_count().unwrap(), 2);
        // 次の日に延ばしたら、その日にもう一度知らせる
        let mut draft = TaskDraft::from(&s.task(&mine.id).unwrap());
        draft.due = Some("2026-12-25".into());
        s.update_task(&mine.id, &draft).unwrap();
        assert_eq!(s.sweep_due_today("2026-12-25").unwrap(), 2);
    }

    #[test]
    fn read_and_delivery_state() {
        let (mut s, me, _) = setup();
        let a = task(&mut s, "a", Some("2026-12-24"), &[&me]);
        task(&mut s, "b", Some("2026-12-24"), &[&me]);
        s.sweep_due_today("2026-12-24").unwrap();
        let pending = s.undelivered_notices().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].headline(), "「a」の期限は今日です");
        let ids: Vec<Id> = pending.iter().map(|n| n.id.clone()).collect();
        s.mark_notices_delivered(&ids[..1]).unwrap();
        assert_eq!(s.undelivered_notices().unwrap().len(), 1);
        // 既読にしたものは、OS にも出さない
        s.mark_notice_read(&ids[1]).unwrap();
        assert!(s.undelivered_notices().unwrap().is_empty());
        assert_eq!(s.unread_notice_count().unwrap(), 1);
        assert_eq!(s.mark_all_notices_read().unwrap(), 1);
        assert_eq!(s.mark_all_notices_read().unwrap(), 0);
        assert_eq!(s.unread_notice_count().unwrap(), 0);
        // タスクを消すと、その通知も消える
        s.delete_task(&a.id).unwrap();
        assert_eq!(s.notices(10).unwrap().len(), 1);
    }

    #[test]
    fn old_and_excess_notices_are_removed() {
        let (mut s, me, _) = setup();
        let t = task(&mut s, "a", None, &[&me]);
        for i in 0..MAX_NOTICES + 20 {
            let new = NewNotice {
                task_id: &t.id,
                excerpt: &"あ".repeat(300),
                dedupe: Some(format!("k{i}")),
                ..Default::default()
            };
            assert!(insert(&s.conn, NoticeKind::Commented, &new).unwrap());
        }
        assert_eq!(
            s.notices(1).unwrap()[0].excerpt.chars().count(),
            EXCERPT_CHARS
        );
        s.sweep_due_today("2026-12-24").unwrap();
        assert_eq!(s.notices(10_000).unwrap().len(), MAX_NOTICES);
        // 既読にして 90 日を過ぎたものは消える
        let old = now_ms() - (KEEP_READ_DAYS + 1) * 24 * 60 * 60 * 1000;
        s.conn
            .execute(
                "UPDATE notices SET read_at = ?1 WHERE rowid % 2 = 0",
                params![old],
            )
            .unwrap();
        s.sweep_due_today("2026-12-24").unwrap();
        let left = s.notices(10_000).unwrap();
        assert!(left.len() < MAX_NOTICES && left.iter().all(Notice::is_unread));
    }

    #[test]
    fn headlines() {
        let base = Notice {
            id: "n".into(),
            kind: NoticeKind::Commented,
            task_id: "t".into(),
            task_title: "告知".into(),
            actor: "佐藤".into(),
            comment_id: None,
            excerpt: String::new(),
            status: None,
            done_count: 0,
            total_count: 0,
            created_at: 0,
            read_at: None,
        };
        let with = |f: &dyn Fn(&mut Notice)| {
            let mut n = base.clone();
            f(&mut n);
            n.headline()
        };
        assert_eq!(base.headline(), "佐藤 さんが「告知」にコメントしました");
        assert_eq!(
            with(&|n| n.kind = NoticeKind::Mentioned),
            "佐藤 さんが「告知」であなたにメンションしました"
        );
        assert_eq!(
            with(&|n| {
                n.kind = NoticeKind::StatusChanged;
                n.status = Some(Status::Completed);
            }),
            "佐藤 さんが「告知」を「完了」にしました"
        );
        assert_eq!(
            with(&|n| {
                n.kind = NoticeKind::StatusChanged;
                n.done_count = 1;
                n.total_count = 3;
            }),
            "佐藤 さんが「告知」の自分の分を完了にしました（1/3 人）"
        );
        assert_eq!(
            with(&|n| {
                n.kind = NoticeKind::Assigned;
                n.actor.clear();
            }),
            "「告知」の担当になりました"
        );
        assert_eq!(
            with(&|n| {
                n.kind = NoticeKind::Commented;
                n.actor.clear();
            }),
            "だれかが「告知」にコメントしました"
        );
    }

    #[test]
    fn importing_tasks_assigned_to_me_notifies_once() {
        let (mut theirs, me, _) = setup();
        let mut mine = Store::open_in_memory().unwrap();
        // 先に「自分」だけを共有しておく（同じ人として扱うため）
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&theirs, &mut zip).unwrap();
        zip.set_position(0);
        crate::archive::import(&mut mine, &mut zip).unwrap();
        // まっさらなところへの取り込み（復元）では知らせない
        assert_eq!(mine.unread_notice_count().unwrap(), 0);

        let t = task(&mut theirs, "受付を頼みたい", None, &[&me]);
        task(&mut theirs, "関係ない", None, &[]);
        for _ in 0..2 {
            let mut zip = std::io::Cursor::new(Vec::new());
            crate::archive::export(&theirs, &mut zip).unwrap();
            zip.set_position(0);
            crate::archive::import(&mut mine, &mut zip).unwrap();
        }
        let notices = mine.notices(10).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, NoticeKind::Assigned);
        assert_eq!(notices[0].task_id, t.id);
        assert_eq!(
            notices[0].headline(),
            "「受付を頼みたい」の担当になりました"
        );
    }
}
