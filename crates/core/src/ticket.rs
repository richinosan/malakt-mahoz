//! 完了チケット。
//!
//! アプリを開いていない人や、無人の処理が、あとでタスクを完了報告つきで完了に
//! するための引換券。画面で発行しておき、コマンドで引き換える。
//!
//! - 自分として発行したチケットは、タスク全体を完了にする（自分が完了報告を書いたのと同じ）
//! - 担当者として発行したチケットは、その人の分だけを完了にする
//!
//! チケットの文字列（secret）は 256 ビットの乱数で、データベースにはそのハッシュだけを置く。

use rand::RngCore;
use rusqlite::{params, OptionalExtension};

use crate::attachment::hash_of;
use crate::model::*;
use crate::notice::{self, NewNotice, NoticeKind};
use crate::{Error, Result, Store};

/// 1 つのタスクに持てる、引き換えていない有効なチケットの数。
pub const MAX_ACTIVE_TICKETS_PER_TASK: usize = 1000;
/// チケットの有効期間（発行から）。
pub const TICKET_LIFETIME_DAYS: i64 = 400;
pub const SECRET_PREFIX: &str = "mmct_";
const SECRET_BYTES: usize = 32;
/// 32 バイトを base64url（パディングなし）にした長さ。
const SECRET_BODY_CHARS: usize = 43;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// 発行したチケット。`secret` はこのときしか分からない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedTicket {
    pub id: Id,
    pub task_id: Id,
    /// だれとして報告するか。`None` は「自分」（担当者の登録がないとき）。
    pub person_id: Option<Id>,
    /// タスク全体を完了にするか（そうでなければ、その人の分だけ）。
    pub whole_task: bool,
    pub secret: String,
    pub expires_at: i64,
}

struct Row {
    id: Id,
    task_id: Id,
    person_id: Option<Id>,
    whole_task: bool,
    redeemed_at: Option<i64>,
    comment_id: Option<Id>,
}

fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

fn generate_secret() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{SECRET_PREFIX}{}", base64_url(&bytes))
}

/// 発行する secret の形か。この形でないものは、探すまでもなく「見つからない」。
fn looks_like_secret(secret: &str) -> bool {
    secret.strip_prefix(SECRET_PREFIX).is_some_and(|body| {
        body.len() == SECRET_BODY_CHARS
            && body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// secret が違う・期限切れ・タスクが消えたは、区別できないように同じエラーにする。
fn not_found() -> Error {
    Error::NotFound("完了チケット".into())
}

impl Store {
    /// 完了チケットを発行する。`person_id` は、自分か、そのタスクの担当者。
    /// 自分として発行するとタスク全体を、担当者として発行するとその人の分だけを完了にできる。
    pub fn issue_ticket(&mut self, task_id: &str, person_id: Option<&str>) -> Result<IssuedTicket> {
        let task = self.task(task_id)?;
        let people = self.people()?;
        let whole_task = match person_id {
            None => true,
            Some(id) => {
                let person = people
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or_else(|| Error::NotFound(format!("担当者 {id}")))?;
                if !person.is_self && task.roles_of(id).is_none() {
                    return Err(Error::Invalid(
                        "完了チケットを発行できるのは、自分か、そのタスクの担当者の分だけです"
                            .into(),
                    ));
                }
                person.is_self
            }
        };
        let now = now_ms();
        let tx = self.conn.transaction()?;
        // 期限の切れたチケットは、もう使えないので消す（ディスクに残さない）
        tx.execute("DELETE FROM tickets WHERE expires_at <= ?1", params![now])?;
        let active: usize = tx.query_row(
            "SELECT COUNT(*) FROM tickets WHERE task_id = ?1 AND redeemed_at IS NULL",
            params![task_id],
            |r| r.get(0),
        )?;
        if active >= MAX_ACTIVE_TICKETS_PER_TASK {
            return Err(Error::Invalid(format!(
                "1 つのタスクに持てる、引き換えていない完了チケットは {MAX_ACTIVE_TICKETS_PER_TASK} 枚までです"
            )));
        }
        let ticket = IssuedTicket {
            id: new_id(),
            task_id: task_id.to_owned(),
            person_id: person_id.map(str::to_owned),
            whole_task,
            secret: generate_secret(),
            expires_at: now + TICKET_LIFETIME_DAYS * DAY_MS,
        };
        tx.execute(
            "INSERT INTO tickets(id, task_id, secret_hash, person_id, whole_task, created_at, \
             expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ticket.id,
                ticket.task_id,
                hash_of(ticket.secret.as_bytes()),
                ticket.person_id,
                ticket.whole_task,
                now,
                ticket.expires_at
            ],
        )?;
        tx.commit()?;
        Ok(ticket)
    }

    /// そのタスクの、引き換えていない有効なチケットの数。
    pub fn active_ticket_count(&self, task_id: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM tickets \
             WHERE task_id = ?1 AND redeemed_at IS NULL AND expires_at > ?2",
            params![task_id, now_ms()],
            |r| r.get(0),
        )?)
    }

    /// そのタスクの、引き換えていないチケットをすべて取り消す。取り消した数を返す。
    pub fn revoke_tickets(&mut self, task_id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM tickets WHERE task_id = ?1 AND redeemed_at IS NULL",
            params![task_id],
        )?)
    }

    fn ticket_row(&self, secret: &str) -> Result<Row> {
        if secret.is_empty() {
            return Err(Error::Invalid("完了チケットを指定してください".into()));
        }
        if !looks_like_secret(secret) {
            return Err(not_found());
        }
        self.conn
            .prepare_cached(
                "SELECT id, task_id, person_id, whole_task, redeemed_at, comment_id \
                 FROM tickets WHERE secret_hash = ?1 AND expires_at > ?2",
            )?
            .query_row(params![hash_of(secret.as_bytes()), now_ms()], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    person_id: r.get(2)?,
                    whole_task: r.get(3)?,
                    redeemed_at: r.get(4)?,
                    comment_id: r.get(5)?,
                })
            })
            .optional()?
            .ok_or_else(not_found)
    }

    /// チケットのタスクを読む。引き換えず、何も書かない。引き換え済みでも読める。
    pub fn ticket_task(&self, secret: &str) -> Result<Task> {
        let row = self.ticket_row(secret)?;
        self.task(&row.task_id).map_err(|_| not_found())
    }

    /// チケットを引き換えて、完了報告を書く。もう引き換えてあれば、そのとき書いた
    /// 完了報告を返して何もしない（失敗したときに、そのままやり直してよい）。
    pub fn redeem_ticket(&mut self, secret: &str, body: &str) -> Result<Comment> {
        if crate::text::normalize(body).trim().is_empty() {
            return Err(Error::Invalid("完了報告の本文が空です".into()));
        }
        let row = self.ticket_row(secret)?;
        if row.redeemed_at.is_some() {
            return row
                .comment_id
                .as_deref()
                .and_then(|id| self.comment(id).ok())
                .ok_or_else(|| {
                    Error::Invalid(
                        "この完了チケットは引き換え済みですが、そのとき書いた完了報告はもうありません"
                            .into(),
                    )
                });
        }
        // 担当者として発行したチケットは、その人がまだ担当者のときだけ使える
        let task = self.task(&row.task_id).map_err(|_| not_found())?;
        if let Some(person) = row.person_id.as_deref().filter(|_| !row.whole_task) {
            if task.roles_of(person).is_none() {
                return Err(Error::Invalid(
                    "この完了チケットの人は、もうこのタスクの担当者ではありません".into(),
                ));
            }
        }
        // 先に「引き換えた」と書いて、同時に引き換えようとした片方だけが進むようにする
        let won = self.conn.execute(
            "UPDATE tickets SET redeemed_at = ?2 WHERE id = ?1 AND redeemed_at IS NULL",
            params![row.id, now_ms()],
        )?;
        if won == 0 {
            return self.redeem_ticket(secret, body);
        }
        let written = self.write_report(&row, body);
        match written {
            Ok(comment) => {
                self.conn.execute(
                    "UPDATE tickets SET comment_id = ?2 WHERE id = ?1",
                    params![row.id, comment.id],
                )?;
                Ok(comment)
            }
            Err(error) => {
                // 書けなかったら、チケットを使っていない状態に戻す
                self.conn.execute(
                    "UPDATE tickets SET redeemed_at = NULL WHERE id = ?1",
                    params![row.id],
                )?;
                Err(error)
            }
        }
    }

    fn write_report(&mut self, row: &Row, body: &str) -> Result<Comment> {
        let author = row.person_id.as_deref();
        let mut finished = true;
        if !row.whole_task {
            if let Some(person) = author {
                finished =
                    self.set_assignee_done(&row.task_id, person, true)?.status == Status::Completed;
            }
        }
        let comment = self.add_comment_with(
            &row.task_id,
            author,
            &NewComment {
                body: body.to_owned(),
                // 全員の分がそろった（か、タスク全体を完了にする）ときだけ、完了報告になる
                status: finished.then_some(Status::Completed),
                ..Default::default()
            },
        )?;
        self.notify_report(row, &comment, finished)?;
        Ok(comment)
    }

    /// 自分以外の人の報告なら、通知に出す。
    fn notify_report(&mut self, row: &Row, comment: &Comment, finished: bool) -> Result<()> {
        let people = self.people()?;
        let me = people.iter().find(|p| p.is_self);
        let Some(actor) = row
            .person_id
            .as_deref()
            .and_then(|id| people.iter().find(|p| p.id == id))
            .filter(|p| !p.is_self)
        else {
            return Ok(());
        };
        let (done, total) = self.task(&row.task_id)?.done_count();
        let counts = if finished {
            (0, 0)
        } else {
            (done as u32, total as u32)
        };
        let new = NewNotice {
            task_id: &row.task_id,
            actor: &actor.name,
            comment_id: Some(&comment.id),
            excerpt: &comment.body,
            status: finished.then_some(Status::Completed),
            done_count: counts.0,
            total_count: counts.1,
            dedupe: None,
        };
        notice::insert(&self.conn, NoticeKind::StatusChanged, &new)?;
        if me.is_some_and(|me| comment.mentions.contains(&me.id)) {
            notice::insert(&self.conn, NoticeKind::Mentioned, &new)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Store, Person, Person, Task) {
        let mut s = Store::open_in_memory().unwrap();
        let me = s.create_person("自分", true).unwrap();
        let sato = s.create_person("佐藤", false).unwrap();
        let task = s
            .create_task(
                None,
                &TaskDraft {
                    title: "告知を出す".into(),
                    assignee_ids: vec![me.id.clone(), sato.id.clone()],
                    ..Default::default()
                },
            )
            .unwrap();
        (s, me, sato, task)
    }

    #[test]
    fn secrets_have_the_documented_shape() {
        assert_eq!(base64_url(b""), "");
        assert_eq!(base64_url(b"f"), "Zg");
        assert_eq!(base64_url(b"fo"), "Zm8");
        assert_eq!(base64_url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_url(&[0xfb, 0xff]), "-_8");
        let a = generate_secret();
        assert!(looks_like_secret(&a), "{a}");
        assert_eq!(a.len(), SECRET_PREFIX.len() + SECRET_BODY_CHARS);
        assert_ne!(a, generate_secret());
        assert!(!looks_like_secret(&a[..a.len() - 1]));
        assert!(!looks_like_secret(&a.replace("mmct_", "xxxx_")));
        assert!(!looks_like_secret(&format!("{}!", &a[..a.len() - 1])));
    }

    #[test]
    fn my_ticket_completes_the_whole_task_with_a_report() {
        let (mut s, me, _, task) = setup();
        let ticket = s.issue_ticket(&task.id, Some(&me.id)).unwrap();
        assert!(ticket.whole_task);
        assert_eq!(s.active_ticket_count(&task.id).unwrap(), 1);
        // データベースには secret そのものを置かない
        let stored: String = s
            .conn
            .query_row("SELECT secret_hash FROM tickets", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, ticket.secret);
        assert!(!stored.contains(&ticket.secret[SECRET_PREFIX.len()..]));
        // 引き換えずに読める
        assert_eq!(s.ticket_task(&ticket.secret).unwrap().id, task.id);
        assert_eq!(s.task(&task.id).unwrap().status, Status::NeedsAction);

        let report = s
            .redeem_ticket(&ticket.secret, " 公開しました\r\n")
            .unwrap();
        assert_eq!(report.body, "公開しました");
        assert!(report.is_completion_report());
        assert_eq!(report.author_id.as_deref(), Some(me.id.as_str()));
        let done = s.task(&task.id).unwrap();
        assert_eq!(done.status, Status::Completed);
        assert_eq!(done.done_count(), (2, 2));
        assert_eq!(s.active_ticket_count(&task.id).unwrap(), 0);
        // 自分の報告は、通知にしない
        assert_eq!(s.unread_notice_count().unwrap(), 0);
        // もう一度引き換えても、同じ報告が返るだけで増えない。引き換え済みでも読める
        assert_eq!(s.redeem_ticket(&ticket.secret, "別の文").unwrap(), report);
        assert_eq!(s.comments(&task.id).unwrap().len(), 1);
        assert_eq!(
            s.ticket_task(&ticket.secret).unwrap().status,
            Status::Completed
        );
        // 報告を消したあとは、引き換え済みだと分かるエラーになる
        s.delete_comment(&report.id).unwrap();
        assert!(matches!(
            s.redeem_ticket(&ticket.secret, "もう一度"),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn an_assignees_ticket_completes_only_their_part_and_notifies_me() {
        let (mut s, me, sato, task) = setup();
        let ticket = s.issue_ticket(&task.id, Some(&sato.id)).unwrap();
        assert!(!ticket.whole_task);
        let report = s
            .redeem_ticket(&ticket.secret, "@自分 チラシを配り終えました")
            .unwrap();
        assert_eq!(report.author_id.as_deref(), Some(sato.id.as_str()));
        assert!(!report.is_completion_report());
        let t = s.task(&task.id).unwrap();
        assert_eq!(t.status, Status::NeedsAction);
        assert!(t.is_done_by(&sato.id) && !t.is_done_by(&me.id));
        let notices = s.notices(10).unwrap();
        let kinds: Vec<NoticeKind> = notices.iter().map(|n| n.kind).collect();
        assert!(
            kinds.contains(&NoticeKind::StatusChanged) && kinds.contains(&NoticeKind::Mentioned)
        );
        let changed = notices
            .iter()
            .find(|n| n.kind == NoticeKind::StatusChanged)
            .unwrap();
        assert_eq!(
            changed.headline(),
            "佐藤 さんが「告知を出す」の自分の分を完了にしました（1/2 人）"
        );
        assert_eq!(changed.excerpt, "@自分 チラシを配り終えました");
        assert_eq!(changed.comment_id.as_deref(), Some(report.id.as_str()));

        // 最後の 1 人の分がそろうと、タスクが完了になり、報告は完了報告になる
        s.set_assignee_done(&task.id, &me.id, true).unwrap();
        s.set_assignee_done(&task.id, &sato.id, false).unwrap();
        let again = s.issue_ticket(&task.id, Some(&sato.id)).unwrap();
        let last = s.redeem_ticket(&again.secret, "全部終わりました").unwrap();
        assert!(last.is_completion_report());
        assert_eq!(s.task(&task.id).unwrap().status, Status::Completed);
        assert_eq!(
            s.notices(10).unwrap()[0].headline(),
            "佐藤 さんが「告知を出す」を「完了」にしました"
        );
    }

    #[test]
    fn who_can_have_a_ticket() {
        let (mut s, me, sato, task) = setup();
        let stranger = s.create_person("田中", false).unwrap();
        assert!(matches!(
            s.issue_ticket(&task.id, Some(&stranger.id)),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.issue_ticket(&task.id, Some("ない")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            s.issue_ticket("ない", None),
            Err(Error::NotFound(_))
        ));
        // 担当者でなくなった人のチケットは使えず、使っていない状態のまま残る
        let ticket = s.issue_ticket(&task.id, Some(&sato.id)).unwrap();
        let mut draft = TaskDraft::from(&s.task(&task.id).unwrap());
        draft.assignee_ids = vec![me.id.clone()];
        s.update_task(&task.id, &draft).unwrap();
        assert!(matches!(
            s.redeem_ticket(&ticket.secret, "終わりました"),
            Err(Error::Invalid(_))
        ));
        assert_eq!(s.active_ticket_count(&task.id).unwrap(), 1);
        assert!(s.comments(&task.id).unwrap().is_empty());
        // 担当者の登録がなくても、自分としては発行できる
        let mut plain = Store::open_in_memory().unwrap();
        let t = plain
            .create_task(
                None,
                &TaskDraft {
                    title: "メモ".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let ticket = plain.issue_ticket(&t.id, None).unwrap();
        let report = plain.redeem_ticket(&ticket.secret, "済み").unwrap();
        assert_eq!(report.author_id, None);
        assert_eq!(plain.task(&t.id).unwrap().status, Status::Completed);
    }

    #[test]
    fn wrong_expired_and_orphaned_tickets_look_the_same() {
        let (mut s, me, _, task) = setup();
        let ticket = s.issue_ticket(&task.id, Some(&me.id)).unwrap();
        let is_missing = |r: Result<Comment>| matches!(r, Err(Error::NotFound(_)));
        assert!(is_missing(s.redeem_ticket(&generate_secret(), "x")));
        assert!(is_missing(s.redeem_ticket("mmct_short", "x")));
        assert!(matches!(s.redeem_ticket("", "x"), Err(Error::Invalid(_))));
        // 本文が空なら、チケットを使わない
        assert!(matches!(
            s.redeem_ticket(&ticket.secret, " \n"),
            Err(Error::Invalid(_))
        ));
        assert_eq!(s.active_ticket_count(&task.id).unwrap(), 1);
        // 期限は発行から 400 日
        let lifetime = ticket.expires_at - now_ms();
        assert!((399 * DAY_MS..=400 * DAY_MS).contains(&lifetime));
        s.conn
            .execute("UPDATE tickets SET expires_at = ?1", params![now_ms() - 1])
            .unwrap();
        assert!(is_missing(s.redeem_ticket(&ticket.secret, "x")));
        assert!(matches!(
            s.ticket_task(&ticket.secret),
            Err(Error::NotFound(_))
        ));
        assert_eq!(s.active_ticket_count(&task.id).unwrap(), 0);
        // 次に発行するとき、期限の切れたものは消える
        let fresh = s.issue_ticket(&task.id, Some(&me.id)).unwrap();
        let rows: usize = s
            .conn
            .query_row("SELECT COUNT(*) FROM tickets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        // タスクを消すと、チケットも消える
        s.delete_task(&task.id).unwrap();
        assert!(is_missing(s.redeem_ticket(&fresh.secret, "x")));
    }

    #[test]
    fn limit_and_revoke() {
        let (mut s, me, _, task) = setup();
        for _ in 0..MAX_ACTIVE_TICKETS_PER_TASK {
            s.issue_ticket(&task.id, Some(&me.id)).unwrap();
        }
        assert!(matches!(
            s.issue_ticket(&task.id, Some(&me.id)),
            Err(Error::Invalid(_))
        ));
        assert_eq!(
            s.revoke_tickets(&task.id).unwrap(),
            MAX_ACTIVE_TICKETS_PER_TASK
        );
        let ticket = s.issue_ticket(&task.id, Some(&me.id)).unwrap();
        s.redeem_ticket(&ticket.secret, "済み").unwrap();
        // 引き換え済みのものは取り消さない（やり直しに同じ報告を返せるように）
        assert_eq!(s.revoke_tickets(&task.id).unwrap(), 0);
        assert!(s.redeem_ticket(&ticket.secret, "済み").is_ok());
    }
}
