//! CalDAV のサーバーとの同期のうち、データベースに関わる部分。
//!
//! サーバーのカレンダー 1 つを、ワークスペース 1 つにつなぐ。つないだワークスペースに直接入っている
//! タスクが、そのカレンダーのタスクになる。サーバーとのやり取りと、何を送り・受け取るかの判断は
//! `malakt-caldav` にあり、ここはタスクと項目（[`Fields`]）の読み替えと、結果の保存をする。
//!
//! - アカウントのパスワードは OS のキーチェーンに置き、データベースには入れない（アプリが持つ）
//! - ワークスペースを消しても、サーバーのタスクは消さない（カレンダーとのつなぎが外れる）
//! - 担当者は、サーバーの URI（ATTENDEE）とアプリの担当者を `caldav_people` で結ぶ。知らない人は、
//!   そのワークスペースだけの担当者として作る。アカウントの自分の URI は「自分」
//! - 説明の末尾の URL だけの行（サーバーが足すタスクの画面の URL など）は、タスクの URL 欄に出し、
//!   送るときに説明の末尾へ戻す

use std::collections::{HashMap, HashSet};

pub use malakt_caldav::http::Found;
pub use malakt_caldav::{Calendar as SyncCalendar, Fields, Input, Item, Local, Outcome};
use rusqlite::params;

use crate::actor::Actor;
use crate::model::*;
use crate::text::{normalize, normalize_line};
use crate::{Error, Result, Store};

/// サーバーの担当者（人、役、自分の分が完了か）。
type Assigned = (Id, Role, bool);

/// アカウント。パスワードは持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: Id,
    /// 調べた結果たどり着いたサーバーの URL。
    pub url: String,
    pub username: String,
    /// 自分を表す URI。
    pub addresses: Vec<String>,
    pub created_at: i64,
}

impl Account {
    /// 画面と変更履歴に出す名前（`ユーザー名@ホスト名`）。
    pub fn label(&self) -> String {
        format!("{}@{}", self.username, self.host())
    }

    /// サーバーのホスト名。
    pub fn host(&self) -> &str {
        let after = self.url.find("://").map_or(0, |i| i + 3);
        let rest = &self.url[after..];
        rest.split(['/', '?', '#']).next().unwrap_or(rest)
    }
}

/// サーバーのカレンダー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub id: Id,
    pub account_id: Id,
    pub href: String,
    pub name: String,
    pub read_only: bool,
    /// つないだワークスペース。
    pub workspace_id: Option<Id>,
    /// 最後に同期できた時刻。
    pub synced_at: Option<i64>,
}

/// 同期の結果をアプリに入れたときのこと。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    /// タスクを変えた。
    pub changed: bool,
    /// 入れられなかったこと。
    pub errors: Vec<String>,
}

/// ULID の id を UUID の形にする（担当者の URI `urn:uuid:…` に使う）。
fn uuid_of(id: &str) -> Option<String> {
    let n = ulid::Ulid::from_string(id).ok()?.0;
    let h = format!("{n:032x}");
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

fn id_of_uuid(uuid: &str) -> Option<String> {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let n = u128::from_str_radix(&hex, 16).ok()?;
    Some(ulid::Ulid(n).to_string().to_lowercase())
}

/// URI から、名前がないときに出す名前を作る。
fn name_from_uri(uri: &str) -> String {
    let rest = uri
        .strip_prefix("mailto:")
        .map(|m| m.split('@').next().unwrap_or(m))
        .unwrap_or_else(|| {
            uri.trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()
                .unwrap_or(uri)
        });
    let name = normalize_line(rest);
    if name.is_empty() {
        "担当者".into()
    } else {
        name
    }
}

/// サーバーから読んだ項目を、アプリが保存する形にそろえる。
pub fn normalize_fields(f: &mut Fields) {
    f.summary = normalize_line(&f.summary);
    f.body = normalize(&f.body);
    f.links = f
        .links
        .iter()
        .filter_map(|l| crate::weblink::clean(l))
        .collect();
    f.url = f.url.as_deref().and_then(crate::weblink::clean);
    f.priority = f.priority.min(9);
    f.percent = f.percent.min(100);
    f.start = f.start.take().filter(|w| is_valid_when(w));
    f.due = f.due.take().filter(|w| is_valid_when(w));
    let mut categories: Vec<String> = f
        .categories
        .iter()
        .map(|c| normalize_line(c))
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort_unstable();
    categories.dedup();
    f.categories = categories;
    for a in &mut f.attendees {
        a.name = normalize_line(&a.name);
    }
}

fn status_of(s: &str) -> Status {
    match s {
        "IN-PROCESS" => Status::InProcess,
        "COMPLETED" => Status::Completed,
        "CANCELLED" => Status::Cancelled,
        _ => Status::NeedsAction,
    }
}

fn status_text(s: Status) -> &'static str {
    match s {
        Status::NeedsAction => "NEEDS-ACTION",
        Status::InProcess => "IN-PROCESS",
        Status::Completed => "COMPLETED",
        Status::Cancelled => "CANCELLED",
    }
}

/// 役の集まりのうち、サーバーに出す 1 つ（R・A・C・I の順で先のもの）。
fn primary_role(roles: Roles) -> &'static str {
    roles.iter().next().map_or("R", Role::code)
}

/// 同期を組み立てるときに使う、アカウントの中の対応。
struct Maps {
    /// タスクの id → UID（このアカウントでつながっているもの）。
    uid_of: HashMap<Id, String>,
    /// UID → タスクの id。
    task_of: HashMap<String, Id>,
    /// 担当者の id → URI。
    uri_of: HashMap<Id, String>,
    /// URI → 担当者の id。
    person_of: HashMap<String, Id>,
    /// 担当者の id → 名前。
    names: HashMap<Id, String>,
    self_id: Option<Id>,
    /// 自分を表す URI のうち、送るときに使うもの。
    self_uri: Option<String>,
    self_uris: HashSet<String>,
}

impl Maps {
    fn person_uri(&self, person_id: &str) -> Option<String> {
        if let Some(uri) = self.uri_of.get(person_id) {
            return Some(uri.clone());
        }
        if self.self_id.as_deref() == Some(person_id) {
            if let Some(uri) = &self.self_uri {
                return Some(uri.clone());
            }
        }
        uuid_of(person_id).map(|u| format!("urn:uuid:{u}"))
    }

    /// タスクを、サーバーに送る項目にする。`base` は前の同期の項目（URL と親の読み替えに使う）。
    fn fields_of(&self, task: &Task, base: Option<&Fields>) -> Fields {
        let base_links: &[String] = base.map_or(&[], |b| &b.links);
        let links: Vec<String> = base_links
            .iter()
            .filter(|l| task.urls.contains(l))
            .cloned()
            .collect();
        let url = task.urls.iter().find(|u| !links.contains(u)).cloned();
        let parent = match task.parent_id.as_deref() {
            Some(p) => self.uid_of.get(p).cloned(),
            None => None,
        }
        .or_else(|| {
            // 親がこのアプリにないタスク（ほかのカレンダーのものなど）は、サーバーの親のまま
            base.and_then(|b| b.parent.clone())
                .filter(|u| !self.task_of.contains_key(u))
                .filter(|_| task.parent_id.is_none())
        });
        let attendees = task
            .assignee_ids
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                Some(malakt_caldav::Attendee {
                    uri: self.person_uri(p)?,
                    name: self.names.get(p).cloned().unwrap_or_default(),
                    role: primary_role(task.assignee_roles.get(i).copied().unwrap_or_default())
                        .into(),
                    done: task.assignee_done.get(i).copied().flatten().is_some(),
                })
            })
            .collect();
        let mut f = Fields {
            summary: task.title.clone(),
            body: task.description.clone(),
            links,
            url,
            status: status_text(task.status).into(),
            priority: task.priority,
            percent: task.percent,
            estimate: task.estimate.map(|e| e.to_ical_duration()),
            start: task.start.clone(),
            due: task.due.clone(),
            categories: task.categories.clone(),
            parent,
            attendees,
        };
        f.settle();
        f
    }
}

fn account_row(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    let addresses: String = r.get(3)?;
    Ok(Account {
        id: r.get(0)?,
        url: r.get(1)?,
        username: r.get(2)?,
        addresses: serde_json::from_str(&addresses).unwrap_or_default(),
        created_at: r.get(4)?,
    })
}

fn calendar_row(r: &rusqlite::Row) -> rusqlite::Result<Calendar> {
    Ok(Calendar {
        id: r.get(0)?,
        account_id: r.get(1)?,
        href: r.get(2)?,
        name: r.get(3)?,
        read_only: r.get(4)?,
        workspace_id: r.get(5)?,
        synced_at: r.get(6)?,
    })
}

const CALENDAR_COLUMNS: &str = "id, account_id, href, name, read_only, workspace_id, synced_at";

impl Store {
    // ---- アカウントとカレンダー ----

    pub fn caldav_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, url, username, addresses, created_at FROM caldav_accounts \
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], account_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn caldav_account(&self, id: &str) -> Result<Account> {
        self.conn
            .query_row(
                "SELECT id, url, username, addresses, created_at FROM caldav_accounts WHERE id = ?1",
                [id],
                account_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound("アカウント".into()),
                e => e.into(),
            })
    }

    /// アカウントを足す。見つけたカレンダーは、どこにもつながないで入れる。
    pub fn add_caldav_account(
        &mut self,
        url: &str,
        username: &str,
        addresses: &[String],
        found: &[Found],
    ) -> Result<Account> {
        let account = Account {
            id: new_id(),
            url: url.trim().to_owned(),
            username: username.trim().to_owned(),
            addresses: addresses.to_vec(),
            created_at: now_ms(),
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO caldav_accounts (id, url, username, addresses, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                account.id,
                account.url,
                account.username,
                serde_json::to_string(&account.addresses).unwrap_or_else(|_| "[]".into()),
                account.created_at
            ],
        )?;
        put_calendars(&tx, &account.id, found)?;
        tx.commit()?;
        Ok(account)
    }

    /// サーバーのカレンダーの一覧を新しくする。名前と書き込めるかを直し、新しいものを足す。
    /// サーバーからなくなったカレンダーは、つないでいなければ消す。
    pub fn refresh_caldav_calendars(
        &mut self,
        account_id: &str,
        addresses: &[String],
        found: &[Found],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE caldav_accounts SET addresses = ?2 WHERE id = ?1",
            params![
                account_id,
                serde_json::to_string(addresses).unwrap_or_else(|_| "[]".into())
            ],
        )?;
        put_calendars(&tx, account_id, found)?;
        let hrefs: HashSet<&str> = found.iter().map(|f| f.href.as_str()).collect();
        let mut gone = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id, href FROM caldav_calendars WHERE account_id = ?1 AND workspace_id IS NULL",
            )?;
            let rows = stmt.query_map([account_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, href) = row?;
                if !hrefs.contains(href.as_str()) {
                    gone.push(id);
                }
            }
        }
        for id in gone {
            tx.execute("DELETE FROM caldav_calendars WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// アカウントを外す。タスクはこのアプリにもサーバーにも残る。
    pub fn remove_caldav_account(&mut self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM caldav_accounts WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn caldav_calendars(&self, account_id: &str) -> Result<Vec<Calendar>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {CALENDAR_COLUMNS} FROM caldav_calendars WHERE account_id = ?1 \
             ORDER BY position, name, id"
        ))?;
        let rows = stmt.query_map([account_id], calendar_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// ワークスペースにつないだカレンダー。
    pub fn caldav_calendar_of(&self, workspace_id: &str) -> Result<Option<Calendar>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {CALENDAR_COLUMNS} FROM caldav_calendars WHERE workspace_id = ?1"
        ))?;
        let mut rows = stmt.query_map([workspace_id], calendar_row)?;
        Ok(rows.next().transpose()?)
    }

    /// カレンダーをワークスペースにつなぐ（`None` で外す）。つなぐと、次の同期でサーバーと
    /// ワークスペースのタスクを合わせ直す。
    pub fn link_caldav_calendar(
        &mut self,
        calendar_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        if let Some(w) = workspace_id {
            self.workspace(w)?;
            let other: Option<String> = self
                .conn
                .query_row(
                    "SELECT name FROM caldav_calendars WHERE workspace_id = ?1 AND id != ?2",
                    params![w, calendar_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(name) = other {
                return Err(Error::Invalid(format!(
                    "このワークスペースは、もうカレンダー「{name}」とつないでいます"
                )));
            }
        }
        let tx = self.conn.transaction()?;
        // 外すときはつながりを残す（同じワークスペースにつなぎ直したとき、同じタスクのまま続ける）。
        // つなぐときは、そのワークスペースにないタスクのつながりを捨てる（サーバーのものは消さない）
        if let Some(w) = workspace_id {
            tx.execute(
                "DELETE FROM caldav_items WHERE calendar_id = ?1 AND task_id NOT IN \
                 (SELECT id FROM tasks WHERE workspace_id = ?2)",
                params![calendar_id, w],
            )?;
        }
        let n = tx.execute(
            "UPDATE caldav_calendars SET workspace_id = ?2, token = NULL, synced_at = NULL \
             WHERE id = ?1",
            params![calendar_id, workspace_id],
        )?;
        if n == 0 {
            return Err(Error::NotFound("カレンダー".into()));
        }
        tx.commit()?;
        Ok(())
    }

    /// タスクがサーバーのカレンダーとつながっているか（URL 欄でサーバーの URL を見分けるのに使う）。
    pub fn caldav_links(&self, task_id: &str) -> Result<Vec<String>> {
        let base: Option<String> = self
            .conn
            .query_row(
                "SELECT base FROM caldav_items WHERE task_id = ?1",
                [task_id],
                |r| r.get(0),
            )
            .ok();
        Ok(base
            .and_then(|b| serde_json::from_str::<Fields>(&b).ok())
            .map(|f| f.links)
            .unwrap_or_default())
    }

    // ---- 同期 ----

    fn caldav_maps(&self, account: &Account) -> Result<Maps> {
        let mut uid_of = HashMap::new();
        let mut task_of = HashMap::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT i.task_id, i.uid FROM caldav_items i \
                 JOIN caldav_calendars c ON c.id = i.calendar_id WHERE c.account_id = ?1",
            )?;
            let rows = stmt.query_map([&account.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (task, uid) = row?;
                task_of.insert(uid.clone(), task.clone());
                uid_of.insert(task, uid);
            }
        }
        let mut uri_of = HashMap::new();
        let mut person_of = HashMap::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT uri, person_id FROM caldav_people WHERE account_id = ?1 ORDER BY uri",
            )?;
            let rows = stmt.query_map([&account.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (uri, person) = row?;
                uri_of.entry(person.clone()).or_insert_with(|| uri.clone());
                person_of.insert(uri, person);
            }
        }
        let people = self.people()?;
        let self_id = people.iter().find(|p| p.is_self).map(|p| p.id.clone());
        let names = people.into_iter().map(|p| (p.id, p.name)).collect();
        let self_uris: HashSet<String> = account.addresses.iter().cloned().collect();
        Ok(Maps {
            uid_of,
            task_of,
            uri_of,
            person_of,
            names,
            self_id,
            // calendar-user-address-set の最初のもの。なければプリンシパルの URL
            self_uri: account.addresses.first().cloned(),
            self_uris,
        })
    }

    fn workspace_task_ids(&self, workspace_id: &str) -> Result<Vec<Id>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id FROM tasks WHERE workspace_id = ?1 ORDER BY position, id")?;
        let rows = stmt.query_map([workspace_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 同期に渡すもの。つないだワークスペースがなくなったカレンダーは、ここでつなぎを外す。
    pub fn caldav_input(&mut self, account_id: &str) -> Result<Input> {
        let account = self.caldav_account(account_id)?;
        let workspaces: HashSet<Id> = self.workspaces()?.into_iter().map(|w| w.id).collect();
        let mut calendars = Vec::new();
        for cal in self.caldav_calendars(account_id)? {
            match cal.workspace_id.as_deref() {
                Some(w) if workspaces.contains(w) => calendars.push(cal),
                Some(_) => self.link_caldav_calendar(&cal.id, None)?,
                None => {}
            }
        }
        let maps = self.caldav_maps(&account)?;
        let mut items = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT i.calendar_id, i.href, i.task_id, i.uid, i.etag, i.ics, i.base \
                 FROM caldav_items i JOIN caldav_calendars c ON c.id = i.calendar_id \
                 WHERE c.account_id = ?1 AND c.workspace_id IS NOT NULL ORDER BY i.href",
            )?;
            let rows = stmt.query_map([account_id], |r| {
                let base: String = r.get(6)?;
                Ok(Item {
                    calendar: r.get(0)?,
                    href: r.get(1)?,
                    key: Some(r.get(2)?),
                    uid: r.get(3)?,
                    etag: r.get(4)?,
                    ics: r.get(5)?,
                    base: serde_json::from_str(&base).unwrap_or_default(),
                })
            })?;
            for row in rows {
                items.push(row?);
            }
        }
        let base_of: HashMap<&str, &Fields> = items
            .iter()
            .filter_map(|i| Some((i.key.as_deref()?, &i.base)))
            .collect();
        let mut locals = Vec::new();
        for cal in &calendars {
            let Some(w) = cal.workspace_id.as_deref() else {
                continue;
            };
            for id in self.workspace_task_ids(w)? {
                let task = self.task(&id)?;
                let fields = maps.fields_of(&task, base_of.get(id.as_str()).copied());
                locals.push(Local {
                    uid: maps.uid_of.get(&id).cloned().unwrap_or_else(|| id.clone()),
                    key: id,
                    calendar: cal.id.clone(),
                    fields,
                });
            }
        }
        let calendars = calendars
            .into_iter()
            .map(|c| {
                let token: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT token FROM caldav_calendars WHERE id = ?1",
                        [&c.id],
                        |r| r.get(0),
                    )
                    .ok()
                    .flatten();
                SyncCalendar {
                    id: c.id,
                    href: c.href,
                    name: c.name,
                    token,
                    read_only: c.read_only,
                }
            })
            .collect();
        Ok(Input {
            calendars,
            items,
            locals,
            zone: jiff::tz::TimeZone::system(),
            now: jiff::Timestamp::now(),
            normalize: normalize_fields,
        })
    }

    /// サーバーの URI の担当者。いなければ作る（自分の URI なら自分）。
    fn caldav_person(
        &mut self,
        account: &Account,
        maps: &mut Maps,
        attendee: &malakt_caldav::Attendee,
        workspace_id: &str,
    ) -> Result<Id> {
        let known: HashSet<Id> = maps.names.keys().cloned().collect();
        if let Some(p) = maps.person_of.get(&attendee.uri) {
            if known.contains(p) {
                return Ok(p.clone());
            }
        }
        let person = if maps.self_uris.contains(&attendee.uri) {
            match maps.self_id.clone() {
                Some(id) => id,
                None => {
                    self.ensure_self()?;
                    let me = self
                        .people()?
                        .into_iter()
                        .find(|p| p.is_self)
                        .ok_or_else(|| Error::NotFound("自分".into()))?;
                    maps.names.insert(me.id.clone(), me.name);
                    maps.self_id = Some(me.id.clone());
                    me.id
                }
            }
        } else if let Some(id) = attendee
            .uri
            .strip_prefix("urn:uuid:")
            .and_then(id_of_uuid)
            .filter(|id| known.contains(id))
        {
            id
        } else {
            let name = if attendee.name.is_empty() {
                name_from_uri(&attendee.uri)
            } else {
                attendee.name.clone()
            };
            let p = self.create_local_person(&name, workspace_id)?;
            maps.names.insert(p.id.clone(), p.name);
            p.id
        };
        self.conn.execute(
            "INSERT INTO caldav_people (account_id, uri, person_id) VALUES (?1, ?2, ?3) \
             ON CONFLICT (account_id, uri) DO UPDATE SET person_id = excluded.person_id",
            params![account.id, attendee.uri, person],
        )?;
        maps.person_of.insert(attendee.uri.clone(), person.clone());
        maps.uri_of
            .entry(person.clone())
            .or_insert_with(|| attendee.uri.clone());
        Ok(person)
    }

    /// サーバーの項目を、タスクの下書きと担当者の役・完了にする。
    fn caldav_draft(
        &mut self,
        account: &Account,
        maps: &mut Maps,
        fields: &Fields,
        workspace_id: &str,
        current: Option<(&Task, &Fields)>,
    ) -> Result<(TaskDraft, Vec<Assigned>)> {
        let mut people = Vec::new();
        for a in &fields.attendees {
            let person = self.caldav_person(account, maps, a, workspace_id)?;
            let role = Role::parse(&a.role).unwrap_or(Role::R);
            people.push((person, role, a.done));
        }
        // サーバーの URL と説明の末尾の URL のあとに、このアプリだけの URL を残す
        let mut urls: Vec<String> = fields.url.iter().chain(&fields.links).cloned().collect();
        if let Some((task, base)) = current {
            let synced: HashSet<&String> = base.url.iter().chain(&base.links).collect();
            for u in &task.urls {
                if !synced.contains(u) && !urls.contains(u) {
                    urls.push(u.clone());
                }
            }
        }
        let parent_id = match fields.parent.as_deref() {
            Some(uid) => maps.task_of.get(uid).cloned(),
            None => None,
        }
        .or_else(|| {
            // 親がこのアプリのほかのタスク（同期していないもの）なら、そのまま残す
            current
                .and_then(|(t, _)| t.parent_id.clone())
                .filter(|p| !maps.uid_of.contains_key(p) && fields.parent.is_none())
        });
        let mut ids: Vec<Id> = people.iter().map(|(p, _, _)| p.clone()).collect();
        ids.dedup();
        // DURATION には月がない（1 か月は 30 日で送る）。サーバーの値が今の予想工数と同じなら、今のまま
        let estimate = match (current, fields.estimate.as_deref()) {
            (Some((task, _)), Some(d))
                if task.estimate.map(|e| e.to_ical_duration()).as_deref() == Some(d) =>
            {
                task.estimate
            }
            (_, Some(d)) => crate::estimate::Estimate::from_ical_duration(d)
                .and_then(crate::estimate::Estimate::non_zero),
            (_, None) => None,
        };
        let draft = TaskDraft {
            title: if fields.summary.is_empty() {
                "（タイトルなし）".into()
            } else {
                fields.summary.clone()
            },
            description: fields.body.clone(),
            status: status_of(&fields.status),
            priority: fields.priority,
            start: fields.start.clone(),
            due: fields.due.clone(),
            percent: fields.percent,
            parent_id,
            assignee_ids: ids,
            categories: fields.categories.clone(),
            urls,
            estimate,
            // 所要時間の基準はこのアプリだけのもの
            time_basis: current.and_then(|(t, _)| t.time_basis),
        };
        Ok((draft, people))
    }

    /// 役と、担当者ごとの完了をサーバーに合わせる。
    fn caldav_people_state(&mut self, task_id: &str, people: &[Assigned]) -> Result<()> {
        for (person, role, done) in people {
            let task = self.task(task_id)?;
            let Some(i) = task.assignee_ids.iter().position(|p| p == person) else {
                continue;
            };
            let roles = task.assignee_roles.get(i).copied().unwrap_or_default();
            // サーバーには 1 つ目の役しかないので、それが同じなら、このアプリの役のまま
            if primary_role(roles) != role.code() {
                self.set_assignee_roles(task_id, person, Roles::of(&[*role]))?;
            }
            let task = self.task(task_id)?;
            if task.status != Status::Completed {
                let now_done = task.assignee_done.get(i).copied().flatten().is_some();
                if now_done != *done {
                    self.set_assignee_done(task_id, person, *done)?;
                }
            }
        }
        Ok(())
    }

    /// 同期の結果を入れる。サーバーからの変更は `Actor::Sync` がしたものとして残し、元に戻す相手にしない。
    pub fn apply_caldav(&mut self, account_id: &str, outcome: &Outcome) -> Result<Applied> {
        let account = self.caldav_account(account_id)?;
        self.undo_mark()?;
        let actor = Actor::sync(account.host());
        let result = self.acting(actor, |s| s.apply_caldav_inner(&account, outcome));
        self.undo_skip()?;
        result
    }

    fn apply_caldav_inner(&mut self, account: &Account, outcome: &Outcome) -> Result<Applied> {
        let mut applied = Applied::default();
        let mut maps = self.caldav_maps(account)?;
        let calendars: HashMap<Id, Calendar> = self
            .caldav_calendars(&account.id)?
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        let mut items: Vec<Item> = outcome.items.clone();

        // サーバーで新しく見つけたタスクを作る（親は、全部作ってからつなぐ）
        for (uid, task_id) in items
            .iter()
            .filter_map(|i| Some((i.uid.clone(), i.key.clone()?)))
        {
            maps.task_of.insert(uid.clone(), task_id.clone());
            maps.uid_of.insert(task_id, uid);
        }
        let mut created: Vec<(Id, Fields)> = Vec::new();
        for item in items.iter_mut().filter(|i| i.key.is_none()) {
            let Some(workspace) = calendars
                .get(&item.calendar)
                .and_then(|c| c.workspace_id.clone())
            else {
                continue;
            };
            let made = (|| -> Result<Id> {
                let (mut draft, people) =
                    self.caldav_draft(account, &mut maps, &item.base, &workspace, None)?;
                draft.parent_id = None;
                let task = self.create_task(Some(&workspace), &draft)?;
                self.caldav_people_state(&task.id, &people)?;
                Ok(task.id)
            })();
            match made {
                Ok(id) => {
                    maps.task_of.insert(item.uid.clone(), id.clone());
                    maps.uid_of.insert(id.clone(), item.uid.clone());
                    item.key = Some(id.clone());
                    created.push((id, item.base.clone()));
                    applied.changed = true;
                }
                Err(e) => applied.errors.push(format!(
                    "サーバーのタスク「{}」を入れられませんでした。{e}",
                    item.base.summary
                )),
            }
        }
        for (id, fields) in &created {
            if let Some(parent) = fields.parent.as_deref().and_then(|u| maps.task_of.get(u)) {
                if parent != id {
                    let mut draft = TaskDraft::from(&self.task(id)?);
                    draft.parent_id = Some(parent.clone());
                    if let Err(e) = self.update_task(id, &draft) {
                        applied.errors.push(format!(
                            "「{}」の親タスクをつなげませんでした。{e}",
                            fields.summary
                        ));
                    }
                }
            }
        }

        // サーバーで変わったものを入れる。同期の間にこのアプリで変えられていたら入れない
        let mut stale: HashMap<Id, Fields> = HashMap::new();
        for u in &outcome.updates {
            let task = match self.task(&u.key) {
                Ok(t) => t,
                Err(Error::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            if maps.fields_of(&task, Some(&u.base)) != u.expected {
                stale.insert(u.key.clone(), u.expected.clone());
                continue;
            }
            let workspace = task.workspace_id.clone().unwrap_or_default();
            let result = (|| -> Result<()> {
                let (draft, people) = self.caldav_draft(
                    account,
                    &mut maps,
                    &u.fields,
                    &workspace,
                    Some((&task, &u.base)),
                )?;
                self.update_task(&u.key, &draft)?;
                self.caldav_people_state(&u.key, &people)?;
                Ok(())
            })();
            match result {
                Ok(()) => applied.changed = true,
                Err(e) => {
                    stale.insert(u.key.clone(), u.expected.clone());
                    applied.errors.push(format!(
                        "サーバーの変更を「{}」に入れられませんでした。{e}",
                        task.title
                    ));
                }
            }
        }

        // サーバーで消えたものを消す。同期の間にこのアプリで変えられていたり、ほかのワークスペースへ
        // 移していたりしたら残す（次の同期で送り直す・つながりを外す）
        for d in &outcome.deletes {
            let Ok(task) = self.task(&d.key) else {
                continue;
            };
            let still_there = calendars
                .get(&d.calendar)
                .and_then(|c| c.workspace_id.as_deref())
                .is_some_and(|w| task.workspace_id.as_deref() == Some(w));
            if !still_there || maps.fields_of(&task, Some(&d.base)) != d.expected {
                continue;
            }
            match self.delete_task(&d.key) {
                Ok(()) => applied.changed = true,
                Err(e) => applied.errors.push(format!(
                    "サーバーで消された「{}」を消せませんでした。{e}",
                    task.title
                )),
            }
        }

        // つながりと印を保存する。入れ替えるのは、この同期で扱ったカレンダーの分だけ
        let ids: Vec<&Id> = outcome
            .calendars
            .iter()
            .filter(|id| calendars.contains_key(*id))
            .collect();
        let synced: HashSet<&str> = ids.iter().map(|id| id.as_str()).collect();
        let now = now_ms();
        let tx = self.conn.transaction()?;
        for id in &ids {
            tx.execute("DELETE FROM caldav_items WHERE calendar_id = ?1", [id])?;
        }
        let mut seen = HashSet::new();
        for item in &items {
            let Some(key) = item.key.as_deref() else {
                continue;
            };
            if !synced.contains(item.calendar.as_str()) || !seen.insert(key.to_owned()) {
                continue;
            }
            let base = stale.get(key).unwrap_or(&item.base);
            tx.execute(
                "INSERT INTO caldav_items (calendar_id, href, task_id, uid, etag, ics, base) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (calendar_id, href) DO NOTHING",
                params![
                    item.calendar,
                    item.href,
                    key,
                    item.uid,
                    item.etag,
                    item.ics,
                    serde_json::to_string(base).unwrap_or_default()
                ],
            )?;
        }
        for (id, token) in &outcome.tokens {
            tx.execute(
                "UPDATE caldav_calendars SET token = ?2, synced_at = ?3 WHERE id = ?1",
                params![id, token, now],
            )?;
        }
        tx.commit()?;
        Ok(applied)
    }
}

fn put_calendars(tx: &rusqlite::Transaction, account_id: &str, found: &[Found]) -> Result<()> {
    for (i, f) in found.iter().enumerate() {
        tx.execute(
            "INSERT INTO caldav_calendars (id, account_id, href, name, read_only, position) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (account_id, href) DO UPDATE SET name = excluded.name, \
             read_only = excluded.read_only, position = excluded.position",
            params![new_id(), account_id, f.href, f.name, f.read_only, i as i64],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
