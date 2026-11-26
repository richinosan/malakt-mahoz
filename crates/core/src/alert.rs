//! 期限の通知のタイミング（期限の何分前・何日前に知らせるか）。
//!
//! 時刻つきの期限と日付だけの期限で、選べるものと標準が別にある。全体の標準のほかに、
//! タスクごとに上書きできる。アプリの通知（[`Store::sweep_due_alerts`]）と、
//! OS のリマインダーとの連携（[`crate::remind`]）が同じタイミングを使う。

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::{Error, Result, Store};

/// 時刻つきの期限で選べるもの（期限の何分前か）。0 は期限ちょうど。
pub const TIMED_CHOICES: [u32; 10] = [0, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080];
/// 日付だけの期限で選べるもの（期限の何日前か）。0 は当日。
pub const DATE_CHOICES: [u32; 4] = [0, 1, 2, 7];
/// 日付だけの期限を知らせる時刻（時）で選べるもの。
pub const HOUR_CHOICES: [u32; 8] = [0, 6, 7, 8, 9, 10, 12, 18];

/// 時刻つきの期限の標準。期限ちょうどに知らせる。
pub const STANDARD_TIMED: &[u32] = &[0];
/// 日付だけの期限の標準。当日に知らせる。
pub const STANDARD_DATE: &[u32] = &[0];
/// 日付だけの期限を知らせる時刻の標準（時）。
pub const STANDARD_HOUR: u32 = 9;

/// 何分前まで選べるか（4 週間）。
pub const MAX_MINUTES: u32 = 28 * 24 * 60;
/// 何日前まで選べるか。
pub const MAX_DAYS: u32 = 28;
/// 1 つの期限に付けられる通知の数。
pub const MAX_ALERTS: usize = 10;

const TIMED_KEY: &str = "due_alerts.timed";
const DATE_KEY: &str = "due_alerts.date";
const HOUR_KEY: &str = "due_alerts.hour";

/// 全体の標準。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Standard {
    /// 時刻つきの期限の、何分前に知らせるか（小さい順）。空なら知らせない。
    pub timed: Vec<u32>,
    /// 日付だけの期限の、何日前に知らせるか（小さい順）。空なら知らせない。
    pub date: Vec<u32>,
    /// 日付だけの期限を知らせる時刻（時）。
    pub hour: u32,
}

impl Default for Standard {
    fn default() -> Self {
        Standard {
            timed: STANDARD_TIMED.to_vec(),
            date: STANDARD_DATE.to_vec(),
            hour: STANDARD_HOUR,
        }
    }
}

/// タスクごとの上書き。`None` の側は全体の標準を使う。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAlerts {
    pub task_id: Id,
    /// 時刻つきの期限のとき。空なら知らせない。
    #[serde(default)]
    pub timed: Option<Vec<u32>>,
    /// 日付だけの期限のとき。空なら知らせない。
    #[serde(default)]
    pub date: Option<Vec<u32>>,
}

impl TaskAlerts {
    pub fn is_standard(&self) -> bool {
        self.timed.is_none() && self.date.is_none()
    }
}

/// 時刻つきの期限の、1 つ分の名前。
pub fn timed_label(minutes: u32) -> String {
    const DAY: u32 = 24 * 60;
    match minutes {
        0 => "期限ちょうど".into(),
        m if m % (7 * DAY) == 0 => format!("{} 週間前", m / (7 * DAY)),
        m if m % DAY == 0 => format!("{} 日前", m / DAY),
        m if m % 60 == 0 => format!("{} 時間前", m / 60),
        m if m > 60 => format!("{} 時間 {} 分前", m / 60, m % 60),
        m => format!("{m} 分前"),
    }
}

/// 日付だけの期限の、1 つ分の名前。
pub fn date_label(days: u32) -> String {
    match days {
        0 => "当日".into(),
        1 => "前日".into(),
        d if d % 7 == 0 => format!("{} 週間前", d / 7),
        d => format!("{d} 日前"),
    }
}

pub fn hour_label(hour: u32) -> String {
    format!("{hour}:00")
}

/// 選んでいるものを 1 行にする。空なら「通知しない」。
pub fn summary(values: &[u32], label: fn(u32) -> String) -> String {
    if values.is_empty() {
        "通知しない".into()
    } else {
        values
            .iter()
            .map(|v| label(*v))
            .collect::<Vec<_>>()
            .join("、")
    }
}

fn encode(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn decode(text: &str) -> Vec<u32> {
    text.split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect()
}

fn clean(values: &[u32], max: u32, what: &str) -> Result<Vec<u32>> {
    let mut out = values.to_vec();
    out.sort_unstable();
    out.dedup();
    if let Some(bad) = out.iter().find(|v| **v > max) {
        return Err(Error::Invalid(format!(
            "{what}は 0〜{max} にしてください: {bad}"
        )));
    }
    if out.len() > MAX_ALERTS {
        return Err(Error::Invalid(format!(
            "通知は 1 つの期限に {MAX_ALERTS} 個までです"
        )));
    }
    Ok(out)
}

/// 時刻つきの期限の通知（何分前）を整えて確かめる。
pub fn clean_timed(values: &[u32]) -> Result<Vec<u32>> {
    clean(values, MAX_MINUTES, "期限の何分前か")
}

/// 日付だけの期限の通知（何日前）を整えて確かめる。
pub fn clean_date(values: &[u32]) -> Result<Vec<u32>> {
    clean(values, MAX_DAYS, "期限の何日前か")
}

/// 端末の時刻帯で、その日時の UNIX 秒。夏時間で飛んだ時刻は、そのあとの時刻にする。
fn local(at: NaiveDateTime) -> Option<i64> {
    Local
        .from_local_datetime(&at)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(at + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|t| t.timestamp())
}

fn parse_date(due: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(date_of(due), "%Y-%m-%d").ok()
}

fn parse_time(due: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(time_of(due)?, "%H:%M").ok()
}

/// 期限の時刻（UNIX 秒）。日付だけの期限は `None`。
pub fn due_at(due: &str) -> Option<i64> {
    local(parse_date(due)?.and_time(parse_time(due)?))
}

/// 期限の日の終わり（翌日の 0:00、UNIX 秒）。これを過ぎたら、もう知らせない。
pub fn day_end(due: &str) -> Option<i64> {
    local(parse_date(due)?.succ_opt()?.and_time(NaiveTime::MIN))
}

/// 期限を知らせる時刻（UNIX 秒、古い順、重なりなし）。時刻つきの期限は `timed`、
/// 日付だけの期限は `date` と `hour` を使う。
pub fn fire_times(due: &str, timed: &[u32], date: &[u32], hour: u32) -> Vec<i64> {
    let mut out: Vec<i64> = match due_at(due) {
        Some(at) => timed.iter().map(|m| at - i64::from(*m) * 60).collect(),
        None => {
            let (Some(day), Some(time)) = (parse_date(due), NaiveTime::from_hms_opt(hour, 0, 0))
            else {
                return Vec::new();
            };
            date.iter()
                .filter_map(|d| {
                    let day = day.checked_sub_days(chrono::Days::new(u64::from(*d)))?;
                    local(day.and_time(time))
                })
                .collect()
        }
    };
    out.sort_unstable();
    out.dedup();
    out
}

/// 期限の通知の 1 行。`created_at` は通知を作った時刻（ミリ秒）で、そこから見た残りを書く。
pub fn headline(title: &str, due: &str, created_at: i64) -> String {
    let created = created_at.div_euclid(1000);
    let Some(today) = Local
        .timestamp_opt(created, 0)
        .single()
        .map(|t| t.date_naive())
    else {
        return format!("「{title}」の期限は {due} です");
    };
    let Some(day) = parse_date(due) else {
        return format!("「{title}」の期限は {due} です");
    };
    let days = (day - today).num_days();
    let when = match days {
        0 => "今日".to_owned(),
        1 => "明日".to_owned(),
        _ => day.format("%-m月%-d日").to_string(),
    };
    match (due_at(due), time_of(due)) {
        (Some(at), Some(time)) => {
            // 数秒の遅れは丸める
            let left = (at - created + 30).div_euclid(60);
            if left == 0 {
                format!("「{title}」の期限です")
            } else if left < 0 {
                format!("「{title}」の期限（{time}）を過ぎました")
            } else if left < 60 {
                format!("「{title}」の期限まであと {left} 分です")
            } else if days <= 1 {
                format!("「{title}」の期限は{when}の {time} です")
            } else {
                format!("「{title}」の期限は {when} {time} です")
            }
        }
        _ if days <= 1 => format!("「{title}」の期限は{when}です"),
        _ => format!("「{title}」の期限は {when}です"),
    }
}

fn read_list(store: &Store, key: &str, standard: &[u32]) -> Result<Vec<u32>> {
    Ok(match store.setting(key)? {
        Some(text) => decode(&text),
        None => standard.to_vec(),
    })
}

fn opt_list(text: Option<String>) -> Option<Vec<u32>> {
    text.map(|t| decode(&t))
}

pub(crate) fn save(conn: &Connection, alerts: &TaskAlerts) -> Result<()> {
    if alerts.is_standard() {
        conn.execute(
            "DELETE FROM task_alerts WHERE task_id = ?1",
            params![alerts.task_id],
        )?;
        return Ok(());
    }
    conn.prepare_cached(
        "INSERT INTO task_alerts(task_id, timed, date) VALUES (?1, ?2, ?3) \
         ON CONFLICT(task_id) DO UPDATE SET timed = excluded.timed, date = excluded.date",
    )?
    .execute(params![
        alerts.task_id,
        alerts.timed.as_deref().map(encode),
        alerts.date.as_deref().map(encode),
    ])?;
    Ok(())
}

/// 取り込むときに、上書きの値を確かめる。
pub(crate) fn clean_task(alerts: &TaskAlerts) -> Result<TaskAlerts> {
    Ok(TaskAlerts {
        task_id: alerts.task_id.clone(),
        timed: alerts.timed.as_deref().map(clean_timed).transpose()?,
        date: alerts.date.as_deref().map(clean_date).transpose()?,
    })
}

impl Store {
    /// 全体の標準。
    pub fn due_alert_standard(&self) -> Result<Standard> {
        let hour = match self.setting(HOUR_KEY)? {
            Some(text) => text
                .parse()
                .ok()
                .filter(|h| *h < 24)
                .unwrap_or(STANDARD_HOUR),
            None => STANDARD_HOUR,
        };
        Ok(Standard {
            timed: read_list(self, TIMED_KEY, STANDARD_TIMED)?,
            date: read_list(self, DATE_KEY, STANDARD_DATE)?,
            hour,
        })
    }

    /// 時刻つきの期限の標準を変える。
    pub fn set_due_alert_timed(&mut self, values: &[u32]) -> Result<()> {
        let values = clean_timed(values)?;
        let text = (values != STANDARD_TIMED).then(|| encode(&values));
        self.set_setting(TIMED_KEY, text.as_deref())
    }

    /// 日付だけの期限の標準を変える。
    pub fn set_due_alert_date(&mut self, values: &[u32]) -> Result<()> {
        let values = clean_date(values)?;
        let text = (values != STANDARD_DATE).then(|| encode(&values));
        self.set_setting(DATE_KEY, text.as_deref())
    }

    /// 日付だけの期限を知らせる時刻（時）を変える。
    pub fn set_due_alert_hour(&mut self, hour: u32) -> Result<()> {
        if hour >= 24 {
            return Err(Error::Invalid(format!(
                "知らせる時刻は 0〜23 時にしてください: {hour}"
            )));
        }
        let text = (hour != STANDARD_HOUR).then(|| hour.to_string());
        self.set_setting(HOUR_KEY, text.as_deref())
    }

    /// タスクごとの上書き。なければ、両方 `None`。
    pub fn task_alerts(&self, task_id: &str) -> Result<TaskAlerts> {
        let row = self
            .conn
            .prepare_cached("SELECT timed, date FROM task_alerts WHERE task_id = ?1")?
            .query_row(params![task_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .optional()?;
        let (timed, date) = row.unwrap_or_default();
        Ok(TaskAlerts {
            task_id: task_id.to_owned(),
            timed: opt_list(timed),
            date: opt_list(date),
        })
    }

    /// タスクごとの上書きを変える。両方 `None` なら標準に戻す。
    pub fn set_task_alerts(&mut self, alerts: &TaskAlerts) -> Result<()> {
        let alerts = clean_task(alerts)?;
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            params![alerts.task_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::NotFound(format!("タスク {}", alerts.task_id)));
        }
        save(&self.conn, &alerts)
    }

    /// そのタスクの期限を知らせる時刻（UNIX 秒、古い順）。期限がなければ空。
    pub fn task_fire_times(&self, task_id: &str, due: Option<&str>) -> Result<Vec<i64>> {
        let Some(due) = due else {
            return Ok(Vec::new());
        };
        let standard = self.due_alert_standard()?;
        let own = self.task_alerts(task_id)?;
        Ok(fire_times(
            due,
            own.timed.as_deref().unwrap_or(&standard.timed),
            own.date.as_deref().unwrap_or(&standard.date),
            standard.hour,
        ))
    }

    pub(crate) fn all_task_alerts(&self) -> Result<Vec<TaskAlerts>> {
        let mut stmt = self
            .conn
            .prepare("SELECT task_id, timed, date FROM task_alerts ORDER BY task_id")?;
        let rows = stmt.query_map([], |r| {
            Ok(TaskAlerts {
                task_id: r.get(0)?,
                timed: opt_list(r.get(1)?),
                date: opt_list(r.get(2)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> i64 {
        local(NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()).unwrap()
    }

    #[test]
    fn labels_read_naturally() {
        let names: Vec<String> = TIMED_CHOICES.iter().map(|m| timed_label(*m)).collect();
        assert_eq!(
            names,
            [
                "期限ちょうど",
                "5 分前",
                "10 分前",
                "15 分前",
                "30 分前",
                "1 時間前",
                "2 時間前",
                "1 日前",
                "2 日前",
                "1 週間前"
            ]
        );
        assert_eq!(timed_label(90), "1 時間 30 分前");
        let names: Vec<String> = DATE_CHOICES.iter().map(|d| date_label(*d)).collect();
        assert_eq!(names, ["当日", "前日", "2 日前", "1 週間前"]);
        assert_eq!(summary(&[0, 15], timed_label), "期限ちょうど、15 分前");
        assert_eq!(summary(&[], timed_label), "通知しない");
    }

    #[test]
    fn fire_times_for_timed_and_date_only() {
        assert_eq!(
            fire_times("2026-12-24 18:00", &[0, 5, 1440], &[0], 9),
            [
                at("2026-12-23 18:00"),
                at("2026-12-24 17:55"),
                at("2026-12-24 18:00")
            ]
        );
        assert_eq!(
            fire_times("2026-12-24", &[0], &[0, 1, 7], 8),
            [
                at("2026-12-17 08:00"),
                at("2026-12-23 08:00"),
                at("2026-12-24 08:00")
            ]
        );
        assert!(fire_times("2026-12-24", &[0], &[], 9).is_empty());
        assert!(fire_times("2026-12-24 18:00", &[], &[0], 9).is_empty());
    }

    #[test]
    fn standard_starts_with_the_due_moment_and_can_be_changed() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.due_alert_standard().unwrap(), Standard::default());
        s.set_due_alert_timed(&[15, 0, 15]).unwrap();
        s.set_due_alert_date(&[]).unwrap();
        s.set_due_alert_hour(7).unwrap();
        assert_eq!(
            s.due_alert_standard().unwrap(),
            Standard {
                timed: vec![0, 15],
                date: vec![],
                hour: 7
            }
        );
        assert!(s.set_due_alert_timed(&[MAX_MINUTES + 1]).is_err());
        assert!(s.set_due_alert_hour(24).is_err());
        // 標準と同じにしたら、設定を消す
        s.set_due_alert_timed(&[0]).unwrap();
        assert_eq!(s.setting(TIMED_KEY).unwrap(), None);
    }

    #[test]
    fn a_task_can_override_and_go_back_to_the_standard() {
        let mut s = Store::open_in_memory().unwrap();
        let draft = TaskDraft {
            title: "会議".into(),
            due: Some("2026-12-24 18:00".into()),
            ..Default::default()
        };
        let task = s.create_task(None, &draft).unwrap();
        let due = task.due.as_deref();
        assert_eq!(
            s.task_fire_times(&task.id, due).unwrap(),
            [at("2026-12-24 18:00")]
        );
        s.set_task_alerts(&TaskAlerts {
            task_id: task.id.clone(),
            timed: Some(vec![15, 5]),
            date: None,
        })
        .unwrap();
        assert_eq!(
            s.task_fire_times(&task.id, due).unwrap(),
            [at("2026-12-24 17:45"), at("2026-12-24 17:55")]
        );
        // 日付だけにしたら、日付だけの標準を使う
        assert_eq!(
            s.task_fire_times(&task.id, Some("2026-12-24")).unwrap(),
            [at("2026-12-24 09:00")]
        );
        s.set_task_alerts(&TaskAlerts {
            task_id: task.id.clone(),
            ..Default::default()
        })
        .unwrap();
        assert!(s.task_alerts(&task.id).unwrap().is_standard());
        assert!(s
            .set_task_alerts(&TaskAlerts {
                task_id: "nope".into(),
                timed: Some(vec![]),
                date: None,
            })
            .is_err());
        // タスクを消すと、上書きも消える
        s.set_task_alerts(&TaskAlerts {
            task_id: task.id.clone(),
            timed: Some(vec![]),
            date: None,
        })
        .unwrap();
        s.delete_task(&task.id).unwrap();
        assert!(s.all_task_alerts().unwrap().is_empty());
    }

    #[test]
    fn export_and_import_keep_the_overrides() {
        let mut s = Store::open_in_memory().unwrap();
        let draft = TaskDraft {
            title: "会議".into(),
            due: Some("2026-12-24 18:00".into()),
            ..Default::default()
        };
        let task = s.create_task(None, &draft).unwrap();
        let own = TaskAlerts {
            task_id: task.id.clone(),
            timed: Some(vec![5, 15]),
            date: Some(vec![]),
        };
        s.set_task_alerts(&own).unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&s, &mut zip).unwrap();
        let mut other = Store::open_in_memory().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut other, zip).unwrap();
        assert_eq!(other.task_alerts(&task.id).unwrap(), own);
    }

    #[test]
    fn changing_a_task_override_can_be_undone() {
        let mut s = Store::open_in_memory().unwrap();
        let task = s
            .create_task(
                None,
                &TaskDraft {
                    title: "会議".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        s.undo_mark().unwrap();
        s.set_task_alerts(&TaskAlerts {
            task_id: task.id.clone(),
            timed: Some(vec![15]),
            date: None,
        })
        .unwrap();
        s.undo_mark().unwrap();
        assert!(s.undo().unwrap());
        assert!(s.task_alerts(&task.id).unwrap().is_standard());
    }

    #[test]
    fn headlines_say_how_long_is_left() {
        let ms = |text: &str| at(text) * 1000;
        let timed = "2026-12-24 18:00";
        assert_eq!(
            headline("会議", timed, ms("2026-12-24 18:00") + 3_000),
            "「会議」の期限です"
        );
        assert_eq!(
            headline("会議", timed, ms("2026-12-24 17:55")),
            "「会議」の期限まであと 5 分です"
        );
        assert_eq!(
            headline("会議", timed, ms("2026-12-24 16:00")),
            "「会議」の期限は今日の 18:00 です"
        );
        assert_eq!(
            headline("会議", timed, ms("2026-12-23 18:00")),
            "「会議」の期限は明日の 18:00 です"
        );
        assert_eq!(
            headline("会議", timed, ms("2026-12-17 18:00")),
            "「会議」の期限は 12月24日 18:00 です"
        );
        assert_eq!(
            headline("会議", timed, ms("2026-12-24 19:00")),
            "「会議」の期限（18:00）を過ぎました"
        );
        let date = "2026-12-24";
        assert_eq!(
            headline("牛乳", date, ms("2026-12-24 09:00")),
            "「牛乳」の期限は今日です"
        );
        assert_eq!(
            headline("牛乳", date, ms("2026-12-23 09:00")),
            "「牛乳」の期限は明日です"
        );
        assert_eq!(
            headline("牛乳", date, ms("2026-12-17 09:00")),
            "「牛乳」の期限は 12月24日です"
        );
    }
}
