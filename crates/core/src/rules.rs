//! AI のルール。名前と本文を持ち、1 つずつ有効・無効を切り替えられる。
//!
//! ワークスペースを持たないルールはすべてのワークスペースで使う（グローバル）。
//! ワークスペースのルールは、そのワークスペースとサブワークスペースで使う。

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::model::{new_id, now_ms, Id};
use crate::text::{normalize, normalize_line};
use crate::{Error, Result, Store};

/// 1 つの場所（グローバル、またはワークスペース 1 つ）に置けるルールの数。
pub const MAX_RULES: usize = 50;
/// ルールの本文の長さの上限。
pub const MAX_RULE_CHARS: usize = 4000;
/// ルールの名前の長さの上限。
pub const MAX_NAME_CHARS: usize = 100;
/// 名前を付けずに足したルールの名前。
pub const NEW_RULE_NAME: &str = "新規ルール";

/// ルールを AI に渡す場面。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// チャットと自動の処理の両方。
    #[default]
    Always,
    /// ユーザーが始めたチャットだけ。
    Chat,
    /// アプリが始める処理（新しいタスクを整える、コメントから更新、定期チェック）だけ。
    Auto,
}

impl Apply {
    pub const ALL: [Apply; 3] = [Apply::Always, Apply::Chat, Apply::Auto];

    pub fn as_str(self) -> &'static str {
        match self {
            Apply::Always => "always",
            Apply::Chat => "chat",
            Apply::Auto => "auto",
        }
    }

    pub fn parse(text: &str) -> Option<Apply> {
        Apply::ALL.into_iter().find(|a| a.as_str() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            Apply::Always => "いつも",
            Apply::Chat => "チャット",
            Apply::Auto => "自動の処理",
        }
    }

    /// `auto` の実行（アプリが始めたもの）で、このルールを渡すか。
    pub fn applies(self, auto: bool) -> bool {
        match self {
            Apply::Always => true,
            Apply::Chat => !auto,
            Apply::Auto => auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRule {
    pub id: Id,
    /// なければグローバル。
    pub workspace_id: Option<Id>,
    pub name: String,
    pub body: String,
    pub enabled: bool,
    #[serde(default)]
    pub apply: Apply,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// ルールの書き換え。`None` の項目は変えない。
#[derive(Debug, Clone, Default)]
pub struct RulePatch {
    pub name: Option<String>,
    pub body: Option<String>,
    pub enabled: Option<bool>,
    pub apply: Option<Apply>,
}

const COLUMNS: &str =
    "id, workspace_id, name, body, enabled, apply, position, created_at, updated_at";

fn from_row(r: &Row) -> rusqlite::Result<AiRule> {
    let apply: String = r.get(5)?;
    Ok(AiRule {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        body: r.get(3)?,
        enabled: r.get(4)?,
        apply: Apply::parse(&apply).unwrap_or_default(),
        position: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// 名前と本文を整えて、長さを確かめる。
fn clean_name(name: &str) -> Result<String> {
    let name = normalize_line(name);
    if name.is_empty() {
        return Err(Error::Invalid("ルールの名前が空です".into()));
    }
    let count = name.chars().count();
    if count > MAX_NAME_CHARS {
        return Err(Error::Invalid(format!(
            "ルールの名前は {MAX_NAME_CHARS} 文字までです（今は {count} 文字）"
        )));
    }
    Ok(name)
}

fn clean_body(body: &str) -> Result<String> {
    let body = normalize(body).trim().to_owned();
    let count = body.chars().count();
    if count > MAX_RULE_CHARS {
        return Err(Error::Invalid(format!(
            "ルールは {MAX_RULE_CHARS} 文字までです（今は {count} 文字）"
        )));
    }
    Ok(body)
}

/// 取り込むルールを整えて確かめる。
pub(crate) fn clean(rule: &AiRule) -> Result<AiRule> {
    if rule.id.is_empty() {
        return Err(Error::Invalid("ルールの ID が空です".into()));
    }
    Ok(AiRule {
        name: clean_name(&rule.name)?,
        body: clean_body(&rule.body)?,
        ..rule.clone()
    })
}

/// ID が同じものは上書きし、なければ足す（インポート用）。
/// 上書きは UPDATE にして、元に戻すで前の中身に戻るようにする（REPLACE は削除のトリガーを通らない）。
pub(crate) fn save(conn: &Connection, rule: &AiRule) -> Result<()> {
    conn.prepare_cached(&format!(
        "INSERT INTO ai_rules({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(id) DO UPDATE SET workspace_id = excluded.workspace_id, \
         name = excluded.name, body = excluded.body, enabled = excluded.enabled, \
         apply = excluded.apply, position = excluded.position, \
         created_at = excluded.created_at, updated_at = excluded.updated_at"
    ))?
    .execute(params![
        rule.id,
        rule.workspace_id,
        rule.name,
        rule.body,
        rule.enabled,
        rule.apply.as_str(),
        rule.position,
        rule.created_at,
        rule.updated_at
    ])?;
    Ok(())
}

impl Store {
    /// その場所のルール。`None` ならグローバル。並びは足した順。
    pub fn ai_rules(&self, workspace: Option<&str>) -> Result<Vec<AiRule>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM ai_rules WHERE workspace_id IS ?1 ORDER BY position, id"
        ))?;
        let rows = stmt.query_map(params![workspace], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn all_ai_rules(&self) -> Result<Vec<AiRule>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM ai_rules ORDER BY position, id"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ai_rule(&self, id: &str) -> Result<AiRule> {
        self.conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM ai_rules WHERE id = ?1"))?
            .query_row(params![id], from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("ルール {id}")))
    }

    /// ルールを足す。名前が空なら「新規ルール」にする。
    pub fn create_ai_rule(
        &mut self,
        workspace: Option<&str>,
        name: &str,
        body: &str,
    ) -> Result<AiRule> {
        if let Some(id) = workspace {
            self.workspace(id)?;
        }
        let name = if name.trim().is_empty() {
            NEW_RULE_NAME.to_owned()
        } else {
            clean_name(name)?
        };
        let body = clean_body(body)?;
        let (count, last): (usize, Option<i64>) = self.conn.query_row(
            "SELECT COUNT(*), MAX(position) FROM ai_rules WHERE workspace_id IS ?1",
            params![workspace],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?)),
        )?;
        if count >= MAX_RULES {
            return Err(Error::Invalid(format!(
                "ルールは 1 つの場所に {MAX_RULES} 個までです"
            )));
        }
        let now = now_ms();
        let rule = AiRule {
            id: new_id(),
            workspace_id: workspace.map(str::to_owned),
            name,
            body,
            enabled: true,
            apply: Apply::Always,
            position: last.map_or(0, |p| p + 1),
            created_at: now,
            updated_at: now,
        };
        save(&self.conn, &rule)?;
        Ok(rule)
    }

    pub fn update_ai_rule(&mut self, id: &str, patch: &RulePatch) -> Result<AiRule> {
        let mut rule = self.ai_rule(id)?;
        let before = rule.clone();
        if let Some(name) = &patch.name {
            rule.name = clean_name(name)?;
        }
        if let Some(body) = &patch.body {
            rule.body = clean_body(body)?;
        }
        if let Some(enabled) = patch.enabled {
            rule.enabled = enabled;
        }
        if let Some(apply) = patch.apply {
            rule.apply = apply;
        }
        if rule == before {
            return Ok(rule);
        }
        rule.updated_at = now_ms();
        self.conn.execute(
            "UPDATE ai_rules SET name = ?2, body = ?3, enabled = ?4, apply = ?5, updated_at = ?6 \
             WHERE id = ?1",
            params![
                rule.id,
                rule.name,
                rule.body,
                rule.enabled,
                rule.apply.as_str(),
                rule.updated_at
            ],
        )?;
        Ok(rule)
    }

    pub fn delete_ai_rule(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM ai_rules WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound(format!("ルール {id}")));
        }
        Ok(())
    }

    /// そのワークスペースで使うルール。いちばん上の親から順に、ワークスペースの名前と一緒に返す。
    /// 無効なルールと、本文が空のルールは入れない。
    pub fn ai_rules_in(&self, workspace: &str) -> Result<Vec<(String, AiRule)>> {
        let mut out = Vec::new();
        for w in self.workspace_path(workspace)? {
            for rule in self.ai_rules(Some(&w.id))? {
                if rule.enabled && !rule.body.is_empty() {
                    out.push((w.name.clone(), rule));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undoing_an_import_brings_back_the_rule_it_overwrote() {
        let mut store = Store::open_in_memory().unwrap();
        let rule = store.create_ai_rule(None, "口調", "敬語で答える").unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&store, &mut zip).unwrap();
        let patch = RulePatch {
            body: Some("短く答える".into()),
            ..Default::default()
        };
        store.update_ai_rule(&rule.id, &patch).unwrap();
        store.undo_mark().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut store, zip).unwrap();
        assert_eq!(store.ai_rule(&rule.id).unwrap().body, "敬語で答える");
        store.undo_mark().unwrap();
        assert!(store.undo().unwrap());
        assert_eq!(store.ai_rule(&rule.id).unwrap().body, "短く答える");
    }

    #[test]
    fn rules_are_added_changed_and_removed_per_place() {
        let mut store = Store::open_in_memory().unwrap();
        let home = store.create_workspace(None, "家").unwrap();
        let first = store.create_ai_rule(None, "", "敬語で答える").unwrap();
        assert_eq!(first.name, NEW_RULE_NAME);
        assert!(first.enabled);
        let local = store.create_ai_rule(Some(&home.id), "買い物", "").unwrap();
        assert_eq!(store.ai_rules(None).unwrap(), vec![first.clone()]);
        assert_eq!(store.ai_rules(Some(&home.id)).unwrap(), vec![local.clone()]);
        let changed = store
            .update_ai_rule(
                &first.id,
                &RulePatch {
                    name: Some(" 言葉づかい ".into()),
                    enabled: Some(false),
                    apply: Some(Apply::Chat),
                    ..RulePatch::default()
                },
            )
            .unwrap();
        assert_eq!(changed.name, "言葉づかい");
        assert!(!changed.enabled);
        assert_eq!(changed.apply, Apply::Chat);
        assert_eq!(changed.body, "敬語で答える");
        // 名前を空にはできない。長すぎる本文は断る
        let empty = RulePatch {
            name: Some(" ".into()),
            ..RulePatch::default()
        };
        assert!(store.update_ai_rule(&first.id, &empty).is_err());
        let long = RulePatch {
            body: Some("あ".repeat(MAX_RULE_CHARS + 1)),
            ..RulePatch::default()
        };
        assert!(store.update_ai_rule(&first.id, &long).is_err());
        store.delete_ai_rule(&first.id).unwrap();
        assert!(store.ai_rules(None).unwrap().is_empty());
        assert!(store.delete_ai_rule(&first.id).is_err());
        // ワークスペースを消すと、そのルールも消える
        store.delete_workspace(&home.id).unwrap();
        assert!(store.ai_rule(&local.id).is_err());
    }

    #[test]
    fn a_sub_workspace_uses_the_rules_of_its_parents() {
        let mut store = Store::open_in_memory().unwrap();
        let work = store.create_workspace(None, "仕事").unwrap();
        let team = store.create_workspace(Some(&work.id), "チーム").unwrap();
        store
            .create_ai_rule(Some(&work.id), "", "PR は優先度を高")
            .unwrap();
        let off = store
            .create_ai_rule(Some(&team.id), "", "使わない")
            .unwrap();
        store
            .update_ai_rule(
                &off.id,
                &RulePatch {
                    enabled: Some(false),
                    ..RulePatch::default()
                },
            )
            .unwrap();
        store.create_ai_rule(Some(&team.id), "", "").unwrap();
        store
            .create_ai_rule(Some(&team.id), "", "期限は金曜")
            .unwrap();
        let used: Vec<(String, String)> = store
            .ai_rules_in(&team.id)
            .unwrap()
            .into_iter()
            .map(|(w, r)| (w, r.body))
            .collect();
        assert_eq!(
            used,
            vec![
                ("仕事".to_owned(), "PR は優先度を高".to_owned()),
                ("チーム".to_owned(), "期限は金曜".to_owned()),
            ]
        );
        assert_eq!(store.ai_rules_in(&work.id).unwrap().len(), 1);
    }

    #[test]
    fn one_place_holds_a_limited_number_of_rules() {
        let mut store = Store::open_in_memory().unwrap();
        for _ in 0..MAX_RULES {
            store.create_ai_rule(None, "", "").unwrap();
        }
        assert!(store.create_ai_rule(None, "", "").is_err());
        let home = store.create_workspace(None, "家").unwrap();
        store.create_ai_rule(Some(&home.id), "", "").unwrap();
    }

    #[test]
    fn ルールを渡す場面の名前と判定() {
        let names: Vec<&str> = Apply::ALL.iter().map(|a| a.label()).collect();
        assert_eq!(names, ["いつも", "チャット", "自動の処理"]);
        assert!(Apply::Always.applies(true) && Apply::Always.applies(false));
        assert!(Apply::Chat.applies(false) && !Apply::Chat.applies(true));
        assert!(Apply::Auto.applies(true) && !Apply::Auto.applies(false));
    }

    #[test]
    fn 名前と本文は上限ちょうどまで使える() {
        assert!(clean_name(&"あ".repeat(MAX_NAME_CHARS)).is_ok());
        assert!(clean_name(&"あ".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(clean_body(&"あ".repeat(MAX_RULE_CHARS)).is_ok());
        assert!(clean_body(&"あ".repeat(MAX_RULE_CHARS + 1)).is_err());
    }

    #[test]
    fn 取り込むルールは名前と本文を整える() {
        let mut store = Store::open_in_memory().unwrap();
        let rule = store.create_ai_rule(None, "口調", "敬語").unwrap();
        let dirty = AiRule {
            name: "  言葉づかい ".into(),
            body: " 短く答える \n".into(),
            ..rule
        };
        let cleaned = clean(&dirty).unwrap();
        assert_eq!(cleaned.name, "言葉づかい");
        assert_eq!(cleaned.body, "短く答える");
        assert_eq!(cleaned.id, dirty.id);
        let no_id = AiRule {
            id: String::new(),
            ..dirty
        };
        assert!(clean(&no_id).is_err());
    }

    #[test]
    fn 新しいルールは同じ場所の末尾に並ぶ() {
        let mut store = Store::open_in_memory().unwrap();
        let positions: Vec<i64> = (0..3)
            .map(|_| store.create_ai_rule(None, "", "").unwrap().position)
            .collect();
        assert_eq!(positions, [0, 1, 2]);
    }
}
