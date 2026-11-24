//! zip へのエクスポートと、zip からのインポート。
//!
//! OS が違っても同じ結果になるように、次を守る。
//!
//! - zip の中のファイル名は ASCII だけ。タイトルなどの利用者の文字列をファイル名にしない
//!   （文字コードの違い、Windows の予約名、パス区切りの違いを避ける）
//! - 中身は UTF-8（BOM なし）の JSON。文字列は NFC、改行は LF
//! - 読むときは BOM・CRLF・NFD が混ざっていても受け入れ、正規化して取り込む

use std::io::{Read, Seek, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::model::*;
use crate::text::{decode_utf8, normalize, normalize_line};
use crate::{Error, Result, Store};

pub const FORMAT: &str = "malakt-mahoz";
pub const FORMAT_VERSION: u32 = 1;

/// 1 ファイルあたりの上限。壊れた zip や zip 爆弾でメモリを使い切らないため。
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    pub app_version: String,
    pub exported_at: i64,
    pub counts: Counts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub workspaces: usize,
    pub people: usize,
    pub tasks: usize,
    pub comments: usize,
    pub changes: usize,
    /// 古い書き出しにはない。
    #[serde(default)]
    pub attachments: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Data {
    pub workspaces: Vec<Workspace>,
    pub people: Vec<Person>,
    pub tasks: Vec<Task>,
    pub comments: Vec<Comment>,
    pub changes: Vec<Change>,
    /// 添付の名前や大きさ。中身は zip の `files/<ハッシュ>` に 1 つずつ入る。
    pub attachments: Vec<Attachment>,
    pub task_defaults: Vec<TaskDefaults>,
}

impl Data {
    pub fn counts(&self) -> Counts {
        Counts {
            workspaces: self.workspaces.len(),
            people: self.people.len(),
            tasks: self.tasks.len(),
            comments: self.comments.len(),
            changes: self.changes.len(),
            attachments: self.attachments.len(),
        }
    }

    fn normalize(&mut self) {
        for w in &mut self.workspaces {
            w.name = normalize_line(&w.name);
        }
        for p in &mut self.people {
            p.name = normalize_line(&p.name);
            p.note = normalize(&p.note);
        }
        for t in &mut self.tasks {
            t.title = normalize_line(&t.title);
            t.description = normalize(&t.description);
            t.assignee_ids.sort_unstable();
            t.assignee_ids.dedup();
            for c in &mut t.categories {
                *c = normalize_line(c);
            }
            t.categories.retain(|c| !c.is_empty());
            t.categories.sort_unstable();
            t.categories.dedup();
        }
        for c in &mut self.comments {
            c.body = normalize(&c.body);
        }
        for a in &mut self.attachments {
            a.name = crate::attachment::clean_name(&a.name);
        }
        for c in &mut self.changes {
            c.old_value = normalize(&c.old_value);
            c.new_value = normalize(&c.new_value);
        }
    }

    fn validate(&self) -> Result<()> {
        let bad = |what: &str| Err(Error::Invalid(format!("{what}が正しくありません")));
        if self.workspaces.iter().any(|w| {
            w.name.is_empty() || w.id.is_empty() || !w.date.as_deref().is_none_or(is_valid_date)
        }) {
            return bad("ワークスペース");
        }
        if self
            .people
            .iter()
            .any(|p| p.name.is_empty() || p.id.is_empty())
        {
            return bad("担当者");
        }
        for t in &self.tasks {
            let due_ok = t.due.as_deref().is_none_or(is_valid_when);
            let start_ok = t.start.as_deref().is_none_or(is_valid_when);
            if t.title.is_empty()
                || t.id.is_empty()
                || t.priority > 9
                || t.percent > 100
                || !due_ok
                || !start_ok
                || t.parent_id.as_deref() == Some(t.id.as_str())
            {
                return bad("タスク");
            }
        }
        // ハッシュは zip の中のファイル名になるので、16 進 64 桁だけを通す
        if self.attachments.iter().any(|a| {
            a.id.is_empty()
                || a.size > MAX_ATTACHMENT_BYTES
                || a.hash.len() != 64
                || !a
                    .hash
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        }) {
            return bad("添付");
        }
        Ok(())
    }
}

fn file_entry(hash: &str) -> String {
    format!("files/{hash}")
}

/// すべてのデータを zip に書き出す。
pub fn export<W: Write + Seek>(store: &Store, out: W) -> Result<Manifest> {
    let data = store.dump()?;
    let manifest = Manifest {
        format: FORMAT.to_owned(),
        format_version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        exported_at: now_ms(),
        counts: data.counts(),
    };
    let mut zip = ZipWriter::new(out);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut put = |name: &str, bytes: Vec<u8>| -> Result<()> {
        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
        Ok(())
    };
    put("manifest.json", serde_json::to_vec_pretty(&manifest)?)?;
    put(
        "data/workspaces.json",
        serde_json::to_vec(&data.workspaces)?,
    )?;
    put("data/people.json", serde_json::to_vec(&data.people)?)?;
    put("data/tasks.json", serde_json::to_vec(&data.tasks)?)?;
    put("data/comments.json", serde_json::to_vec(&data.comments)?)?;
    put("data/changes.json", serde_json::to_vec(&data.changes)?)?;
    put(
        "data/task_defaults.json",
        serde_json::to_vec(&data.task_defaults)?,
    )?;
    put(
        "data/attachments.json",
        serde_json::to_vec(&data.attachments)?,
    )?;
    // 中身は 1 つずつ読んで書く（全部を一度にメモリへ載せない）。同じ中身は 1 回だけ
    let mut written = std::collections::HashSet::new();
    for a in &data.attachments {
        if written.insert(a.hash.as_str()) {
            put(&file_entry(&a.hash), store.blob(&a.hash)?)?;
        }
    }
    zip.finish()?.flush()?;
    Ok(manifest)
}

/// ファイルに書き出す。途中で失敗しても壊れたファイルを残さないよう、別名で書いてから置き換える。
pub fn export_to_path(store: &Store, path: impl AsRef<Path>) -> Result<Manifest> {
    let path = path.as_ref();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = std::path::PathBuf::from(tmp);
    let result = (|| {
        let file = std::fs::File::create(&tmp)?;
        let manifest = export(store, std::io::BufWriter::new(file))?;
        std::fs::rename(&tmp, path)?;
        Ok(manifest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// zip を読んで取り込む。ID が同じものは上書きし、ないものは追加する。
/// 途中で失敗したら何も変えない。
pub fn import<R: Read + Seek>(store: &mut Store, input: R) -> Result<Manifest> {
    let mut zip = ZipArchive::new(input)?;
    let (manifest, data) = read_zip(&mut zip)?;
    // 「自分」がまだいないところへの取り込みは復元なので、割り当ての通知は作らない
    let had_self = store.people()?.iter().any(|p| p.is_self);
    let before = store.open_tasks_of_self()?;
    store.merge(&data, &mut |hash| {
        read_entry(&mut zip, &file_entry(hash), MAX_ATTACHMENT_BYTES)
    })?;
    if had_self {
        store.notify_newly_assigned(&before)?;
    }
    Ok(manifest)
}

pub fn import_from_path(store: &mut Store, path: impl AsRef<Path>) -> Result<Manifest> {
    import(store, std::io::BufReader::new(std::fs::File::open(path)?))
}

/// zip を読んで確かめるだけで、取り込まない。
pub fn read<R: Read + Seek>(input: R) -> Result<(Manifest, Data)> {
    read_zip(&mut ZipArchive::new(input)?)
}

fn read_zip<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<(Manifest, Data)> {
    let manifest: Manifest = read_json(zip, "manifest.json")?;
    if manifest.format != FORMAT {
        return Err(Error::Invalid(
            "malakt-mahoz のエクスポートではありません".into(),
        ));
    }
    if manifest.format_version != FORMAT_VERSION {
        return Err(Error::UnsupportedFormat(manifest.format_version));
    }
    // あとから足した種類のデータは、古い書き出しにはない
    let attachments = read_optional(zip, "data/attachments.json")?;
    let task_defaults: Vec<TaskDefaults> = read_optional(zip, "data/task_defaults.json")?;
    let task_defaults = task_defaults
        .iter()
        .map(crate::defaults::clean)
        .collect::<Result<_>>()?;
    let mut data = Data {
        workspaces: read_json(zip, "data/workspaces.json")?,
        people: read_json(zip, "data/people.json")?,
        tasks: read_json(zip, "data/tasks.json")?,
        comments: read_json(zip, "data/comments.json")?,
        changes: read_json(zip, "data/changes.json")?,
        attachments,
        task_defaults,
    };
    data.normalize();
    data.validate()?;
    for a in &data.attachments {
        if zip.index_for_name(&file_entry(&a.hash)).is_none() {
            return Err(Error::Invalid(format!(
                "添付「{}」の中身がありません",
                a.name
            )));
        }
    }
    Ok((manifest, data))
}

fn read_optional<R: Read + Seek, T: for<'de> Deserialize<'de>>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> Result<Vec<T>> {
    if zip.index_for_name(name).is_some() {
        read_json(zip, name)
    } else {
        Ok(Vec::new())
    }
}

fn read_json<R: Read + Seek, T: for<'de> Deserialize<'de>>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> Result<T> {
    let bytes = read_entry(zip, name, MAX_ENTRY_BYTES)?;
    Ok(serde_json::from_str(decode_utf8(&bytes)?)?)
}

fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let entry = zip.by_name(name).map_err(|e| match e {
        zip::result::ZipError::FileNotFound => Error::Invalid(format!("{name} がありません")),
        e => Error::Zip(e),
    })?;
    if entry.size() > limit {
        return Err(Error::Invalid(format!("{name} が大きすぎます")));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    // 申告されたサイズが嘘でも、上限までしか読まない。
    entry.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Invalid(format!("{name} が大きすぎます")));
    }
    Ok(bytes)
}
