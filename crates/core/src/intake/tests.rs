use std::io::Write;

use super::*;

fn store_with_workspace() -> (Store, Id) {
    let mut s = Store::open_in_memory().unwrap();
    s.ensure_self().unwrap();
    let w = s.create_workspace(None, "取り込み先").unwrap();
    (s, w.id)
}

const REDMINE_JA: &str = "#,トラッカー,ステータス,優先度,題名,担当者,開始日,期日,進捗率,親チケット,説明,最新の注記\n\
12,機能,新規,高め,ログイン画面を作る,山田 太郎,2024/05/01,2024/05/10,30,,\"画面の\n設計\",確認しました\n\
13,バグ,終了,通常,ボタンが押せない,,,,100,#12,,\n";

#[test]
fn redmine_csv_in_shift_jis_is_read() {
    let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(REDMINE_JA);
    let batch = read_bytes(
        &sjis,
        "issues.csv",
        &FileOptions {
            base_url: Some("redmine.example.com/".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(batch.source, Source::Redmine);
    assert_eq!(batch.host.as_deref(), Some("redmine.example.com"));
    let a = &batch.items[0];
    assert_eq!(a.key, "12");
    assert_eq!(a.title, "ログイン画面を作る");
    assert_eq!(a.status, Status::NeedsAction);
    assert_eq!(a.priority, 3);
    assert_eq!(a.start.as_deref(), Some("2024-05-01"));
    assert_eq!(a.due.as_deref(), Some("2024-05-10"));
    assert_eq!(a.percent, 30);
    assert_eq!(a.assignees, vec!["山田 太郎"]);
    assert_eq!(a.categories, vec!["機能"]);
    assert_eq!(a.description, "画面の\n設計");
    assert_eq!(a.comments[0].body, "確認しました");
    assert_eq!(a.urls, vec!["https://redmine.example.com/issues/12"]);
    let b = &batch.items[1];
    assert_eq!(b.status, Status::Completed);
    assert_eq!(b.parent.as_deref(), Some("12"));
}

#[test]
fn importing_maps_people_parents_and_skips_what_was_imported() {
    let (mut s, w) = store_with_workspace();
    let batch = read_bytes(REDMINE_JA.as_bytes(), "issues.csv", &FileOptions::default()).unwrap();
    let report = s.import_batch(Some(&w), &batch).unwrap();
    assert_eq!(report.added, 2);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.people, vec!["山田 太郎"]);
    let tasks = s.tasks(w.as_str(), &TaskFilter::default()).unwrap();
    let parent = tasks
        .iter()
        .find(|t| t.title == "ログイン画面を作る")
        .unwrap();
    let child = tasks
        .iter()
        .find(|t| t.title == "ボタンが押せない")
        .unwrap();
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(child.status, Status::Completed);
    assert_eq!(parent.assignee_ids.len(), 1);
    let person = s
        .people()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "山田 太郎")
        .unwrap();
    // 新しい担当者は、取り込んだワークスペースの中だけで使う人
    assert_eq!(person.workspace_ids, vec![w.clone()]);
    assert_eq!(s.comments(&parent.id).unwrap()[0].body, "確認しました");
    assert_eq!(
        s.task_origin(&parent.id).unwrap().unwrap().key,
        "12".to_owned()
    );

    // もう一度取り込んでも増えない。担当者も同じ人を使う
    let again = s.import_batch(Some(&w), &batch).unwrap();
    assert_eq!((again.added, again.skipped), (0, 2));
    assert!(again.people.is_empty());
    assert_eq!(
        s.tasks(w.as_str(), &TaskFilter::default()).unwrap().len(),
        2
    );

    // 消したタスクは、もう一度取り込める
    s.delete_task(&child.id).unwrap();
    let third = s.import_batch(Some(&w), &batch).unwrap();
    assert_eq!((third.added, third.skipped), (1, 1));
    let child = s
        .tasks(w.as_str(), &TaskFilter::default())
        .unwrap()
        .into_iter()
        .find(|t| t.title == "ボタンが押せない")
        .unwrap();
    // 親は前に取り込んだタスク
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
}

#[test]
fn a_bad_row_imports_nothing_and_names_every_bad_row() {
    let (s, w) = store_with_workspace();
    let csv = "#,題名,期日,進捗率\n1,A,2024/05/01,\n2,B,来月のどこか,\n3,,,\n4,D,,abc\n";
    let err = read_bytes(csv.as_bytes(), "x.csv", &FileOptions::default()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("3 行目「B」: 期限「来月のどこか」"), "{text}");
    assert!(text.contains("4 行目: タイトルが空です"), "{text}");
    assert!(text.contains("5 行目「D」: 進捗「abc」"), "{text}");
    assert!(s
        .tasks(w.as_str(), &TaskFilter::default())
        .unwrap()
        .is_empty());
}

#[test]
fn backlog_csv_is_read_with_its_keys_and_urls() {
    let csv = "ID,プロジェクトID,プロジェクト名,キー,種別,カテゴリー名,件名,詳細,状態,優先度,担当者,開始日,期限日,親課題キー\n\
               1,10,Web,WEB-1,タスク,デザイン,トップページ,詳しく,処理中,高,佐藤,2024/06/01,2024/06/30,\n\
               2,10,Web,WEB-2,バグ,,リンク切れ,,完了,中,,,,WEB-1\n";
    let batch = read_bytes(
        csv.as_bytes(),
        "backlog.csv",
        &FileOptions {
            base_url: Some("https://acme.backlog.jp".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(batch.source, Source::Backlog);
    let a = &batch.items[0];
    assert_eq!(a.key, "WEB-1");
    assert_eq!(a.status, Status::InProcess);
    assert_eq!(a.priority, 3);
    assert_eq!(a.categories, vec!["タスク", "デザイン"]);
    assert_eq!(a.urls, vec!["https://acme.backlog.jp/view/WEB-1"]);
    assert_eq!(batch.items[1].parent.as_deref(), Some("WEB-1"));
    assert_eq!(batch.items[1].status, Status::Completed);
    assert_eq!(batch.items[1].priority, 5);
}

#[test]
fn clickup_csv_is_read_with_comments_and_task_urls() {
    let csv = "Task ID,Task Name,Task Content,Status,Priority,Assignees,Due Date,Start Date,Parent ID,Tags,Comments\n\
               86a1,Write docs,Draft the guide,in progress,2,\"[Ann Lee, Bob]\",1714521600000,,,\"[docs, q2]\",\"[{\"\"text\"\":\"\"looks good\"\",\"\"by\"\":\"\"Ann Lee\"\",\"\"date\"\":1714521600000}]\"\n\
               86a2,Review,,complete,,,\"Wednesday, May 1st 2024\",,86a1,,\n";
    let batch = read_bytes(csv.as_bytes(), "clickup.csv", &FileOptions::default()).unwrap();
    assert_eq!(batch.source, Source::ClickUp);
    let a = &batch.items[0];
    assert_eq!(a.status, Status::InProcess);
    assert_eq!(a.priority, 3);
    assert_eq!(a.assignees, vec!["Ann Lee", "Bob"]);
    assert_eq!(a.categories, vec!["docs", "q2"]);
    assert!(a.due.as_deref().unwrap().starts_with("2024-05-0"));
    assert_eq!(a.urls, vec!["https://app.clickup.com/t/86a1"]);
    assert_eq!(a.comments[0].author, "Ann Lee");
    assert_eq!(a.comments[0].body, "looks good");
    assert_eq!(a.comments[0].at, Some(1714521600000));
    let b = &batch.items[1];
    assert_eq!(b.due.as_deref(), Some("2024-05-01"));
    assert_eq!(b.parent.as_deref(), Some("86a1"));
    assert_eq!(b.status, Status::Completed);

    // コメントを書いた人が担当者にいれば、その人のコメントにする
    let (mut s, w) = store_with_workspace();
    let report = s.import_batch(Some(&w), &batch).unwrap();
    let id = &report.task_ids[0];
    let c = &s.comments(id).unwrap()[0];
    assert!(c.author_id.is_some());
    assert_eq!(c.body, "looks good");
    assert_eq!(c.created_at, 1714521600000);
}

fn notion_zip() -> Vec<u8> {
    let id_a = "0123456789abcdef0123456789abcdef";
    let id_b = "fedcba9876543210fedcba9876543210";
    let mut inner = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut inner));
        let opt = zip::write::SimpleFileOptions::default();
        z.start_file("Tasks 1111111111111111aaaaaaaaaaaaaaaa.csv", opt)
            .unwrap();
        z.write_all("Name,Status\n買い物,Done\n".as_bytes())
            .unwrap();
        z.start_file("Tasks 1111111111111111aaaaaaaaaaaaaaaa_all.csv", opt)
            .unwrap();
        z.write_all(
            format!(
                "\u{feff}Name,Status,Assignee,Due,Tags,Parent item\n\
                 買い物,Done,花子,\"May 1, 2024 → May 3, 2024\",\"家, 週末\",\n\
                 牛乳,Not started,,2024/05/02,,\"買い物 (https://www.notion.so/{id_a})\"\n"
            )
            .as_bytes(),
        )
        .unwrap();
        z.start_file(format!("Tasks/買い物 {id_a}.md"), opt)
            .unwrap();
        z.write_all("# 買い物\n\nStatus: Done\n\n週末にまとめて\n".as_bytes())
            .unwrap();
        z.start_file(format!("Tasks/牛乳 {id_b}.md"), opt).unwrap();
        z.write_all("# 牛乳\n\nStatus: Not started\n".as_bytes())
            .unwrap();
        z.finish().unwrap();
    }
    // 大きいエクスポートは zip の中に zip が入る
    let mut outer = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut outer));
        z.start_file(
            "Export-1-Part-1.zip",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(&inner).unwrap();
        z.finish().unwrap();
    }
    outer
}

#[test]
fn notion_zip_is_read_with_pages_and_ranges() {
    let batch = read_bytes(&notion_zip(), "Export.zip", &FileOptions::default()).unwrap();
    assert_eq!(batch.source, Source::Notion);
    // フィルターのかかった方の CSV は読まない
    assert_eq!(batch.items.len(), 2);
    let a = &batch.items[0];
    assert_eq!(a.key, "0123456789abcdef0123456789abcdef");
    assert_eq!(
        a.urls,
        vec!["https://www.notion.so/0123456789abcdef0123456789abcdef"]
    );
    assert_eq!(a.description, "週末にまとめて");
    assert_eq!(a.status, Status::Completed);
    assert_eq!(a.start.as_deref(), Some("2024-05-01"));
    assert_eq!(a.due.as_deref(), Some("2024-05-03"));
    assert_eq!(a.categories, vec!["家", "週末"]);
    assert_eq!(a.assignees, vec!["花子"]);
    let b = &batch.items[1];
    assert_eq!(b.status, Status::NeedsAction);
    assert_eq!(b.parent.as_deref(), Some(a.key.as_str()));

    let (mut s, w) = store_with_workspace();
    let report = s.import_batch(Some(&w), &batch).unwrap();
    assert_eq!(report.added, 2);
    let child = s.task(&report.task_ids[1]).unwrap();
    assert_eq!(
        child.parent_id.as_deref(),
        Some(report.task_ids[0].as_str())
    );
}

#[test]
fn notion_csv_without_pages_is_deduplicated_by_its_rows() {
    let csv = "タスク名,ステータス,期限\n資料,未着手,2024年5月1日\n";
    let batch = read_bytes(csv.as_bytes(), "notion.csv", &FileOptions::default()).unwrap();
    assert_eq!(batch.source, Source::Notion);
    assert_eq!(batch.items[0].due.as_deref(), Some("2024-05-01"));
    let (mut s, w) = store_with_workspace();
    assert_eq!(s.import_batch(Some(&w), &batch).unwrap().added, 1);
    assert_eq!(s.import_batch(Some(&w), &batch).unwrap().skipped, 1);
}

#[test]
fn files_that_are_not_task_lists_are_refused() {
    let err = read_bytes(b"foo,bar\n1,2\n", "x.csv", &FileOptions::default()).unwrap_err();
    assert!(err.to_string().contains("読めませんでした"));
    let err = read_bytes(b"", "x.csv", &FileOptions::default()).unwrap_err();
    assert!(err.to_string().contains("空"));
    // 選んだサービスの CSV でなければ、足りない列を言う
    let err = read_bytes(
        b"Name\nA\n",
        "x.csv",
        &FileOptions {
            source: Some(Source::Redmine),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("列"), "{err}");
}

#[test]
fn values_are_read_in_many_forms() {
    assert_eq!(status_of("Closed"), Status::Completed);
    assert_eq!(status_of("却下"), Status::Cancelled);
    assert_eq!(status_of("未完了"), Status::NeedsAction);
    assert_eq!(status_of("Not started"), Status::NeedsAction);
    assert_eq!(status_of("処理済み"), Status::InProcess);
    assert_eq!(status_of("to do"), Status::NeedsAction);
    assert_eq!(priority_of("今すぐ"), 1);
    assert_eq!(priority_of("低め"), 9);
    assert_eq!(priority_of("Urgent"), 1);
    assert_eq!(priority_of(""), 0);
    assert_eq!(
        date_of("2024-05-01T10:30:00").unwrap().as_deref(),
        Some("2024-05-01 10:30")
    );
    assert_eq!(
        date_of("2024/5/1 午後3:05").unwrap().as_deref(),
        Some("2024-05-01 15:05")
    );
    assert_eq!(
        date_of("@May 1, 2024 10:00 AM (GMT+9)").unwrap().as_deref(),
        Some("2024-05-01 10:00")
    );
    assert_eq!(
        date_of("1 May 2024").unwrap().as_deref(),
        Some("2024-05-01")
    );
    assert_eq!(
        date_of("2024/05/01(水)").unwrap().as_deref(),
        Some("2024-05-01")
    );
    assert_eq!(date_of("").unwrap(), None);
    assert!(date_of("2024/13/40").is_err());
    assert!(date_of("someday").is_err());
    assert_eq!(percent_of("0.5").unwrap(), 50);
    assert_eq!(percent_of("80%").unwrap(), 80);
    assert_eq!(split_names("[a, b、c]"), vec!["a", "b", "c"]);
    assert_eq!(
        clean_base_url("x.example.com/redmine/").unwrap(),
        "https://x.example.com/redmine"
    );
    assert!(clean_base_url("ftp://x").is_err());
}

#[test]
fn origins_survive_export_and_import() {
    let (mut s, w) = store_with_workspace();
    let batch = read_bytes(REDMINE_JA.as_bytes(), "issues.csv", &FileOptions::default()).unwrap();
    s.import_batch(Some(&w), &batch).unwrap();
    let mut zip = Vec::new();
    crate::archive::export(&s, std::io::Cursor::new(&mut zip)).unwrap();
    let mut other = Store::open_in_memory().unwrap();
    crate::archive::import(&mut other, std::io::Cursor::new(&zip)).unwrap();
    assert_eq!(other.import_origins().unwrap(), s.import_origins().unwrap());
    // 書き出して読み込んだ先でも、同じものは取り込み済み
    let again = other.import_batch(Some(&w), &batch).unwrap();
    assert_eq!((again.added, again.skipped), (0, 2));
}

#[test]
fn an_import_is_undone_in_one_step() {
    let (mut s, w) = store_with_workspace();
    s.undo_mark().unwrap();
    let batch = read_bytes(REDMINE_JA.as_bytes(), "issues.csv", &FileOptions::default()).unwrap();
    s.import_batch(Some(&w), &batch).unwrap();
    s.undo().unwrap();
    assert!(s
        .tasks(w.as_str(), &TaskFilter::default())
        .unwrap()
        .is_empty());
}
