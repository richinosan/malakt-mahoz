//! CalDAV（RFC 4791）のタスク（VTODO）との同期。
//!
//! サーバーのカレンダー 1 つを、アプリのワークスペース 1 つと同期する。サーバーとのやり取りは
//! [`Server`] の向こうにあり（本物は [`http::Http`]、テストは [`memory::Memory`]）、
//! 何を送り・受け取るかの判断（[`sync`]）はサーバーによらない。
//!
//! - 書き込みはまずアプリのデータベースに入り、同期のときにサーバーへ送る。つながらない間は
//!   何も送らず、つながったときに、たまっていた変更をまとめて送る
//! - 前の同期のときの項目（[`Item::base`]）と、アプリの今の項目、サーバーの今の項目を比べ、
//!   変わった側の値を使う。同じ項目が両方で変わっていたら、サーバーの値にして知らせる
//! - サーバーの ics はそのまま持ち、変えた項目の行だけを書き換えて送る（[`ical::patch`]）

pub mod http;
pub mod ical;
pub mod memory;

use std::collections::{HashMap, HashSet};

pub use ical::{Attendee, Fields};
use jiff::tz::TimeZone;
use jiff::Timestamp;

/// サーバーにあるタスク 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub href: String,
    pub etag: String,
    pub ics: String,
}

/// 前の同期からの、カレンダーの変化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changes {
    /// 変わったもの（href と ETag）と、消えたものの href。次に渡す印つき（RFC 6578）。
    Delta {
        token: String,
        changed: Vec<(String, String)>,
        removed: Vec<String>,
    },
    /// 差分を出せないサーバーの、今あるもの全部（href と ETag）。
    Full { all: Vec<(String, String)> },
    /// 印なしで読み直した、今あるもの全部と次の印（初めての同期と、印が古くなったとき）。
    /// ここにないものは、サーバーで消えたものとして扱う。
    Reset {
        token: String,
        all: Vec<(String, String)>,
    },
}

/// 書き込みの失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// ほかで先に変えられていた（412）。
    Conflict,
    /// 書き込む権限がない（403）。
    Forbidden(String),
    /// もうない（404）。
    Gone,
    Other(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Conflict => f.write_str("ほかで先に変更されていました"),
            WriteError::Forbidden(m) if m.is_empty() => f.write_str("書き込む権限がありません"),
            WriteError::Forbidden(m) => write!(f, "書き込む権限がありません（{m}）"),
            WriteError::Gone => f.write_str("サーバーにありません"),
            WriteError::Other(m) => f.write_str(m),
        }
    }
}

/// サーバー。href はサーバーが返したまま（`/dav/…` のようなパス）。
pub trait Server {
    /// `token` からの変化。`None` は最初から。
    fn changes(&mut self, calendar: &str, token: Option<&str>) -> Result<Changes, String>;
    /// 中身をまとめて読む。
    fn fetch(&mut self, calendar: &str, hrefs: &[String]) -> Result<Vec<Object>, String>;
    /// 1 件読む。なければ `None`。
    fn get(&mut self, href: &str) -> Result<Option<Object>, String>;
    /// 書く。`etag` が `None` なら新しく作る（すでにあれば [`WriteError::Conflict`]）。
    /// 書いた中身の ETag を返す。サーバーが中身を整え直したときは返らないことがある。
    fn put(
        &mut self,
        href: &str,
        ics: &str,
        etag: Option<&str>,
    ) -> Result<Option<String>, WriteError>;
    fn delete(&mut self, href: &str) -> Result<(), WriteError>;
}

/// 同期するカレンダー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    /// アプリが付けた id。
    pub id: String,
    /// サーバーのコレクションの href（`/` で終わる）。
    pub href: String,
    pub name: String,
    /// 前の同期の印。
    pub token: Option<String>,
    pub read_only: bool,
}

/// サーバーのタスクと、アプリのタスクのつながり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub calendar: String,
    pub href: String,
    /// アプリのタスクの id。サーバーで新しく見つけて、まだタスクを作っていないものは `None`。
    pub key: Option<String>,
    pub uid: String,
    pub etag: String,
    /// サーバーの中身（最後に読んだ・書いたもの）。
    pub ics: String,
    /// 前の同期で、アプリとサーバーがそろっていた項目。
    pub base: Fields,
}

/// アプリのタスク。同期するカレンダーにつないだワークスペースにあるもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub key: String,
    pub calendar: String,
    /// サーバーで使う UID。まだ送っていないタスクは、アプリが決める。
    pub uid: String,
    pub fields: Fields,
}

pub struct Input {
    pub calendars: Vec<Calendar>,
    pub items: Vec<Item>,
    pub locals: Vec<Local>,
    /// 時刻を読み書きする時刻帯（この PC の時刻帯）。
    pub zone: TimeZone,
    pub now: Timestamp,
    /// サーバーから読んだ項目を、アプリが保存する形にそろえる（文字の正規化など）。
    /// そろえないと、保存したときの違いを変更と数えてしまう。
    pub normalize: fn(&mut Fields),
}

/// アプリのタスクに入れる、サーバーからの変更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub key: String,
    pub fields: Fields,
    /// 前の同期の項目（アプリだけの URL を残すのに使う）。
    pub base: Fields,
    /// 同期を始めたときのアプリの項目。今と違えば、その間に変えられたので入れない。
    pub expected: Fields,
}

/// サーバーで消えたので、アプリでも消すタスク。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delete {
    /// つながっていたカレンダー。
    pub calendar: String,
    pub key: String,
    /// 前の同期の項目。
    pub base: Fields,
    pub expected: Fields,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// 同期したカレンダー（失敗したものも含む）。つながりはこのカレンダーの分だけを入れ替える。
    pub calendars: Vec<String>,
    /// 同期できたカレンダーの、次の印。
    pub tokens: Vec<(String, Option<String>)>,
    /// つながりの全部。`key` が `None` のものは、アプリでタスクを作る。
    pub items: Vec<Item>,
    pub updates: Vec<Update>,
    pub deletes: Vec<Delete>,
    /// 知らせること（両方で変わっていた、書き込めないので戻した、など）。
    pub notices: Vec<String>,
    /// 失敗したこと。次の同期でやり直す。
    pub errors: Vec<String>,
}

/// 項目の名前。両方で変わっていたことを知らせるときに使う。
fn changed_labels(base: &Fields, local: &Fields, remote: &Fields) -> (Fields, Vec<&'static str>) {
    let mut merged = remote.clone();
    let mut conflicts = Vec::new();
    macro_rules! field {
        ($name:ident, $label:expr) => {
            if local.$name == remote.$name {
                // 同じなら、どちらでもよい
            } else if local.$name == base.$name {
                // アプリでは変えていない。サーバーの値
            } else if remote.$name == base.$name {
                merged.$name = local.$name.clone();
            } else {
                if !conflicts.contains(&$label) {
                    conflicts.push($label);
                }
            }
        };
    }
    field!(summary, "タイトル");
    field!(body, "説明");
    field!(links, "説明");
    field!(url, "URL");
    field!(status, "状態");
    field!(priority, "優先度");
    field!(percent, "進捗");
    field!(estimate, "予想工数");
    field!(start, "開始日");
    field!(due, "期限");
    field!(categories, "カテゴリ");
    field!(parent, "親タスク");
    field!(attendees, "担当");
    merged.settle();
    (merged, conflicts)
}

/// 前の同期の項目 `base` からの、アプリ `local` とサーバー `remote` の変更を合わせる。
/// 両方で変わった項目はサーバーの値にし、その名前を返す。
pub fn merge(base: &Fields, local: &Fields, remote: &Fields) -> (Fields, Vec<&'static str>) {
    changed_labels(base, local, remote)
}

/// href に使える形の UID。
fn href_segment(uid: &str) -> String {
    let mut out = String::new();
    for b in uid.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn title(f: &Fields) -> &str {
    if f.summary.is_empty() {
        "（タイトルなし）"
    } else {
        &f.summary
    }
}

struct Run<'a, S: Server + ?Sized> {
    server: &'a mut S,
    input: &'a Input,
    out: Outcome,
}

impl<S: Server + ?Sized> Run<'_, S> {
    fn read(&self, ics: &str) -> Result<Option<(String, Fields)>, String> {
        Ok(ical::parse(ics, &self.input.zone)?.map(|t| {
            let mut f = t.fields;
            (self.input.normalize)(&mut f);
            f.settle();
            (t.uid, f)
        }))
    }

    /// 書いたあとのサーバーの中身を `item` に入れる。ETag が返らなかったら読み直す。
    fn after_write(
        &mut self,
        item: &mut Item,
        ics: String,
        etag: Option<String>,
    ) -> Result<(), String> {
        match etag {
            Some(etag) => {
                item.etag = etag;
                item.ics = ics;
                Ok(())
            }
            None => match self.server.get(&item.href)? {
                Some(obj) => {
                    item.etag = obj.etag;
                    item.ics = obj.ics;
                    Ok(())
                }
                None => Err("書いたタスクがサーバーにありません".into()),
            },
        }
    }

    /// 書いたあとサーバーが中身を整え直していたら（既定値を入れたなど）、それをアプリにも入れる。
    fn take_server_form(&mut self, item: &mut Item, local: &Local, sent: Fields) {
        let after = match self.read(&item.ics) {
            Ok(Some((_, f))) => f,
            _ => sent.clone(),
        };
        if after != local.fields {
            self.out.updates.push(Update {
                key: local.key.clone(),
                fields: after.clone(),
                base: sent,
                expected: local.fields.clone(),
            });
        }
        item.base = after;
    }

    /// まだサーバーにないタスクを送る。
    fn push_new(&mut self, cal: &Calendar, local: &Local, href: Option<String>) -> Option<Item> {
        let href = href.unwrap_or_else(|| {
            let base = if cal.href.ends_with('/') {
                cal.href.clone()
            } else {
                format!("{}/", cal.href)
            };
            format!("{base}{}.ics", href_segment(&local.uid))
        });
        let ics = ical::build(&local.uid, &local.fields, &self.input.zone, self.input.now);
        let mut item = Item {
            calendar: cal.id.clone(),
            href: href.clone(),
            key: Some(local.key.clone()),
            uid: local.uid.clone(),
            etag: String::new(),
            ics: String::new(),
            base: local.fields.clone(),
        };
        match self.server.put(&href, &ics, None) {
            Ok(etag) => match self.after_write(&mut item, ics, etag) {
                Ok(()) => {
                    self.take_server_form(&mut item, local, local.fields.clone());
                    Some(item)
                }
                Err(e) => {
                    self.fail(local, &e);
                    None
                }
            },
            // 同じ名前のものがもうある（前の同期が途中で止まったなど）。サーバーのものにつなぎ、
            // 次の同期でアプリの内容を送る
            Err(WriteError::Conflict) => match self.server.get(&href) {
                Ok(Some(obj)) => match self.read(&obj.ics) {
                    Ok(Some((uid, remote))) => Some(Item {
                        uid,
                        etag: obj.etag,
                        ics: obj.ics,
                        base: remote,
                        ..item
                    }),
                    _ => {
                        self.fail(local, "同じ名前の、タスクでないものがサーバーにあります");
                        None
                    }
                },
                Ok(None) => {
                    self.fail(local, &WriteError::Conflict.to_string());
                    None
                }
                Err(e) => {
                    self.fail(local, &e);
                    None
                }
            },
            Err(e) => {
                self.fail(local, &e.to_string());
                None
            }
        }
    }

    fn fail(&mut self, local: &Local, why: &str) {
        self.out.errors.push(format!(
            "「{}」をサーバーに送れませんでした。{why}",
            title(&local.fields)
        ));
    }

    /// アプリとサーバーの両方にあるタスクを合わせる。サーバーに送れなかったら `false`
    /// （つながりは前のまま残し、次の同期でやり直す）。
    fn reconcile(
        &mut self,
        cal: &Calendar,
        item: &mut Item,
        local: &Local,
        remote: Fields,
    ) -> bool {
        let base = item.base.clone();
        let (mut merged, mut conflicts) = merge(&base, &local.fields, &remote);
        let mut remote = remote;
        if merged != remote {
            if cal.read_only {
                self.out.notices.push(format!(
                    "「{}」は書き込めないカレンダー「{}」にあるので、サーバーの内容に戻しました",
                    title(&local.fields),
                    cal.name
                ));
                merged = remote.clone();
                conflicts.clear();
            } else {
                let mut retried = false;
                loop {
                    let ics = ical::patch(
                        &item.ics,
                        &remote,
                        &merged,
                        &self.input.zone,
                        self.input.now,
                    );
                    match self.server.put(&item.href, &ics, Some(&item.etag)) {
                        Ok(etag) => {
                            if let Err(e) = self.after_write(item, ics, etag) {
                                self.fail(local, &e);
                                return false;
                            }
                            if let Ok(Some((_, after))) = self.read(&item.ics) {
                                merged = after;
                            }
                            break;
                        }
                        Err(WriteError::Conflict) if !retried => {
                            // その間にサーバーで変わった。読み直して、もう一度合わせる
                            retried = true;
                            let fresh = match self.server.get(&item.href) {
                                Ok(Some(obj)) => obj,
                                Ok(None) => {
                                    self.fail(local, &WriteError::Gone.to_string());
                                    return false;
                                }
                                Err(e) => {
                                    self.fail(local, &e);
                                    return false;
                                }
                            };
                            remote = match self.read(&fresh.ics) {
                                Ok(Some((_, f))) => f,
                                _ => {
                                    self.fail(local, "サーバーのタスクを読めませんでした");
                                    return false;
                                }
                            };
                            item.etag = fresh.etag;
                            item.ics = fresh.ics;
                            let (again, more) = merge(&base, &local.fields, &remote);
                            merged = again;
                            for label in more {
                                if !conflicts.contains(&label) {
                                    conflicts.push(label);
                                }
                            }
                            if merged == remote {
                                break;
                            }
                        }
                        Err(WriteError::Forbidden(_)) => {
                            self.out.notices.push(format!(
                                "「{}」はサーバーで書き込めないので、サーバーの内容に戻しました",
                                title(&local.fields)
                            ));
                            merged = remote.clone();
                            conflicts.clear();
                            break;
                        }
                        Err(e) => {
                            self.fail(local, &e.to_string());
                            return false;
                        }
                    }
                }
            }
        }
        if !conflicts.is_empty() {
            self.out.notices.push(format!(
                "「{}」の{}は、このアプリとサーバーの両方で変わっていたので、サーバーの内容にしました。このアプリの内容は変更履歴にあります",
                title(&merged),
                conflicts.join("・")
            ));
        }
        if merged != local.fields {
            self.out.updates.push(Update {
                key: local.key.clone(),
                fields: merged.clone(),
                base,
                expected: local.fields.clone(),
            });
        }
        item.base = merged;
        true
    }

    fn calendar(&mut self, cal: &Calendar) {
        let input = self.input;
        let mut items: Vec<Item> = input
            .items
            .iter()
            .filter(|i| i.calendar == cal.id)
            .cloned()
            .collect();
        let changes = match self.server.changes(&cal.href, cal.token.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.out
                    .errors
                    .push(format!("「{}」と同期できませんでした。{e}", cal.name));
                self.out.items.extend(items);
                return;
            }
        };
        let (token, changed, removed): (Option<String>, Vec<(String, String)>, HashSet<String>) =
            match changes {
                // 印なしで頼んだ差分は、今あるもの全部
                Changes::Delta { token, changed, .. } if cal.token.is_none() => {
                    let removed = absent(&items, &changed);
                    (Some(token), changed, removed)
                }
                Changes::Delta {
                    token,
                    changed,
                    removed,
                } => (Some(token), changed, removed.into_iter().collect()),
                Changes::Reset { token, all } => {
                    let removed = absent(&items, &all);
                    (Some(token), all, removed)
                }
                Changes::Full { all } => {
                    let removed = absent(&items, &all);
                    (None, all, removed)
                }
            };
        let known: HashMap<&str, &str> = items
            .iter()
            .map(|i| (i.href.as_str(), i.etag.as_str()))
            .collect();
        let wanted: Vec<String> = changed
            .iter()
            .filter(|(h, e)| {
                h.trim_end_matches('/') != cal.href.trim_end_matches('/')
                    && known.get(h.as_str()).is_none_or(|k| k != e)
            })
            .map(|(h, _)| h.clone())
            .collect();
        let objects = if wanted.is_empty() {
            Vec::new()
        } else {
            match self.server.fetch(&cal.href, &wanted) {
                Ok(o) => o,
                Err(e) => {
                    self.out
                        .errors
                        .push(format!("「{}」と同期できませんでした。{e}", cal.name));
                    self.out.items.extend(items);
                    return;
                }
            }
        };
        let mut fresh: HashMap<String, Fields> = HashMap::new();
        for obj in objects {
            match self.read(&obj.ics) {
                Ok(Some((uid, fields))) => {
                    if let Some(it) = items.iter_mut().find(|i| i.href == obj.href) {
                        it.etag = obj.etag;
                        it.ics = obj.ics;
                        fresh.insert(obj.href, fields);
                    } else if !items.iter().any(|i| i.uid == uid) {
                        // このアプリが前に送ったタスク（つなぎ直したときなど）は、そのタスクにつなぐ。
                        // 項目はサーバーのものを前の同期の値にし、違いはこのアプリの変更として送る
                        let key = input
                            .locals
                            .iter()
                            .find(|l| l.calendar == cal.id && l.uid == uid)
                            .map(|l| l.key.clone())
                            .filter(|k| !items.iter().any(|i| i.key.as_ref() == Some(k)));
                        items.push(Item {
                            calendar: cal.id.clone(),
                            href: obj.href,
                            key,
                            uid,
                            etag: obj.etag,
                            ics: obj.ics,
                            base: fields,
                        });
                    }
                }
                // 予定など、タスクでないもの
                Ok(None) => {}
                Err(e) => self.out.errors.push(format!(
                    "「{}」のタスク {} を読めませんでした。{e}",
                    cal.name, obj.href
                )),
            }
        }

        let locals: HashMap<&str, &Local> =
            input.locals.iter().map(|l| (l.key.as_str(), l)).collect();
        let mut kept: Vec<Item> = Vec::new();
        // つながりのあったタスク。サーバーで消えたものも、新しいタスクとして送り直さない
        let mut handled: HashSet<String> = HashSet::new();
        for mut item in items {
            let Some(key) = item.key.clone() else {
                kept.push(item);
                continue;
            };
            handled.insert(key.clone());
            let here = locals
                .get(key.as_str())
                .copied()
                .filter(|l| l.calendar == cal.id);
            if removed.contains(&item.href) {
                match here {
                    Some(local) if local.fields != item.base && !cal.read_only => {
                        if let Some(new) = self.push_new(cal, local, Some(item.href.clone())) {
                            self.out.notices.push(format!(
                                "「{}」はサーバーで消されていましたが、このアプリで変更していたので、サーバーに作り直しました",
                                title(&local.fields)
                            ));
                            kept.push(new);
                        }
                    }
                    Some(local) => self.out.deletes.push(Delete {
                        key,
                        calendar: cal.id.clone(),
                        base: item.base.clone(),
                        expected: local.fields.clone(),
                    }),
                    None => {}
                }
                continue;
            }
            let Some(local) = here else {
                // アプリで消した・ほかのワークスペースへ移した
                if cal.read_only {
                    continue;
                }
                match self.server.delete(&item.href) {
                    Ok(()) | Err(WriteError::Gone) => {}
                    Err(e) => {
                        self.out.errors.push(format!(
                            "「{}」をサーバーから消せませんでした。{e}",
                            title(&item.base)
                        ));
                        kept.push(item);
                    }
                }
                continue;
            };
            let remote = match fresh.remove(&item.href) {
                Some(f) => f,
                None if local.fields == item.base => {
                    kept.push(item);
                    continue;
                }
                None => match self.read(&item.ics) {
                    Ok(Some((_, f))) => f,
                    _ => item.base.clone(),
                },
            };
            self.reconcile(cal, &mut item, local, remote);
            kept.push(item);
        }

        for local in input.locals.iter().filter(|l| l.calendar == cal.id) {
            if handled.contains(&local.key) {
                continue;
            }
            if cal.read_only {
                self.out.errors.push(format!(
                    "「{}」は書き込めないカレンダー「{}」のワークスペースにあるので、サーバーに送れません",
                    title(&local.fields),
                    cal.name
                ));
                continue;
            }
            if let Some(item) = self.push_new(cal, local, None) {
                kept.push(item);
            }
        }
        self.out.tokens.push((cal.id.clone(), token));
        self.out.items.extend(kept);
    }
}

/// 今あるもの全部（href と ETag）にない、つながりの href。
fn absent(items: &[Item], all: &[(String, String)]) -> HashSet<String> {
    let present: HashSet<&str> = all.iter().map(|(h, _)| h.as_str()).collect();
    items
        .iter()
        .filter(|i| !present.contains(i.href.as_str()))
        .map(|i| i.href.clone())
        .collect()
}

/// サーバーとアプリを合わせる。サーバーへの書き込みはここで行い、アプリに入れる変更を返す。
pub fn sync<S: Server + ?Sized>(server: &mut S, input: &Input) -> Outcome {
    let mut run = Run {
        server,
        input,
        out: Outcome::default(),
    };
    run.out.calendars = input.calendars.iter().map(|c| c.id.clone()).collect();
    for cal in &input.calendars {
        run.calendar(cal);
    }
    // つないでいないカレンダーのつながりは捨てる
    run.out
}

#[cfg(test)]
mod tests;
