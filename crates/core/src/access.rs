//! アクセスキー。アプリの外のエージェント（MCP サーバーとコマンド）に渡す合い言葉。
//!
//! キーは画面で発行し、エージェントには環境変数で渡す。キーごとに、読み取りのみにするか、
//! アプリがロックしている間も使えるかを選べる。失効させると、動いているサーバーもすぐ使えなくなる。
//!
//! キーを付けずにつないできたものをどう扱うかは [`keyless`] で選ぶ。標準は「読み書き」で、
//! アクセスキーを入れる前と同じ（ロックを解除している間だけ、すべてのツールが使える）。
//!
//! キーの文字列（secret）は 256 ビットの乱数で、データベースにはそのハッシュと最後の 4 文字だけを置く。

use rusqlite::{params, OptionalExtension};

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

/// キーを付けずにつないできたものを、どう扱うか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keyless {
    /// すべてのツールが使える（アクセスキーを入れる前と同じ）
    #[default]
    Full,
    /// 読み取りのツールだけ
    ReadOnly,
    /// 使えない。アクセスキーが要る
    Off,
}

impl Keyless {
    pub const ALL: [Keyless; 3] = [Keyless::Full, Keyless::ReadOnly, Keyless::Off];

    pub fn code(self) -> &'static str {
        match self {
            Keyless::Full => "full",
            Keyless::ReadOnly => "read",
            Keyless::Off => "off",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.code() == code)
    }

    pub fn label(self) -> &'static str {
        match self {
            Keyless::Full => "読み書き",
            Keyless::ReadOnly => "読み取りのみ",
            Keyless::Off => "使えない",
        }
    }
}

const KEYLESS_KEY: &str = "mcp.keyless";

/// キーを付けずにつないできたものの扱い。読めない値が入っていたら（手で書き換えたなど）、使えないものとして扱う。
pub fn keyless(store: &Store) -> Result<Keyless> {
    Ok(match store.setting(KEYLESS_KEY)? {
        None => Keyless::Full,
        Some(code) => Keyless::parse(&code).unwrap_or(Keyless::Off),
    })
}

pub fn set_keyless(store: &mut Store, keyless: Keyless) -> Result<()> {
    match keyless {
        Keyless::Full => store.set_setting(KEYLESS_KEY, None),
        other => store.set_setting(KEYLESS_KEY, Some(other.code())),
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
}

impl AccessKey {
    /// 画面に出す、伏せたキー。
    pub fn masked(&self) -> String {
        format!("{SECRET_PREFIX}••••{}", self.hint)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// アプリがロックしている（または動いていない）
    Locked,
    /// キーなしの接続は使えない設定
    NeedsKey,
    /// キーが違う、または失効している
    BadKey,
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

const COLUMNS: &str = "id, name, hint, read_only, while_locked, created_at, used_at";

fn key_row(r: &rusqlite::Row) -> rusqlite::Result<AccessKey> {
    Ok(AccessKey {
        id: r.get(0)?,
        name: r.get(1)?,
        hint: r.get(2)?,
        read_only: r.get(3)?,
        while_locked: r.get(4)?,
        created_at: r.get(5)?,
        used_at: r.get(6)?,
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
        let count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM access_keys", [], |r| r.get(0))?;
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
            "SELECT {COLUMNS} FROM access_keys ORDER BY created_at, id"
        ))?;
        let keys = stmt
            .query_map([], key_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
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
        self.conn.execute(
            "UPDATE access_keys SET name = ?2, read_only = ?3, while_locked = ?4 WHERE id = ?1",
            params![key.id, key.name, key.read_only, key.while_locked],
        )?;
        Ok(key)
    }

    /// キーを失効させる（消す）。元に戻せない。
    pub fn revoke_access_key(&mut self, id: &str) -> Result<()> {
        let removed = self
            .conn
            .execute("DELETE FROM access_keys WHERE id = ?1", params![id])?;
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
                "SELECT {COLUMNS} FROM access_keys WHERE secret_hash = ?1"
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
        let mode = keyless(store)?;
        if mode == Keyless::Off {
            return Ok(Err(Denied::NeedsKey));
        }
        if !lock::allows_outside(store, now)? {
            return Ok(Err(Denied::Locked));
        }
        return Ok(Ok(Grant {
            key: None,
            read_only: mode == Keyless::ReadOnly,
        }));
    };
    let Some(mut key) = store.access_key_by_secret(secret)? else {
        return Ok(Err(Denied::BadKey));
    };
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

    #[test]
    fn keyless_connections_work_as_before_unless_changed() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(keyless(&s).unwrap(), Keyless::Full);
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

        set_keyless(&mut s, Keyless::ReadOnly).unwrap();
        assert_eq!(keyless(&s).unwrap(), Keyless::ReadOnly);
        assert_eq!(
            check(&mut s, None, 1_000).unwrap(),
            Ok(Grant {
                key: None,
                read_only: true
            })
        );
        set_keyless(&mut s, Keyless::Off).unwrap();
        assert_eq!(check(&mut s, None, 1_000).unwrap(), Err(Denied::NeedsKey));
        set_keyless(&mut s, Keyless::Full).unwrap();
        assert!(s.setting(KEYLESS_KEY).unwrap().is_none());
        // 読めない値は、使えないのと同じ
        s.set_setting(KEYLESS_KEY, Some("maybe")).unwrap();
        assert_eq!(keyless(&s).unwrap(), Keyless::Off);
        for k in Keyless::ALL {
            assert_eq!(Keyless::parse(k.code()), Some(k));
        }
    }

    #[test]
    fn keys_decide_lock_and_writes() {
        let mut s = Store::open_in_memory().unwrap();
        set_keyless(&mut s, Keyless::Off).unwrap();
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
        set_keyless(&mut s, Keyless::Full).unwrap();
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
}
