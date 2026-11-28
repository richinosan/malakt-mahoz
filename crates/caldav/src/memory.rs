//! メモリの中のサーバー（テスト用）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{Changes, Object, Server, WriteError};

#[derive(Debug, Default)]
pub struct Memory {
    /// href → (ETag, ics)
    pub objects: BTreeMap<String, (String, String)>,
    /// 版の番号。変わるたびに 1 進む。
    version: u64,
    /// (版, href)。消したものも残す。
    log: Vec<(u64, String)>,
    /// 差分（sync-collection）を出せる。
    pub delta: bool,
    /// 前の印を受け付けない（印が古くなったサーバーのまね）。印なしで全部を返す。
    pub expired: bool,
    /// つながらない。
    pub offline: bool,
    /// 書き込めない。
    pub read_only: bool,
    /// 書いたときに ETag を返さない（中身を整え直すサーバーのまね）。
    pub no_etag: bool,
    /// 書いた中身をサーバーが整え直す。
    pub rewrite: Option<fn(&str) -> String>,
    /// 呼ばれた操作（`put /a.ics` など）。
    pub calls: Vec<String>,
}

impl Memory {
    pub fn new() -> Self {
        Memory {
            delta: true,
            ..Default::default()
        }
    }

    fn bump(&mut self, href: &str) -> String {
        self.version += 1;
        self.log.push((self.version, href.to_owned()));
        format!("\"{}\"", self.version)
    }

    /// サーバーの側で書く（ほかの人の変更のまね）。
    pub fn edit(&mut self, href: &str, ics: &str) {
        let etag = self.bump(href);
        self.objects.insert(href.to_owned(), (etag, ics.to_owned()));
    }

    /// サーバーの側で消す。
    pub fn remove(&mut self, href: &str) {
        self.objects.remove(href);
        self.bump(href);
    }

    pub fn ics(&self, href: &str) -> Option<&str> {
        self.objects.get(href).map(|(_, ics)| ics.as_str())
    }

    fn check(&self) -> Result<(), String> {
        if self.offline {
            Err("サーバーに接続できませんでした".into())
        } else {
            Ok(())
        }
    }
}

impl Server for Memory {
    fn changes(&mut self, calendar: &str, token: Option<&str>) -> Result<Changes, String> {
        self.check()?;
        self.calls.push(format!("changes {calendar}"));
        let inside = |h: &str| h.starts_with(calendar) && h != calendar;
        if !self.delta {
            return Ok(Changes::Full {
                all: self
                    .objects
                    .iter()
                    .filter(|(h, _)| inside(h))
                    .map(|(h, (e, _))| (h.clone(), e.clone()))
                    .collect(),
            });
        }
        if self.expired && token.is_some() {
            return Ok(Changes::Reset {
                token: self.version.to_string(),
                all: self
                    .objects
                    .iter()
                    .filter(|(h, _)| inside(h))
                    .map(|(h, (e, _))| (h.clone(), e.clone()))
                    .collect(),
            });
        }
        let since: u64 = token.and_then(|t| t.parse().ok()).unwrap_or(0);
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (_, href) in self.log.iter().rev().filter(|(v, _)| *v > since) {
            if !inside(href) || !seen.insert(href.clone()) {
                continue;
            }
            match self.objects.get(href) {
                Some((etag, _)) => changed.push((href.clone(), etag.clone())),
                None if since > 0 => removed.push(href.clone()),
                None => {}
            }
        }
        Ok(Changes::Delta {
            token: self.version.to_string(),
            changed,
            removed,
        })
    }

    fn fetch(&mut self, _calendar: &str, hrefs: &[String]) -> Result<Vec<Object>, String> {
        self.check()?;
        self.calls.push(format!("fetch {}", hrefs.len()));
        Ok(hrefs
            .iter()
            .filter_map(|h| {
                self.objects.get(h).map(|(etag, ics)| Object {
                    href: h.clone(),
                    etag: etag.clone(),
                    ics: ics.clone(),
                })
            })
            .collect())
    }

    fn get(&mut self, href: &str) -> Result<Option<Object>, String> {
        self.check()?;
        Ok(self.objects.get(href).map(|(etag, ics)| Object {
            href: href.to_owned(),
            etag: etag.clone(),
            ics: ics.clone(),
        }))
    }

    fn put(
        &mut self,
        href: &str,
        ics: &str,
        etag: Option<&str>,
    ) -> Result<Option<String>, WriteError> {
        self.check().map_err(WriteError::Other)?;
        self.calls.push(format!("put {href}"));
        if self.read_only {
            return Err(WriteError::Forbidden(String::new()));
        }
        match (self.objects.get(href), etag) {
            (Some(_), None) => return Err(WriteError::Conflict),
            (None, Some(_)) => return Err(WriteError::Gone),
            (Some((now, _)), Some(want)) if now != want => return Err(WriteError::Conflict),
            _ => {}
        }
        let stored = self.rewrite.map_or_else(|| ics.to_owned(), |f| f(ics));
        let new = self.bump(href);
        self.objects.insert(href.to_owned(), (new.clone(), stored));
        Ok((!self.no_etag).then_some(new))
    }

    fn delete(&mut self, href: &str) -> Result<(), WriteError> {
        self.check().map_err(WriteError::Other)?;
        self.calls.push(format!("delete {href}"));
        if self.read_only {
            return Err(WriteError::Forbidden(String::new()));
        }
        if self.objects.remove(href).is_none() {
            return Err(WriteError::Gone);
        }
        self.bump(href);
        Ok(())
    }
}

/// ほかのスレッドと分け合うメモリのサーバー（アプリのテスト用）。
#[derive(Debug, Clone, Default)]
pub struct Shared(pub Arc<Mutex<Memory>>);

impl Shared {
    pub fn new(memory: Memory) -> Self {
        Shared(Arc::new(Mutex::new(memory)))
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Server for Shared {
    fn changes(&mut self, calendar: &str, token: Option<&str>) -> Result<Changes, String> {
        self.lock().changes(calendar, token)
    }

    fn fetch(&mut self, calendar: &str, hrefs: &[String]) -> Result<Vec<Object>, String> {
        self.lock().fetch(calendar, hrefs)
    }

    fn get(&mut self, href: &str) -> Result<Option<Object>, String> {
        self.lock().get(href)
    }

    fn put(
        &mut self,
        href: &str,
        ics: &str,
        etag: Option<&str>,
    ) -> Result<Option<String>, WriteError> {
        self.lock().put(href, ics, etag)
    }

    fn delete(&mut self, href: &str) -> Result<(), WriteError> {
        self.lock().delete(href)
    }
}
