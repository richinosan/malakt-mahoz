//! 誰が変えたか。タスクの作成者・最終更新者、変更履歴の 1 件、コメントに付ける。
//!
//! データベースには 1 つの文字列で置く（[`Actor::as_string`]）。空は「記録する前の変更」で、
//! 誰がしたか分からない。

use serde::{Deserialize, Serialize};

use crate::model::Id;

/// 名前として残す文字数の上限。外から来る名前（MCP の相手の名前など）を切り詰める。
const MAX_NAME_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Actor {
    /// 記録する前の変更。
    Unknown,
    /// アプリの画面を使っている自分。
    #[default]
    Me,
    /// 完了チケットで報告した人。`None` は担当者を決めずに発行したチケット。
    Ticket(Option<Id>),
    /// アプリの中の AI。`name` は AI の名前（Claude など）、`run` は自動で始めた実行の種類
    /// （GitHub の定期チェックなど）。チャットで頼んだときは `None`。
    Ai { name: String, run: Option<String> },
    /// MCP でつないだ外のエージェント。名前は相手が名乗ったもの。
    Agent(String),
    /// `malakt-mahoz call` などのコマンド。
    Command,
    /// OS のリマインダーで完了にした・戻した自分。
    Reminders,
}

/// 名前から、改行などの制御文字を除き、長すぎる分を切る。
fn clean(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && *c != '|')
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Actor {
    pub fn ai(name: &str, run: Option<&str>) -> Self {
        Actor::Ai {
            name: clean(name),
            run: run.map(clean).filter(|r| !r.is_empty()),
        }
    }

    /// 外のエージェント。名乗らなかったら「外部エージェント」と出す。
    pub fn agent(name: &str) -> Self {
        Actor::Agent(clean(name))
    }

    pub fn as_string(&self) -> String {
        match self {
            Actor::Unknown => String::new(),
            Actor::Me => "me".into(),
            Actor::Ticket(person) => format!("ticket:{}", person.as_deref().unwrap_or_default()),
            Actor::Ai { name, run: None } => format!("ai:{name}"),
            Actor::Ai {
                name,
                run: Some(run),
            } => format!("ai:{name}|{run}"),
            Actor::Agent(name) => format!("mcp:{name}"),
            Actor::Command => "cli".into(),
            Actor::Reminders => "reminders".into(),
        }
    }

    /// 知らない形は、誰がしたか分からないものとして読む。
    pub fn parse(s: &str) -> Self {
        match s.split_once(':') {
            None => match s {
                "me" => Actor::Me,
                "cli" => Actor::Command,
                "reminders" => Actor::Reminders,
                _ => Actor::Unknown,
            },
            Some(("ticket", person)) => {
                Actor::Ticket((!person.is_empty()).then(|| person.to_owned()))
            }
            Some(("ai", rest)) => match rest.split_once('|') {
                Some((name, run)) => Actor::ai(name, Some(run)),
                None => Actor::ai(rest, None),
            },
            Some(("mcp", name)) => Actor::agent(name),
            _ => Actor::Unknown,
        }
    }

    pub fn is_unknown(&self) -> bool {
        *self == Actor::Unknown
    }

    /// 自分の操作か（画面、リマインダー）。
    pub fn is_me(&self) -> bool {
        matches!(self, Actor::Me | Actor::Reminders)
    }

    /// 人ではなく、AI や外のエージェント、コマンドか。
    pub fn is_machine(&self) -> bool {
        matches!(self, Actor::Ai { .. } | Actor::Agent(_) | Actor::Command)
    }

    /// 人として名前を出すときの担当者。自分なら `self_id`、完了チケットならその人。
    pub fn person<'a>(&'a self, self_id: Option<&'a str>) -> Option<&'a str> {
        match self {
            Actor::Me | Actor::Reminders => self_id,
            Actor::Ticket(person) => person.as_deref().or(self_id),
            _ => None,
        }
    }

    /// 人でないときに出す名前。
    pub fn machine_name(&self) -> Option<String> {
        match self {
            Actor::Ai { name, .. } if name.is_empty() => Some("AI".into()),
            Actor::Ai { name, .. } => Some(format!("AI（{name}）")),
            Actor::Agent(name) if name.is_empty() => Some("外部エージェント".into()),
            Actor::Agent(name) => Some(name.clone()),
            Actor::Command => Some("コマンド".into()),
            _ => None,
        }
    }

    /// 名前の横に付ける小さな印。どこから変えたか。
    pub fn badge(&self) -> Option<String> {
        match self {
            Actor::Ticket(_) => Some("完了チケット".into()),
            Actor::Ai { run: Some(run), .. } => Some(run.clone()),
            Actor::Ai { run: None, .. } => Some("AI".into()),
            Actor::Agent(_) => Some("外部エージェント".into()),
            Actor::Reminders => Some("リマインダー".into()),
            _ => None,
        }
    }

    /// 人でないときのアイコン。
    pub fn machine_icon(&self) -> &'static str {
        match self {
            Actor::Ai { .. } => "bot",
            Actor::Agent(_) => "plug",
            _ => "terminal",
        }
    }
}

impl Serialize for Actor {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Actor::parse(&String::deserialize(d)?))
    }
}

/// 読み込むときの既定。前の版のデータには誰がしたかの記録がない。
pub fn unknown() -> Actor {
    Actor::Unknown
}

impl rusqlite::ToSql for Actor {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_string().into())
    }
}

impl rusqlite::types::FromSql for Actor {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        Ok(Actor::parse(value.as_str()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_kind() {
        let all = [
            Actor::Unknown,
            Actor::Me,
            Actor::Ticket(None),
            Actor::Ticket(Some("p1".into())),
            Actor::ai("Claude", None),
            Actor::ai("Claude", Some("GitHub の定期チェック")),
            Actor::agent("claude-code"),
            Actor::agent(""),
            Actor::Command,
            Actor::Reminders,
        ];
        for actor in all {
            assert_eq!(Actor::parse(&actor.as_string()), actor, "{actor:?}");
            let json = serde_json::to_string(&actor).unwrap();
            assert_eq!(serde_json::from_str::<Actor>(&json).unwrap(), actor);
        }
    }

    #[test]
    fn unknown_text_is_unknown() {
        assert_eq!(Actor::parse(""), Actor::Unknown);
        assert_eq!(Actor::parse("someone"), Actor::Unknown);
        assert_eq!(Actor::parse("x:y"), Actor::Unknown);
    }

    #[test]
    fn outside_names_are_cleaned() {
        let long = "a".repeat(200);
        assert_eq!(Actor::agent(&long).as_string().len(), "mcp:".len() + 60);
        assert_eq!(Actor::agent("bad\nname|x"), Actor::Agent("badnamex".into()));
        // 区切りの | を名前に入れられない
        let ai = Actor::ai("A|B", Some("C"));
        assert_eq!(Actor::parse(&ai.as_string()), ai);
    }

    #[test]
    fn names_and_badges() {
        assert_eq!(Actor::Me.person(Some("me-id")), Some("me-id"));
        assert_eq!(
            Actor::Ticket(Some("p".into())).person(Some("me-id")),
            Some("p")
        );
        assert_eq!(Actor::Reminders.badge().as_deref(), Some("リマインダー"));
        assert_eq!(
            Actor::ai("Claude", None).machine_name().as_deref(),
            Some("AI（Claude）")
        );
        assert_eq!(
            Actor::ai("Claude", Some("GitHub の定期チェック"))
                .badge()
                .as_deref(),
            Some("GitHub の定期チェック")
        );
        assert_eq!(
            Actor::agent("").machine_name().as_deref(),
            Some("外部エージェント")
        );
        assert!(Actor::Unknown.person(Some("me")).is_none());
        assert!(Actor::Unknown.machine_name().is_none());
    }
}
