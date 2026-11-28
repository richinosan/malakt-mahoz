//! 本物の CalDAV サーバーとの往復。サーバーがないと動かないので、ふだんは飛ばす。
//!
//! ```sh
//! MALAKT_CALDAV_URL=http://127.0.0.1:5232/ MALAKT_CALDAV_USER=u MALAKT_CALDAV_PASSWORD=p \
//!   cargo test -p malakt-caldav --test server -- --ignored
//! ```
//!
//! タスクを入れられるカレンダーが 1 つ以上あり、書き込めること。テストのタスクは最後に消す。

use malakt_caldav::http::Http;
use malakt_caldav::{ical, Calendar, Changes, Fields, Input, Local, Server};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} を設定してください"))
}

#[test]
#[ignore]
fn round_trip_with_a_real_server() {
    let (url, user, password) = (
        env("MALAKT_CALDAV_URL"),
        env("MALAKT_CALDAV_USER"),
        env("MALAKT_CALDAV_PASSWORD"),
    );
    let found = Http::discover(&url, &user, &password).expect("アカウントを調べる");
    let cal = found
        .calendars
        .iter()
        .find(|c| !c.read_only)
        .expect("書き込めるカレンダー");
    let mut server = Http::new(&found.base, &user, &password);
    let zone = jiff::tz::TimeZone::system();
    let calendar = Calendar {
        id: "c".into(),
        href: cal.href.clone(),
        name: cal.name.clone(),
        token: None,
        read_only: false,
    };
    let mut fields = Fields::empty();
    fields.summary = "往復のテスト".into();
    fields.body = "1 行目\n2 行目".into();
    fields.due = Some("2026-12-24 18:30".into());
    fields.categories = vec!["テスト".into()];
    fields.settle();
    let uid = format!("malakt-test-{}", jiff::Timestamp::now().as_millisecond());
    let input = Input {
        calendars: vec![calendar.clone()],
        items: Vec::new(),
        locals: vec![Local {
            key: "t".into(),
            calendar: "c".into(),
            uid: uid.clone(),
            fields: fields.clone(),
        }],
        zone: zone.clone(),
        now: jiff::Timestamp::now(),
        normalize: |_| {},
    };
    let out = malakt_caldav::sync(&mut server, &input);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let item = out
        .items
        .iter()
        .find(|i| i.key.as_deref() == Some("t"))
        .expect("送ったタスク")
        .clone();
    let token = out.tokens[0].1.clone();

    // サーバーから読み直すと同じ
    let got = server.get(&item.href).unwrap().expect("サーバーにある");
    let read = ical::parse(&got.ics, &zone).unwrap().unwrap();
    assert_eq!(read.uid, uid);
    assert_eq!(read.fields, fields);

    // 変えて送る → 差分で見える
    let mut changed = fields.clone();
    changed.status = "COMPLETED".into();
    changed.settle();
    let input = Input {
        calendars: vec![Calendar {
            token: token.clone(),
            ..calendar.clone()
        }],
        items: out.items.clone(),
        locals: vec![Local {
            key: "t".into(),
            calendar: "c".into(),
            uid: uid.clone(),
            fields: changed.clone(),
        }],
        zone: zone.clone(),
        now: jiff::Timestamp::now(),
        normalize: |_| {},
    };
    let out = malakt_caldav::sync(&mut server, &input);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let got = server.get(&item.href).unwrap().unwrap();
    assert_eq!(
        ical::parse(&got.ics, &zone).unwrap().unwrap().fields,
        changed
    );
    match server.changes(&cal.href, token.as_deref()).unwrap() {
        Changes::Delta { changed, .. } => assert!(changed.iter().any(|(h, _)| *h == item.href)),
        Changes::Full { all } | Changes::Reset { all, .. } => {
            assert!(all.iter().any(|(h, _)| *h == item.href))
        }
    }

    // 消す
    let input = Input {
        calendars: vec![calendar],
        items: out.items.clone(),
        locals: Vec::new(),
        zone,
        now: jiff::Timestamp::now(),
        normalize: |_| {},
    };
    let out = malakt_caldav::sync(&mut server, &input);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(server.get(&item.href).unwrap().is_none());
}
