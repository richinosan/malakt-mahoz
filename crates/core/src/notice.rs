//! 通知。
//!
//! 自分の操作では作らない。作るのは、時間で起きること（期限が近い。いつ知らせるかは [`crate::alert`]。
//! タイマーとポモドーロの段が終わった）と、自分以外から届いた変更（完了チケットでの報告、
//! 取り込んだデータでの割り当て）だけ。
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
    /// 期限が今日（前の版が作ったもの。今は [`NoticeKind::Due`] を作る）
    DueToday,
    /// 期限が近い・期限になった（いつ知らせるかは [`crate::alert`]）
    Due,
    /// メンションされた
    Mentioned,
    /// タイマー（[`crate::timer`]）が終わった。タスクには付かない
    Timer,
    /// ポモドーロ（[`crate::pomodoro`]）の段が終わった。タスクには付かない
    Pomodoro,
}

impl NoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeKind::Assigned => "assigned",
            NoticeKind::Commented => "commented",
            NoticeKind::StatusChanged => "status_changed",
            NoticeKind::DueToday => "due_today",
            NoticeKind::Due => "due",
            NoticeKind::Mentioned => "mentioned",
            NoticeKind::Timer => "timer",
            NoticeKind::Pomodoro => "pomodoro",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "assigned" => NoticeKind::Assigned,
            "commented" => NoticeKind::Commented,
            "status_changed" => NoticeKind::StatusChanged,
            "due_today" => NoticeKind::DueToday,
            "due" => NoticeKind::Due,
            "mentioned" => NoticeKind::Mentioned,
            "timer" => NoticeKind::Timer,
            "pomodoro" => NoticeKind::Pomodoro,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: Id,
    pub kind: NoticeKind,
    /// 通知の相手のタスク。タイマーとポモドーロの通知では `None`。
    pub task_id: Option<Id>,
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
    /// 期限の通知のときの、その時点の期限。
    pub due: Option<String>,
    /// タスクに付かない通知の 1 行（タイマーとポモドーロ）。
    pub text: String,
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
            NoticeKind::Timer | NoticeKind::Pomodoro => self.text.clone(),
            NoticeKind::DueToday => format!("「{title}」の期限は今日です"),
            NoticeKind::Due => match self.due.as_deref() {
                Some(due) => crate::alert::headline(title, due, self.created_at),
                None => format!("「{title}」の期限です"),
            },
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
    /// 期限の通知のときの期限。
    pub due: Option<&'a str>,
    /// 作った時刻（ミリ秒）。`None` なら今。
    pub at: Option<i64>,
}

/// [`Store::sweep_due_alerts`] の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sweep {
    /// 作った通知の数。
    pub made: usize,
    /// 次に知らせる時刻（UNIX 秒）。なければ `None`。
    pub next: Option<i64>,
}

/// 既読の通知を残す日数。
pub const KEEP_READ_DAYS: i64 = 90;
/// 残す通知の数。超えたら古いものから消す。
pub const MAX_NOTICES: usize = 500;
/// 抜粋の長さ（文字）。
pub const EXCERPT_CHARS: usize = 200;

const COLUMNS: &str = "id, kind, task_id, task_title, actor, comment_id, excerpt, status, \
                       done_count, total_count, created_at, read_at, due, text";

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
        due: r.get(12)?,
        text: r.get(13)?,
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
             excerpt, status, done_count, total_count, created_at, dedupe, due) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
            new.at.unwrap_or_else(now_ms),
            new.dedupe,
            new.due
        ])?;
    Ok(n > 0)
}

/// タスクに付かない通知（タイマーとポモドーロ）を作る。同じ `dedupe` の通知がすでにあれば作らない。
/// 作ったら `true`。`excerpt` は 2 行目に出す文。
pub(crate) fn insert_plain(
    conn: &Connection,
    kind: NoticeKind,
    text: &str,
    excerpt: &str,
    dedupe: &str,
    at: i64,
) -> Result<bool> {
    let excerpt: String = excerpt.chars().take(EXCERPT_CHARS).collect();
    let n = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO notices(id, kind, task_id, task_title, excerpt, created_at, \
             dedupe, text) VALUES (?1, ?2, NULL, '', ?3, ?4, ?5, ?6)",
        )?
        .execute(params![new_id(), kind.as_str(), excerpt, at, dedupe, text])?;
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

    /// 期限の通知を作る。`now` は今の時刻（UNIX 秒）。
    ///
    /// 対象は、自分が担当の未完了のタスクと、担当者のいない未完了のタスク。知らせる時刻
    /// （[`crate::alert`]）が来たら、期限の日のうちに 1 回知らせる。アプリを閉じている間に
    /// いくつも時刻が過ぎていたら、いちばん新しいものだけを知らせる。期限を変えたら、
    /// 新しい期限で知らせ直す。ついでに、古い通知を片付ける。
    pub fn sweep_due_alerts(&mut self, now: i64) -> Result<Sweep> {
        use chrono::TimeZone;
        let standard = self.due_alert_standard()?;
        // 知らせる時刻は、いちばん早くて期限の 4 週間前。それより先の期限は見ない
        let day = |secs: i64| {
            chrono::Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_default()
        };
        let from = day(now - 86_400);
        let until = day(now + i64::from(crate::alert::MAX_DAYS + 1) * 86_400);
        let tx = self.conn.transaction()?;
        let rows: Vec<(Id, String, Option<String>, Option<String>)> = tx
            .prepare_cached(
                "SELECT t.id, t.due, al.timed, al.date FROM tasks t \
                 LEFT JOIN task_alerts al ON al.task_id = t.id \
                 WHERE t.due IS NOT NULL AND substr(t.due, 1, 10) BETWEEN ?1 AND ?2 \
                   AND t.status IN ('needs_action', 'in_process') \
                   AND (NOT EXISTS (SELECT 1 FROM task_assignees a WHERE a.task_id = t.id) \
                        OR EXISTS (SELECT 1 FROM task_assignees a JOIN people p \
                                   ON p.id = a.person_id \
                                   WHERE a.task_id = t.id AND p.is_self = 1)) \
                 ORDER BY t.due, t.id",
            )?
            .query_map(params![from, until], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut sweep = Sweep::default();
        for (id, due, timed, date) in &rows {
            let list = |text: &Option<String>, standard: &[u32]| -> Vec<u32> {
                match text {
                    Some(text) => crate::alert::decode(text),
                    None => standard.to_vec(),
                }
            };
            let fires = crate::alert::fire_times(
                due,
                &list(timed, &standard.timed),
                &list(date, &standard.date),
                standard.hour,
            );
            let Some(end) = crate::alert::day_end(due) else {
                continue;
            };
            if now >= end {
                continue;
            }
            if let Some(next) = fires.iter().copied().find(|at| *at > now) {
                sweep.next = Some(sweep.next.map_or(next, |n| n.min(next)));
            }
            let Some(latest) = fires.iter().copied().filter(|at| *at <= now).max() else {
                continue;
            };
            // この期限で、もう同じか新しい時刻の分を知らせていたら作らない
            let prefix = format!("due:{id}:{due}:");
            let told: Option<i64> = tx
                .prepare_cached(
                    "SELECT dedupe FROM notices WHERE task_id = ?1 AND kind = 'due' \
                     AND substr(dedupe, 1, length(?2)) = ?2",
                )?
                .query_map(params![id, prefix], |r| r.get::<_, String>(0))?
                .filter_map(|key| key.ok()?.strip_prefix(&prefix)?.parse::<i64>().ok())
                .max();
            if told.is_some_and(|t| t >= latest) {
                continue;
            }
            let new = NewNotice {
                task_id: id,
                dedupe: Some(format!("{prefix}{latest}")),
                due: Some(due),
                at: Some(now * 1000),
                ..Default::default()
            };
            sweep.made += usize::from(insert(&tx, NoticeKind::Due, &new)?);
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
        Ok(sweep)
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

    /// 端末の時刻帯での日時（UNIX 秒）。
    fn at(text: &str) -> i64 {
        use chrono::TimeZone;
        let t = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
        chrono::Local
            .from_local_datetime(&t)
            .earliest()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn due_alerts_come_at_the_chosen_times_once() {
        let (mut s, me, sato) = setup();
        let mine = task(&mut s, "自分の", Some("2026-12-24"), &[&me, &sato]);
        let timed = task(&mut s, "時刻つき", Some("2026-12-24 18:00"), &[]);
        task(&mut s, "ほかの人の", Some("2026-12-24"), &[&sato]);
        task(&mut s, "明日", Some("2026-12-25"), &[&me]);
        let done = task(&mut s, "済み", Some("2026-12-24"), &[&me]);
        s.set_task_status(&done.id, Status::Completed).unwrap();

        // 標準は、日付だけの期限は当日の 9:00、時刻つきは期限ちょうど
        let sweep = s.sweep_due_alerts(at("2026-12-24 08:59")).unwrap();
        assert_eq!(sweep.made, 0);
        assert_eq!(sweep.next, Some(at("2026-12-24 09:00")));
        let sweep = s.sweep_due_alerts(at("2026-12-24 09:00")).unwrap();
        assert_eq!(sweep.made, 1);
        assert_eq!(sweep.next, Some(at("2026-12-24 18:00")));
        assert_eq!(s.sweep_due_alerts(at("2026-12-24 12:00")).unwrap().made, 0);
        assert_eq!(s.sweep_due_alerts(at("2026-12-24 18:00")).unwrap().made, 1);
        let notices = s.notices(10).unwrap();
        let ids: Vec<&str> = notices
            .iter()
            .filter_map(|n| n.task_id.as_deref())
            .collect();
        assert_eq!(ids, [timed.id.as_str(), mine.id.as_str()]);
        assert!(notices
            .iter()
            .all(|n| n.kind == NoticeKind::Due && n.is_unread()));
        assert_eq!(notices[1].headline(), "「自分の」の期限は今日です");
        // 次の日に延ばしたら、その日にもう一度知らせる
        let mut draft = TaskDraft::from(&s.task(&mine.id).unwrap());
        draft.due = Some("2026-12-25".into());
        s.update_task(&mine.id, &draft).unwrap();
        assert_eq!(s.sweep_due_alerts(at("2026-12-25 09:30")).unwrap().made, 2);
    }

    #[test]
    fn several_alerts_and_overrides() {
        let (mut s, me, _) = setup();
        let meeting = task(&mut s, "会議", Some("2026-12-24 18:00"), &[&me]);
        s.set_due_alert_timed(&[0, 5, 1440]).unwrap();
        assert_eq!(s.sweep_due_alerts(at("2026-12-23 18:00")).unwrap().made, 1);
        let sweep = s.sweep_due_alerts(at("2026-12-24 17:55")).unwrap();
        assert_eq!(sweep.made, 1);
        assert_eq!(sweep.next, Some(at("2026-12-24 18:00")));
        assert_eq!(
            s.notices(1).unwrap()[0].headline(),
            "「会議」の期限まであと 5 分です"
        );
        // タスクごとに「通知しない」にすると、もう知らせない
        s.set_task_alerts(&crate::alert::TaskAlerts {
            task_id: meeting.id.clone(),
            timed: Some(vec![]),
            date: None,
        })
        .unwrap();
        let sweep = s.sweep_due_alerts(at("2026-12-24 18:00")).unwrap();
        assert_eq!((sweep.made, sweep.next), (0, None));
    }

    #[test]
    fn opening_late_tells_only_the_latest_alert() {
        let (mut s, me, _) = setup();
        task(&mut s, "会議", Some("2026-12-24 18:00"), &[&me]);
        s.set_due_alert_timed(&[0, 60, 1440]).unwrap();
        // 閉じていた間に 1 日前と 1 時間前が過ぎた。新しいほうだけを 1 回知らせる
        assert_eq!(s.sweep_due_alerts(at("2026-12-24 17:10")).unwrap().made, 1);
        assert_eq!(s.sweep_due_alerts(at("2026-12-24 17:20")).unwrap().made, 0);
        assert_eq!(
            s.notices(1).unwrap()[0].headline(),
            "「会議」の期限まであと 50 分です"
        );
        // 期限の日を過ぎたら、もう知らせない
        assert_eq!(s.sweep_due_alerts(at("2026-12-25 00:00")).unwrap().made, 0);
    }

    #[test]
    fn read_and_delivery_state() {
        let (mut s, me, _) = setup();
        let a = task(&mut s, "a", Some("2026-12-24"), &[&me]);
        task(&mut s, "b", Some("2026-12-24"), &[&me]);
        s.sweep_due_alerts(at("2026-12-24 10:00")).unwrap();
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
        s.sweep_due_alerts(at("2026-12-24 10:00")).unwrap();
        assert_eq!(s.notices(10_000).unwrap().len(), MAX_NOTICES);
        // 既読にして 90 日を過ぎたものは消える
        let old = now_ms() - (KEEP_READ_DAYS + 1) * 24 * 60 * 60 * 1000;
        s.conn
            .execute(
                "UPDATE notices SET read_at = ?1 WHERE rowid % 2 = 0",
                params![old],
            )
            .unwrap();
        s.sweep_due_alerts(at("2026-12-24 10:00")).unwrap();
        let left = s.notices(10_000).unwrap();
        assert!(left.len() < MAX_NOTICES && left.iter().all(Notice::is_unread));
    }

    #[test]
    fn headlines() {
        let base = Notice {
            id: "n".into(),
            kind: NoticeKind::Commented,
            task_id: Some("t".into()),
            task_title: "告知".into(),
            actor: "佐藤".into(),
            comment_id: None,
            excerpt: String::new(),
            status: None,
            done_count: 0,
            total_count: 0,
            created_at: 0,
            read_at: None,
            due: None,
            text: String::new(),
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
        assert_eq!(notices[0].task_id.as_ref(), Some(&t.id));
        assert_eq!(
            notices[0].headline(),
            "「受付を頼みたい」の担当になりました"
        );
    }

    #[test]
    fn 通知の種類は保存した名前から元に戻る() {
        for kind in [
            NoticeKind::Assigned,
            NoticeKind::Commented,
            NoticeKind::StatusChanged,
            NoticeKind::DueToday,
            NoticeKind::Due,
            NoticeKind::Mentioned,
        ] {
            assert_eq!(NoticeKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(NoticeKind::parse("commented"), Some(NoticeKind::Commented));
        assert_eq!(NoticeKind::parse("due_today"), Some(NoticeKind::DueToday));
        assert_eq!(NoticeKind::parse("ない種類"), None);
    }

    #[test]
    fn 既読にした通知は未読に数えない() {
        let (mut s, me, _) = setup();
        let t = task(&mut s, "タスク", None, &[&me]);
        s.notify_newly_assigned(&[]).unwrap();
        let n = s.notices(10).unwrap().remove(0);
        assert!(n.is_unread());
        s.mark_notice_read(&n.id).unwrap();
        let n = s.notices(10).unwrap().remove(0);
        assert_eq!(n.task_id.as_ref(), Some(&t.id));
        assert!(!n.is_unread());
    }

    #[test]
    fn 担当になった通知は操作した人がいれば名前を出す() {
        let (mut s, me, _) = setup();
        task(&mut s, "タスク", None, &[&me]);
        s.notify_newly_assigned(&[]).unwrap();
        let mut n = s.notices(10).unwrap().remove(0);
        assert_eq!(n.headline(), "「タスク」の担当になりました");
        n.actor = "佐藤".into();
        assert_eq!(
            n.headline(),
            "佐藤 さんが「タスク」の担当にあなたを入れました"
        );
    }

    #[test]
    fn 同じ印の通知は二度作らない() {
        let (mut s, me, _) = setup();
        let t = task(&mut s, "タスク", None, &[&me]);
        let new = NewNotice {
            task_id: &t.id,
            dedupe: Some("same".into()),
            ..Default::default()
        };
        assert!(insert(&s.conn, NoticeKind::Commented, &new).unwrap());
        assert!(!insert(&s.conn, NoticeKind::Commented, &new).unwrap());
        assert_eq!(s.notices(10).unwrap().len(), 1);
    }

    #[test]
    fn 取り込みで新しく担当になったタスクだけ一度知らせる() {
        let (mut s, me, _) = setup();
        let t = task(&mut s, "タスク", None, &[&me]);
        // もともと担当だったタスクは知らせない
        assert_eq!(s.notify_newly_assigned(&[t.id.clone()]).unwrap(), 0);
        assert!(s.notices(10).unwrap().is_empty());
        assert_eq!(s.notify_newly_assigned(&[]).unwrap(), 1);
        // 同じタスクをもう一度取り込んでも、通知は増えない
        assert_eq!(s.notify_newly_assigned(&[]).unwrap(), 0);
        assert_eq!(s.notices(10).unwrap().len(), 1);
        let other = task(&mut s, "もう一つ", None, &[&me]);
        assert_eq!(s.notify_newly_assigned(&[t.id.clone()]).unwrap(), 1);
        assert_eq!(s.notices(10).unwrap().len(), 2);
        assert_eq!(s.notices(10).unwrap()[0].task_id.as_ref(), Some(&other.id));
    }

    #[test]
    fn 期限の4週間前の通知は先の期限でも次の時刻に入る() {
        use chrono::Days;
        let (mut s, me, _) = setup();
        let now = at("2026-12-01 12:00");
        let day = |n: u64| {
            chrono::NaiveDate::from_ymd_opt(2026, 12, 1)
                .unwrap()
                .checked_add_days(Days::new(n))
                .unwrap()
                .format("%Y-%m-%d")
                .to_string()
        };
        // 28 日後の期限の 28 日前（今日の 9:00）は過ぎている。29 日後の期限は、明日の 9:00
        s.set_due_alert_date(&[28]).unwrap();
        task(&mut s, "29 日後", Some(&day(29)), &[&me]);
        let sweep = s.sweep_due_alerts(now).unwrap();
        assert_eq!(sweep.made, 0);
        assert_eq!(sweep.next, Some(at("2026-12-02 09:00")));
        // 28 日後の期限は、28 日前の分を今知らせる
        task(&mut s, "28 日後", Some(&day(28)), &[&me]);
        let sweep = s.sweep_due_alerts(now).unwrap();
        assert_eq!(sweep.made, 1);
    }
}
