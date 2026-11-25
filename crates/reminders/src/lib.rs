//! OS のリマインダーとの連携。
//!
//! 未完了のタスクを、期限つきで OS のリマインダーに写す。ワークスペースはリストになる。
//! 正はこのアプリのデータで、OS の側で変えたことのうち受け取るのは「完了にした / 完了を外した」だけ。
//! このアプリが作ったもの（[`Link`] に控えたもの）にしか触らない。
//!
//! - macOS: 「リマインダー」アプリ（EventKit）。ワークスペースごとにリストを作る
//! - Windows: 標準のリマインダーアプリがないので、期限の時刻に出る通知を OS に予約する。
//!   アプリを閉じていても出る。ワークスペースは通知の見出しになる
//!
//! OS とのやり取りは [`Os`] の向こうにあり、何を作り・直し・消すかの判断（[`sync`]）は OS によらない。

use std::collections::{HashMap, HashSet};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// 連携の相手。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// macOS の「リマインダー」。
    Reminders,
    /// Windows の、予約した通知。
    Toasts,
}

/// この OS での連携の相手。なければ `None`。
pub fn target() -> Option<Target> {
    if cfg!(target_os = "macos") {
        Some(Target::Reminders)
    } else if cfg!(windows) {
        Some(Target::Toasts)
    } else {
        None
    }
}

/// OS の側を `snapshot` に合わせる。`app_id` と `app_name` は、通知の出どころとして OS に見せる。
/// 答えが出るまで戻らない（macOS は最初にアクセスの許可を聞く）ので、画面のスレッドでは呼ばない。
/// 始められなかったとき（許可がないなど）は `Err`。
pub fn run(
    app_id: &str,
    app_name: &str,
    snapshot: &Snapshot,
    links: &[Link],
) -> Result<Outcome, String> {
    #[cfg(target_os = "macos")]
    {
        let _ = (app_id, app_name);
        let mut os = macos::open()?;
        Ok(sync(&mut os, snapshot, links))
    }
    #[cfg(windows)]
    {
        let mut os = windows::open(app_id, app_name)?;
        Ok(sync(&mut os, snapshot, links))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (app_id, app_name, snapshot, links);
        Err("この OS にはリマインダーがありません".into())
    }
}

/// リスト。ワークスペース 1 つに 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// ワークスペースの id。どこにも入れていないタスクのリストは空文字。
    pub key: String,
    /// 親からの名前を「 / 」でつないだもの。
    pub name: String,
}

/// 期限の年・月・日と、あれば時・分。
pub type DueParts = (i32, u32, u32, Option<(u32, u32)>);

/// リマインダー 1 件。タスク 1 つに 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub task_id: String,
    /// 入れるリストの [`List::key`]。
    pub list: String,
    pub title: String,
    pub notes: String,
    /// 期限。`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`。
    pub due: Option<String>,
    /// 期限を知らせる時刻（UNIX 秒）。時刻のない期限は、その日の 9 時。
    pub fire_at: Option<i64>,
    /// 0 = なし、1 = 最高 … 9 = 最低。
    pub priority: u8,
    pub done: bool,
}

impl Item {
    /// 期限の年・月・日と、あれば時・分。
    pub fn due_parts(&self) -> Option<DueParts> {
        let due = self.due.as_deref()?;
        let (date, time) = match due.split_once(' ') {
            Some((date, time)) => (date, Some(time)),
            None => (due, None),
        };
        let mut ymd = date.splitn(3, '-');
        let year = ymd.next()?.parse().ok()?;
        let month = ymd.next()?.parse().ok()?;
        let day = ymd.next()?.parse().ok()?;
        let time = time.and_then(|t| {
            let (h, m) = t.split_once(':')?;
            Some((h.parse().ok()?, m.parse().ok()?))
        });
        Some((year, month, day, time))
    }
}

/// いま OS にあってほしいもの。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub lists: Vec<List>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    List,
    Item,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::List => "list",
            Kind::Item => "item",
        }
    }
}

/// このアプリが OS に作ったものの控え。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: Kind,
    /// [`List::key`] か [`Item::task_id`]。
    pub key: String,
    /// OS の側の id。
    pub external: String,
    /// 最後に書いた中身の要約。同じなら書き直さない。
    pub hash: String,
    /// 最後に書いたとき、完了だったか。
    pub done: bool,
}

/// OS の側で見えた、リマインダーの今の様子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// OS の側で消されている。
    Missing,
    Open,
    Done,
    /// 読み取れない OS（予約した通知など）。
    Unknown,
}

/// OS のリマインダーへの操作。失敗は、利用者に見せる文で返す。
pub trait Os {
    /// リストを作るか、名前を合わせる。`external` のリストがもうなければ作り直す。OS の側の id を返す。
    fn save_list(
        &mut self,
        external: Option<&str>,
        list: &List,
    ) -> std::result::Result<String, String>;
    /// リストを消す。このアプリが作ったもの以外が入っているリストは残す。
    fn drop_list(&mut self, external: &str) -> std::result::Result<(), String>;
    fn look(&mut self, external: &str) -> std::result::Result<Seen, String>;
    /// リマインダーを作るか、書き直す。OS の側の id を返す。
    fn save_item(
        &mut self,
        external: Option<&str>,
        list_external: &str,
        item: &Item,
    ) -> std::result::Result<String, String>;
    fn drop_item(&mut self, external: &str) -> std::result::Result<(), String>;
}

/// 合わせた結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// 新しい控え。これまでの控えと丸ごと置き換える。
    pub links: Vec<Link>,
    /// OS の側で完了にされたタスク。
    pub completed: Vec<String>,
    /// OS の側で完了を外されたタスク。
    pub reopened: Vec<String>,
    /// 失敗したことと、その相手。
    pub errors: Vec<String>,
}

/// Windows の予約した通知の id の長さ。Windows が受け付けるのは 15 文字まで。
pub const TOAST_ID_CHARS: usize = 12;
const _: () = assert!(TOAST_ID_CHARS <= 15);

/// タスクから決まる、Windows の予約した通知の id。
pub fn toast_id(task_id: &str) -> String {
    let mut id = digest(&[task_id]);
    id.truncate(TOAST_ID_CHARS);
    id
}

/// 中身の要約（FNV-1a、16 進 16 桁）。版が変わっても同じ値になる。
pub fn digest(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for part in parts {
        eat(&(part.len() as u64).to_le_bytes());
        eat(part.as_bytes());
    }
    format!("{hash:016x}")
}

fn item_hash(item: &Item, list_external: &str) -> String {
    digest(&[
        list_external,
        &item.title,
        &item.notes,
        item.due.as_deref().unwrap_or(""),
        &item.priority.to_string(),
        if item.done { "1" } else { "0" },
    ])
}

/// OS の側を `snapshot` に合わせる。途中で失敗したものは控えを前のままにして、次の回にやり直す。
pub fn sync(os: &mut dyn Os, snapshot: &Snapshot, links: &[Link]) -> Outcome {
    let mut out = Outcome::default();
    let old: HashMap<(Kind, &str), &Link> = links
        .iter()
        .map(|l| ((l.kind, l.key.as_str()), l))
        .collect();

    // リスト
    let mut list_ids: HashMap<&str, String> = HashMap::new();
    for list in &snapshot.lists {
        let link = old.get(&(Kind::List, list.key.as_str())).copied();
        match os.save_list(link.map(|l| l.external.as_str()), list) {
            Ok(external) => {
                list_ids.insert(&list.key, external.clone());
                out.links.push(Link {
                    kind: Kind::List,
                    key: list.key.clone(),
                    external,
                    hash: String::new(),
                    done: false,
                });
            }
            Err(error) => {
                out.errors.push(format!("リスト「{}」: {error}", list.name));
                out.links.extend(link.cloned());
            }
        }
    }

    // リマインダー
    for item in &snapshot.items {
        let link = old.get(&(Kind::Item, item.task_id.as_str())).copied();
        let Some(list_external) = list_ids.get(item.list.as_str()) else {
            // リストを用意できなかった。控えはそのままにして、次の回にやり直す
            out.links.extend(link.cloned());
            continue;
        };
        let fail = |out: &mut Outcome, error: String| {
            out.errors.push(format!("「{}」: {error}", item.title));
            out.links.extend(link.cloned());
        };
        let seen = match link {
            Some(link) => match os.look(&link.external) {
                Ok(seen) => seen,
                Err(error) => {
                    fail(&mut out, error);
                    continue;
                }
            },
            None => Seen::Missing,
        };
        // 完了は、前に書いたときから変わった側に合わせる
        let mut want = item.clone();
        if let Some(link) = link {
            match (seen, link.done, item.done) {
                (Seen::Done, false, false) => {
                    out.completed.push(item.task_id.clone());
                    want.done = true;
                }
                (Seen::Open, true, true) => {
                    out.reopened.push(item.task_id.clone());
                    want.done = false;
                }
                _ => {}
            }
        }
        if seen == Seen::Missing && want.done {
            // 完了したタスクのリマインダーは、なくなっていても作り直さない
            continue;
        }
        let hash = item_hash(&want, list_external);
        let external = match (link, seen) {
            (Some(link), Seen::Open | Seen::Done | Seen::Unknown)
                if link.hash == hash && seen != done_as_seen(!want.done) =>
            {
                link.external.clone()
            }
            (Some(link), Seen::Open | Seen::Done | Seen::Unknown) => {
                match os.save_item(Some(&link.external), list_external, &want) {
                    Ok(external) => external,
                    Err(error) => {
                        fail(&mut out, error);
                        continue;
                    }
                }
            }
            _ => match os.save_item(None, list_external, &want) {
                Ok(external) => external,
                Err(error) => {
                    fail(&mut out, error);
                    continue;
                }
            },
        };
        out.links.push(Link {
            kind: Kind::Item,
            key: item.task_id.clone(),
            external,
            hash,
            done: want.done,
        });
    }

    // なくなったもの。リマインダーを先に消して、空いたリストを消す
    let wanted: HashSet<(Kind, &str)> = snapshot
        .items
        .iter()
        .map(|i| (Kind::Item, i.task_id.as_str()))
        .chain(snapshot.lists.iter().map(|l| (Kind::List, l.key.as_str())))
        .collect();
    for kind in [Kind::Item, Kind::List] {
        for link in links.iter().filter(|l| l.kind == kind) {
            if wanted.contains(&(link.kind, link.key.as_str())) {
                continue;
            }
            let dropped = match kind {
                Kind::Item => os.drop_item(&link.external),
                Kind::List => os.drop_list(&link.external),
            };
            if let Err(error) = dropped {
                out.errors.push(error);
                out.links.push(link.clone());
            }
        }
    }
    // 控えを読み出すときと同じ並びにして、変わっていないことを比べられるようにする
    out.links
        .sort_by(|a, b| (a.kind.as_str(), &a.key).cmp(&(b.kind.as_str(), &b.key)));
    out
}

fn done_as_seen(done: bool) -> Seen {
    if done {
        Seen::Done
    } else {
        Seen::Open
    }
}

/// テスト用の、メモリの中のリマインダー。
#[derive(Debug, Default, Clone)]
pub struct Memory {
    /// id → (名前)
    pub lists: std::collections::BTreeMap<String, String>,
    /// id → (リストの id, 中身)
    pub items: std::collections::BTreeMap<String, (String, Item)>,
    /// 書き込んだ回数。
    pub writes: usize,
    /// このタイトルのリマインダーの保存を失敗させる。
    pub fail_title: Option<String>,
    next: usize,
}

impl Memory {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    /// タイトルからリマインダーを探す。
    pub fn find(&self, title: &str) -> Option<&Item> {
        self.items
            .values()
            .map(|(_, i)| i)
            .find(|i| i.title == title)
    }

    /// OS の側でリマインダーの完了を変える。
    pub fn set_done(&mut self, title: &str, done: bool) {
        for (_, item) in self.items.values_mut() {
            if item.title == title {
                item.done = done;
            }
        }
    }

    /// リストの名前から、入っているリマインダーのタイトルを並べる。
    pub fn titles_in(&self, list: &str) -> Vec<String> {
        let Some((id, _)) = self.lists.iter().find(|(_, name)| name.as_str() == list) else {
            return Vec::new();
        };
        self.items
            .values()
            .filter(|(l, _)| l == id)
            .map(|(_, i)| i.title.clone())
            .collect()
    }
}

impl Os for Memory {
    fn save_list(
        &mut self,
        external: Option<&str>,
        list: &List,
    ) -> std::result::Result<String, String> {
        let id = match external.filter(|id| self.lists.contains_key(*id)) {
            Some(id) => id.to_owned(),
            None => self.id("L"),
        };
        if self.lists.get(&id) != Some(&list.name) {
            self.writes += 1;
            self.lists.insert(id.clone(), list.name.clone());
        }
        Ok(id)
    }

    fn drop_list(&mut self, external: &str) -> std::result::Result<(), String> {
        if !self.items.values().any(|(l, _)| l == external) {
            self.lists.remove(external);
        }
        Ok(())
    }

    fn look(&mut self, external: &str) -> std::result::Result<Seen, String> {
        Ok(match self.items.get(external) {
            None => Seen::Missing,
            Some((_, item)) => done_as_seen(item.done),
        })
    }

    fn save_item(
        &mut self,
        external: Option<&str>,
        list_external: &str,
        item: &Item,
    ) -> std::result::Result<String, String> {
        if self.fail_title.as_deref() == Some(item.title.as_str()) {
            return Err("保存できませんでした".into());
        }
        let id = match external {
            Some(id) => id.to_owned(),
            None => self.id("R"),
        };
        self.writes += 1;
        self.items
            .insert(id.clone(), (list_external.to_owned(), item.clone()));
        Ok(id)
    }

    fn drop_item(&mut self, external: &str) -> std::result::Result<(), String> {
        self.items.remove(external);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows は、16 文字の id を「大きすぎます」と断る。
    #[test]
    fn toast_ids_fit_the_windows_limit_and_stay_the_same() {
        let a = toast_id("01JDF3Z8Q4X7M2N5P9R6S1T0VW");
        let b = toast_id("01JDF3Z8Q4X7M2N5P9R6S1T0VX");
        assert_eq!(a.chars().count(), TOAST_ID_CHARS);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(a, toast_id("01JDF3Z8Q4X7M2N5P9R6S1T0VW"));
    }
}
