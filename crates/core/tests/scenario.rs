//! 利用者の操作の流れを、最初から最後まで通して確かめるシナリオテスト。

#![allow(clippy::cloned_ref_to_slice_refs)]

use std::io::{Cursor, Write};

use malakt_core::archive::{self, FORMAT, FORMAT_VERSION};
use malakt_core::rules::{Apply, RulePatch};
use malakt_core::schedule::SchedulePatch;
use malakt_core::{lock, Error, Status, Store, TaskDraft, TaskFilter};

fn draft(title: &str) -> TaskDraft {
    TaskDraft {
        title: title.into(),
        ..Default::default()
    }
}

/// 入れ子のワークスペースを作り、担当を割り当て、進め、別の端末に移す。
#[test]
fn plan_work_and_move_to_another_device() {
    let mut a = Store::open_in_memory().unwrap();
    let event = a.create_workspace(None, "冬のイベント").unwrap();
    let venue = a.create_workspace(Some(&event.id), "会場").unwrap();
    let print = a.create_workspace(Some(&event.id), "印刷物").unwrap();
    let me = a.create_person("自分", true).unwrap();
    let guest = a.create_person("手伝いの佐藤さん", false).unwrap();

    let mut d = draft("机と椅子を借りる");
    d.assignee_ids = vec![me.id.clone(), guest.id.clone()];
    d.due = Some("2026-12-20".into());
    d.priority = 1;
    d.urls = vec![
        "https://example.com/rental?id=1,2".into(),
        "github.com/acme/event/issues/3".into(),
    ];
    // 予想工数と所要時間の基準も一緒に移す
    d.estimate = malakt_core::estimate::parse("1日3時間");
    let desk = a.create_task(Some(&venue.id), &d).unwrap();
    let created_first = malakt_core::timing::Basis::new(
        malakt_core::timing::StartBasis::Created,
        malakt_core::timing::EndBasis::Completed,
    );
    a.set_workspace_time_basis(&venue.id, Some(created_first))
        .unwrap();
    let flyer = a
        .create_task(Some(&print.id), &draft("チラシを刷る"))
        .unwrap();
    a.set_task_status(&desk.id, Status::InProcess).unwrap();
    a.add_comment(&desk.id, Some(&me.id), "見積もりを依頼した")
        .unwrap();
    let pin = a
        .add_comment(&desk.id, Some(&guest.id), "連絡先: 受付まで")
        .unwrap();
    a.set_comment_pinned(&pin.id, true).unwrap();
    a.set_task_status(&flyer.id, Status::Completed).unwrap();
    // 繰り返しも一緒に移す
    let weekly = malakt_core::repeat::Rule::weekly("2026-12-20");
    a.set_repeat(&desk.id, Some(&weekly), "2026-11-27 12:00")
        .unwrap();
    a.set_repeat_ai(&desk.id, "借りる机の数を確かめる").unwrap();
    // 定時タスクも一緒に移す
    let report_rule = malakt_core::repeat::Rule {
        time: Some("09:00".into()),
        ..malakt_core::repeat::Rule::weekly("2026-11-30")
    };
    let report = a
        .create_schedule(
            Some(&venue.id),
            "週次定例の報告",
            Some(&report_rule),
            "2026-11-27 12:00",
        )
        .unwrap();
    let report = a
        .update_schedule(
            &report.id,
            &SchedulePatch {
                ai: Some("先週の進み具合をまとめる".into()),
                assignee_ids: Some(vec![guest.id.clone()]),
                due_days: Some(Some(1)),
                due_time: Some(Some("17:00".into())),
                ..SchedulePatch::default()
            },
            "2026-11-27 12:00",
        )
        .unwrap();
    // AI のルールも一緒に移す
    let global = a
        .create_ai_rule(None, "言葉づかい", "敬語で答える")
        .unwrap();
    let local = a
        .create_ai_rule(Some(&venue.id), "", "会場は予約が先")
        .unwrap();
    a.update_ai_rule(
        &local.id,
        &RulePatch {
            enabled: Some(false),
            apply: Some(Apply::Auto),
            ..RulePatch::default()
        },
    )
    .unwrap();

    // 親のワークスペースから、子孫の未完了だけを見る
    let open = a
        .tasks(
            &event.id,
            &TaskFilter {
                include_descendants: true,
                open_only: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, desk.id);

    // エクスポートして、空の別の端末に取り込む
    let mut zip = Cursor::new(Vec::new());
    let manifest = archive::export(&a, &mut zip).unwrap();
    assert_eq!(manifest.format, FORMAT);
    assert_eq!(manifest.counts.workspaces, 3);
    assert_eq!(manifest.counts.tasks, 2);
    assert_eq!(manifest.counts.people, 2);
    assert_eq!(manifest.counts.comments, 2);

    let mut b = Store::open_in_memory().unwrap();
    zip.set_position(0);
    let imported = archive::import(&mut b, &mut zip).unwrap();
    assert_eq!(imported, manifest);

    assert_eq!(b.workspaces().unwrap(), a.workspaces().unwrap());
    assert_eq!(b.people().unwrap(), a.people().unwrap());
    assert_eq!(b.task(&desk.id).unwrap(), a.task(&desk.id).unwrap());
    assert_eq!(
        b.task(&desk.id).unwrap().urls,
        [
            "https://example.com/rental?id=1,2",
            "https://github.com/acme/event/issues/3"
        ]
    );
    assert_eq!(b.task(&flyer.id).unwrap(), a.task(&flyer.id).unwrap());
    let moved = b.task(&desk.id).unwrap();
    assert_eq!(
        moved.estimate.map(|e| e.label()).as_deref(),
        Some("1日3時間")
    );
    assert!(moved.started_at.is_some() && moved.assigned_at.is_some());
    assert_eq!(
        b.workspace(&venue.id).unwrap().time_basis,
        Some(created_first)
    );
    assert_eq!(b.comments(&desk.id).unwrap(), a.comments(&desk.id).unwrap());
    assert_eq!(b.changes(&desk.id).unwrap(), a.changes(&desk.id).unwrap());
    assert_eq!(b.workspace_path(&venue.id).unwrap().len(), 2);
    assert_eq!(b.ai_rules(None).unwrap(), vec![global]);
    assert_eq!(
        b.ai_rules(Some(&venue.id)).unwrap(),
        a.ai_rules(Some(&venue.id)).unwrap()
    );
    assert_eq!(b.ai_rule(&local.id).unwrap().apply, Apply::Auto);
    let repeat = b.repeat(&desk.id).unwrap().unwrap();
    assert_eq!(Some(repeat.clone()), a.repeat(&desk.id).unwrap());
    assert_eq!(repeat.rule.label(), "毎週 日");
    assert_eq!(repeat.ai, "借りる机の数を確かめる");
    // 取り込んだ先でも、次の回が作られる
    let made = b.sweep_repeats(&repeat.next_at).unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(b.task(&made[0]).unwrap().due.as_deref(), Some("2026-12-27"));
    // 定時タスクも同じ中身で戻り、取り込んだ先でタスクを作る
    assert_eq!(b.schedules(Some(&venue.id)).unwrap(), vec![report.clone()]);
    let made = b.sweep_schedules(&report.next_at).unwrap();
    assert_eq!(made.len(), 1);
    let task = b.task(&made[0]).unwrap();
    assert_eq!(task.title, "週次定例の報告");
    assert_eq!(task.assignee_ids, vec![guest.id.clone()]);
    assert_eq!(task.due.as_deref(), Some("2026-12-01 17:00"));
}

/// 2 台で別々に進めたあと取り込むと、同じものは上書き、ないものは追加になる。
#[test]
fn import_merges_by_id() {
    let mut a = Store::open_in_memory().unwrap();
    let ws = a.create_workspace(None, "共有").unwrap();
    let me_a = a.create_person("A の自分", true).unwrap();
    let task = a.create_task(Some(&ws.id), &draft("最初の名前")).unwrap();

    let mut first = Cursor::new(Vec::new());
    archive::export(&a, &mut first).unwrap();

    let mut b = Store::open_in_memory().unwrap();
    let me_b = b.create_person("B の自分", true).unwrap();
    let own_ws = b.create_workspace(None, "B だけ").unwrap();
    let own_task = b
        .create_task(Some(&own_ws.id), &draft("B のタスク"))
        .unwrap();
    first.set_position(0);
    archive::import(&mut b, &mut first).unwrap();

    // 「自分」は B のまま。A の自分は普通の担当者として入る
    let people = b.people().unwrap();
    assert_eq!(people.iter().filter(|p| p.is_self).count(), 1);
    assert!(people.iter().any(|p| p.id == me_b.id && p.is_self));
    assert!(people.iter().any(|p| p.id == me_a.id && !p.is_self));

    // A で進めて、もう一度取り込む
    let mut d = TaskDraft::from(&task);
    d.title = "変えた名前".into();
    d.assignee_ids = vec![me_a.id.clone()];
    a.update_task(&task.id, &d).unwrap();
    let extra = a.create_task(Some(&ws.id), &draft("追加")).unwrap();
    let mut second = Cursor::new(Vec::new());
    archive::export(&a, &mut second).unwrap();
    second.set_position(0);
    archive::import(&mut b, &mut second).unwrap();

    assert_eq!(b.task(&task.id).unwrap().title, "変えた名前");
    assert_eq!(b.task(&task.id).unwrap().assignee_ids, [me_a.id.clone()]);
    assert!(b.task(&extra.id).is_ok());
    assert_eq!(b.task(&own_task.id).unwrap().title, "B のタスク");
    assert_eq!(b.tasks(&ws.id, &TaskFilter::default()).unwrap().len(), 2);
    assert_eq!(b.people().unwrap().iter().filter(|p| p.is_self).count(), 1);

    // 同じものを 2 回取り込んでも増えない
    second.set_position(0);
    archive::import(&mut b, &mut second).unwrap();
    assert_eq!(b.tasks(&ws.id, &TaskFilter::default()).unwrap().len(), 2);
    assert_eq!(
        b.changes(&task.id).unwrap().len(),
        a.changes(&task.id).unwrap().len()
    );
}

fn zip_with(files: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    let mut out = zip.finish().unwrap();
    out.set_position(0);
    out
}

fn manifest_json(format: &str, version: u32) -> Vec<u8> {
    format!(
        r#"{{"format":"{format}","format_version":{version},"app_version":"0.0.0","exported_at":0,
            "counts":{{"workspaces":0,"people":0,"tasks":0,"comments":0,"changes":0}}}}"#
    )
    .into_bytes()
}

const WS: &str = "01aaaaaaaaaaaaaaaaaaaaaaaa";

/// Windows のエディタで保存し直したような、BOM・CRLF・NFD の混ざった zip も読める。
#[test]
fn import_accepts_other_os_text() {
    let mut manifest = vec![0xEF, 0xBB, 0xBF];
    manifest.extend(manifest_json(FORMAT, FORMAT_VERSION));
    // 「が」が NFD（か + 濁点）、説明が CRLF
    let workspaces = format!(
        "\u{feff}[{{\"id\":\"{WS}\",\"parent_id\":null,\"name\":\" か\u{3099}っこう \",\"position\":0,\"created_at\":1,\"updated_at\":1}}]"
    );
    let tasks = format!(
        r#"[{{"id":"01bbbbbbbbbbbbbbbbbbbbbbbb","workspace_id":"{WS}","title":"か{}んばる","description":"1\r\n2\r3",
            "status":"in_process","priority":2,"due":"2026-12-31","assignee_ids":[],"position":0,
            "created_at":1,"updated_at":1,"completed_at":null}}]"#,
        '\u{3099}'
    );
    let mut zip = zip_with(&[
        ("manifest.json", &manifest),
        ("data/workspaces.json", workspaces.as_bytes()),
        ("data/people.json", b"[]"),
        ("data/tasks.json", tasks.as_bytes()),
        ("data/comments.json", b"[]"),
        ("data/changes.json", b"[]"),
    ]);
    let mut s = Store::open_in_memory().unwrap();
    archive::import(&mut s, &mut zip).unwrap();
    assert_eq!(s.workspace(WS).unwrap().name, "がっこう");
    let t = s.task("01bbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    assert_eq!(t.title, "がんばる");
    assert_eq!(t.description, "1\n2\n3");
    assert_eq!(t.status, Status::InProcess);
}

/// 壊れた zip や別物の zip は取り込まず、元のデータを変えない。
#[test]
fn bad_archives_are_rejected_without_changes() {
    let mut s = Store::open_in_memory().unwrap();
    let keep = s.create_workspace(None, "そのまま").unwrap();
    let ok_manifest = manifest_json(FORMAT, FORMAT_VERSION);
    let ws = |parent: &str, id: &str| {
        format!(
            r#"{{"id":"{id}","parent_id":{parent},"name":"x","position":0,"created_at":1,"updated_at":1}}"#
        )
    };
    let task = |extra: &str| {
        format!(
            r#"[{{"id":"01cccccccccccccccccccccccc","workspace_id":"{WS}","title":"t","description":"",
                "status":"needs_action","priority":0,"due":null,"assignee_ids":[],"position":0,
                "created_at":1,"updated_at":1,"completed_at":null{extra}}}]"#
        )
    };
    let full = |manifest: &[u8], workspaces: &str, tasks: &str| {
        zip_with(&[
            ("manifest.json", manifest),
            ("data/workspaces.json", workspaces.as_bytes()),
            ("data/people.json", b"[]"),
            ("data/tasks.json", tasks.as_bytes()),
            ("data/comments.json", b"[]"),
            ("data/changes.json", b"[]"),
        ])
    };
    let one_ws = format!("[{}]", ws("null", WS));

    // zip ではない
    assert!(matches!(
        archive::import(&mut s, Cursor::new(b"not a zip".to_vec())),
        Err(Error::Zip(_))
    ));
    // 別のアプリの zip
    assert!(matches!(
        archive::import(&mut s, full(&manifest_json("other", 1), &one_ws, "[]")),
        Err(Error::Invalid(_))
    ));
    // 新しすぎる形式
    assert!(matches!(
        archive::import(&mut s, full(&manifest_json(FORMAT, FORMAT_VERSION + 1), &one_ws, "[]")),
        Err(Error::UnsupportedFormat(v)) if v == FORMAT_VERSION + 1
    ));
    // ファイルが足りない
    assert!(matches!(
        archive::import(&mut s, zip_with(&[("manifest.json", &ok_manifest)])),
        Err(Error::Invalid(_))
    ));
    // UTF-8 ではない（Shift_JIS）
    let sjis = zip_with(&[
        ("manifest.json", &ok_manifest),
        (
            "data/workspaces.json",
            &[b'[', b'"', 0x82, 0xA0, b'"', b']'],
        ),
    ]);
    assert!(matches!(
        archive::import(&mut s, sjis),
        Err(Error::Invalid(_))
    ));
    // 知らない状態
    assert!(matches!(
        archive::import(
            &mut s,
            full(
                &ok_manifest,
                &one_ws,
                &task("").replace("needs_action", "done")
            )
        ),
        Err(Error::Json(_))
    ));
    // 正しくない期限・優先度・空のタイトル
    for bad in [
        task("").replace("\"due\":null", "\"due\":\"2026-02-30\""),
        task("").replace("\"priority\":0", "\"priority\":10"),
        task("").replace("\"title\":\"t\"", "\"title\":\" \""),
    ] {
        assert!(matches!(
            archive::import(&mut s, full(&ok_manifest, &one_ws, &bad)),
            Err(Error::Invalid(_))
        ));
    }
    // 空の名前のワークスペース
    assert!(matches!(
        archive::import(
            &mut s,
            full(&ok_manifest, &one_ws.replace("\"x\"", "\"\""), "[]")
        ),
        Err(Error::Invalid(_))
    ));
    // 存在しないワークスペースを指すタスク
    assert!(matches!(
        archive::import(&mut s, full(&ok_manifest, "[]", &task(""))),
        Err(Error::Invalid(_))
    ));
    // 親子が輪になっている
    let a = "01dddddddddddddddddddddddd";
    let b = "01eeeeeeeeeeeeeeeeeeeeeeee";
    let cycle = format!(
        "[{},{}]",
        ws(&format!("\"{b}\""), a),
        ws(&format!("\"{a}\""), b)
    );
    assert!(matches!(
        archive::import(&mut s, full(&ok_manifest, &cycle, "[]")),
        Err(Error::Cycle)
    ));

    // どれも取り込まれていない
    let all = s.workspaces().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0], keep);

    // 子が親より先に並んでいても、正しければ取り込める
    let child_first = format!("[{},{}]", ws(&format!("\"{b}\""), a), ws("null", b));
    archive::import(&mut s, full(&ok_manifest, &child_first, "[]")).unwrap();
    assert_eq!(s.workspace(a).unwrap().parent_id.as_deref(), Some(b));
}

/// zip の中のファイル名は ASCII だけで、中身は BOM なしの UTF-8。
#[test]
fn archive_layout_is_portable() {
    let mut s = Store::open_in_memory().unwrap();
    let w = s.create_workspace(None, "日本語/の\\名前:CON").unwrap();
    s.create_task(Some(&w.id), &draft("改行\r\nあり")).unwrap();
    let mut buf = Cursor::new(Vec::new());
    archive::export(&s, &mut buf).unwrap();
    buf.set_position(0);
    let mut zip = zip::ZipArchive::new(buf).unwrap();
    let mut names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "data/ai_rules.json",
            "data/attachments.json",
            "data/changes.json",
            "data/comments.json",
            "data/people.json",
            "data/repeats.json",
            "data/schedule_files.json",
            "data/schedules.json",
            "data/task_alerts.json",
            "data/task_defaults.json",
            "data/tasks.json",
            "data/work_logs.json",
            "data/workspaces.json",
            "manifest.json"
        ]
    );
    for name in names {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut zip.by_name(&name).unwrap(), &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.starts_with('\u{feff}'), "{name} に BOM がある");
        assert!(!text.contains('\r'), "{name} に CR がある");
        assert!(!text.contains("\\r"), "{name} に CR がある");
    }
}

/// ファイルへの書き出しと読み込み。失敗しても書きかけを残さない。
#[test]
fn export_and_import_through_files() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a.sqlite3");
    let out = dir.path().join("書き出し 2026.zip");
    let mut a = Store::open(&db).unwrap();
    let w = a.create_workspace(None, "W").unwrap();
    archive::export_to_path(&a, &out).unwrap();
    assert!(out.exists());
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part")
            })
            .count(),
        0
    );

    let mut b = Store::open(dir.path().join("b.sqlite3")).unwrap();
    archive::import_from_path(&mut b, &out).unwrap();
    assert_eq!(b.workspace(&w.id).unwrap().name, "W");

    let missing = dir.path().join("no-such-dir").join("x.zip");
    assert!(matches!(
        archive::export_to_path(&a, &missing),
        Err(Error::Io(_))
    ));
    assert!(matches!(
        archive::import_from_path(&mut b, &missing),
        Err(Error::Io(_))
    ));
}

/// ロックを掛けて閉じ、開き直してもロックされたまま。無効にすると、開き直しても無効のまま。
#[test]
fn lock_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("locked.sqlite3");
    {
        let mut s = Store::open(&db).unwrap();
        lock::enable(&mut s).unwrap();
    }
    {
        let mut s = Store::open(&db).unwrap();
        assert!(lock::is_enabled(&s).unwrap());
        lock::disable(&mut s).unwrap();
    }
    let s = Store::open(&db).unwrap();
    assert!(!lock::is_enabled(&s).unwrap());
}
