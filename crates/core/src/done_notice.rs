//! ほかの人が完了にしたとき（完了チケットでの報告）に、通知を作るかどうか。
//!
//! 全体のオン・オフ、知らせるとき（1 人ずつか、タスクが完了したときだけか）、
//! 完了にした人の役（RACI）、知らせない担当者、知らせないワークスペース
//! （サブワークスペースも含む）で決める。設定は `settings` の 1 行に JSON で置く。

use serde_json::{json, Value};

use crate::model::*;
use crate::{Result, Store};

/// 設定を置く鍵。標準のままなら行を置かない。
const KEY: &str = "notice.done";

/// いつ知らせるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum When {
    /// 1 人が自分の分を完了にするたび。
    #[default]
    Each,
    /// タスクが完了になったときだけ（最後の 1 人がそろったときと、タスク全体の報告）。
    Finished,
}

impl When {
    pub const ALL: [When; 2] = [When::Each, When::Finished];

    pub fn code(self) -> &'static str {
        match self {
            When::Each => "each",
            When::Finished => "finished",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.code() == code)
    }

    pub fn label(self) -> &'static str {
        match self {
            When::Each => "1 人ずつ",
            When::Finished => "タスクが完了したとき",
        }
    }
}

/// ほかの人の完了を知らせるかの設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneNotices {
    pub enabled: bool,
    pub when: When,
    /// 完了にした人がこのどれかの役なら知らせる。
    pub roles: Roles,
    /// 知らせない担当者。
    pub muted_people: Vec<Id>,
    /// 知らせないワークスペース。サブワークスペースのタスクも知らせない。
    pub muted_workspaces: Vec<Id>,
}

/// R・A・C・I のすべて。
pub fn all_roles() -> Roles {
    Roles::of(&Role::ALL)
}

impl Default for DoneNotices {
    fn default() -> Self {
        DoneNotices {
            enabled: true,
            when: When::Each,
            roles: all_roles(),
            muted_people: Vec::new(),
            muted_workspaces: Vec::new(),
        }
    }
}

impl DoneNotices {
    pub fn is_standard(&self) -> bool {
        *self == DoneNotices::default()
    }

    /// 通知を作るか。`person` は完了にした人、`finished` はタスクが完了になったか、
    /// `places` はタスクのワークスペースと、その上の親すべての ID。
    pub fn wants(&self, task: &Task, person: &str, finished: bool, places: &[Id]) -> bool {
        if !self.enabled || (self.when == When::Finished && !finished) {
            return false;
        }
        if self.muted_people.iter().any(|p| p == person) {
            return false;
        }
        if places
            .iter()
            .any(|w| self.muted_workspaces.iter().any(|m| m == w))
        {
            return false;
        }
        match task.roles_of(person) {
            Some(roles) => roles.iter().any(|r| self.roles.contains(r)),
            // 担当者でない人の報告は、役で絞っていないときだけ知らせる
            None => self.roles == all_roles(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "when": self.when.code(),
            "roles": self.roles.iter().map(Role::code).collect::<Vec<_>>(),
            "muted_people": self.muted_people,
            "muted_workspaces": self.muted_workspaces,
        })
    }

    /// 読めない項目は標準のままにする（前の版や手で書いた値でも通知を止めない）。
    pub fn from_json(value: &Value) -> Self {
        let mut out = DoneNotices::default();
        if let Some(on) = value.get("enabled").and_then(Value::as_bool) {
            out.enabled = on;
        }
        if let Some(when) = value
            .get("when")
            .and_then(Value::as_str)
            .and_then(When::parse)
        {
            out.when = when;
        }
        if let Some(items) = value.get("roles").and_then(Value::as_array) {
            out.roles = Roles::of(
                &items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(Role::parse)
                    .collect::<Vec<_>>(),
            );
        }
        let ids = |key: &str| -> Vec<Id> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        out.muted_people = ids("muted_people");
        out.muted_workspaces = ids("muted_workspaces");
        out
    }
}

/// 同じものを 2 回入れない。順は最初に出たもの。
fn dedupe(ids: &mut Vec<Id>) {
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
}

impl Store {
    /// ほかの人の完了を知らせるかの設定。消えた担当者・ワークスペースは除いて返す。
    pub fn done_notices(&self) -> Result<DoneNotices> {
        let mut out = match self.setting(KEY)? {
            Some(text) => match serde_json::from_str::<Value>(&text) {
                Ok(value) => DoneNotices::from_json(&value),
                Err(_) => DoneNotices::default(),
            },
            None => DoneNotices::default(),
        };
        if !out.muted_people.is_empty() {
            let people = self.people()?;
            out.muted_people
                .retain(|id| people.iter().any(|p| &p.id == id));
        }
        if !out.muted_workspaces.is_empty() {
            let workspaces = self.workspaces()?;
            out.muted_workspaces
                .retain(|id| workspaces.iter().any(|w| &w.id == id));
        }
        Ok(out)
    }

    /// 設定を変える。知らない担当者・ワークスペースの ID はエラーにする。
    pub fn set_done_notices(&mut self, notices: &DoneNotices) -> Result<()> {
        let mut notices = notices.clone();
        dedupe(&mut notices.muted_people);
        dedupe(&mut notices.muted_workspaces);
        if notices.roles.is_empty() {
            return Err(crate::Error::Invalid(
                "知らせる役を 1 つ以上選んでください".into(),
            ));
        }
        let people = self.people()?;
        if let Some(id) = notices
            .muted_people
            .iter()
            .find(|id| !people.iter().any(|p| &p.id == *id))
        {
            return Err(crate::Error::NotFound(format!("担当者 {id}")));
        }
        let workspaces = self.workspaces()?;
        if let Some(id) = notices
            .muted_workspaces
            .iter()
            .find(|id| !workspaces.iter().any(|w| &w.id == *id))
        {
            return Err(crate::Error::NotFound(format!("ワークスペース {id}")));
        }
        if notices.is_standard() {
            self.set_setting(KEY, None)
        } else {
            self.set_setting(KEY, Some(&notices.to_json().to_string()))
        }
    }

    /// `person` が `task` を完了にした（`finished` はタスクごと完了になった）ことを知らせるか。
    pub(crate) fn wants_done_notice(
        &self,
        task: &Task,
        person: &str,
        finished: bool,
    ) -> Result<bool> {
        let notices = self.done_notices()?;
        let places = match &task.workspace_id {
            Some(w) => self.workspace_path(w)?.into_iter().map(|w| w.id).collect(),
            None => Vec::new(),
        };
        Ok(notices.wants(task, person, finished, &places))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::NoticeKind;

    struct Setup {
        s: Store,
        sato: Person,
        suzuki: Person,
        event: Workspace,
        booth: Workspace,
        task: Task,
    }

    /// 自分（R）・佐藤（R）・鈴木（A）が担当の、「イベント > 受付」のタスク。
    fn setup() -> Setup {
        let mut s = Store::open_in_memory().unwrap();
        let me = s.create_person("自分", true).unwrap();
        let sato = s.create_person("佐藤", false).unwrap();
        let suzuki = s.create_person("鈴木", false).unwrap();
        let event = s.create_workspace(None, "イベント").unwrap();
        let booth = s.create_workspace(Some(&event.id), "受付").unwrap();
        let task = s
            .create_task(
                Some(&booth.id),
                &TaskDraft {
                    title: "受付の準備".into(),
                    assignee_ids: vec![me.id.clone(), sato.id.clone(), suzuki.id.clone()],
                    ..Default::default()
                },
            )
            .unwrap();
        let task = s
            .set_assignee_roles(&task.id, &suzuki.id, Roles::of(&[Role::A]))
            .unwrap();
        Setup {
            s,
            sato,
            suzuki,
            event,
            booth,
            task,
        }
    }

    /// その人の完了チケットで報告して、できた「完了にした」通知の数を返す。
    fn report(s: &mut Store, task: &Task, person: &Person) -> usize {
        let before = done_count(s);
        let ticket = s.issue_ticket(&task.id, Some(&person.id)).unwrap();
        s.redeem_ticket(&ticket.secret, "終わりました").unwrap();
        done_count(s) - before
    }

    fn done_count(s: &Store) -> usize {
        s.notices(100)
            .unwrap()
            .iter()
            .filter(|n| n.kind == NoticeKind::StatusChanged)
            .count()
    }

    #[test]
    fn standard_tells_every_report() {
        let Setup {
            mut s,
            sato,
            suzuki,
            task,
            ..
        } = setup();
        assert!(s.done_notices().unwrap().is_standard());
        assert_eq!(report(&mut s, &task, &sato), 1);
        assert_eq!(report(&mut s, &task, &suzuki), 1);
    }

    #[test]
    fn turning_off_stops_done_notices_but_not_mentions() {
        let Setup {
            mut s, sato, task, ..
        } = setup();
        s.set_done_notices(&DoneNotices {
            enabled: false,
            ..Default::default()
        })
        .unwrap();
        let ticket = s.issue_ticket(&task.id, Some(&sato.id)).unwrap();
        s.redeem_ticket(&ticket.secret, "@自分 終わりました")
            .unwrap();
        let kinds: Vec<NoticeKind> = s.notices(10).unwrap().iter().map(|n| n.kind).collect();
        assert_eq!(kinds, [NoticeKind::Mentioned]);
        // 報告そのもの（自分の分の完了）は残る
        assert!(s.task(&task.id).unwrap().is_done_by(&sato.id));
    }

    #[test]
    fn only_when_the_task_is_finished() {
        let Setup {
            mut s,
            sato,
            suzuki,
            task,
            ..
        } = setup();
        s.set_done_notices(&DoneNotices {
            when: When::Finished,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(report(&mut s, &task, &sato), 0);
        let me = s.people().unwrap().into_iter().find(|p| p.is_self).unwrap();
        s.set_assignee_done(&task.id, &me.id, true).unwrap();
        // 最後の 1 人でタスクが完了になったときだけ知らせる
        assert_eq!(report(&mut s, &task, &suzuki), 1);
        assert_eq!(s.task(&task.id).unwrap().status, Status::Completed);
    }

    #[test]
    fn by_role() {
        let Setup {
            mut s,
            sato,
            suzuki,
            task,
            ..
        } = setup();
        // 説明責任者（A）の完了だけ
        s.set_done_notices(&DoneNotices {
            roles: Roles::of(&[Role::A]),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(report(&mut s, &task, &sato), 0);
        assert_eq!(report(&mut s, &task, &suzuki), 1);
    }

    #[test]
    fn by_person() {
        let Setup {
            mut s,
            sato,
            suzuki,
            task,
            ..
        } = setup();
        s.set_done_notices(&DoneNotices {
            muted_people: vec![sato.id.clone()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(report(&mut s, &task, &sato), 0);
        assert_eq!(report(&mut s, &task, &suzuki), 1);
    }

    #[test]
    fn by_workspace_with_sub_workspaces() {
        let Setup {
            mut s,
            sato,
            suzuki,
            event,
            booth,
            task,
        } = setup();
        // 親のワークスペースを外すと、サブワークスペースのタスクも知らせない
        s.set_done_notices(&DoneNotices {
            muted_workspaces: vec![event.id.clone()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(report(&mut s, &task, &sato), 0);
        // 外していないワークスペースへ移すと知らせる
        let other = s.create_workspace(None, "ほか").unwrap();
        s.move_task(&task.id, Some(&other.id)).unwrap();
        let task = s.task(&task.id).unwrap();
        assert_eq!(report(&mut s, &task, &suzuki), 1);
        // サブワークスペースだけを外しても、親のほかのタスクは知らせる
        s.set_done_notices(&DoneNotices {
            muted_workspaces: vec![booth.id.clone()],
            ..Default::default()
        })
        .unwrap();
        assert!(s.wants_done_notice(&task, &sato.id, false).unwrap());
    }

    #[test]
    fn saving_checks_and_cleans() {
        let Setup {
            mut s, sato, booth, ..
        } = setup();
        assert!(s
            .set_done_notices(&DoneNotices {
                roles: Roles::NONE,
                ..Default::default()
            })
            .is_err());
        assert!(s
            .set_done_notices(&DoneNotices {
                muted_people: vec!["ない".into()],
                ..Default::default()
            })
            .is_err());
        assert!(s
            .set_done_notices(&DoneNotices {
                muted_workspaces: vec!["ない".into()],
                ..Default::default()
            })
            .is_err());
        let wanted = DoneNotices {
            when: When::Finished,
            roles: Roles::of(&[Role::R, Role::A]),
            muted_people: vec![sato.id.clone(), sato.id.clone()],
            muted_workspaces: vec![booth.id.clone()],
            ..Default::default()
        };
        s.set_done_notices(&wanted).unwrap();
        let read = s.done_notices().unwrap();
        assert_eq!(read.muted_people, [sato.id.clone()]);
        assert_eq!(read.when, When::Finished);
        assert_eq!(read.roles, Roles::of(&[Role::R, Role::A]));
        // JSON を通しても同じ
        assert_eq!(DoneNotices::from_json(&read.to_json()), read);
        // 消えた担当者・ワークスペースは読むときに除く
        s.delete_person(&sato.id).unwrap();
        s.delete_workspace(&booth.id).unwrap();
        let read = s.done_notices().unwrap();
        assert!(read.muted_people.is_empty() && read.muted_workspaces.is_empty());
        // 標準に戻すと行を置かない
        s.set_done_notices(&DoneNotices::default()).unwrap();
        assert_eq!(s.setting(KEY).unwrap(), None);
        // 壊れた値でも、標準として読む
        s.set_setting(KEY, Some("{")).unwrap();
        assert!(s.done_notices().unwrap().is_standard());
    }
}
