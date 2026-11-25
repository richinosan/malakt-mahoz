//! OS のリマインダーとの連携のうち、データベースに関わる部分。
//!
//! いま OS にあってほしいもの（[`Snapshot`]）をタスクから作り、OS に作ったものの控え（[`Link`]）を持つ。
//! OS とのやり取りと、何を作り・直し・消すかの判断は `malakt-reminders` にある。

use std::collections::{HashMap, HashSet};

pub use malakt_reminders::{sync, Item, Kind, Link, List, Memory, Os, Outcome, Seen, Snapshot};
use rusqlite::params;

use crate::model::*;
use crate::{Result, Store};

/// 連携するかどうかの設定の鍵。値は `"1"`。
pub const SETTING: &str = "reminders.enabled";

/// どのワークスペースにも入れていないタスクを入れるリストの名前。
pub const UNFILED_LIST: &str = "Malakt Mahoz";

/// リマインダーのメモに写す説明の長さ（文字数）。
const NOTES_CHARS: usize = 2000;

/// 時刻のない期限を知らせる時刻（時）。
const ALL_DAY_HOUR: u32 = 9;

/// 期限を知らせる時刻（UNIX 秒、端末の時刻帯）。
fn fire_at(due: &str) -> Option<i64> {
    use chrono::TimeZone;
    let date = chrono::NaiveDate::parse_from_str(date_of(due), "%Y-%m-%d").ok()?;
    let time = match time_of(due) {
        Some(time) => chrono::NaiveTime::parse_from_str(time, "%H:%M").ok()?,
        None => chrono::NaiveTime::from_hms_opt(ALL_DAY_HOUR, 0, 0)?,
    };
    chrono::Local
        .from_local_datetime(&date.and_time(time))
        .earliest()
        .map(|t| t.timestamp())
}

impl Store {
    /// 連携が有効か。
    pub fn reminders_enabled(&self) -> Result<bool> {
        Ok(self.setting(SETTING)?.as_deref() == Some("1"))
    }

    pub fn set_reminders_enabled(&mut self, on: bool) -> Result<()> {
        self.set_setting(SETTING, on.then_some("1"))
    }

    /// いま OS にあってほしいリストとリマインダー。
    ///
    /// 未完了のタスクすべてと、前にリマインダーを作ったタスクのうち完了になったもの。
    /// 中止・削除したタスクは入らないので、合わせるとリマインダーが消える。
    pub fn reminder_snapshot(&self) -> Result<Snapshot> {
        let workspaces = self.workspaces()?;
        let by_id: HashMap<&str, &Workspace> =
            workspaces.iter().map(|w| (w.id.as_str(), w)).collect();
        let path = |id: &str| {
            let mut names = Vec::new();
            let mut at = by_id.get(id).copied();
            while let Some(w) = at {
                names.push(w.name.as_str());
                at = w.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
            }
            names.reverse();
            names.join(" / ")
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT t.id, t.workspace_id, t.title, t.description, t.status, t.priority, t.due \
             FROM tasks t \
             WHERE t.status IN ('needs_action', 'in_process') \
                OR (t.status = 'completed' AND EXISTS (SELECT 1 FROM reminder_links l \
                    WHERE l.kind = 'item' AND l.key = t.id)) \
             ORDER BY t.id",
        )?;
        let mut snapshot = Snapshot::default();
        let mut seen = HashSet::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let list: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let description: String = row.get(3)?;
            let status: String = row.get(4)?;
            let due: Option<String> = row.get(6)?;
            if seen.insert(list.clone()) {
                snapshot.lists.push(List {
                    name: if list.is_empty() {
                        UNFILED_LIST.to_owned()
                    } else {
                        path(&list)
                    },
                    key: list.clone(),
                });
            }
            snapshot.items.push(Item {
                task_id: row.get(0)?,
                list,
                title: row.get(2)?,
                notes: description.chars().take(NOTES_CHARS).collect(),
                fire_at: due.as_deref().and_then(fire_at),
                due,
                priority: row.get(5)?,
                done: status == "completed",
            });
        }
        snapshot.lists.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(snapshot)
    }

    /// OS に作ったものの控え。
    pub fn reminder_links(&self) -> Result<Vec<Link>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT kind, key, external, hash, done FROM reminder_links ORDER BY kind, key",
        )?;
        let links = stmt
            .query_map([], |r| {
                Ok(Link {
                    kind: if r.get::<_, String>(0)? == "list" {
                        Kind::List
                    } else {
                        Kind::Item
                    },
                    key: r.get(1)?,
                    external: r.get(2)?,
                    hash: r.get(3)?,
                    done: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(links)
    }

    /// 控えを丸ごと置き換える。
    pub fn replace_reminder_links(&mut self, links: &[Link]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM reminder_links", [])?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO reminder_links(kind, key, external, hash, done) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for link in links {
                insert.execute(params![
                    link.kind.as_str(),
                    link.key,
                    link.external,
                    link.hash,
                    link.done
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(title: &str, due: Option<&str>) -> TaskDraft {
        TaskDraft {
            title: title.into(),
            due: due.map(Into::into),
            ..Default::default()
        }
    }

    /// 1 回合わせて、結果をストアに反映する（アプリがすることと同じ）。
    fn run(store: &mut Store, os: &mut Memory) -> Outcome {
        let out = sync(
            os,
            &store.reminder_snapshot().unwrap(),
            &store.reminder_links().unwrap(),
        );
        store.replace_reminder_links(&out.links).unwrap();
        for id in &out.completed {
            store.set_task_status(id, Status::Completed).unwrap();
        }
        for id in &out.reopened {
            store.set_task_status(id, Status::NeedsAction).unwrap();
        }
        out
    }

    #[test]
    fn tasks_become_reminders_in_a_list_per_workspace() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        let event = store.create_workspace(None, "冬のイベント").unwrap();
        let venue = store.create_workspace(Some(&event.id), "会場").unwrap();
        let mut d = draft("机を借りる", Some("2026-12-20 18:30"));
        d.priority = 1;
        d.description = "受付に電話する".into();
        store.create_task(Some(&venue.id), &d).unwrap();
        store
            .create_task(None, &draft("牛乳を買う", Some("2026-12-01")))
            .unwrap();
        store.create_task(None, &draft("いつかやる", None)).unwrap();

        let out = run(&mut store, &mut os);
        assert!(out.errors.is_empty());
        assert_eq!(os.titles_in("冬のイベント / 会場"), ["机を借りる"]);
        assert_eq!(os.titles_in(UNFILED_LIST).len(), 2);
        // タスクのないワークスペースのリストは作らない
        assert_eq!(os.lists.len(), 2);

        let desk = os.find("机を借りる").unwrap();
        assert_eq!(desk.due_parts(), Some((2026, 12, 20, Some((18, 30)))));
        assert_eq!((desk.priority, desk.notes.as_str()), (1, "受付に電話する"));
        let milk = os.find("牛乳を買う").unwrap();
        assert_eq!(milk.due_parts(), Some((2026, 12, 1, None)));
        // 時刻のない期限は、その日の 9 時に知らせる
        assert_eq!(
            milk.fire_at.unwrap() + 9 * 3600 + 30 * 60,
            os.find("机を借りる").unwrap().fire_at.unwrap() - 19 * 86400
        );
        assert_eq!(os.find("いつかやる").unwrap().due_parts(), None);
    }

    #[test]
    fn nothing_is_rewritten_when_nothing_changed() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        store
            .create_task(None, &draft("牛乳を買う", Some("2026-12-01")))
            .unwrap();
        run(&mut store, &mut os);
        let writes = os.writes;
        let before = store.reminder_links().unwrap();
        let out = run(&mut store, &mut os);
        assert_eq!(os.writes, writes);
        assert_eq!(out.links, before);
    }

    #[test]
    fn edits_moves_and_renames_follow() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        let a = store.create_workspace(None, "仕事").unwrap();
        let task = store
            .create_task(None, &draft("資料を作る", Some("2026-12-01")))
            .unwrap();
        run(&mut store, &mut os);

        let mut d = draft("資料を仕上げる", Some("2026-12-03 10:00"));
        d.status = task.status;
        store.update_task(&task.id, &d).unwrap();
        store.move_task(&task.id, Some(&a.id)).unwrap();
        run(&mut store, &mut os);
        assert_eq!(os.items.len(), 1);
        assert_eq!(os.titles_in("仕事"), ["資料を仕上げる"]);
        assert_eq!(
            os.find("資料を仕上げる").unwrap().due.as_deref(),
            Some("2026-12-03 10:00")
        );
        // 空になった「未所属」のリストは消える
        assert_eq!(os.lists.len(), 1);

        store.rename_workspace(&a.id, "しごと").unwrap();
        run(&mut store, &mut os);
        assert_eq!(os.titles_in("しごと"), ["資料を仕上げる"]);
        assert_eq!(os.lists.len(), 1);
    }

    #[test]
    fn completion_goes_both_ways() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        let a = store.create_task(None, &draft("A", None)).unwrap();
        let b = store.create_task(None, &draft("B", None)).unwrap();
        run(&mut store, &mut os);

        // アプリで完了にすると、リマインダーも完了になる
        store.set_task_status(&a.id, Status::Completed).unwrap();
        // リマインダーで完了にすると、タスクも完了になる
        os.set_done("B", true);
        let out = run(&mut store, &mut os);
        assert_eq!(out.completed, [b.id.clone()]);
        assert!(os.find("A").unwrap().done);
        assert_eq!(store.task(&b.id).unwrap().status, Status::Completed);

        // 落ち着いたら、もう何も書かない
        let writes = os.writes;
        let out = run(&mut store, &mut os);
        assert!(out.completed.is_empty() && out.reopened.is_empty());
        assert_eq!(os.writes, writes);

        // 完了を外すのも両方向
        store.set_task_status(&a.id, Status::NeedsAction).unwrap();
        os.set_done("B", false);
        let out = run(&mut store, &mut os);
        assert_eq!(out.reopened, [b.id.clone()]);
        assert!(!os.find("A").unwrap().done);
        assert_eq!(store.task(&b.id).unwrap().status, Status::NeedsAction);
    }

    #[test]
    fn cancelled_and_deleted_tasks_lose_their_reminders() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        let a = store.create_task(None, &draft("A", None)).unwrap();
        let b = store.create_task(None, &draft("B", None)).unwrap();
        store.create_task(None, &draft("C", None)).unwrap();
        run(&mut store, &mut os);
        store.set_task_status(&a.id, Status::Cancelled).unwrap();
        store.delete_task(&b.id).unwrap();
        run(&mut store, &mut os);
        assert_eq!(os.titles_in(UNFILED_LIST), ["C"]);
        assert_eq!(store.reminder_links().unwrap().len(), 2);
    }

    #[test]
    fn a_reminder_deleted_outside_comes_back_and_the_task_stays() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        let a = store.create_task(None, &draft("A", None)).unwrap();
        let b = store.create_task(None, &draft("B", None)).unwrap();
        run(&mut store, &mut os);
        store.set_task_status(&b.id, Status::Completed).unwrap();
        run(&mut store, &mut os);
        os.items.clear();
        run(&mut store, &mut os);
        assert_eq!(store.task(&a.id).unwrap().status, Status::NeedsAction);
        // 未完了のものだけ作り直す
        assert_eq!(os.titles_in(UNFILED_LIST), ["A"]);
    }

    #[test]
    fn a_list_with_other_reminders_is_kept() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        let a = store.create_task(None, &draft("A", None)).unwrap();
        run(&mut store, &mut os);
        // 利用者が、同じリストに自分でリマインダーを足した
        let list = os.lists.keys().next().unwrap().clone();
        let mine = Item {
            task_id: String::new(),
            list: String::new(),
            title: "自分で足した".into(),
            notes: String::new(),
            due: None,
            fire_at: None,
            priority: 0,
            done: false,
        };
        os.items.insert("mine".into(), (list, mine));
        store.delete_task(&a.id).unwrap();
        run(&mut store, &mut os);
        assert_eq!(os.titles_in(UNFILED_LIST), ["自分で足した"]);
        assert!(store.reminder_links().unwrap().is_empty());
    }

    #[test]
    fn a_failure_names_the_task_and_is_retried() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        os.fail_title = Some("B".into());
        store.create_task(None, &draft("A", None)).unwrap();
        store.create_task(None, &draft("B", None)).unwrap();
        let out = run(&mut store, &mut os);
        assert_eq!(out.errors, ["「B」: 保存できませんでした"]);
        assert_eq!(os.titles_in(UNFILED_LIST), ["A"]);
        os.fail_title = None;
        let out = run(&mut store, &mut os);
        assert!(out.errors.is_empty());
        assert_eq!(os.items.len(), 2);
    }

    #[test]
    fn turning_it_off_removes_what_was_made() {
        let mut store = Store::open_in_memory().unwrap();
        let mut os = Memory::default();
        store.create_task(None, &draft("A", None)).unwrap();
        run(&mut store, &mut os);
        let out = sync(
            &mut os,
            &Snapshot::default(),
            &store.reminder_links().unwrap(),
        );
        assert!(out.links.is_empty() && os.items.is_empty() && os.lists.is_empty());
    }

    #[test]
    fn enabled_is_off_until_chosen() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(!store.reminders_enabled().unwrap());
        store.set_reminders_enabled(true).unwrap();
        assert!(store.reminders_enabled().unwrap());
        store.set_reminders_enabled(false).unwrap();
        assert!(!store.reminders_enabled().unwrap());
    }
}
