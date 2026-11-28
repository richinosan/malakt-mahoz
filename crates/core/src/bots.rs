//! AI bot。名前・インストラクション・アイコン・色・動きを持ち、いくつでも作れる。
//!
//! 会話はどれか 1 つの bot と話す。bot のインストラクションは、その bot の会話の最初の指示に入る。
//! bot は 1 つ以上ある（最初の版のデータベースを開くと「AI bot」が 1 つできる。最後の 1 つは消せない）。

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::model::{new_id, now_ms, Id};
use crate::text::{normalize, normalize_line};
use crate::{Error, Result, Store};

/// 作れる bot の数。
pub const MAX_BOTS: usize = 30;
/// インストラクションの長さの上限。
pub const MAX_INSTRUCTIONS_CHARS: usize = 8000;
/// 名前の長さの上限。
pub const MAX_NAME_CHARS: usize = 40;
/// 覚えたことの長さの上限。あふれたら古いものから消す。
pub const MAX_MEMORY_CHARS: usize = 4000;
/// 名前を付けずに足した bot の名前。
pub const NEW_BOT_NAME: &str = "新しい bot";
/// アイコンを選んでいない bot のアイコン。
pub const DEFAULT_BOT_ICON: &str = "bot";

/// bot のアイコンの動き。考えている間と、会話が空のときに動く。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    /// 動かない。
    Still,
    /// 上下にゆっくり浮く。
    #[default]
    Float,
    /// 跳ねる。
    Bounce,
    /// 左右に傾く。
    Sway,
    /// 回る。
    Spin,
    /// 大きくなったり小さくなったりする。
    Pulse,
}

impl Motion {
    pub const ALL: [Motion; 6] = [
        Motion::Still,
        Motion::Float,
        Motion::Bounce,
        Motion::Sway,
        Motion::Spin,
        Motion::Pulse,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Motion::Still => "still",
            Motion::Float => "float",
            Motion::Bounce => "bounce",
            Motion::Sway => "sway",
            Motion::Spin => "spin",
            Motion::Pulse => "pulse",
        }
    }

    pub fn parse(text: &str) -> Option<Motion> {
        Motion::ALL.into_iter().find(|m| m.as_str() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            Motion::Still => "なし",
            Motion::Float => "ふわふわ",
            Motion::Bounce => "ぴょんぴょん",
            Motion::Sway => "ゆらゆら",
            Motion::Spin => "くるくる",
            Motion::Pulse => "どきどき",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiBot {
    pub id: Id,
    pub name: String,
    /// この bot への指示。会話の最初の指示に入る。
    #[serde(default)]
    pub instructions: String,
    /// アイコンの名前（ワークスペースと同じもの）。`None` なら [`DEFAULT_BOT_ICON`]。
    #[serde(default)]
    pub icon: Option<String>,
    /// アイコンの色（ワークスペースと同じ色の名前）。`None` なら文字と同じ色。
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub motion: Motion,
    /// bot が会話で覚えたこと（好みや直されたこと）。1 行に 1 つ。
    #[serde(default)]
    pub memory: String,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// 毎日この時刻（`HH:MM`、手元の時刻）に、bot から声をかける。`None` なら声をかけない。
    #[serde(default)]
    pub check_in: Option<String>,
}

impl AiBot {
    /// 出すアイコンの名前。
    pub fn icon_name(&self) -> &str {
        self.icon.as_deref().unwrap_or(DEFAULT_BOT_ICON)
    }

    /// 覚えたことを 1 行ずつ。
    pub fn memories(&self) -> impl Iterator<Item = &str> {
        self.memory.lines().filter(|l| !l.trim().is_empty())
    }
}

/// bot の書き換え。`None` の項目は変えない。
#[derive(Debug, Clone, Default)]
pub struct BotPatch {
    pub name: Option<String>,
    pub instructions: Option<String>,
    /// `Some(None)` でアイコンを最初のものに戻す。
    pub icon: Option<Option<String>>,
    /// `Some(None)` で色を外す。
    pub color: Option<Option<String>>,
    pub motion: Option<Motion>,
    /// 覚えたことを丸ごと置き換える。
    pub memory: Option<String>,
    /// `Some(None)` で声かけをやめる。
    pub check_in: Option<Option<String>>,
}

const COLUMNS: &str = "id, name, instructions, icon, color, motion, memory, position, \
                       created_at, updated_at, check_in";

fn from_row(r: &Row) -> rusqlite::Result<AiBot> {
    let motion: String = r.get(5)?;
    Ok(AiBot {
        id: r.get(0)?,
        name: r.get(1)?,
        instructions: r.get(2)?,
        icon: r.get(3)?,
        color: r.get(4)?,
        motion: Motion::parse(&motion).unwrap_or_default(),
        memory: r.get(6)?,
        position: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        check_in: r.get(10)?,
    })
}

fn clean_name(name: &str) -> Result<String> {
    let name = normalize_line(name);
    if name.is_empty() {
        return Err(Error::Invalid("bot の名前が空です".into()));
    }
    let count = name.chars().count();
    if count > MAX_NAME_CHARS {
        return Err(Error::Invalid(format!(
            "bot の名前は {MAX_NAME_CHARS} 文字までです（今は {count} 文字）"
        )));
    }
    Ok(name)
}

fn clean_instructions(text: &str) -> Result<String> {
    let text = normalize(text).trim().to_owned();
    let count = text.chars().count();
    if count > MAX_INSTRUCTIONS_CHARS {
        return Err(Error::Invalid(format!(
            "bot のインストラクションは {MAX_INSTRUCTIONS_CHARS} 文字までです（今は {count} 文字）"
        )));
    }
    Ok(text)
}

fn clean_icon(icon: Option<&str>) -> Option<String> {
    icon.map(str::trim)
        .filter(|i| !i.is_empty())
        .map(str::to_owned)
}

fn clean_color(color: Option<&str>) -> Result<Option<String>> {
    match color.map(str::trim).filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) if crate::model::WORKSPACE_COLORS.contains(&c) => Ok(Some(c.to_owned())),
        Some(c) => Err(Error::Invalid(format!(
            "bot の色「{c}」は使えません（{}）",
            crate::model::WORKSPACE_COLORS.join("、")
        ))),
    }
}

/// 声かけの時刻を `HH:MM` に整える。空なら声をかけない。
fn clean_check_in(time: Option<&str>) -> Result<Option<String>> {
    let Some(time) = time.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let digits = |t: &str| (1..=2).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit());
    let parsed = time
        .split_once(':')
        .filter(|(h, m)| digits(h) && digits(m))
        .and_then(|(h, m)| {
            let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
            (h < 24 && m < 60).then_some((h, m))
        });
    match parsed {
        Some((h, m)) => Ok(Some(format!("{h:02}:{m:02}"))),
        None => Err(Error::Invalid(format!(
            "声かけの時刻「{time}」は HH:MM（例: 09:00）で書いてください"
        ))),
    }
}

/// 覚えたことを 1 行ずつに整え、上限を超えたら古い行（上）から消す。
fn clean_memory(text: &str) -> String {
    let lines: Vec<String> = normalize(text)
        .lines()
        .map(normalize_line)
        .filter(|l| !l.is_empty())
        .collect();
    let mut total: usize = 0;
    let mut keep = lines.len();
    for (ix, line) in lines.iter().enumerate().rev() {
        total += line.chars().count() + 1;
        if total > MAX_MEMORY_CHARS {
            break;
        }
        keep = ix;
    }
    lines[keep..].join("\n")
}

/// 取り込む bot を整えて確かめる。
pub(crate) fn clean(bot: &AiBot) -> Result<AiBot> {
    if bot.id.is_empty() {
        return Err(Error::Invalid("bot の ID が空です".into()));
    }
    Ok(AiBot {
        name: clean_name(&bot.name)?,
        instructions: clean_instructions(&bot.instructions)?,
        icon: clean_icon(bot.icon.as_deref()),
        color: clean_color(bot.color.as_deref())?,
        memory: clean_memory(&bot.memory),
        check_in: clean_check_in(bot.check_in.as_deref())?,
        ..bot.clone()
    })
}

/// ID が同じものは上書きし、なければ足す（インポート用）。
pub(crate) fn save(conn: &Connection, bot: &AiBot) -> Result<()> {
    conn.prepare_cached(&format!(
        "INSERT INTO ai_bots({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
         instructions = excluded.instructions, icon = excluded.icon, color = excluded.color, \
         motion = excluded.motion, memory = excluded.memory, position = excluded.position, \
         created_at = excluded.created_at, updated_at = excluded.updated_at, \
         check_in = excluded.check_in"
    ))?
    .execute(params![
        bot.id,
        bot.name,
        bot.instructions,
        bot.icon,
        bot.color,
        bot.motion.as_str(),
        bot.memory,
        bot.position,
        bot.created_at,
        bot.updated_at,
        bot.check_in
    ])?;
    Ok(())
}

impl Store {
    /// すべての bot。並びは足した順。
    pub fn ai_bots(&self) -> Result<Vec<AiBot>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM ai_bots ORDER BY position, id"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ai_bot(&self, id: &str) -> Result<AiBot> {
        self.conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM ai_bots WHERE id = ?1"))?
            .query_row(params![id], from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("bot {id}")))
    }

    /// ID の bot。なければ（消した bot の会話など）最初の bot。
    pub fn ai_bot_or_first(&self, id: Option<&str>) -> Result<AiBot> {
        if let Some(bot) = id.and_then(|id| self.ai_bot(id).ok()) {
            return Ok(bot);
        }
        self.ai_bots()?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound("bot".into()))
    }

    /// bot を足す。名前が空なら「新しい bot」にする。
    pub fn create_ai_bot(&mut self, name: &str, instructions: &str) -> Result<AiBot> {
        let name = if name.trim().is_empty() {
            NEW_BOT_NAME.to_owned()
        } else {
            clean_name(name)?
        };
        let instructions = clean_instructions(instructions)?;
        let (count, last): (usize, Option<i64>) =
            self.conn
                .query_row("SELECT COUNT(*), MAX(position) FROM ai_bots", [], |r| {
                    Ok((r.get::<_, i64>(0)? as usize, r.get(1)?))
                })?;
        if count >= MAX_BOTS {
            return Err(Error::Invalid(format!("bot は {MAX_BOTS} 個までです")));
        }
        let now = now_ms();
        let bot = AiBot {
            id: new_id(),
            name,
            instructions,
            icon: None,
            color: None,
            motion: Motion::default(),
            memory: String::new(),
            position: last.map_or(0, |p| p + 1),
            created_at: now,
            updated_at: now,
            check_in: None,
        };
        save(&self.conn, &bot)?;
        Ok(bot)
    }

    pub fn update_ai_bot(&mut self, id: &str, patch: &BotPatch) -> Result<AiBot> {
        let mut bot = self.ai_bot(id)?;
        let before = bot.clone();
        if let Some(name) = &patch.name {
            bot.name = clean_name(name)?;
        }
        if let Some(text) = &patch.instructions {
            bot.instructions = clean_instructions(text)?;
        }
        if let Some(icon) = &patch.icon {
            bot.icon = clean_icon(icon.as_deref());
        }
        if let Some(color) = &patch.color {
            bot.color = clean_color(color.as_deref())?;
        }
        if let Some(motion) = patch.motion {
            bot.motion = motion;
        }
        if let Some(memory) = &patch.memory {
            bot.memory = clean_memory(memory);
        }
        if let Some(time) = &patch.check_in {
            bot.check_in = clean_check_in(time.as_deref())?;
        }
        if bot == before {
            return Ok(bot);
        }
        bot.updated_at = now_ms();
        save(&self.conn, &bot)?;
        Ok(bot)
    }

    /// 覚えたことを 1 つ足す。同じものがあれば足さない。上限を超えたら古いものから消す。
    pub fn remember_ai_bot(&mut self, id: &str, fact: &str) -> Result<AiBot> {
        let fact = normalize_line(fact);
        if fact.is_empty() {
            return Err(Error::Invalid("覚えることが空です".into()));
        }
        let bot = self.ai_bot(id)?;
        if bot.memories().any(|m| m == fact) {
            return Ok(bot);
        }
        let memory = if bot.memory.is_empty() {
            fact
        } else {
            format!("{}\n{fact}", bot.memory)
        };
        self.update_ai_bot(
            id,
            &BotPatch {
                memory: Some(memory),
                ..Default::default()
            },
        )
    }

    /// 覚えたことを 1 つ消す。`fact` と同じ行を消す。
    pub fn forget_ai_bot(&mut self, id: &str, fact: &str) -> Result<AiBot> {
        let fact = normalize_line(fact);
        let bot = self.ai_bot(id)?;
        if !bot.memories().any(|m| m == fact) {
            return Err(Error::NotFound(format!(
                "bot「{}」が覚えていること「{fact}」",
                bot.name
            )));
        }
        let memory: Vec<&str> = bot.memories().filter(|m| *m != fact).collect();
        self.update_ai_bot(
            id,
            &BotPatch {
                memory: Some(memory.join("\n")),
                ..Default::default()
            },
        )
    }

    /// bot を消す。最後の 1 つは消せない。
    pub fn delete_ai_bot(&mut self, id: &str) -> Result<()> {
        let bot = self.ai_bot(id)?;
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM ai_bots", [], |r| r.get(0))?;
        if count <= 1 {
            return Err(Error::Invalid(format!(
                "bot「{}」は最後の 1 つなので消せません",
                bot.name
            )));
        }
        self.conn
            .execute("DELETE FROM ai_bots WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// bot の並びを変える。`ids` の順にする（ない ID は後ろに残す）。
    pub fn reorder_ai_bots(&mut self, ids: &[String]) -> Result<()> {
        let all = self.ai_bots()?;
        let mut order: Vec<&str> = Vec::new();
        for id in ids {
            if !all.iter().any(|b| &b.id == id) {
                return Err(Error::NotFound(format!("bot {id}")));
            }
            if !order.contains(&id.as_str()) {
                order.push(id);
            }
        }
        for b in &all {
            if !order.contains(&b.id.as_str()) {
                order.push(&b.id);
            }
        }
        for (pos, id) in order.iter().enumerate() {
            self.conn.execute(
                "UPDATE ai_bots SET position = ?2 WHERE id = ?1 AND position <> ?2",
                params![id, pos as i64],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_database_has_one_bot_that_cannot_be_deleted() {
        let mut store = Store::open_in_memory().unwrap();
        let bots = store.ai_bots().unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].name, "AI bot");
        assert_eq!(bots[0].icon_name(), DEFAULT_BOT_ICON);
        assert!(store.delete_ai_bot(&bots[0].id).is_err());
        let second = store.create_ai_bot("", "").unwrap();
        assert_eq!(second.name, NEW_BOT_NAME);
        store.delete_ai_bot(&bots[0].id).unwrap();
        assert_eq!(store.ai_bots().unwrap(), vec![second.clone()]);
        assert_eq!(store.ai_bot_or_first(Some("消えた")).unwrap(), second);
    }

    #[test]
    fn check_in_times_are_cleaned_and_noticed() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store.ai_bots().unwrap()[0].id.clone();
        let set = |store: &mut Store, time: Option<&str>| {
            store.update_ai_bot(
                &id,
                &BotPatch {
                    check_in: Some(time.map(str::to_owned)),
                    ..Default::default()
                },
            )
        };
        assert_eq!(
            set(&mut store, Some("9:5")).unwrap().check_in.as_deref(),
            Some("09:05")
        );
        assert_eq!(
            set(&mut store, Some(" 18:30 "))
                .unwrap()
                .check_in
                .as_deref(),
            Some("18:30")
        );
        for bad in ["24:00", "12:60", "9時", "12:005"] {
            assert!(set(&mut store, Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(
            store.ai_bot(&id).unwrap().check_in.as_deref(),
            Some("18:30")
        );
        assert_eq!(set(&mut store, Some("")).unwrap().check_in, None);

        // 声かけの通知は、同じ鍵では 1 回だけ。押すと開く会話を持つ
        assert!(store
            .add_bot_check_in_notice("AI bot から", "今日は 2 件です", "chat-1", "check:1")
            .unwrap());
        assert!(!store
            .add_bot_check_in_notice("AI bot から", "今日は 2 件です", "chat-2", "check:1")
            .unwrap());
        let notices = store.notices(10).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, crate::notice::NoticeKind::BotCheckIn);
        assert_eq!(notices[0].headline(), "AI bot から");
        assert_eq!(notices[0].excerpt, "今日は 2 件です");
        assert_eq!(notices[0].chat_id.as_deref(), Some("chat-1"));
        assert_eq!(notices[0].task_id, None);
    }

    #[test]
    fn bots_are_changed_and_checked() {
        let mut store = Store::open_in_memory().unwrap();
        let bot = store.create_ai_bot(" 秘書 ", " 敬語で短く ").unwrap();
        assert_eq!(
            (bot.name.as_str(), bot.instructions.as_str()),
            ("秘書", "敬語で短く")
        );
        let changed = store
            .update_ai_bot(
                &bot.id,
                &BotPatch {
                    icon: Some(Some("cat".into())),
                    color: Some(Some("blue".into())),
                    motion: Some(Motion::Bounce),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(changed.icon_name(), "cat");
        assert_eq!(changed.color.as_deref(), Some("blue"));
        assert_eq!(changed.motion, Motion::Bounce);
        let bad = BotPatch {
            color: Some(Some("金色".into())),
            ..Default::default()
        };
        assert!(store.update_ai_bot(&bot.id, &bad).is_err());
        let empty = BotPatch {
            name: Some("  ".into()),
            ..Default::default()
        };
        assert!(store.update_ai_bot(&bot.id, &empty).is_err());
        let long = "あ".repeat(MAX_INSTRUCTIONS_CHARS + 1);
        assert!(store.create_ai_bot("長い", &long).is_err());
        let reset = BotPatch {
            icon: Some(None),
            color: Some(None),
            ..Default::default()
        };
        let reset = store.update_ai_bot(&bot.id, &reset).unwrap();
        assert_eq!(
            (reset.icon_name(), reset.color.as_deref()),
            (DEFAULT_BOT_ICON, None)
        );
    }

    #[test]
    fn memories_are_added_once_removed_and_trimmed_from_the_oldest() {
        let mut store = Store::open_in_memory().unwrap();
        let bot = store.create_ai_bot("秘書", "").unwrap();
        store
            .remember_ai_bot(&bot.id, "締め切りは金曜にまとめる")
            .unwrap();
        store
            .remember_ai_bot(&bot.id, " 締め切りは金曜にまとめる ")
            .unwrap();
        let bot = store.remember_ai_bot(&bot.id, "敬語は要らない").unwrap();
        assert_eq!(
            bot.memories().collect::<Vec<_>>(),
            ["締め切りは金曜にまとめる", "敬語は要らない"]
        );
        let bot = store
            .forget_ai_bot(&bot.id, "締め切りは金曜にまとめる")
            .unwrap();
        assert_eq!(bot.memory, "敬語は要らない");
        assert!(store.forget_ai_bot(&bot.id, "ない").is_err());
        let line = "あ".repeat(999);
        for _ in 0..5 {
            store
                .remember_ai_bot(&bot.id, &format!("{line}{}", 0))
                .unwrap();
        }
        let mut last = store.ai_bot(&bot.id).unwrap();
        for n in 1..6 {
            last = store
                .remember_ai_bot(&bot.id, &format!("{line}{n}"))
                .unwrap();
        }
        assert!(last.memory.chars().count() <= MAX_MEMORY_CHARS);
        // 新しいものが残る
        assert!(last.memory.ends_with(&format!("{line}5")));
        assert!(!last.memory.contains("敬語は要らない"));
    }

    #[test]
    fn bots_go_out_and_come_back_with_an_export_and_undo() {
        let mut store = Store::open_in_memory().unwrap();
        let bot = store.create_ai_bot("秘書", "敬語で").unwrap();
        let bot = store
            .update_ai_bot(
                &bot.id,
                &BotPatch {
                    icon: Some(Some("cat".into())),
                    color: Some(Some("pink".into())),
                    motion: Some(Motion::Spin),
                    memory: Some("朝は短く".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&store, &mut zip).unwrap();
        store.undo_mark().unwrap();
        store
            .update_ai_bot(
                &bot.id,
                &BotPatch {
                    instructions: Some("くだけた口調で".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        store.undo_mark().unwrap();
        assert!(store.undo().unwrap());
        assert_eq!(store.ai_bot(&bot.id).unwrap().instructions, "敬語で");

        let mut other = Store::open_in_memory().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut other, zip).unwrap();
        assert_eq!(other.ai_bot(&bot.id).unwrap(), bot);
    }

    #[test]
    fn bots_are_reordered() {
        let mut store = Store::open_in_memory().unwrap();
        let first = store.ai_bots().unwrap().remove(0);
        store.create_ai_bot("A", "").unwrap();
        let b = store.create_ai_bot("B", "").unwrap();
        store
            .reorder_ai_bots(&[b.id.clone(), first.id.clone()])
            .unwrap();
        let names: Vec<String> = store
            .ai_bots()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, ["B", "AI bot", "A"]);
        assert!(store.reorder_ai_bots(&["ない".into()]).is_err());
    }
}
