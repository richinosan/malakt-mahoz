use malakt_caldav::memory::Memory;

use super::*;

const CAL: &str = "/dav/cal/";
const ME: &str = "https://id.example.com/users/me";

fn setup() -> (Store, Account, Calendar, Workspace) {
    let mut s = Store::open_in_memory().unwrap();
    s.ensure_self().unwrap();
    let account = s
        .add_caldav_account(
            "https://dav.example.com/dav/",
            "me",
            &[ME.to_owned()],
            &[Found {
                href: CAL.into(),
                name: "共有".into(),
                read_only: false,
            }],
        )
        .unwrap();
    let cal = s.caldav_calendars(&account.id).unwrap().remove(0);
    let w = s.create_workspace(None, "共有").unwrap();
    s.link_caldav_calendar(&cal.id, Some(&w.id)).unwrap();
    s.undo_mark().unwrap();
    (s, account, cal, w)
}

fn sync(s: &mut Store, account: &Account, server: &mut Memory) -> (Outcome, Applied) {
    let input = s.caldav_input(&account.id).unwrap();
    let out = malakt_caldav::sync(server, &input);
    let applied = s.apply_caldav(&account.id, &out).unwrap();
    (out, applied)
}

fn draft(title: &str) -> TaskDraft {
    TaskDraft {
        title: title.into(),
        ..Default::default()
    }
}

fn find(s: &Store, w: &Workspace, title: &str) -> Task {
    s.tasks(w.id.as_str(), &Default::default())
        .unwrap()
        .into_iter()
        .find(|t| t.title == title)
        .unwrap_or_else(|| panic!("{title} がない"))
}

const REMOTE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Other//EN\r\nBEGIN:VTODO\r\nUID:r1\r\nSUMMARY:記事のレビュー\r\nDESCRIPTION:下書きを読む\\n\\nhttps://tasks.example.com/t/r1\r\nURL:https://blog.example.com/p/1\r\nSTATUS:NEEDS-ACTION\r\nDUE;VALUE=DATE:20261205\r\nATTENDEE;CN=自分;ROLE=REQ-PARTICIPANT:https://id.example.com/users/me\r\nATTENDEE;CN=アリス;ROLE=CHAIR:https://id.example.com/users/alice\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

#[test]
fn tasks_go_out_and_come_in() {
    let (mut s, account, _, w) = setup();
    s.create_task(Some(&w.id), &draft("会場を押さえる"))
        .unwrap();
    let mut server = Memory::new();
    server.edit(&format!("{CAL}r1.ics"), REMOTE);
    let (out, applied) = sync(&mut s, &account, &mut server);
    assert!(
        out.errors.is_empty() && applied.errors.is_empty(),
        "{out:?} {applied:?}"
    );

    // アプリのタスクはサーバーへ
    assert_eq!(server.objects.len(), 2);
    assert!(server
        .objects
        .values()
        .any(|(_, ics)| ics.contains("SUMMARY:会場を押さえる")));

    // サーバーのタスクはワークスペースへ。説明の末尾の URL は URL 欄に出る
    let t = find(&s, &w, "記事のレビュー");
    assert_eq!(t.description, "下書きを読む");
    assert_eq!(
        t.urls,
        vec![
            "https://blog.example.com/p/1".to_owned(),
            "https://tasks.example.com/t/r1".to_owned()
        ]
    );
    assert_eq!(t.due.as_deref(), Some("2026-12-05"));
    assert!(matches!(t.created_by, Actor::Sync(ref h) if h == "dav.example.com"));
    // 自分の URI は自分、知らない人はこのワークスペースの担当者になる
    let people = s.people().unwrap();
    let me = people.iter().find(|p| p.is_self).unwrap();
    let alice = people.iter().find(|p| p.name == "アリス").unwrap();
    assert!(t.assignee_ids.contains(&me.id));
    assert!(t.assignee_ids.contains(&alice.id));
    assert_eq!(alice.workspace_ids, vec![w.id.clone()]);
    assert_eq!(t.roles_of(&alice.id), Some(Roles::of(&[Role::A])));

    // 何も変えなければ、何も送らない
    server.calls.clear();
    let (out, _) = sync(&mut s, &account, &mut server);
    assert!(
        server.calls.iter().all(|c| !c.starts_with("put")),
        "{:?}",
        server.calls
    );
    assert!(out.updates.is_empty());

    // サーバーからの変更は、元に戻す相手にしない（戻るのは自分で作ったタスクだけ）
    assert!(s.undo().unwrap());
    find(&s, &w, "記事のレビュー");
    let titles: Vec<String> = s
        .tasks(w.id.as_str(), &Default::default())
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(titles, vec!["記事のレビュー".to_owned()]);
}

#[test]
fn editing_the_description_keeps_the_server_link() {
    let (mut s, account, _, w) = setup();
    let mut server = Memory::new();
    server.edit(&format!("{CAL}r1.ics"), REMOTE);
    sync(&mut s, &account, &mut server);
    let t = find(&s, &w, "記事のレビュー");
    let mut d = TaskDraft::from(&t);
    d.description = "下書きを読んで直す".into();
    d.urls.push("https://example.com/mine".into());
    s.update_task(&t.id, &d).unwrap();
    let (out, applied) = sync(&mut s, &account, &mut server);
    assert!(out.errors.is_empty() && applied.errors.is_empty());
    let ics = server.ics(&format!("{CAL}r1.ics")).unwrap();
    assert!(
        ics.contains("DESCRIPTION:下書きを読んで直す\\n\\nhttps://tasks.example.com/t/r1"),
        "{ics}"
    );
    // このアプリだけの URL は、サーバーの URL を変えない
    assert!(ics.contains("URL:https://blog.example.com/p/1"));
    let t = s.task(&t.id).unwrap();
    assert_eq!(t.urls.len(), 3);
    assert!(t.urls.contains(&"https://example.com/mine".to_owned()));
}

#[test]
fn server_changes_and_deletes_come_in() {
    let (mut s, account, _, w) = setup();
    let mut server = Memory::new();
    server.edit(&format!("{CAL}r1.ics"), REMOTE);
    sync(&mut s, &account, &mut server);
    let t = find(&s, &w, "記事のレビュー");
    let ics = REMOTE
        .replace("STATUS:NEEDS-ACTION", "STATUS:COMPLETED")
        .replace("SUMMARY:記事のレビュー", "SUMMARY:記事のレビュー（済）");
    server.edit(&format!("{CAL}r1.ics"), &ics);
    let (_, applied) = sync(&mut s, &account, &mut server);
    assert!(applied.changed);
    let t = s.task(&t.id).unwrap();
    assert_eq!(t.title, "記事のレビュー（済）");
    assert_eq!(t.status, Status::Completed);
    // 完了で全員の分が完了になっても、サーバーには送り返さない
    server.calls.clear();
    sync(&mut s, &account, &mut server);
    assert!(
        server.calls.iter().all(|c| !c.starts_with("put")),
        "{:?}",
        server.calls
    );

    server.remove(&format!("{CAL}r1.ics"));
    sync(&mut s, &account, &mut server);
    assert!(s.task(&t.id).is_err());
}

#[test]
fn deleting_the_workspace_leaves_the_server_alone() {
    let (mut s, account, cal, w) = setup();
    s.create_task(Some(&w.id), &draft("A")).unwrap();
    let mut server = Memory::new();
    sync(&mut s, &account, &mut server);
    assert_eq!(server.objects.len(), 1);
    s.delete_workspace(&w.id).unwrap();
    server.calls.clear();
    sync(&mut s, &account, &mut server);
    assert_eq!(server.objects.len(), 1);
    assert!(server.calls.is_empty(), "{:?}", server.calls);
    let cal = s
        .caldav_calendars(&account.id)
        .unwrap()
        .into_iter()
        .find(|c| c.id == cal.id)
        .unwrap();
    assert_eq!(cal.workspace_id, None);
}

#[test]
fn edits_made_during_a_sync_are_not_overwritten() {
    let (mut s, account, _, w) = setup();
    let mut server = Memory::new();
    server.edit(&format!("{CAL}r1.ics"), REMOTE);
    sync(&mut s, &account, &mut server);
    let t = find(&s, &w, "記事のレビュー");
    server.edit(
        &format!("{CAL}r1.ics"),
        &REMOTE.replace("SUMMARY:記事のレビュー", "SUMMARY:サーバー"),
    );
    let input = s.caldav_input(&account.id).unwrap();
    let out = malakt_caldav::sync(&mut server, &input);
    // 同期の間に、このアプリで優先度を変えた
    let mut d = TaskDraft::from(&s.task(&t.id).unwrap());
    d.priority = 2;
    s.update_task(&t.id, &d).unwrap();
    s.apply_caldav(&account.id, &out).unwrap();
    assert_eq!(s.task(&t.id).unwrap().title, "記事のレビュー");
    // 次の同期で、両方の変更がそろう
    sync(&mut s, &account, &mut server);
    let t = s.task(&t.id).unwrap();
    assert_eq!((t.title.as_str(), t.priority), ("サーバー", 2));
    assert!(server
        .ics(&format!("{CAL}r1.ics"))
        .unwrap()
        .contains("PRIORITY:2"));
}

#[test]
fn one_workspace_per_calendar() {
    let (mut s, account, _, w) = setup();
    let mut found = vec![Found {
        href: CAL.into(),
        name: "共有".into(),
        read_only: false,
    }];
    found.push(Found {
        href: "/dav/other/".into(),
        name: "ほか".into(),
        read_only: true,
    });
    s.refresh_caldav_calendars(&account.id, &[ME.to_owned()], &found)
        .unwrap();
    let cals = s.caldav_calendars(&account.id).unwrap();
    assert_eq!(cals.len(), 2);
    let other = cals.iter().find(|c| c.name == "ほか").unwrap();
    assert!(other.read_only);
    assert!(s.link_caldav_calendar(&other.id, Some(&w.id)).is_err());
    s.remove_caldav_account(&account.id).unwrap();
    assert!(s.caldav_accounts().unwrap().is_empty());
    assert!(s.caldav_calendar_of(&w.id).unwrap().is_none());
}

#[test]
fn local_people_get_stable_uris() {
    let id = new_id();
    let uuid = uuid_of(&id).unwrap();
    assert_eq!(uuid.len(), 36);
    assert_eq!(id_of_uuid(&uuid).as_deref(), Some(id.as_str()));
    assert_eq!(name_from_uri("mailto:bob@example.com"), "bob");
    assert_eq!(name_from_uri("https://id.example.com/users/42"), "42");
}
