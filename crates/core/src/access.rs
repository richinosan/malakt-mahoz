//! アクセスキー。アプリの外のエージェント（MCP サーバーとコマンド）に渡す合い言葉。
//!
//! キーは画面で発行し、エージェントには環境変数で渡す。キーごとに、読み取りのみにするか、
//! アプリがロックしている間も使えるかを選べる。失効させると、動いているサーバーもすぐ使えなくなる。
//!
//! キーを付けずにつないできたものは、「キーなしの接続」の決まり（[`Store::keyless_rules`]）で決める。
//! 決まりごとに、読み取りのみか、ロック中も使えるか、使える期間と時間帯を選べる。どれか 1 つに
//! 当てはまれば使え、当てはまるものに読み書きがあれば読み書き。決まりがなければ、キーが必要。
//! 標準は「読み書き・いつでも」の 1 つで、アクセスキーを入れる前と同じ
//! （ロックを解除している間だけ、すべてのツールが使える）。
//!
//! キーごとに、使える期間（開始と期限）と、使える時間帯（曜日と時刻、[`Window`]）も決められる。
//! 期間と時間帯は、その PC の時刻で見る。
//!
//! キーの文字列（secret）は 256 ビットの乱数で、データベースにはそのハッシュと最後の 4 文字だけを置く。

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::attachment::hash_of;
use crate::model::{new_id, now_ms, Id};
use crate::ticket::{has_secret_shape, random_secret};
use crate::{lock, Error, Result, Store};

pub const SECRET_PREFIX: &str = "mmak_";
/// 持てるキーの数。
pub const MAX_KEYS: usize = 100;
/// 名前の長さの上限（文字）。
pub const MAX_NAME_CHARS: usize = 100;
/// 発行したばかりのキーの名前。
pub const NEW_KEY_NAME: &str = "新規アクセスキー";
/// 「最後に使った時刻」を書き直す間隔（ミリ秒）。呼び出しのたびにディスクに書かない。
const TOUCH_EVERY_MS: i64 = 60_000;

/// キーなしの接続の決まりの名前。画面と、使えない理由の文に出す。
pub const KEYLESS_NAME: &str = "キーなし";
/// キーなしの接続の決まりの数。
pub const MAX_RULES: usize = 20;

/// 1 つのキーに付けられる時間帯の数。
pub const MAX_WINDOWS: usize = 20;
/// 曜日の名前。0 が月曜。
pub const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

/// キーを使える時間帯。`days` の曜日の `from` 分から `to` 分まで（0 時からの分）。
/// `to` が `from` より前なら、日をまたぐ（22:00〜6:00 は、その曜日の 22 時から翌朝 6 時まで）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// 0 が月曜、6 が日曜
    pub days: Vec<u8>,
    pub from: u32,
    pub to: u32,
}

impl Window {
    /// 毎日 9:00〜18:00。時間帯を足したときに最初に入るもの。
    pub fn standard() -> Self {
        Window {
            days: (0..7).collect(),
            from: 9 * 60,
            to: 18 * 60,
        }
    }

    fn check(&self) -> Result<()> {
        let bad = |why: &str| {
            Err(Error::Invalid(format!(
                "アクセスキーの時間帯が正しくありません: {why}"
            )))
        };
        if self.days.is_empty() {
            return bad("曜日がありません");
        }
        if self.days.iter().any(|d| *d > 6) {
            return bad("曜日は 0（月）から 6（日）です");
        }
        if self.from >= 24 * 60 || self.to > 24 * 60 {
            return bad("時刻は 0:00 から 24:00 です");
        }
        if self.from == self.to || (self.to == 24 * 60 && self.from == 0) && self.days.len() == 7 {
            return bad("始まりと終わりが同じです");
        }
        Ok(())
    }

    /// その時刻（その PC の時刻）が、この時間帯に入るか。
    pub fn contains(&self, at: NaiveDateTime) -> bool {
        let day = at.weekday().num_days_from_monday() as u8;
        let minute = at.hour() * 60 + at.minute();
        let on = |d: u8| self.days.contains(&d);
        if self.from < self.to {
            on(day) && (self.from..self.to).contains(&minute)
        } else {
            // 日をまたぐ。前の日に始まった分は、前の日の曜日で見る
            (on(day) && minute >= self.from) || (on((day + 6) % 7) && minute < self.to)
        }
    }

    /// 曜日の表示（「毎日」「平日」「月・水・金」「土日」）。
    pub fn days_label(&self) -> String {
        let mut days = self.days.clone();
        days.sort_unstable();
        days.dedup();
        match days.as_slice() {
            [0, 1, 2, 3, 4, 5, 6] => "毎日".into(),
            [0, 1, 2, 3, 4] => "平日".into(),
            [5, 6] => "土日".into(),
            _ => days
                .iter()
                .map(|d| WEEKDAYS[*d as usize])
                .collect::<Vec<_>>()
                .join("・"),
        }
    }

    /// 「平日 9:00〜18:00」の形。
    pub fn label(&self) -> String {
        format!(
            "{} {}〜{}",
            self.days_label(),
            clock(self.from),
            clock(self.to)
        )
    }
}

/// 0 時からの分を「9:05」の形にする。
pub fn clock(minutes: u32) -> String {
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

/// 時刻の入力（`9`、`9:30`、`21時`、`24:00` など）を 0 時からの分にする。読めなければ `None`。
pub fn parse_clock(input: &str) -> Option<u32> {
    // 全角の数字とコロンは半角にする
    let text: String = input
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
            '：' => ':',
            c => c,
        })
        .collect();
    if matches!(text.as_str(), "24:00" | "24" | "24時") {
        return Some(24 * 60);
    }
    // 数字だけなら、2 けたまでは時、4 けたは時と分（0930）
    if !text.is_empty() && text.len() <= 4 && text.bytes().all(|b| b.is_ascii_digit()) {
        let n: u32 = text.parse().ok()?;
        let (h, m) = if text.len() <= 2 {
            (n, 0)
        } else {
            (n / 100, n % 100)
        };
        return (h < 24 && m < 60).then_some(h * 60 + m);
    }
    crate::when::parse_time(&text)
        .filter(|(h, m)| *h < 24 && *m < 60)
        .map(|(h, m)| h * 60 + m)
}

/// 開始・期限の保存する形（`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`）を読む。
enum Moment {
    Day(NaiveDate),
    At(NaiveDateTime),
}

fn moment(text: &str) -> Option<Moment> {
    if let Ok(at) = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M") {
        return Some(Moment::At(at));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(Moment::Day)
}

fn clean_moment(text: Option<&str>, what: &str) -> Result<Option<String>> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) if moment(t).is_some() => Ok(Some(t.to_owned())),
        Some(t) => Err(Error::Invalid(format!(
            "アクセスキーの{what}が正しくありません: {t}"
        ))),
    }
}

/// 「2026/12/31 18:00」の形にする。日付だけならその日のまま。
pub fn moment_label(text: &str) -> String {
    match moment(text) {
        Some(Moment::At(at)) => at.format("%Y/%m/%d %H:%M").to_string(),
        Some(Moment::Day(day)) => day.format("%Y/%m/%d").to_string(),
        None => text.to_owned(),
    }
}

/// 発行したキー。secret は持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessKey {
    pub id: Id,
    pub name: String,
    /// secret の最後の 4 文字。どのキーかを見分けるため。
    pub hint: String,
    /// 読み取りのツールだけ使える
    pub read_only: bool,
    /// アプリがロックしている間も使える
    pub while_locked: bool,
    pub created_at: i64,
    /// 最後に使った時刻（ミリ秒。1 分ごとにだけ書き直す）
    pub used_at: Option<i64>,
    /// 使えるようになる日時（`YYYY-MM-DD` ならその日の始まりから）。`None` は発行したときから
    pub starts: Option<String>,
    /// 期限（`YYYY-MM-DD` ならその日の終わりまで）。`None` は期限なし
    pub expires: Option<String>,
    /// 使える時間帯。どれかに入っていれば使える。空なら、いつでも
    pub windows: Vec<Window>,
}

impl AccessKey {
    /// 画面に出す、伏せたキー。
    pub fn masked(&self) -> String {
        format!("{SECRET_PREFIX}••••{}", self.hint)
    }

    /// 期間か時間帯で絞っているか。
    pub fn limited(&self) -> bool {
        self.starts.is_some() || self.expires.is_some() || !self.windows.is_empty()
    }

    /// 使える期間と時間帯の表示（「2026/12/31 まで・平日 9:00〜18:00」）。絞っていなければ「いつでも」。
    pub fn when_label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(starts) = &self.starts {
            parts.push(format!("{} から", moment_label(starts)));
        }
        if let Some(expires) = &self.expires {
            parts.push(format!("{} まで", moment_label(expires)));
        }
        parts.extend(self.windows.iter().map(Window::label));
        if parts.is_empty() {
            return "いつでも".into();
        }
        parts.join("・")
    }

    /// その時刻（その PC の時刻）に、期間と時間帯の上で使えるか。
    pub fn usable_at(&self, at: NaiveDateTime) -> std::result::Result<(), Denied> {
        if let Some(expires) = self.expires.as_deref() {
            let over = match moment(expires) {
                Some(Moment::Day(day)) => at.date() > day,
                Some(Moment::At(end)) => at >= end,
                // 読めない期限（手で書き換えたなど）は、切れているものとして扱う
                None => true,
            };
            if over {
                return Err(Denied::Expired {
                    name: self.name.clone(),
                    until: moment_label(expires),
                });
            }
        }
        if let Some(starts) = self.starts.as_deref() {
            let early = match moment(starts) {
                Some(Moment::Day(day)) => at.date() < day,
                Some(Moment::At(start)) => at < start,
                None => true,
            };
            if early {
                return Err(Denied::NotYet {
                    name: self.name.clone(),
                    from: moment_label(starts),
                });
            }
        }
        if !self.windows.is_empty() && !self.windows.iter().any(|w| w.contains(at)) {
            return Err(Denied::OutsideHours {
                name: self.name.clone(),
                hours: self
                    .windows
                    .iter()
                    .map(Window::label)
                    .collect::<Vec<_>>()
                    .join("、"),
            });
        }
        Ok(())
    }
}

/// 発行したばかりのキー。`secret` はこのときしか分からない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedKey {
    pub key: AccessKey,
    pub secret: String,
}

/// キーの変えられるところ。`None` は変えない。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyPatch {
    pub name: Option<String>,
    pub read_only: Option<bool>,
    pub while_locked: Option<bool>,
    /// 開始。`Some(None)` で外す
    pub starts: Option<Option<String>>,
    /// 期限。`Some(None)` で外す
    pub expires: Option<Option<String>>,
    pub windows: Option<Vec<Window>>,
}

/// 使ってよいと分かったときの、できること。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// 使ったキー。キーなしでつないできたときは `None`。
    pub key: Option<AccessKey>,
    /// 読み取りのツールだけ使える
    pub read_only: bool,
}

/// 使えない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denied {
    /// アプリがロックしている（または動いていない）
    Locked,
    /// キーなしの接続は使えない設定
    NeedsKey,
    /// キーが違う、または失効している
    BadKey,
    /// キーの期限が切れている
    Expired { name: String, until: String },
    /// キーを使えるようになる前
    NotYet { name: String, from: String },
    /// キーを使える時間帯の外
    OutsideHours { name: String, hours: String },
    /// キーなしの接続を使える時間の外（`hours` は使えるとき）
    KeylessHours { hours: String },
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Denied::Locked => f.write_str(
                "malakt-mahoz はロックされています。アプリを開いて、ロックを解除してください。",
            ),
            Denied::NeedsKey => f.write_str(
                "malakt-mahoz にはアクセスキーが必要です。アプリの設定の「アクセスキー」でキーを発行して、環境変数 MALAKT_MAHOZ_KEY（HTTP では Authorization: Bearer）で渡してください。",
            ),
            Denied::BadKey => f.write_str(
                "アクセスキーが違うか、失効しています。アプリの設定の「アクセスキー」でキーを発行し直してください。",
            ),
            Denied::Expired { name, until } => {
                write!(f, "アクセスキー「{name}」の期限が切れています（{until} まで）。")
            }
            Denied::NotYet { name, from } => {
                write!(f, "アクセスキー「{name}」は {from} から使えます。")
            }
            Denied::OutsideHours { name, hours } => {
                write!(f, "アクセスキー「{name}」を使える時間帯は {hours} です。")
            }
            Denied::KeylessHours { hours } => write!(
                f,
                "malakt-mahoz をアクセスキーなしで使えるのは {hours} です。ほかのときはアクセスキーが必要です。"
            ),
        }
    }
}

fn clean_name(name: &str) -> Result<String> {
    let name = crate::text::normalize(name).trim().to_owned();
    if name.is_empty() {
        return Err(Error::Invalid("アクセスキーの名前が空です".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(Error::Invalid(format!(
            "アクセスキーの名前は {MAX_NAME_CHARS} 文字までです"
        )));
    }
    Ok(name)
}

const COLUMNS: &str =
    "id, name, hint, read_only, while_locked, created_at, used_at, starts, expires, windows";

fn key_row(r: &rusqlite::Row) -> rusqlite::Result<AccessKey> {
    let windows: String = r.get(9)?;
    Ok(AccessKey {
        id: r.get(0)?,
        name: r.get(1)?,
        hint: r.get(2)?,
        read_only: r.get(3)?,
        while_locked: r.get(4)?,
        created_at: r.get(5)?,
        used_at: r.get(6)?,
        starts: r.get(7)?,
        expires: r.get(8)?,
        // 読めない時間帯（手で書き換えたなど）は、どの時刻にも入らないものにする
        windows: serde_json::from_str(&windows).unwrap_or_else(|_| {
            vec![Window {
                days: Vec::new(),
                from: 0,
                to: 0,
            }]
        }),
    })
}

impl Store {
    /// アクセスキーを発行する。secret は返り値でしか分からない。
    pub fn issue_access_key(
        &mut self,
        name: &str,
        read_only: bool,
        while_locked: bool,
    ) -> Result<IssuedKey> {
        let name = clean_name(name)?;
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM access_keys WHERE keyless = 0",
            [],
            |r| r.get(0),
        )?;
        if count >= MAX_KEYS {
            return Err(Error::Invalid(format!(
                "アクセスキーは {MAX_KEYS} 個までです。使っていないキーを失効させてください"
            )));
        }
        let secret = random_secret(SECRET_PREFIX);
        let hint: String = secret[secret.len() - 4..].to_owned();
        let key = AccessKey {
            id: new_id(),
            name,
            hint,
            read_only,
            while_locked,
            created_at: now_ms(),
            used_at: None,
            starts: None,
            expires: None,
            windows: Vec::new(),
        };
        self.conn.execute(
            "INSERT INTO access_keys(id, name, secret_hash, hint, read_only, while_locked, \
             created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                key.id,
                key.name,
                hash_of(secret.as_bytes()),
                key.hint,
                key.read_only,
                key.while_locked,
                key.created_at
            ],
        )?;
        Ok(IssuedKey { key, secret })
    }

    /// 発行したキー。発行した順。
    pub fn access_keys(&self) -> Result<Vec<AccessKey>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM access_keys WHERE keyless = 0 ORDER BY created_at, id"
        ))?;
        let keys = stmt
            .query_map([], key_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
    }

    /// キーなしの接続の決まり。足した順。名前は [`KEYLESS_NAME`]。
    pub fn keyless_rules(&self) -> Result<Vec<AccessKey>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM access_keys WHERE keyless = 1 ORDER BY created_at, id"
        ))?;
        let rules = stmt
            .query_map([], key_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rules)
    }

    /// キーなしの接続の決まりを足す。最初はいつでも使え、ロック中は使えない。
    /// 開始・期限・時間帯などは、キーと同じく [`Store::update_access_key`] で変える。
    pub fn add_keyless_rule(&mut self, read_only: bool) -> Result<AccessKey> {
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM access_keys WHERE keyless = 1",
            [],
            |r| r.get(0),
        )?;
        if count >= MAX_RULES {
            return Err(Error::Invalid(format!(
                "キーなしの接続の決まりは {MAX_RULES} 個までです"
            )));
        }
        let rule = AccessKey {
            id: new_id(),
            name: KEYLESS_NAME.into(),
            hint: String::new(),
            read_only,
            while_locked: false,
            created_at: now_ms(),
            used_at: None,
            starts: None,
            expires: None,
            windows: Vec::new(),
        };
        // secret_hash は一意でなければならない。どのキーのハッシュとも重ならない形にする
        self.conn.execute(
            "INSERT INTO access_keys(id, name, secret_hash, hint, read_only, while_locked, \
             created_at, keyless) VALUES (?1, ?2, 'keyless:' || ?1, '', ?3, 0, ?4, 1)",
            params![rule.id, rule.name, rule.read_only, rule.created_at],
        )?;
        Ok(rule)
    }

    /// キーなしの接続の決まりを外す。
    pub fn remove_keyless_rule(&mut self, id: &str) -> Result<()> {
        let removed = self.conn.execute(
            "DELETE FROM access_keys WHERE id = ?1 AND keyless = 1",
            params![id],
        )?;
        if removed == 0 {
            return Err(Error::NotFound(format!("キーなしの接続の決まり {id}")));
        }
        Ok(())
    }

    pub fn access_key(&self, id: &str) -> Result<AccessKey> {
        self.conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM access_keys WHERE id = ?1"))?
            .query_row(params![id], key_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("アクセスキー {id}")))
    }

    /// キーの名前と、できることを変える。
    pub fn update_access_key(&mut self, id: &str, patch: &KeyPatch) -> Result<AccessKey> {
        let mut key = self.access_key(id)?;
        if let Some(name) = &patch.name {
            key.name = clean_name(name)?;
        }
        if let Some(read_only) = patch.read_only {
            key.read_only = read_only;
        }
        if let Some(while_locked) = patch.while_locked {
            key.while_locked = while_locked;
        }
        if let Some(starts) = &patch.starts {
            key.starts = clean_moment(starts.as_deref(), "開始")?;
        }
        if let Some(expires) = &patch.expires {
            key.expires = clean_moment(expires.as_deref(), "期限")?;
        }
        if let Some(windows) = &patch.windows {
            if windows.len() > MAX_WINDOWS {
                return Err(Error::Invalid(format!(
                    "アクセスキーの時間帯は {MAX_WINDOWS} 個までです"
                )));
            }
            for window in windows {
                window.check()?;
            }
            key.windows = windows
                .iter()
                .map(|w| {
                    let mut w = w.clone();
                    w.days.sort_unstable();
                    w.days.dedup();
                    w
                })
                .collect();
        }
        self.conn.execute(
            "UPDATE access_keys SET name = ?2, read_only = ?3, while_locked = ?4, starts = ?5, \
             expires = ?6, windows = ?7 WHERE id = ?1",
            params![
                key.id,
                key.name,
                key.read_only,
                key.while_locked,
                key.starts,
                key.expires,
                serde_json::to_string(&key.windows)?
            ],
        )?;
        Ok(key)
    }

    /// キーを失効させる（消す）。元に戻せない。
    pub fn revoke_access_key(&mut self, id: &str) -> Result<()> {
        let removed = self.conn.execute(
            "DELETE FROM access_keys WHERE id = ?1 AND keyless = 0",
            params![id],
        )?;
        if removed == 0 {
            return Err(Error::NotFound(format!("アクセスキー {id}")));
        }
        Ok(())
    }

    fn access_key_by_secret(&self, secret: &str) -> Result<Option<AccessKey>> {
        if !has_secret_shape(secret, SECRET_PREFIX) {
            return Ok(None);
        }
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM access_keys WHERE secret_hash = ?1 AND keyless = 0"
            ))?
            .query_row(params![hash_of(secret.as_bytes())], key_row)
            .optional()?)
    }

    /// 書き込みを止めた状態で `f` を動かす（SQLite の `query_only`）。読み取りのみのキーで使う。
    /// 終わったら、`f` が失敗しても書けるように戻す。
    pub fn read_only<T>(&mut self, f: impl FnOnce(&mut Store) -> T) -> Result<T> {
        self.conn.pragma_update(None, "query_only", true)?;
        let out = f(self);
        self.conn.pragma_update(None, "query_only", false)?;
        Ok(out)
    }
}

/// アプリの外から今使ってよいか。`secret` はつないできたものが渡したキー（なければ `None`）。
/// `now` は UNIX 秒。使ってよければ、キーの「最後に使った時刻」を書く。
pub fn check(
    store: &mut Store,
    secret: Option<&str>,
    now: i64,
) -> Result<std::result::Result<Grant, Denied>> {
    let secret = secret.map(str::trim).filter(|s| !s.is_empty());
    let Some(secret) = secret else {
        return keyless_check(store, now);
    };
    let Some(mut key) = store.access_key_by_secret(secret)? else {
        return Ok(Err(Denied::BadKey));
    };
    if let Err(denied) = key.usable_at(local(now)) {
        return Ok(Err(denied));
    }
    if !key.while_locked && !lock::allows_outside(store, now)? {
        return Ok(Err(Denied::Locked));
    }
    let at = now * 1000;
    if key.used_at.is_none_or(|used| used + TOUCH_EVERY_MS <= at) {
        store.conn.execute(
            "UPDATE access_keys SET used_at = ?2 WHERE id = ?1",
            params![key.id, at],
        )?;
        key.used_at = Some(at);
    }
    Ok(Ok(Grant {
        read_only: key.read_only,
        key: Some(key),
    }))
}

/// キーなしでつないできたものを、キーなしの接続の決まりで確かめる。
fn keyless_check(store: &mut Store, now: i64) -> Result<std::result::Result<Grant, Denied>> {
    let rules = store.keyless_rules()?;
    if rules.is_empty() {
        return Ok(Err(Denied::NeedsKey));
    }
    let at = local(now);
    // 期間と時間帯に入っている決まり
    let open: Vec<&AccessKey> = rules.iter().filter(|r| r.usable_at(at).is_ok()).collect();
    if open.is_empty() {
        return Ok(Err(Denied::KeylessHours {
            hours: rules
                .iter()
                .map(AccessKey::when_label)
                .collect::<Vec<_>>()
                .join("、"),
        }));
    }
    let unlocked = lock::allows_outside(store, now)?;
    let allowed: Vec<&&AccessKey> = open.iter().filter(|r| unlocked || r.while_locked).collect();
    if allowed.is_empty() {
        return Ok(Err(Denied::Locked));
    }
    Ok(Ok(Grant {
        key: None,
        read_only: allowed.iter().all(|r| r.read_only),
    }))
}

/// 期限を、今からの長さで選ぶときの選択肢。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expiry {
    Hour,
    Today,
    Week,
    Month,
    Quarter,
}

impl Expiry {
    pub const ALL: [Expiry; 5] = [
        Expiry::Hour,
        Expiry::Today,
        Expiry::Week,
        Expiry::Month,
        Expiry::Quarter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Expiry::Hour => "1 時間",
            Expiry::Today => "今日の終わりまで",
            Expiry::Week => "7 日",
            Expiry::Month => "30 日",
            Expiry::Quarter => "90 日",
        }
    }

    /// `now`（その PC の時刻）から見た期限。保存する形（`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`）。
    pub fn until(self, now: NaiveDateTime) -> String {
        let at = |t: NaiveDateTime| t.format("%Y-%m-%d %H:%M").to_string();
        let days = |n: i64| at(now + chrono::Duration::days(n));
        match self {
            Expiry::Hour => at(now + chrono::Duration::hours(1)),
            Expiry::Today => now.date().format("%Y-%m-%d").to_string(),
            Expiry::Week => days(7),
            Expiry::Month => days(30),
            Expiry::Quarter => days(90),
        }
    }
}

/// 今の、その PC の時刻。
pub fn now_local() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// UNIX 秒を、その PC の時刻にする。
fn local(now: i64) -> NaiveDateTime {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(now, 0)
        .earliest()
        .map(|t| t.naive_local())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_list_update_and_revoke() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(s.access_keys().unwrap().is_empty());
        let issued = s.issue_access_key(" Claude Code ", false, false).unwrap();
        assert!(issued.secret.starts_with(SECRET_PREFIX));
        assert!(has_secret_shape(&issued.secret, SECRET_PREFIX));
        assert_eq!(issued.key.name, "Claude Code");
        assert_eq!(issued.key.hint, &issued.secret[issued.secret.len() - 4..]);
        assert_eq!(issued.key.masked(), format!("mmak_••••{}", issued.key.hint));
        // データベースには secret をそのまま置かない
        let stored: String = s
            .conn
            .query_row("SELECT secret_hash FROM access_keys", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, issued.secret);
        assert!(!stored.contains(&issued.secret[SECRET_PREFIX.len()..]));

        let other = s.issue_access_key("Cursor", true, true).unwrap();
        assert_ne!(other.secret, issued.secret);
        let keys = s.access_keys().unwrap();
        assert_eq!(keys, vec![issued.key.clone(), other.key.clone()]);

        let changed = s
            .update_access_key(
                &issued.key.id,
                &KeyPatch {
                    name: Some("Codex".into()),
                    read_only: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (
                changed.name.as_str(),
                changed.read_only,
                changed.while_locked
            ),
            ("Codex", true, false)
        );
        assert_eq!(s.access_key(&issued.key.id).unwrap(), changed);
        let changed = s
            .update_access_key(
                &issued.key.id,
                &KeyPatch {
                    while_locked: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (
                changed.name.as_str(),
                changed.read_only,
                changed.while_locked
            ),
            ("Codex", true, true)
        );
        // 空の名前と長すぎる名前は、何も変えずにエラーにする
        for name in ["  ", &"あ".repeat(MAX_NAME_CHARS + 1)] {
            let patch = KeyPatch {
                name: Some(name.to_owned()),
                read_only: Some(false),
                ..Default::default()
            };
            assert!(s.update_access_key(&issued.key.id, &patch).is_err());
            assert!(s.issue_access_key(name, false, false).is_err());
        }
        assert_eq!(s.access_key(&issued.key.id).unwrap(), changed);

        s.revoke_access_key(&issued.key.id).unwrap();
        assert_eq!(s.access_keys().unwrap(), vec![other.key.clone()]);
        assert!(s.revoke_access_key(&issued.key.id).is_err());
        assert!(s
            .update_access_key(&issued.key.id, &KeyPatch::default())
            .is_err());
    }

    #[test]
    fn at_most_max_keys() {
        let mut s = Store::open_in_memory().unwrap();
        for i in 0..MAX_KEYS {
            s.issue_access_key(&format!("キー {i}"), false, false)
                .unwrap();
        }
        let error = s.issue_access_key("もう 1 つ", false, false).unwrap_err();
        assert!(error.to_string().contains("100 個まで"), "{error}");
        assert_eq!(s.access_keys().unwrap().len(), MAX_KEYS);
    }

    /// キーなしの接続の決まりを、すべて外す。
    fn no_keyless(s: &mut Store) {
        for rule in s.keyless_rules().unwrap() {
            s.remove_keyless_rule(&rule.id).unwrap();
        }
    }

    /// その PC の時刻を、UNIX 秒にする。
    fn unix(text: &str) -> i64 {
        use chrono::TimeZone;
        chrono::Local
            .from_local_datetime(&at(text))
            .earliest()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn keyless_connections_work_as_before_unless_changed() {
        let mut s = Store::open_in_memory().unwrap();
        // 標準は「読み書き・いつでも」の決まりが 1 つ
        let rules = s.keyless_rules().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, KEYLESS_NAME);
        assert!(!rules[0].read_only && !rules[0].while_locked && !rules[0].limited());
        assert!(s.access_keys().unwrap().is_empty());
        let full = Grant {
            key: None,
            read_only: false,
        };
        assert_eq!(check(&mut s, None, 1_000).unwrap(), Ok(full.clone()));
        // 空のキーは、キーなしと同じ
        assert_eq!(check(&mut s, Some("  "), 1_000).unwrap(), Ok(full.clone()));
        // ロックしている間は使えない（アクセスキーを入れる前と同じ）
        lock::enable(&mut s).unwrap();
        assert_eq!(check(&mut s, None, 1_000).unwrap(), Err(Denied::Locked));
        lock::mark_open(&mut s, 1_000).unwrap();
        assert_eq!(check(&mut s, None, 1_000).unwrap(), Ok(full));

        s.update_access_key(
            &rules[0].id,
            &KeyPatch {
                read_only: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            check(&mut s, None, 1_000).unwrap(),
            Ok(Grant {
                key: None,
                read_only: true
            })
        );
        no_keyless(&mut s);
        assert_eq!(check(&mut s, None, 1_000).unwrap(), Err(Denied::NeedsKey));
    }

    /// キーなしの決まりは、ロック中も使えるものや、時間帯で絞ったものにできる。
    /// 当てはまる決まりのどれかが読み書きなら、読み書き。
    #[test]
    fn keyless_rules_by_lock_and_hours() {
        let mut s = Store::open_in_memory().unwrap();
        no_keyless(&mut s);
        // 平日 9:00〜18:00 は読み書き
        let work = s.add_keyless_rule(false).unwrap();
        s.update_access_key(
            &work.id,
            &KeyPatch {
                windows: Some(vec![Window {
                    days: vec![0, 1, 2, 3, 4],
                    from: 9 * 60,
                    to: 18 * 60,
                }]),
                ..Default::default()
            },
        )
        .unwrap();
        // 2026-11-30 は月曜
        let (monday, night, sunday) = (
            unix("2026-11-30 10:00"),
            unix("2026-11-30 20:00"),
            unix("2026-11-29 10:00"),
        );
        let grant = |read_only| {
            Ok(Grant {
                key: None,
                read_only,
            })
        };
        assert_eq!(check(&mut s, None, monday).unwrap(), grant(false));
        let outside = Denied::KeylessHours {
            hours: "平日 9:00〜18:00".into(),
        };
        assert_eq!(check(&mut s, None, night).unwrap(), Err(outside.clone()));
        assert_eq!(
            outside.to_string(),
            "malakt-mahoz をアクセスキーなしで使えるのは 平日 9:00〜18:00 です。ほかのときはアクセスキーが必要です。"
        );
        assert!(check(&mut s, None, sunday).unwrap().is_err());

        // いつでも読み取りのみ、ロック中も
        let read = s.add_keyless_rule(true).unwrap();
        s.update_access_key(
            &read.id,
            &KeyPatch {
                while_locked: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(check(&mut s, None, monday).unwrap(), grant(false));
        assert_eq!(check(&mut s, None, night).unwrap(), grant(true));
        lock::enable(&mut s).unwrap();
        // ロック中は、ロック中も使える決まりだけ
        assert_eq!(check(&mut s, None, monday).unwrap(), grant(true));
        s.update_access_key(
            &read.id,
            &KeyPatch {
                while_locked: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(check(&mut s, None, monday).unwrap(), Err(Denied::Locked));
        lock::mark_open(&mut s, monday).unwrap();
        assert_eq!(check(&mut s, None, monday).unwrap(), grant(false));

        // 期限の切れた決まりは当てはまらない
        s.remove_keyless_rule(&read.id).unwrap();
        s.update_access_key(
            &work.id,
            &KeyPatch {
                expires: Some(Some("2000-01-01".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(
            check(&mut s, None, monday).unwrap(),
            Err(Denied::KeylessHours { hours }) if hours == "2000/01/01 まで・平日 9:00〜18:00"
        ));
    }

    /// キーと決まりは、互いの一覧に出ず、互いの消し方では消えない。
    #[test]
    fn rules_and_keys_stay_apart() {
        let mut s = Store::open_in_memory().unwrap();
        let rule = s.keyless_rules().unwrap().remove(0);
        let key = s.issue_access_key("鍵", false, false).unwrap().key;
        assert_eq!(s.access_keys().unwrap(), vec![key.clone()]);
        assert_eq!(s.keyless_rules().unwrap(), vec![rule.clone()]);
        assert!(s.revoke_access_key(&rule.id).is_err());
        assert!(s.remove_keyless_rule(&key.id).is_err());
        // 決まりの secret_hash では、キーとして通らない
        assert_eq!(
            check(&mut s, Some(&format!("keyless:{}", rule.id)), 1_000).unwrap(),
            Err(Denied::BadKey)
        );
        for _ in 1..MAX_RULES {
            s.add_keyless_rule(false).unwrap();
        }
        let error = s.add_keyless_rule(false).unwrap_err();
        assert!(error.to_string().contains("20 個まで"), "{error}");
        // 決まりの数は、キーの数に入らない
        s.issue_access_key("もう 1 つ", false, false).unwrap();
        assert_eq!(s.access_keys().unwrap().len(), 2);
    }

    #[test]
    fn keys_decide_lock_and_writes() {
        let mut s = Store::open_in_memory().unwrap();
        no_keyless(&mut s);
        let plain = s.issue_access_key("ふつう", false, false).unwrap();
        let locked = s.issue_access_key("ロック中も", true, true).unwrap();

        let grant = check(&mut s, Some(&plain.secret), 1_000).unwrap().unwrap();
        assert!(!grant.read_only);
        assert_eq!(grant.key.as_ref().unwrap().id, plain.key.id);
        // 前後の空白は無視する（環境変数に改行が入ったときなど）
        let padded = format!(" {}\n", plain.secret);
        assert!(check(&mut s, Some(&padded), 1_000).unwrap().is_ok());

        lock::enable(&mut s).unwrap();
        assert_eq!(
            check(&mut s, Some(&plain.secret), 1_000).unwrap(),
            Err(Denied::Locked)
        );
        let grant = check(&mut s, Some(&locked.secret), 1_000).unwrap().unwrap();
        assert!(grant.read_only);
        lock::mark_open(&mut s, 1_000).unwrap();
        assert!(check(&mut s, Some(&plain.secret), 1_000).unwrap().is_ok());

        // 違うキー、形の違う文字、完了チケット、失効したキーは使えない
        // 最後の 1 文字だけ違う
        let mut wrong = plain.secret.clone();
        let last = wrong.pop().unwrap();
        wrong.push(if last == 'A' { 'B' } else { 'A' });
        let ticket = plain.secret.replacen(SECRET_PREFIX, "mmct_", 1);
        for bad in [wrong.as_str(), "mmak_short", ticket.as_str(), "x"] {
            assert_eq!(
                check(&mut s, Some(bad), 1_000).unwrap(),
                Err(Denied::BadKey),
                "{bad}"
            );
        }
        s.revoke_access_key(&plain.key.id).unwrap();
        assert_eq!(
            check(&mut s, Some(&plain.secret), 1_000).unwrap(),
            Err(Denied::BadKey)
        );
        // キーなしの扱いを変えても、キーの扱いは変わらない
        s.add_keyless_rule(false).unwrap();
        assert!(
            check(&mut s, Some(&locked.secret), 1_000)
                .unwrap()
                .unwrap()
                .read_only
        );
    }

    #[test]
    fn last_use_is_written_once_a_minute() {
        let mut s = Store::open_in_memory().unwrap();
        let issued = s.issue_access_key("Claude Code", false, false).unwrap();
        let used = |s: &Store| s.access_key(&issued.key.id).unwrap().used_at;
        assert_eq!(used(&s), None);
        check(&mut s, Some(&issued.secret), 1_000).unwrap().unwrap();
        assert_eq!(used(&s), Some(1_000_000));
        check(&mut s, Some(&issued.secret), 1_059).unwrap().unwrap();
        assert_eq!(used(&s), Some(1_000_000));
        let grant = check(&mut s, Some(&issued.secret), 1_060).unwrap().unwrap();
        assert_eq!(used(&s), Some(1_060_000));
        assert_eq!(grant.key.unwrap().used_at, Some(1_060_000));
    }

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn windows_by_weekday_and_time() {
        // 2026-11-30 は月曜
        let weekdays = Window {
            days: vec![0, 1, 2, 3, 4],
            from: 9 * 60,
            to: 18 * 60,
        };
        assert!(weekdays.contains(at("2026-11-30 09:00")));
        assert!(weekdays.contains(at("2026-11-30 17:59")));
        assert!(!weekdays.contains(at("2026-11-30 18:00")));
        assert!(!weekdays.contains(at("2026-11-30 08:59")));
        // 土曜
        assert!(!weekdays.contains(at("2026-12-05 10:00")));
        assert_eq!(weekdays.label(), "平日 9:00〜18:00");

        // 日をまたぐ: 金曜の 22 時から土曜の 6 時まで
        let night = Window {
            days: vec![4],
            from: 22 * 60,
            to: 6 * 60,
        };
        assert!(night.contains(at("2026-12-04 22:00")));
        assert!(night.contains(at("2026-12-05 05:59")));
        assert!(!night.contains(at("2026-12-05 06:00")));
        assert!(!night.contains(at("2026-12-05 22:30")));
        assert!(!night.contains(at("2026-12-04 05:00")));
        assert_eq!(night.label(), "金 22:00〜6:00");

        let all = Window {
            days: (0..7).collect(),
            from: 0,
            to: 24 * 60,
        };
        assert!(all.contains(at("2026-12-06 23:59")));
        assert_eq!(all.label(), "毎日 0:00〜24:00");
        assert_eq!(Window::standard().label(), "毎日 9:00〜18:00");
        assert_eq!(
            Window {
                days: vec![6, 5],
                from: 600,
                to: 630
            }
            .days_label(),
            "土日"
        );
        assert_eq!(
            Window {
                days: vec![0, 2, 4],
                from: 0,
                to: 1
            }
            .days_label(),
            "月・水・金"
        );
        assert_eq!(parse_clock("9"), Some(540));
        assert_eq!(parse_clock("21時半"), Some(21 * 60 + 30));
        assert_eq!(parse_clock("24:00"), Some(1440));
        assert_eq!(parse_clock("０９３０"), Some(570));
        assert_eq!(parse_clock("25"), None);
        assert_eq!(parse_clock("あとで"), None);
    }

    #[test]
    fn period_and_hours_of_a_key() {
        let mut s = Store::open_in_memory().unwrap();
        let issued = s.issue_access_key("夜間", false, false).unwrap();
        let id = issued.key.id.clone();
        let key = |s: &Store| s.access_key(&id).unwrap();
        assert!(!key(&s).limited());
        assert!(key(&s).usable_at(at("1999-01-01 00:00")).is_ok());

        let patch = KeyPatch {
            starts: Some(Some("2026-12-01".into())),
            expires: Some(Some("2026-12-31 18:00".into())),
            windows: Some(vec![Window {
                days: vec![4, 0, 0],
                from: 9 * 60,
                to: 18 * 60,
            }]),
            ..Default::default()
        };
        s.update_access_key(&id, &patch).unwrap();
        let k = key(&s);
        assert!(k.limited());
        // 曜日はそろえて、重なりを除く
        assert_eq!(k.windows[0].days, vec![0, 4]);
        assert_eq!(
            k.usable_at(at("2026-11-30 10:00")),
            Err(Denied::NotYet {
                name: "夜間".into(),
                from: "2026/12/01".into()
            })
        );
        // 12/7 は月曜
        assert!(k.usable_at(at("2026-12-07 10:00")).is_ok());
        let outside = k.usable_at(at("2026-12-08 10:00")).unwrap_err();
        assert_eq!(
            outside.to_string(),
            "アクセスキー「夜間」を使える時間帯は 月・金 9:00〜18:00 です。"
        );
        assert!(k.usable_at(at("2026-12-07 08:00")).is_err());
        assert_eq!(
            k.usable_at(at("2026-12-31 18:00")).unwrap_err().to_string(),
            "アクセスキー「夜間」の期限が切れています（2026/12/31 18:00 まで）。"
        );
        // 日付だけの期限は、その日の終わりまで
        let patch = KeyPatch {
            starts: Some(None),
            expires: Some(Some("2026-12-31".into())),
            windows: Some(Vec::new()),
            ..Default::default()
        };
        s.update_access_key(&id, &patch).unwrap();
        let k = key(&s);
        assert_eq!(k.starts, None);
        assert!(k.usable_at(at("2026-12-31 23:59")).is_ok());
        assert!(k.usable_at(at("2027-01-01 00:00")).is_err());

        // 読めない日時と時間帯は、何も変えずにエラーにする
        for patch in [
            KeyPatch {
                expires: Some(Some("来年".into())),
                ..Default::default()
            },
            KeyPatch {
                starts: Some(Some("2026-13-01".into())),
                ..Default::default()
            },
            KeyPatch {
                windows: Some(vec![Window {
                    days: vec![],
                    from: 0,
                    to: 60,
                }]),
                ..Default::default()
            },
            KeyPatch {
                windows: Some(vec![Window {
                    days: vec![7],
                    from: 0,
                    to: 60,
                }]),
                ..Default::default()
            },
            KeyPatch {
                windows: Some(vec![Window {
                    days: vec![1],
                    from: 600,
                    to: 600,
                }]),
                ..Default::default()
            },
            KeyPatch {
                windows: Some(vec![Window::standard(); MAX_WINDOWS + 1]),
                ..Default::default()
            },
        ] {
            assert!(s.update_access_key(&id, &patch).is_err(), "{patch:?}");
        }
        assert_eq!(key(&s), k);
        // 空の文字は、外すのと同じ
        let patch = KeyPatch {
            expires: Some(Some(" ".into())),
            ..Default::default()
        };
        assert_eq!(s.update_access_key(&id, &patch).unwrap().expires, None);

        // 期限の切れたキーは、ロックより先に断る
        let patch = KeyPatch {
            expires: Some(Some("2000-01-01".into())),
            ..Default::default()
        };
        s.update_access_key(&id, &patch).unwrap();
        assert!(matches!(
            check(&mut s, Some(&issued.secret), 1_000_000_000).unwrap(),
            Err(Denied::Expired { .. })
        ));
        // 読めない時間帯（手で書き換えたなど）は、いつも時間帯の外
        s.conn
            .execute("UPDATE access_keys SET expires = NULL, windows = 'x'", [])
            .unwrap();
        assert!(matches!(
            check(&mut s, Some(&issued.secret), 1_000_000_000).unwrap(),
            Err(Denied::OutsideHours { .. })
        ));
    }

    #[test]
    fn read_only_blocks_every_write() {
        let mut s = Store::open_in_memory().unwrap();
        let blocked = s
            .read_only(|s| s.create_workspace(None, "冬のイベント"))
            .unwrap();
        assert!(blocked.is_err());
        let read = s.read_only(|s| s.workspaces()).unwrap().unwrap();
        assert!(read.is_empty());
        // 終わったら、また書ける
        s.create_workspace(None, "冬のイベント").unwrap();
    }
    #[test]
    fn expiry_from_now() {
        let now = NaiveDateTime::parse_from_str("2026-11-28 21:40", "%Y-%m-%d %H:%M").unwrap();
        assert_eq!(Expiry::Hour.until(now), "2026-11-28 22:40");
        assert_eq!(Expiry::Today.until(now), "2026-11-28");
        assert_eq!(Expiry::Week.until(now), "2026-12-05 21:40");
        assert_eq!(Expiry::Month.until(now), "2026-12-28 21:40");
        assert_eq!(Expiry::Quarter.until(now), "2027-02-26 21:40");
    }
}
