//! 自分で足したアイコンの画像。
//!
//! 画像は小さな PNG にしてからデータベースに入れる。名前は `custom-` と中身のハッシュの頭で、
//! 同じ画像は 1 つだけ持つ。どのワークスペースも使っていない画像は、次に開いたときに消す。

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::now_ms;
use crate::{Error, Result, Store};

/// 自分で足したアイコンの名前の頭。
pub const CUSTOM_PREFIX: &str = "custom-";
/// 1 つの画像の上限（PNG にしたあとのバイト数）。
pub const MAX_CUSTOM_ICON_BYTES: usize = 256 * 1024;
/// 持てる画像の数。
pub const MAX_CUSTOM_ICONS: i64 = 200;
/// 使っていない画像を残しておく時間。選んでいる途中の画像を消さないため。
const KEEP_UNUSED_MS: i64 = 24 * 60 * 60 * 1000;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const HASH_CHARS: usize = 24;

/// 自分で足したアイコンの名前か。
pub fn is_custom(icon: &str) -> bool {
    icon.strip_prefix(CUSTOM_PREFIX).is_some_and(|hash| {
        hash.len() == HASH_CHARS && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn name_of(png: &[u8]) -> String {
    let hash = crate::attachment::hash_of(png);
    format!("{CUSTOM_PREFIX}{}", &hash[..HASH_CHARS])
}

fn check(png: &[u8]) -> Result<()> {
    if !png.starts_with(PNG_MAGIC) {
        return Err(Error::Invalid("アイコンの画像が PNG ではありません".into()));
    }
    if png.len() > MAX_CUSTOM_ICON_BYTES {
        return Err(Error::Invalid(format!(
            "アイコンの画像が大きすぎます（{} KB まで）",
            MAX_CUSTOM_ICON_BYTES / 1024
        )));
    }
    Ok(())
}

pub(crate) fn put(conn: &Connection, name: &str, png: &[u8]) -> Result<()> {
    check(png)?;
    if name != name_of(png) {
        return Err(Error::Invalid(format!(
            "アイコンの画像 {name} が壊れています"
        )));
    }
    conn.prepare_cached(
        "INSERT OR IGNORE INTO custom_icons(name, data, created_at) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![name, png, now_ms()])?;
    Ok(())
}

pub(crate) fn get(conn: &Connection, name: &str) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .prepare_cached("SELECT data FROM custom_icons WHERE name = ?1")?
        .query_row(params![name], |r| r.get(0))
        .optional()?)
}

/// 古くて、どのワークスペースも使っていない画像を消す。
pub(crate) fn purge(conn: &Connection, older_than: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM custom_icons WHERE created_at <= ?1 \
         AND name NOT IN (SELECT icon FROM workspaces WHERE icon IS NOT NULL)",
        params![older_than],
    )?)
}

impl Store {
    /// 小さな PNG にした画像を足して、アイコンの名前を返す。同じ画像は足し直さない。
    pub fn add_custom_icon(&mut self, png: &[u8]) -> Result<String> {
        check(png)?;
        let name = name_of(png);
        if get(&self.conn, &name)?.is_none() {
            let count: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM custom_icons", [], |r| r.get(0))?;
            if count >= MAX_CUSTOM_ICONS {
                return Err(Error::Invalid(format!(
                    "アイコンの画像は {MAX_CUSTOM_ICONS} 個までです"
                )));
            }
            put(&self.conn, &name, png)?;
        }
        Ok(name)
    }

    /// 足した画像の PNG。
    pub fn custom_icon(&self, name: &str) -> Result<Option<Vec<u8>>> {
        get(&self.conn, name)
    }

    /// 足した画像の名前。新しいものが先。
    pub fn custom_icons(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT name FROM custom_icons ORDER BY created_at DESC, name")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 1 日より前に足して、どのワークスペースも使っていない画像を消す。消した数を返す。
    pub fn purge_custom_icons(&mut self) -> Result<usize> {
        purge(&self.conn, now_ms() - KEEP_UNUSED_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(seed: u8) -> Vec<u8> {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend([seed; 16]);
        bytes
    }

    #[test]
    fn an_added_image_becomes_a_workspace_icon() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = store.create_workspace(None, "Lorem").unwrap();
        let name = store.add_custom_icon(&png(1)).unwrap();
        assert!(is_custom(&name), "{name}");
        // 同じ画像は 1 つ
        assert_eq!(store.add_custom_icon(&png(1)).unwrap(), name);
        assert_eq!(store.custom_icons().unwrap(), [name.clone()]);
        assert_eq!(store.custom_icon(&name).unwrap(), Some(png(1)));

        let ws = store
            .set_workspace_icon(&ws.id, Some(&name), Some("blue"))
            .unwrap();
        assert_eq!(ws.icon.as_deref(), Some(name.as_str()));
        // 持っていない画像は選べない
        let missing = format!("{CUSTOM_PREFIX}{}", "0".repeat(HASH_CHARS));
        assert!(store
            .set_workspace_icon(&ws.id, Some(&missing), None)
            .is_err());
    }

    #[test]
    fn only_small_pngs_are_kept() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(store.add_custom_icon(b"<svg/>").is_err());
        let mut big = png(0);
        big.resize(MAX_CUSTOM_ICON_BYTES + 1, 0);
        assert!(store.add_custom_icon(&big).is_err());
        assert!(store.custom_icons().unwrap().is_empty());
    }

    #[test]
    fn unused_images_are_removed_once_they_are_old() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = store.create_workspace(None, "Lorem").unwrap();
        let used = store.add_custom_icon(&png(1)).unwrap();
        let unused = store.add_custom_icon(&png(2)).unwrap();
        store.set_workspace_icon(&ws.id, Some(&used), None).unwrap();
        // 足したばかりの画像は残す
        assert_eq!(store.purge_custom_icons().unwrap(), 0);
        assert_eq!(purge(&store.conn, now_ms()).unwrap(), 1);
        assert_eq!(store.custom_icons().unwrap(), [used.clone()]);
        assert_eq!(store.custom_icon(&unused).unwrap(), None);

        // ワークスペースを消すと、その画像も使われなくなる
        store.delete_workspace(&ws.id).unwrap();
        assert_eq!(purge(&store.conn, now_ms()).unwrap(), 1);
        assert!(store.custom_icons().unwrap().is_empty());
    }

    #[test]
    fn export_and_import_carry_the_images() {
        let mut from = Store::open_in_memory().unwrap();
        let ws = from.create_workspace(None, "Lorem").unwrap();
        let name = from.add_custom_icon(&png(1)).unwrap();
        from.add_custom_icon(&png(2)).unwrap();
        from.set_workspace_icon(&ws.id, Some(&name), None).unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&from, &mut zip).unwrap();

        let mut to = Store::open_in_memory().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut to, &mut zip).unwrap();
        assert_eq!(to.workspace(&ws.id).unwrap().icon, Some(name.clone()));
        // 使っている画像だけが入る
        assert_eq!(to.custom_icons().unwrap(), [name.clone()]);
        assert_eq!(to.custom_icon(&name).unwrap(), Some(png(1)));
    }

    #[test]
    fn an_export_without_its_image_is_rejected() {
        let mut from = Store::open_in_memory().unwrap();
        let ws = from.create_workspace(None, "Lorem").unwrap();
        let name = from.add_custom_icon(&png(1)).unwrap();
        from.set_workspace_icon(&ws.id, Some(&name), None).unwrap();
        from.conn.execute("DELETE FROM custom_icons", []).unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&from, &mut zip).unwrap();

        let mut to = Store::open_in_memory().unwrap();
        zip.set_position(0);
        let error = crate::archive::import(&mut to, &mut zip).unwrap_err();
        assert!(error.to_string().contains("Lorem"), "{error}");
        assert!(to.workspaces().unwrap().is_empty());
    }
}
