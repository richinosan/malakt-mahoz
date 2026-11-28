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
    /// 自分で足したアイコンの画像（名前と PNG）。zip の `icons/<名前>.png` に 1 つずつ入る。
    pub icons: Vec<(String, Vec<u8>)>,
    pub workspaces: Vec<Workspace>,
    pub people: Vec<Person>,
    pub tasks: Vec<Task>,
    pub comments: Vec<Comment>,
    pub changes: Vec<Change>,
    /// 添付の名前や大きさ。中身は zip の `files/<ハッシュ>` に 1 つずつ入る。
    pub attachments: Vec<Attachment>,
    pub task_defaults: Vec<TaskDefaults>,
    /// タスクごとの、期限をいつ知らせるかの上書き。
    pub task_alerts: Vec<crate::alert::TaskAlerts>,
    /// AI のルール（グローバルとワークスペースごと）。
    pub ai_rules: Vec<crate::rules::AiRule>,
    /// タスクに付けた繰り返し。
    pub repeats: Vec<crate::repeat::Repeat>,
    /// 定時タスク。
    pub schedules: Vec<crate::schedule::Schedule>,
    /// 定時タスクの雛形の添付。中身は添付と同じく zip の `files/<ハッシュ>` に入る。
    pub schedule_files: Vec<crate::schedule::ScheduleFile>,
    /// ポモドーロの作業の記録。
    pub work_logs: Vec<crate::pomodoro::WorkLog>,
    /// ほかのツールから取り込んだタスクの元（同じものを二度取り込まないため）。
    pub import_origins: Vec<crate::intake::Origin>,
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
            p.workspace_ids.sort_unstable();
            p.workspace_ids.dedup();
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
            // URL は足した順のまま。同じものは最初の 1 つだけ
            let mut urls: Vec<String> = Vec::new();
            for url in &t.urls {
                let url = url.trim();
                if !url.is_empty() && !urls.iter().any(|u| u == url) {
                    urls.push(url.to_owned());
                }
            }
            t.urls = urls;
        }
        for c in &mut self.comments {
            c.body = normalize(&c.body);
        }
        for a in &mut self.attachments {
            a.name = crate::attachment::clean_name(&a.name);
        }
        for f in &mut self.schedule_files {
            f.name = crate::attachment::clean_name(&f.name);
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
            use crate::weblink::{MAX_URLS, MAX_URL_CHARS};
            if t.urls.len() > MAX_URLS {
                return Err(Error::Invalid(format!(
                    "タスク「{}」の URL が {} 個あります（{MAX_URLS} 個まで）",
                    t.title,
                    t.urls.len()
                )));
            }
            if t.urls.iter().any(|u| u.chars().count() > MAX_URL_CHARS) {
                return Err(Error::Invalid(format!(
                    "タスク「{}」に {MAX_URL_CHARS} 文字より長い URL があります",
                    t.title
                )));
            }
        }
        if let Some(r) = self.ai_rules.iter().find(|r| {
            r.workspace_id
                .as_ref()
                .is_some_and(|w| !self.workspaces.iter().any(|x| &x.id == w))
        }) {
            return Err(Error::Invalid(format!(
                "AI のルール「{}」のワークスペースがありません",
                r.name
            )));
        }
        if let Some(r) = self
            .repeats
            .iter()
            .find(|r| !self.tasks.iter().any(|t| t.id == r.task_id))
        {
            return Err(Error::Invalid(format!(
                "繰り返しの、タスク {} がありません",
                r.task_id
            )));
        }
        if let Some(s) = self.schedules.iter().find(|s| {
            s.workspace_id
                .as_ref()
                .is_some_and(|w| !self.workspaces.iter().any(|x| &x.id == w))
        }) {
            return Err(Error::Invalid(format!(
                "定時タスク「{}」のワークスペースがありません",
                s.title
            )));
        }
        if let Some(a) = self
            .task_alerts
            .iter()
            .find(|a| !self.tasks.iter().any(|t| t.id == a.task_id))
        {
            return Err(Error::Invalid(format!(
                "期限の通知の設定の、タスク {} がありません",
                a.task_id
            )));
        }
        // ハッシュは zip の中のファイル名になるので、16 進 64 桁だけを通す
        let bad_file = |id: &str, size: u64, hash: &str| {
            id.is_empty()
                || size > MAX_ATTACHMENT_BYTES
                || hash.len() != 64
                || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        if self
            .attachments
            .iter()
            .any(|a| bad_file(&a.id, a.size, &a.hash))
        {
            return bad("添付");
        }
        if self
            .schedule_files
            .iter()
            .any(|f| bad_file(&f.id, f.size, &f.hash))
        {
            return bad("定時タスクの添付");
        }
        if let Some(l) = self.work_logs.iter().find(|l| {
            l.task_id
                .as_ref()
                .is_some_and(|t| !self.tasks.iter().any(|x| &x.id == t))
        }) {
            return Err(Error::Invalid(format!(
                "作業の記録の、タスク {} がありません",
                l.task_id.as_deref().unwrap_or_default()
            )));
        }
        if let Some(f) = self
            .schedule_files
            .iter()
            .find(|f| !self.schedules.iter().any(|s| s.id == f.schedule_id))
        {
            return Err(Error::Invalid(format!(
                "定時タスクの添付「{}」の、定時タスクがありません",
                f.name
            )));
        }
        Ok(())
    }
}

fn icon_entry(name: &str) -> String {
    format!("icons/{name}.png")
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
        "data/task_alerts.json",
        serde_json::to_vec(&data.task_alerts)?,
    )?;
    put(
        "data/attachments.json",
        serde_json::to_vec(&data.attachments)?,
    )?;
    put("data/ai_rules.json", serde_json::to_vec(&data.ai_rules)?)?;
    put("data/repeats.json", serde_json::to_vec(&data.repeats)?)?;
    put("data/schedules.json", serde_json::to_vec(&data.schedules)?)?;
    put(
        "data/schedule_files.json",
        serde_json::to_vec(&data.schedule_files)?,
    )?;
    put("data/work_logs.json", serde_json::to_vec(&data.work_logs)?)?;
    put(
        "data/import_origins.json",
        serde_json::to_vec(&data.import_origins)?,
    )?;
    for (name, png) in &data.icons {
        put(&icon_entry(name), png.clone())?;
    }
    // 中身は 1 つずつ読んで書く（全部を一度にメモリへ載せない）。同じ中身は 1 回だけ
    let mut written = std::collections::HashSet::new();
    let hashes = data
        .attachments
        .iter()
        .map(|a| a.hash.as_str())
        .chain(data.schedule_files.iter().map(|f| f.hash.as_str()));
    for hash in hashes {
        if written.insert(hash) {
            put(&file_entry(hash), store.blob(hash)?)?;
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

/// このアプリのエクスポートの zip か（中に `manifest.json` がある）。
/// ほかのツールの zip（Notion のエクスポートなど）と見分けるのに使う。
pub fn is_export(path: impl AsRef<Path>) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    ZipArchive::new(std::io::BufReader::new(file))
        .is_ok_and(|zip| zip.index_for_name("manifest.json").is_some())
}

/// zip を読んで確かめるだけで、取り込まない。
pub fn read<R: Read + Seek>(input: R) -> Result<(Manifest, Data)> {
    read_zip(&mut ZipArchive::new(input)?)
}

fn read_zip<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<(Manifest, Data)> {
    let manifest: Manifest = read_json(zip, "manifest.json")?;
    if manifest.format != FORMAT {
        return Err(Error::Invalid(
            "Malakt Mahoz のエクスポートではありません".into(),
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
    let task_alerts: Vec<crate::alert::TaskAlerts> = read_optional(zip, "data/task_alerts.json")?;
    let task_alerts = task_alerts
        .iter()
        .map(crate::alert::clean_task)
        .collect::<Result<_>>()?;
    let ai_rules: Vec<crate::rules::AiRule> = read_optional(zip, "data/ai_rules.json")?;
    let ai_rules = ai_rules
        .iter()
        .map(crate::rules::clean)
        .collect::<Result<_>>()?;
    let repeats: Vec<crate::repeat::Repeat> = read_optional(zip, "data/repeats.json")?;
    let repeats = repeats
        .iter()
        .map(crate::repeat::clean_repeat)
        .collect::<Result<_>>()?;
    let schedules: Vec<crate::schedule::Schedule> = read_optional(zip, "data/schedules.json")?;
    let schedules = schedules
        .iter()
        .map(crate::schedule::clean)
        .collect::<Result<_>>()?;
    let schedule_files = read_optional(zip, "data/schedule_files.json")?;
    let work_logs: Vec<crate::pomodoro::WorkLog> = read_optional(zip, "data/work_logs.json")?;
    let work_logs = work_logs
        .iter()
        .map(crate::pomodoro::clean_log)
        .collect::<Result<_>>()?;
    let import_origins: Vec<crate::intake::Origin> =
        read_optional(zip, "data/import_origins.json")?;
    let workspaces: Vec<Workspace> = read_json(zip, "data/workspaces.json")?;
    let mut icons: Vec<(String, Vec<u8>)> = Vec::new();
    for w in &workspaces {
        let Some(icon) = w.icon.as_deref().filter(|i| crate::icon::is_custom(i)) else {
            continue;
        };
        if icons.iter().any(|(name, _)| name == icon) {
            continue;
        }
        let entry = icon_entry(icon);
        if zip.index_for_name(&entry).is_none() {
            return Err(Error::Invalid(format!(
                "ワークスペース「{}」のアイコンの画像がありません",
                w.name
            )));
        }
        let limit = crate::icon::MAX_CUSTOM_ICON_BYTES as u64;
        icons.push((icon.to_owned(), read_entry(zip, &entry, limit)?));
    }
    let mut data = Data {
        icons,
        workspaces,
        people: read_json(zip, "data/people.json")?,
        tasks: read_json(zip, "data/tasks.json")?,
        comments: read_json(zip, "data/comments.json")?,
        changes: read_json(zip, "data/changes.json")?,
        attachments,
        task_defaults,
        task_alerts,
        ai_rules,
        repeats,
        schedules,
        schedule_files,
        work_logs,
        import_origins,
    };
    data.normalize();
    data.validate()?;
    let files = data
        .attachments
        .iter()
        .map(|a| (a.name.as_str(), a.hash.as_str()))
        .chain(
            data.schedule_files
                .iter()
                .map(|f| (f.name.as_str(), f.hash.as_str())),
        );
    for (name, hash) in files {
        if zip.index_for_name(&file_entry(hash)).is_none() {
            return Err(Error::Invalid(format!("添付「{name}」の中身がありません")));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskDraft;

    /// 作業の場所・担当者・タスク・ルール・繰り返し・定時タスク・添付・期限の通知を 1 つずつ持つデータ。
    fn base() -> Data {
        let mut s = Store::open_in_memory().unwrap();
        let ws = s.create_workspace(None, "場所").unwrap();
        s.create_person("担当", false).unwrap();
        let t = s
            .create_task(
                Some(&ws.id),
                &TaskDraft {
                    title: "タスク".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        s.create_ai_rule(Some(&ws.id), "規則", "本文").unwrap();
        s.set_repeat(
            &t.id,
            Some(&crate::repeat::Rule::weekly("2026-11-30")),
            "2026-11-27 12:00",
        )
        .unwrap();
        let schedule = s
            .create_schedule(Some(&ws.id), "定時", None, "2026-11-27 12:00")
            .unwrap();
        s.add_schedule_file(&schedule.id, "b.txt", b"def").unwrap();
        s.add_attachment(&t.id, None, "a.txt", b"abc").unwrap();
        s.set_task_alerts(&crate::alert::TaskAlerts {
            task_id: t.id.clone(),
            timed: Some(vec![5]),
            date: None,
        })
        .unwrap();
        let data = s.dump().unwrap();
        assert!(data.validate().is_ok());
        assert_eq!(data.ai_rules.len(), 1);
        assert_eq!(data.repeats.len(), 1);
        assert_eq!(data.schedules.len(), 1);
        assert_eq!(data.schedule_files.len(), 1);
        assert_eq!(data.attachments.len(), 1);
        assert_eq!(data.task_alerts.len(), 1);
        data
    }

    /// 変えたデータが正しくないと断られること。
    fn rejects(edit: impl FnOnce(&mut Data)) -> bool {
        let mut data = base();
        edit(&mut data);
        data.validate().is_err()
    }

    #[test]
    fn 読む大きさの上限と中の名前() {
        assert_eq!(MAX_ENTRY_BYTES, 268_435_456);
        assert_eq!(icon_entry("custom-abc"), "icons/custom-abc.png");
        assert_eq!(file_entry("abc"), "files/abc");
    }

    #[test]
    fn 取り込むときカテゴリとリンクの空と重なりをなくす() {
        let mut data = base();
        data.tasks[0].categories = vec!["b".into(), " ".into(), "a".into(), "a".into()];
        data.tasks[0].urls = vec![
            " https://a ".into(),
            "".into(),
            "https://b".into(),
            "https://a".into(),
        ];
        data.normalize();
        assert_eq!(data.tasks[0].categories, ["a", "b"]);
        assert_eq!(data.tasks[0].urls, ["https://a", "https://b"]);
    }

    #[test]
    fn ワークスペースと担当者は名前と識別子と日付を確かめる() {
        assert!(rejects(|d| d.workspaces[0].name.clear()));
        assert!(rejects(|d| d.workspaces[0].id.clear()));
        assert!(rejects(|d| d.workspaces[0].date = Some("2026-13-01".into())));
        assert!(!rejects(
            |d| d.workspaces[0].date = Some("2026-12-01".into())
        ));
        assert!(rejects(|d| d.people[0].name.clear()));
        assert!(rejects(|d| d.people[0].id.clear()));
    }

    #[test]
    fn タスクは項目の範囲を確かめる() {
        assert!(rejects(|d| d.tasks[0].title.clear()));
        assert!(rejects(|d| d.tasks[0].id.clear()));
        assert!(rejects(|d| d.tasks[0].priority = 10));
        assert!(!rejects(|d| d.tasks[0].priority = 9));
        assert!(rejects(|d| d.tasks[0].percent = 101));
        assert!(!rejects(|d| d.tasks[0].percent = 100));
        assert!(rejects(|d| d.tasks[0].due = Some("あした".into())));
        assert!(!rejects(
            |d| d.tasks[0].due = Some("2026-12-01 10:00".into())
        ));
        assert!(rejects(|d| d.tasks[0].start = Some("あした".into())));
        assert!(!rejects(|d| d.tasks[0].start = Some("2026-12-01".into())));
        assert!(rejects(
            |d| d.tasks[0].parent_id = Some(d.tasks[0].id.clone())
        ));
        assert!(!rejects(
            |d| d.tasks[0].parent_id = Some("別のタスク".into())
        ));
    }

    #[test]
    fn リンクの数と長さは上限ちょうどまで入る() {
        use crate::weblink::{MAX_URLS, MAX_URL_CHARS};
        assert!(!rejects(|d| {
            d.tasks[0].urls = (0..MAX_URLS).map(|n| format!("https://e.x/{n}")).collect();
        }));
        assert!(rejects(|d| {
            d.tasks[0].urls = (0..=MAX_URLS).map(|n| format!("https://e.x/{n}")).collect();
        }));
        let url = |n: usize| format!("https://e.x/{}", "a".repeat(n - "https://e.x/".len()));
        assert!(!rejects(|d| d.tasks[0].urls = vec![url(MAX_URL_CHARS)]));
        assert!(rejects(|d| d.tasks[0].urls = vec![url(MAX_URL_CHARS + 1)]));
    }

    #[test]
    fn ほかのデータが指す先がなければ断る() {
        assert!(rejects(|d| d.ai_rules[0].workspace_id = Some("ない".into())));
        assert!(!rejects(|d| d.ai_rules[0].workspace_id = None));
        assert!(rejects(|d| d.repeats[0].task_id = "ない".into()));
        assert!(rejects(
            |d| d.schedules[0].workspace_id = Some("ない".into())
        ));
        assert!(!rejects(|d| d.schedules[0].workspace_id = None));
        assert!(rejects(|d| d.task_alerts[0].task_id = "ない".into()));
        assert!(rejects(|d| d.schedule_files[0].schedule_id = "ない".into()));
    }

    #[test]
    fn 定時タスクの添付も識別子と大きさとハッシュを確かめる() {
        assert!(rejects(|d| d.schedule_files[0].id.clear()));
        assert!(!rejects(|d| d.schedule_files[0].size = MAX_ATTACHMENT_BYTES));
        assert!(rejects(
            |d| d.schedule_files[0].size = MAX_ATTACHMENT_BYTES + 1
        ));
        assert!(rejects(|d| d.schedule_files[0].hash = "g".repeat(64)));
        assert!(rejects(|d| d.schedule_files[0].hash.push('0')));
    }

    #[test]
    fn 定時タスクの添付の中身がない書き出しは断る() {
        let mut s = Store::open_in_memory().unwrap();
        let made = s
            .create_schedule(None, "定時", None, "2026-11-27 12:00")
            .unwrap();
        let file = s.add_schedule_file(&made.id, "b.txt", b"def").unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        export(&s, &mut zip).unwrap();
        // 中身のファイルを抜いた zip を作り直す
        zip.set_position(0);
        let mut from = ZipArchive::new(zip).unwrap();
        let mut out = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for i in 0..from.len() {
            let entry = from.by_index(i).unwrap();
            if entry.name() == file_entry(&file.hash) {
                continue;
            }
            out.raw_copy_file(entry).unwrap();
        }
        let mut zip = out.finish().unwrap();
        zip.set_position(0);
        let mut other = Store::open_in_memory().unwrap();
        let error = import(&mut other, zip).unwrap_err();
        assert!(error.to_string().contains("b.txt"), "{error}");
        assert!(other.all_schedules().unwrap().is_empty());
    }

    #[test]
    fn 添付は識別子と大きさとハッシュを確かめる() {
        assert!(rejects(|d| d.attachments[0].id.clear()));
        assert!(!rejects(|d| d.attachments[0].size = MAX_ATTACHMENT_BYTES));
        assert!(rejects(|d| d.attachments[0].size = MAX_ATTACHMENT_BYTES + 1));
        assert!(rejects(|d| d.attachments[0].hash.push('0')));
        assert!(rejects(|d| {
            d.attachments[0].hash.pop();
        }));
        assert!(rejects(|d| d.attachments[0].hash = "g".repeat(64)));
        assert!(rejects(|d| d.attachments[0].hash = "A".repeat(64)));
        assert!(!rejects(|d| d.attachments[0].hash = "a".repeat(64)));
    }

    fn zip_with(name: &str, bytes: &[u8]) -> ZipArchive<std::io::Cursor<Vec<u8>>> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(&mut out);
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
        zip.finish().unwrap();
        out.set_position(0);
        ZipArchive::new(out).unwrap()
    }

    #[test]
    fn 上限ちょうどの大きさのファイルは読めて超えると断る() {
        let mut zip = zip_with("a.bin", &[7u8; 10]);
        assert_eq!(read_entry(&mut zip, "a.bin", 10).unwrap(), vec![7u8; 10]);
        assert!(read_entry(&mut zip, "a.bin", 9).is_err());
        assert!(read_entry(&mut zip, "ない.bin", 10).is_err());
    }

    #[test]
    fn 同じアイコンを使う複数のワークスペースでも画像は一度だけ読む() {
        let mut s = Store::open_in_memory().unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend([3u8; 16]);
        let icon = s.add_custom_icon(&png).unwrap();
        for name in ["甲", "乙"] {
            let w = s.create_workspace(None, name).unwrap();
            s.set_workspace_icon(&w.id, Some(&icon), None).unwrap();
        }
        let mut out = std::io::Cursor::new(Vec::new());
        export(&s, &mut out).unwrap();
        out.set_position(0);
        let (_, data) = read(out).unwrap();
        assert_eq!(data.icons.len(), 1);
        assert_eq!(data.icons[0].0, icon);
    }
}
