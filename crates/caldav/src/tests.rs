//! 同期の判断のテスト。アプリの側はメモリの中の表で代わりをする。

use std::collections::BTreeMap;

use crate::memory::Memory;
use crate::*;

const CAL: &str = "/dav/cal/";

fn zone() -> TimeZone {
    TimeZone::fixed(jiff::tz::offset(9))
}

fn no_normalize(_: &mut Fields) {}

/// アプリの代わり。タスクは key → 項目。
#[derive(Default)]
struct App {
    tasks: BTreeMap<String, Fields>,
    items: Vec<Item>,
    token: Option<String>,
    read_only: bool,
    notices: Vec<String>,
    errors: Vec<String>,
    next: u32,
}

impl App {
    fn add(&mut self, title: &str) -> String {
        self.next += 1;
        let key = format!("t{}", self.next);
        let mut f = Fields::empty();
        f.summary = title.into();
        self.tasks.insert(key.clone(), f);
        key
    }

    fn input(&self) -> Input {
        Input {
            calendars: vec![Calendar {
                id: "c1".into(),
                href: CAL.into(),
                name: "共有".into(),
                token: self.token.clone(),
                read_only: self.read_only,
            }],
            items: self.items.clone(),
            locals: self
                .tasks
                .iter()
                .map(|(k, f)| Local {
                    key: k.clone(),
                    calendar: "c1".into(),
                    uid: self
                        .items
                        .iter()
                        .find(|i| i.key.as_deref() == Some(k))
                        .map_or_else(|| format!("uid-{k}"), |i| i.uid.clone()),
                    fields: f.clone(),
                })
                .collect(),
            zone: zone(),
            now: "2026-11-28T04:00:00Z".parse().unwrap(),
            normalize: no_normalize,
        }
    }

    fn sync(&mut self, server: &mut Memory) -> Outcome {
        let out = crate::sync(server, &self.input());
        self.apply(&out);
        out
    }

    fn apply(&mut self, out: &Outcome) {
        for (id, token) in &out.tokens {
            assert_eq!(id, "c1");
            self.token = token.clone();
        }
        self.items = out.items.clone();
        for item in &mut self.items {
            if item.key.is_none() {
                self.next += 1;
                let key = format!("t{}", self.next);
                self.tasks.insert(key.clone(), item.base.clone());
                item.key = Some(key);
            }
        }
        for u in &out.updates {
            if self.tasks.get(&u.key) == Some(&u.expected) {
                self.tasks.insert(u.key.clone(), u.fields.clone());
            }
        }
        for d in &out.deletes {
            self.tasks.remove(&d.key);
        }
        self.notices.extend(out.notices.clone());
        self.errors.extend(out.errors.clone());
    }

    fn task(&self, title: &str) -> (&String, &Fields) {
        self.tasks
            .iter()
            .find(|(_, f)| f.summary == title)
            .unwrap_or_else(|| panic!("{title} がない"))
    }
}

fn remote_ics(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Other//EN\r\nBEGIN:VTODO\r\nUID:{uid}\r\nSUMMARY:{summary}\r\nSTATUS:NEEDS-ACTION\r\nX-KEEP:1\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
    )
}

fn puts(server: &Memory) -> usize {
    server.calls.iter().filter(|c| c.starts_with("put")).count()
}

#[test]
fn local_tasks_are_sent_once() {
    let mut server = Memory::new();
    let mut app = App::default();
    let key = app.add("会場を押さえる");
    app.sync(&mut server);
    let href = format!("{CAL}uid-{key}.ics");
    assert!(server
        .ics(&href)
        .unwrap()
        .contains("SUMMARY:会場を押さえる"));
    assert_eq!(app.items.len(), 1);
    // 何も変えなければ、何も送らない
    server.calls.clear();
    let out = app.sync(&mut server);
    assert_eq!(puts(&server), 0);
    assert!(out.updates.is_empty() && out.errors.is_empty());
}

#[test]
fn remote_tasks_come_in_and_changes_go_both_ways() {
    let mut server = Memory::new();
    server.edit(&format!("{CAL}a.ics"), &remote_ics("a", "下見"));
    let mut app = App::default();
    app.sync(&mut server);
    let (key, _) = app.task("下見");
    let key = key.clone();

    // サーバーで変わった → アプリに入る
    let ics = server
        .ics(&format!("{CAL}a.ics"))
        .unwrap()
        .replace("SUMMARY:下見", "SUMMARY:下見に行く");
    server.edit(&format!("{CAL}a.ics"), &ics);
    app.sync(&mut server);
    assert_eq!(app.tasks[&key].summary, "下見に行く");

    // アプリで変えた → サーバーへ。読まない項目は残る
    app.tasks.get_mut(&key).unwrap().priority = 1;
    app.sync(&mut server);
    let stored = server.ics(&format!("{CAL}a.ics")).unwrap();
    assert!(stored.contains("PRIORITY:1"));
    assert!(stored.contains("X-KEEP:1"));
    assert!(stored.contains("PRODID:-//Other//EN"));
}

#[test]
fn different_fields_merge_and_the_same_field_takes_the_server() {
    let mut server = Memory::new();
    server.edit(&format!("{CAL}a.ics"), &remote_ics("a", "下見"));
    let mut app = App::default();
    app.sync(&mut server);
    let key = app.task("下見").0.clone();

    // サーバーでタイトル、アプリで優先度
    let ics = server
        .ics(&format!("{CAL}a.ics"))
        .unwrap()
        .replace("SUMMARY:下見", "SUMMARY:下見 2");
    server.edit(&format!("{CAL}a.ics"), &ics);
    app.tasks.get_mut(&key).unwrap().priority = 3;
    app.sync(&mut server);
    assert_eq!(app.tasks[&key].summary, "下見 2");
    assert_eq!(app.tasks[&key].priority, 3);
    assert!(server
        .ics(&format!("{CAL}a.ics"))
        .unwrap()
        .contains("PRIORITY:3"));
    assert!(app.notices.is_empty());

    // 両方でタイトル → サーバーの値にして知らせる
    let ics = server
        .ics(&format!("{CAL}a.ics"))
        .unwrap()
        .replace("SUMMARY:下見 2", "SUMMARY:サーバー");
    server.edit(&format!("{CAL}a.ics"), &ics);
    app.tasks.get_mut(&key).unwrap().summary = "アプリ".into();
    app.sync(&mut server);
    assert_eq!(app.tasks[&key].summary, "サーバー");
    assert_eq!(app.notices.len(), 1);
    assert!(app.notices[0].contains("タイトル"), "{}", app.notices[0]);
}

#[test]
fn deletions_go_both_ways() {
    let mut server = Memory::new();
    server.edit(&format!("{CAL}a.ics"), &remote_ics("a", "A"));
    server.edit(&format!("{CAL}b.ics"), &remote_ics("b", "B"));
    server.edit(&format!("{CAL}c.ics"), &remote_ics("c", "C"));
    let mut app = App::default();
    app.sync(&mut server);
    assert_eq!(app.tasks.len(), 3);

    // サーバーで消した（アプリは変えていない）→ アプリでも消える
    server.remove(&format!("{CAL}a.ics"));
    // サーバーで消したが、アプリで変えていた → 作り直す
    server.remove(&format!("{CAL}b.ics"));
    let b = app.task("B").0.clone();
    app.tasks.get_mut(&b).unwrap().priority = 2;
    // アプリで消した → サーバーでも消える
    let c = app.task("C").0.clone();
    app.tasks.remove(&c);
    app.sync(&mut server);

    assert!(!app.tasks.values().any(|f| f.summary == "A"));
    assert!(server
        .ics(&format!("{CAL}b.ics"))
        .unwrap()
        .contains("PRIORITY:2"));
    assert!(app.notices.iter().any(|n| n.contains("作り直しました")));
    assert!(server.ics(&format!("{CAL}c.ics")).is_none());
    assert_eq!(
        app.items.len(),
        1,
        "{:?}",
        app.items
            .iter()
            .map(|i| (&i.href, &i.key))
            .collect::<Vec<_>>()
    );
}

#[test]
fn offline_keeps_everything_and_sends_later() {
    let mut server = Memory::new();
    let mut app = App::default();
    app.add("A");
    server.offline = true;
    let out = app.sync(&mut server);
    assert_eq!(out.errors.len(), 1);
    assert!(out.tokens.is_empty());
    assert!(server.objects.is_empty());
    server.offline = false;
    let out = app.sync(&mut server);
    assert!(out.errors.is_empty());
    assert_eq!(server.objects.len(), 1);
}

#[test]
fn read_only_calendars_take_the_server() {
    let mut server = Memory::new();
    server.edit(&format!("{CAL}a.ics"), &remote_ics("a", "A"));
    let mut app = App {
        read_only: true,
        ..Default::default()
    };
    app.sync(&mut server);
    let key = app.task("A").0.clone();
    app.tasks.get_mut(&key).unwrap().summary = "変えた".into();
    app.add("新しい");
    app.sync(&mut server);
    assert_eq!(app.tasks[&key].summary, "A");
    assert!(app.notices.iter().any(|n| n.contains("書き込めない")));
    assert!(app.errors.iter().any(|e| e.contains("新しい")));
    assert_eq!(puts(&server), 0);
}

#[test]
fn server_side_rewrites_come_back_once() {
    // 書いた中身を整え直し（説明の末尾にタスクの画面の URL を足す）、ETag を返さないサーバー
    fn add_link(ics: &str) -> String {
        ics.replace(
            "END:VTODO",
            "DESCRIPTION:https://tasks.example.com/t/1\r\nEND:VTODO",
        )
    }
    let mut server = Memory::new();
    server.no_etag = true;
    server.rewrite = Some(add_link);
    let mut app = App::default();
    let key = app.add("A");
    app.sync(&mut server);
    assert_eq!(
        app.tasks[&key].links,
        vec!["https://tasks.example.com/t/1".to_owned()]
    );
    server.calls.clear();
    app.sync(&mut server);
    assert_eq!(puts(&server), 0, "{:?}", server.calls);
}

#[test]
fn servers_without_deltas_still_sync() {
    let mut server = Memory::new();
    server.delta = false;
    server.edit(&format!("{CAL}a.ics"), &remote_ics("a", "A"));
    let mut app = App::default();
    app.add("B");
    app.sync(&mut server);
    assert_eq!(app.tasks.len(), 2);
    assert_eq!(server.objects.len(), 2);
    server.remove(&format!("{CAL}a.ics"));
    app.sync(&mut server);
    assert_eq!(app.tasks.len(), 1);
    server.calls.clear();
    app.sync(&mut server);
    assert_eq!(puts(&server), 0);
}

#[test]
fn a_write_that_loses_the_race_merges_again() {
    let mut server = Memory::new();
    server.edit(&format!("{CAL}a.ics"), &remote_ics("a", "A"));
    let mut app = App::default();
    app.sync(&mut server);
    let key = app.task("A").0.clone();
    app.tasks.get_mut(&key).unwrap().priority = 4;
    // アプリが知らない間にサーバーで変わった（同期の印より後）
    let mut input = app.input();
    let ics = server
        .ics(&format!("{CAL}a.ics"))
        .unwrap()
        .replace("SUMMARY:A", "SUMMARY:A2");
    server.edit(&format!("{CAL}a.ics"), &ics);
    // 変化を受け取らずに書く（印を新しくして、変化がないことにする）
    input.calendars[0].token = Some("999".into());
    let out = crate::sync(&mut server, &input);
    app.apply(&out);
    let stored = server.ics(&format!("{CAL}a.ics")).unwrap();
    assert!(stored.contains("SUMMARY:A2"));
    assert!(stored.contains("PRIORITY:4"));
    assert_eq!(app.tasks[&key].summary, "A2");
}
