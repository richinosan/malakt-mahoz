//! 添付ファイル。
//!
//! 中身はデータベースの中に、内容のハッシュごとに 1 つだけ置く（同じファイルを
//! 何度添付しても増えない）。圧縮が効くものは圧縮して置く。どの添付からも
//! 指されなくなった中身は、その場で消える。

use std::io::{Read, Write};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::model::*;
use crate::text::normalize_line;
use crate::{Error, Result, Store};

const COLUMNS: &str = "id, task_id, author_id, name, size, hash, created_at";

/// 内容の SHA-256 を 16 進で返す。
pub fn hash_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// パスや制御文字を落として、ファイル名だけにする。
pub fn clean_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = normalize_line(name)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name = name.trim().trim_matches('.').trim();
    if name.is_empty() {
        "file".to_owned()
    } else {
        name.to_owned()
    }
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: r.get(0)?,
        task_id: r.get(1)?,
        author_id: r.get(2)?,
        name: r.get(3)?,
        size: r.get::<_, i64>(4)? as u64,
        hash: r.get(5)?,
        created_at: r.get(6)?,
    })
}

/// 中身を置く。すでに同じ中身があれば何もしない。
pub(crate) fn put_blob(conn: &Connection, hash: &str, bytes: &[u8]) -> Result<()> {
    let exists: bool = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)")?
        .query_row(params![hash], |r| r.get(0))?;
    if exists {
        return Ok(());
    }
    // 画像や zip のように縮まないものは、そのまま置く
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    let packed = encoder.finish()?;
    let (compressed, data) = if packed.len() * 10 < bytes.len() * 9 {
        (true, packed.as_slice())
    } else {
        (false, bytes)
    };
    conn.prepare_cached("INSERT INTO blobs(hash, size, compressed, data) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![hash, bytes.len() as i64, compressed, data])?;
    Ok(())
}

pub(crate) fn insert(conn: &Connection, a: &Attachment) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO attachments(id, task_id, author_id, name, size, hash, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, author_id = excluded.author_id",
    )?
    .execute(params![
        a.id,
        a.task_id,
        a.author_id,
        a.name,
        a.size as i64,
        a.hash,
        a.created_at
    ])
}

impl Store {
    /// ファイルを添付する。1 ファイル 10 MiB、1 つのタスクに 500 件まで。
    pub fn add_attachment(
        &mut self,
        task_id: &str,
        author_id: Option<&str>,
        name: &str,
        bytes: &[u8],
    ) -> Result<Attachment> {
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Err(Error::Invalid(format!(
                "添付できるのは 1 ファイル {} MiB までです",
                MAX_ATTACHMENT_BYTES / 1024 / 1024
            )));
        }
        self.task(task_id)?;
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM attachments WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )?;
        if count >= MAX_ATTACHMENTS_PER_TASK {
            return Err(Error::Invalid(format!(
                "1 つのタスクに添付できるのは {MAX_ATTACHMENTS_PER_TASK} 件までです"
            )));
        }
        let attachment = Attachment {
            id: new_id(),
            task_id: task_id.to_owned(),
            author_id: author_id.map(str::to_owned),
            name: clean_name(name),
            size: bytes.len() as u64,
            hash: hash_of(bytes),
            created_at: now_ms(),
        };
        let tx = self.conn.transaction()?;
        put_blob(&tx, &attachment.hash, bytes)?;
        insert(&tx, &attachment)?;
        tx.commit()?;
        Ok(attachment)
    }

    /// 古い順。
    pub fn attachments(&self, task_id: &str) -> Result<Vec<Attachment>> {
        let sql =
            format!("SELECT {COLUMNS} FROM attachments WHERE task_id = ?1 ORDER BY created_at, id");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![task_id], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn attachment(&self, id: &str) -> Result<Attachment> {
        let sql = format!("SELECT {COLUMNS} FROM attachments WHERE id = ?1");
        self.conn
            .prepare_cached(&sql)?
            .query_row(params![id], from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("添付 {id}")))
    }

    /// 添付の中身。
    pub fn attachment_data(&self, id: &str) -> Result<Vec<u8>> {
        let attachment = self.attachment(id)?;
        self.blob(&attachment.hash)
    }

    /// ハッシュで中身を読む。
    pub(crate) fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        let (size, compressed, data): (i64, bool, Vec<u8>) = self
            .conn
            .prepare_cached("SELECT size, compressed, data FROM blobs WHERE hash = ?1")?
            .query_row(params![hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("添付の中身 {hash}")))?;
        if !compressed {
            return Ok(data);
        }
        let mut out = Vec::with_capacity(size as usize);
        // 記録より大きく膨らむデータは読まない
        DeflateDecoder::new(data.as_slice())
            .take(size as u64 + 1)
            .read_to_end(&mut out)?;
        if out.len() as i64 != size {
            return Err(Error::Invalid("添付の中身が壊れています".into()));
        }
        Ok(out)
    }

    /// 添付を消す。ほかに同じ中身の添付がなければ、中身も消える。
    pub fn delete_attachment(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM attachments WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound(format!("添付 {id}")));
        }
        Ok(())
    }

    /// すべての添付（書き出し用）。
    pub(crate) fn all_attachments(&self) -> Result<Vec<Attachment>> {
        let sql = format!("SELECT {COLUMNS} FROM attachments ORDER BY id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 添付の中身がディスクで使っている大きさ（圧縮後、重複なし）。
    pub(crate) fn blob_bytes(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(data)), 0) FROM blobs",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::{clean_name, hash_of};
    use crate::model::*;
    use crate::{Error, Store};

    fn setup() -> (Store, Task) {
        let mut s = Store::open_in_memory().unwrap();
        let task = s
            .create_task(
                None,
                &TaskDraft {
                    title: "資料を集める".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        (s, task)
    }

    /// 縮まないデータ（決まった擬似乱数）。
    fn noise(len: usize) -> Vec<u8> {
        let mut x: u32 = 0x1234_5678;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn names_are_reduced_to_a_safe_file_name() {
        assert_eq!(clean_name("C:\\Users\\me\\見積もり.pdf"), "見積もり.pdf");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name(" メモ\u{0007}.txt "), "メモ.txt");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name(""), "file");
    }

    #[test]
    fn hash_is_sha256_hex() {
        assert_eq!(
            hash_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn add_list_read_and_delete() {
        let (mut s, task) = setup();
        let text = "議事録。".repeat(1000).into_bytes();
        let a = s
            .add_attachment(&task.id, None, "dir/議事録.txt", &text)
            .unwrap();
        assert_eq!(a.name, "議事録.txt");
        assert_eq!(a.size, text.len() as u64);
        assert!(!a.is_image());
        let image = noise(4096);
        let b = s
            .add_attachment(&task.id, None, "写真.PNG", &image)
            .unwrap();
        assert!(b.is_image());
        assert_eq!(s.attachments(&task.id).unwrap(), [a.clone(), b.clone()]);
        // 圧縮したものも、そのまま置いたものも、同じ中身が返る
        assert_eq!(s.attachment_data(&a.id).unwrap(), text);
        assert_eq!(s.attachment_data(&b.id).unwrap(), image);
        s.delete_attachment(&a.id).unwrap();
        assert_eq!(s.attachments(&task.id).unwrap(), [b]);
        assert!(matches!(
            s.delete_attachment(&a.id),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(s.attachment_data(&a.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn compressible_files_take_less_space() {
        let (mut s, task) = setup();
        let text = "同じ行が続くログ\n".repeat(20_000).into_bytes();
        s.add_attachment(&task.id, None, "log.txt", &text).unwrap();
        let stored = s.blob_bytes().unwrap();
        assert!(stored * 20 < text.len() as u64, "{stored} / {}", text.len());
        // 縮まないものは、そのままの大きさで置く
        let image = noise(100_000);
        s.add_attachment(&task.id, None, "photo.jpg", &image)
            .unwrap();
        assert_eq!(s.blob_bytes().unwrap(), stored + image.len() as u64);
    }

    #[test]
    fn same_content_is_stored_once() {
        let (mut s, task) = setup();
        let other = s
            .create_task(
                None,
                &TaskDraft {
                    title: "別".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let bytes = noise(50_000);
        let a = s.add_attachment(&task.id, None, "a.bin", &bytes).unwrap();
        let once = s.blob_bytes().unwrap();
        let b = s.add_attachment(&task.id, None, "b.bin", &bytes).unwrap();
        let c = s.add_attachment(&other.id, None, "c.bin", &bytes).unwrap();
        assert_eq!(a.hash, c.hash);
        assert_eq!(s.blob_bytes().unwrap(), once);
        // 最後の 1 つを消すまで、中身は残る
        s.delete_attachment(&a.id).unwrap();
        s.delete_attachment(&b.id).unwrap();
        assert_eq!(s.blob_bytes().unwrap(), once);
        assert_eq!(s.attachment_data(&c.id).unwrap(), bytes);
        s.delete_attachment(&c.id).unwrap();
        assert_eq!(s.blob_bytes().unwrap(), 0);
    }

    #[test]
    fn deleting_the_task_or_workspace_frees_the_content() {
        let (mut s, task) = setup();
        s.add_attachment(&task.id, None, "a.bin", &noise(10_000))
            .unwrap();
        let ws = s.create_workspace(None, "イベント").unwrap();
        let inside = s
            .create_task(
                Some(&ws.id),
                &TaskDraft {
                    title: "中".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        s.add_attachment(&inside.id, None, "b.bin", &noise(20_000))
            .unwrap();
        assert_eq!(s.blob_bytes().unwrap(), 30_000);
        s.delete_task(&task.id).unwrap();
        assert_eq!(s.blob_bytes().unwrap(), 20_000);
        s.delete_workspace(&ws.id).unwrap();
        assert_eq!(s.blob_bytes().unwrap(), 0);
    }

    #[test]
    fn size_and_count_limits() {
        let (mut s, task) = setup();
        let limit = MAX_ATTACHMENT_BYTES as usize;
        let big = vec![0u8; limit + 1];
        assert!(matches!(
            s.add_attachment(&task.id, None, "big.bin", &big),
            Err(Error::Invalid(_))
        ));
        s.add_attachment(&task.id, None, "ok.bin", &big[..limit])
            .unwrap();
        // 空のファイルも添付できる
        let empty = s.add_attachment(&task.id, None, "empty.txt", b"").unwrap();
        assert_eq!(s.attachment_data(&empty.id).unwrap(), b"");
        for i in 2..MAX_ATTACHMENTS_PER_TASK {
            s.add_attachment(&task.id, None, &format!("{i}.txt"), b"x")
                .unwrap();
        }
        assert!(matches!(
            s.add_attachment(&task.id, None, "over.txt", b"x"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.add_attachment("ない", None, "a.txt", b"x"),
            Err(Error::NotFound(_))
        ));
    }

    /// zip の中身を書き換えた zip を作る。`None` を返したファイルは落とす。
    fn rewrite(zip: &[u8], f: impl Fn(&str, Vec<u8>) -> Option<Vec<u8>>) -> Vec<u8> {
        use std::io::{Cursor, Read, Write};
        let mut input = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..input.len() {
            let mut entry = input.by_index(i).unwrap();
            let name = entry.name().to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if let Some(bytes) = f(&name, bytes) {
                out.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                out.write_all(&bytes).unwrap();
            }
        }
        out.finish().unwrap().into_inner()
    }

    fn exported(s: &Store) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        crate::archive::export(s, &mut out).unwrap();
        out.into_inner()
    }

    #[test]
    fn export_and_import_carry_the_files() {
        let (mut s, task) = setup();
        let text = "議事録。".repeat(500).into_bytes();
        let image = noise(30_000);
        let a = s
            .add_attachment(&task.id, None, "議事録.txt", &text)
            .unwrap();
        let b = s
            .add_attachment(&task.id, None, "写真.png", &image)
            .unwrap();
        s.add_attachment(&task.id, None, "写真の控え.png", &image)
            .unwrap();
        let zip = exported(&s);
        // 同じ中身は zip にも 1 回だけ入る
        let names: Vec<String> = {
            let z = zip::ZipArchive::new(std::io::Cursor::new(&zip)).unwrap();
            z.file_names()
                .filter(|n| n.starts_with("files/"))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| n.is_ascii()));

        let mut other = Store::open_in_memory().unwrap();
        let manifest = crate::archive::import(&mut other, std::io::Cursor::new(&zip)).unwrap();
        assert_eq!(manifest.counts.attachments, 3);
        assert_eq!(
            other.attachments(&task.id).unwrap(),
            s.attachments(&task.id).unwrap()
        );
        assert_eq!(other.attachment_data(&a.id).unwrap(), text);
        assert_eq!(other.attachment_data(&b.id).unwrap(), image);
        // もう一度取り込んでも増えない
        crate::archive::import(&mut other, std::io::Cursor::new(&zip)).unwrap();
        assert_eq!(other.attachments(&task.id).unwrap().len(), 3);
        assert_eq!(other.blob_bytes().unwrap(), s.blob_bytes().unwrap());
    }

    #[test]
    fn import_rejects_broken_or_missing_files_and_changes_nothing() {
        let (mut s, task) = setup();
        s.add_attachment(&task.id, None, "a.bin", &noise(1000))
            .unwrap();
        let zip = exported(&s);
        let broken = rewrite(&zip, |name, mut bytes| {
            if name.starts_with("files/") {
                bytes[0] ^= 1;
            }
            Some(bytes)
        });
        let missing = rewrite(&zip, |name, bytes| {
            (!name.starts_with("files/")).then_some(bytes)
        });
        for bad in [broken, missing] {
            let mut other = Store::open_in_memory().unwrap();
            let result = crate::archive::import(&mut other, std::io::Cursor::new(&bad));
            assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
            assert!(other.task(&task.id).is_err());
            assert_eq!(other.blob_bytes().unwrap(), 0);
        }
        // ハッシュをパスのように書き換えたものは通さない
        let hash = s.attachments(&task.id).unwrap()[0].hash.clone();
        let evil = rewrite(&zip, |name, bytes| {
            if name == "data/attachments.json" {
                let text = String::from_utf8(bytes).unwrap();
                Some(text.replace(&hash, "../../manifest.json").into_bytes())
            } else {
                Some(bytes)
            }
        });
        let mut other = Store::open_in_memory().unwrap();
        assert!(matches!(
            crate::archive::import(&mut other, std::io::Cursor::new(&evil)),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn exports_made_before_attachments_still_import() {
        let (s, task) = setup();
        let old = rewrite(&exported(&s), |name, bytes| match name {
            "data/attachments.json" => None,
            "manifest.json" => {
                let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                v["counts"].as_object_mut().unwrap().remove("attachments");
                Some(serde_json::to_vec(&v).unwrap())
            }
            _ => Some(bytes),
        });
        let mut other = Store::open_in_memory().unwrap();
        let manifest = crate::archive::import(&mut other, std::io::Cursor::new(&old)).unwrap();
        assert_eq!(manifest.counts.attachments, 0);
        assert!(other.task(&task.id).is_ok());
    }
}
