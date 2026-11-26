//! GitHub の定期チェック: issue とプルリクエストの、前回のチェックからの進展を読む。
//!
//! `gh` の JSON から、コメント・レビュー・コードへのコメント・コミットと状態を読み、
//! リンクごとに「どこまで見たか」（[`Mark`]）を覚えておく。次のチェックでは、それより新しい動きと、
//! 状態の変化だけを [`Progress`] として返す。覚えるのは時刻と状態の短い値だけ。

use serde_json::Value;

use crate::github::{quiet_env, Kind, Link};
use crate::shell::Spec;

/// 動き 1 つの本文として AI に渡す文字数。
const MAX_BODY_CHARS: usize = 800;
/// リンク 1 つの動きとして AI に渡す文字数。超えたら古いものから落とす。
pub const MAX_EVENTS_CHARS: usize = 6000;

/// `gh issue view --json` で受け取る項目（定期チェック用）。
const ISSUE_FIELDS: &str = "title,state,closedAt,comments,url";
/// `gh pr view --json` で受け取る項目（定期チェック用）。
const PULL_FIELDS: &str =
    "title,state,isDraft,reviewDecision,mergedAt,closedAt,comments,reviews,commits,url";

/// 進展を読むコマンド。プルリクエストは、コードへのコメントも別に読む。issue とプルリクエスト以外は空。
pub fn commands(link: &Link) -> Vec<Spec> {
    let slug = format!("{}/{}", link.owner, link.repo);
    let (view, fields, number) = match &link.kind {
        Kind::Issue(n) => ("issue", ISSUE_FIELDS, *n),
        Kind::Pull(n) => ("pr", PULL_FIELDS, *n),
        _ => return Vec::new(),
    };
    let spec = |args: Vec<String>| {
        let mut spec = Spec::direct("gh", args);
        spec.env = quiet_env();
        spec
    };
    let mut specs = vec![spec(vec![
        view.into(),
        "view".into(),
        number.to_string(),
        "-R".into(),
        slug.clone(),
        "--json".into(),
        fields.into(),
    ])];
    if matches!(link.kind, Kind::Pull(_)) {
        // 新しいものから 100 件
        specs.push(spec(vec![
            "api".into(),
            format!(
                "repos/{slug}/pulls/{number}/comments?per_page=100&sort=created&direction=desc"
            ),
        ]));
    }
    specs
}

/// リンクごとに覚えておく値。どこまでの動きを見たか（GitHub の時刻）と、そのときの状態。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mark {
    /// いちばん新しい動きの時刻（`2026-11-25T10:00:00Z` の形）。動きがなければ空。
    pub at: String,
    pub state: String,
}

impl Mark {
    pub fn to_json(&self) -> Value {
        serde_json::json!({"at": self.at, "state": self.state})
    }

    /// 保存した値を読む。前の版の値（中身の指紋の文字列）は `None` になり、初めて見るリンクとして扱う。
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            at: value.get("at")?.as_str()?.to_owned(),
            state: value.get("state")?.as_str()?.to_owned(),
        })
    }
}

/// 動き 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// GitHub の時刻（`2026-11-25T10:00:00Z` の形）。
    pub at: String,
    /// GitHub のアカウント。分からなければ空。
    pub who: String,
    /// 「コメント」「レビュー（承認）」のような、動きの種類。
    pub what: String,
    pub body: String,
}

/// 今の GitHub の様子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub title: String,
    /// 「オープン」「マージ済み」のような、今の状態。
    pub state: String,
    /// 古い順。
    pub events: Vec<Activity>,
}

/// 前回のチェックからの進展。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub title: String,
    pub state: String,
    /// 状態が変わったときの、前の状態。
    pub was: Option<String>,
    /// 前回より新しい動き。古い順。
    pub events: Vec<Activity>,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn login(v: &Value, key: &str) -> String {
    v.get(key).map(|a| text(a, "login")).unwrap_or_default()
}

fn review_label(state: &str) -> Option<&'static str> {
    Some(match state {
        "APPROVED" => "レビュー（承認）",
        "CHANGES_REQUESTED" => "レビュー（修正の依頼）",
        "COMMENTED" => "レビュー（コメント）",
        "DISMISSED" => "レビュー（取り下げ）",
        // 書きかけのレビューは、まだほかの人には見えていない
        _ => return None,
    })
}

fn state_label(kind: &Kind, v: &Value) -> String {
    let state = text(v, "state");
    match kind {
        Kind::Pull(_) => {
            let mut label = match state.as_str() {
                "MERGED" => "マージ済み".to_owned(),
                "CLOSED" => "クローズ".to_owned(),
                _ => "オープン".to_owned(),
            };
            if state == "OPEN" {
                if v.get("isDraft").and_then(Value::as_bool) == Some(true) {
                    label.push_str("・下書き");
                }
                match text(v, "reviewDecision").as_str() {
                    "APPROVED" => label.push_str("・承認済み"),
                    "CHANGES_REQUESTED" => label.push_str("・修正の依頼あり"),
                    "REVIEW_REQUIRED" => label.push_str("・レビュー待ち"),
                    _ => {}
                }
            }
            label
        }
        _ => match state.as_str() {
            "CLOSED" => "クローズ".to_owned(),
            _ => "オープン".to_owned(),
        },
    }
}

/// [`commands`] の出力（1 つめは `gh … view --json`、2 つめはコードへのコメント）から、今の様子を読む。
pub fn read(link: &Link, outputs: &[String]) -> Result<Snapshot, String> {
    let view = outputs
        .first()
        .ok_or_else(|| "gh の出力がありません".to_owned())?;
    let v: Value = serde_json::from_str(view.trim())
        .map_err(|e| format!("gh の出力を JSON として読めませんでした: {e}"))?;
    if !v.is_object() {
        return Err("gh の出力が JSON のオブジェクトではありませんでした".into());
    }
    let mut events = Vec::new();
    let list = |key: &str| {
        v.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for c in list("comments") {
        events.push(Activity {
            at: text(&c, "createdAt"),
            who: login(&c, "author"),
            what: "コメント".into(),
            body: text(&c, "body"),
        });
    }
    for r in list("reviews") {
        let Some(what) = review_label(&text(&r, "state")) else {
            continue;
        };
        events.push(Activity {
            at: text(&r, "submittedAt"),
            who: login(&r, "author"),
            what: what.into(),
            body: text(&r, "body"),
        });
    }
    for c in list("commits") {
        let who = c
            .get("authors")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .map(|a| {
                let login = text(a, "login");
                if login.is_empty() {
                    text(a, "name")
                } else {
                    login
                }
            })
            .unwrap_or_default();
        events.push(Activity {
            at: text(&c, "committedDate"),
            who,
            what: "コミット".into(),
            body: text(&c, "messageHeadline"),
        });
    }
    if let Some(inline) = outputs.get(1) {
        let comments: Value = serde_json::from_str(inline.trim()).map_err(|e| {
            format!("コードへのコメント（gh api）の出力を JSON として読めませんでした: {e}")
        })?;
        let comments = comments.as_array().ok_or_else(|| {
            "コードへのコメント（gh api）の出力が一覧ではありませんでした".to_owned()
        })?;
        for c in comments {
            let path = text(c, "path");
            let line = c
                .get("line")
                .or_else(|| c.get("original_line"))
                .and_then(Value::as_u64);
            let place = match line {
                Some(line) => format!("{path}:{line}"),
                None => path,
            };
            events.push(Activity {
                at: text(c, "created_at"),
                who: login(c, "user"),
                what: format!("コードへのコメント（{place}）"),
                body: text(c, "body"),
            });
        }
    }
    for (key, what) in [("mergedAt", "マージ"), ("closedAt", "クローズ")] {
        let at = text(&v, key);
        // マージしたプルリクエストは、閉じた時刻も入る
        if at.is_empty() || (key == "closedAt" && !text(&v, "mergedAt").is_empty()) {
            continue;
        }
        events.push(Activity {
            at,
            who: String::new(),
            what: what.into(),
            body: String::new(),
        });
    }
    events.retain(|e| parse_time(&e.at).is_some());
    events.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(Snapshot {
        title: text(&v, "title"),
        state: state_label(&link.kind, &v),
        events,
    })
}

impl Snapshot {
    /// 次のチェックで比べるための値。
    pub fn mark(&self) -> Mark {
        Mark {
            at: self.events.last().map(|e| e.at.clone()).unwrap_or_default(),
            state: self.state.clone(),
        }
    }

    /// `mark` より新しい動きと、状態の変化。何もなければ `None`。
    pub fn since(&self, mark: &Mark) -> Option<Progress> {
        let events: Vec<Activity> = self
            .events
            .iter()
            .filter(|e| e.at > mark.at)
            .cloned()
            .collect();
        let was = (self.state != mark.state).then(|| mark.state.clone());
        if events.is_empty() && was.is_none() {
            return None;
        }
        Some(Progress {
            title: self.title.clone(),
            state: self.state.clone(),
            was,
            events,
        })
    }

    /// 初めて見るリンクで、`after`（前回のチェックの時刻）より新しい動き。
    /// 前回のチェックがなければ `None`（比べる元を覚えるだけ）。
    pub fn after(&self, after: Option<i64>) -> Option<Progress> {
        let after = after?;
        self.since(&Mark {
            at: format_time(after),
            state: self.state.clone(),
        })
    }
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

impl Progress {
    /// AI に渡す文。`me` はユーザーの GitHub のアカウント。`time` は時刻（UNIX 秒）を画面の形にする。
    pub fn describe(&self, me: Option<&str>, time: impl Fn(i64) -> String) -> String {
        let mut out = format!("題: {}\n状態: {}", self.title, self.state);
        if let Some(was) = &self.was {
            out.push_str(&format!("（前回のチェックでは {was}）"));
        }
        out.push('\n');
        if self.events.is_empty() {
            return out;
        }
        let lines: Vec<String> = self
            .events
            .iter()
            .map(|e| {
                let when = parse_time(&e.at).map(&time).unwrap_or_default();
                let who = match (e.who.as_str(), me) {
                    ("", _) => String::new(),
                    (who, Some(me)) if who.eq_ignore_ascii_case(me) => format!("自分（@{who}）の"),
                    (who, _) => format!("@{who} の"),
                };
                let body = clip(&e.body, MAX_BODY_CHARS);
                let mut line = format!("- {when} {who}{}", e.what);
                if !body.is_empty() {
                    line.push_str(&format!(": {}", body.replace('\n', "\n  ")));
                }
                line
            })
            .collect();
        // 長すぎるときは、古いものから落とす
        let mut size = 0;
        let mut keep = lines.len();
        for line in lines.iter().rev() {
            size += line.chars().count() + 1;
            if size > MAX_EVENTS_CHARS && keep < lines.len() {
                break;
            }
            keep -= 1;
        }
        let dropped = keep;
        out.push_str("前回のチェックからの動き（古い順）:\n");
        if dropped > 0 {
            out.push_str(&format!("（古い {dropped} 件は省略）\n"));
        }
        for line in &lines[dropped..] {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// `2026-11-25T10:00:00Z`（秒の小数や `+09:00` もあり）を UNIX 秒にする。
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "Z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (oh, om) = rest[1..].split_once(':')?;
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec - offset)
}

/// UNIX 秒を `2026-11-25T10:00:00Z` の形にする。
pub fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// 日付と日数の変換（http://howardhinnant.github.io/date_algorithms.html）
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pull() -> Link {
        Link {
            owner: "o".into(),
            repo: "r".into(),
            kind: Kind::Pull(7),
        }
    }

    fn issue() -> Link {
        Link {
            owner: "o".into(),
            repo: "r".into(),
            kind: Kind::Issue(3),
        }
    }

    fn pull_json() -> String {
        json!({
            "title": "ログインを直す",
            "state": "OPEN",
            "isDraft": false,
            "reviewDecision": "CHANGES_REQUESTED",
            "mergedAt": null,
            "closedAt": null,
            "comments": [
                {"author": {"login": "alice"}, "body": "直りました", "createdAt": "2026-11-25T02:00:00Z"},
            ],
            "reviews": [
                {"author": {"login": "kanade"}, "state": "CHANGES_REQUESTED", "body": "テストを足してください", "submittedAt": "2026-11-25T01:00:00Z"},
                {"author": {"login": "kanade"}, "state": "PENDING", "body": "書きかけ", "submittedAt": ""},
            ],
            "commits": [
                {"messageHeadline": "テストを足す", "committedDate": "2026-11-25T03:00:00Z", "authors": [{"login": "alice", "name": "Alice"}]},
            ],
        })
        .to_string()
    }

    fn inline_json() -> String {
        json!([
            {"user": {"login": "kanade"}, "body": "ここは None のとき落ちます", "created_at": "2026-11-25T01:00:30Z", "path": "src/login.rs", "line": 12},
        ])
        .to_string()
    }

    #[test]
    fn commands_read_issues_and_pulls_with_gh() {
        let specs = commands(&pull());
        assert_eq!(specs.len(), 2);
        assert_eq!(
            specs[0].args,
            ["pr", "view", "7", "-R", "o/r", "--json", PULL_FIELDS]
        );
        assert_eq!(
            specs[1].args,
            [
                "api",
                "repos/o/r/pulls/7/comments?per_page=100&sort=created&direction=desc"
            ]
        );
        let specs = commands(&issue());
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].args,
            ["issue", "view", "3", "-R", "o/r", "--json", ISSUE_FIELDS]
        );
        for spec in specs {
            assert_eq!(spec.program, "gh");
            assert!(spec
                .env
                .contains(&("GH_PROMPT_DISABLED".into(), "1".into())));
        }
        let repo = Link {
            kind: Kind::Repo,
            ..issue()
        };
        assert!(commands(&repo).is_empty());
    }

    #[test]
    fn reviews_comments_and_commits_are_read_in_order() {
        let snap = read(&pull(), &[pull_json(), inline_json()]).unwrap();
        assert_eq!(snap.title, "ログインを直す");
        assert_eq!(snap.state, "オープン・修正の依頼あり");
        let whats: Vec<(&str, &str)> = snap
            .events
            .iter()
            .map(|e| (e.who.as_str(), e.what.as_str()))
            .collect();
        assert_eq!(
            whats,
            [
                ("kanade", "レビュー（修正の依頼）"),
                ("kanade", "コードへのコメント（src/login.rs:12）"),
                ("alice", "コメント"),
                ("alice", "コミット"),
            ]
        );
        assert_eq!(snap.mark().at, "2026-11-25T03:00:00Z");
    }

    #[test]
    fn only_new_activity_and_state_changes_are_progress() {
        let snap = read(&pull(), &[pull_json(), inline_json()]).unwrap();
        // 前回と同じなら、進展はない
        assert_eq!(snap.since(&snap.mark()), None);
        // 前回より新しい動きだけ
        let mark = Mark {
            at: "2026-11-25T01:30:00Z".into(),
            state: snap.state.clone(),
        };
        let progress = snap.since(&mark).unwrap();
        assert_eq!(progress.was, None);
        assert_eq!(progress.events.len(), 2);
        assert_eq!(progress.events[0].body, "直りました");
        // 動きがなくても、状態が変われば進展
        let merged = json!({"title": "t", "state": "MERGED", "mergedAt": "2026-11-26T00:00:00Z", "closedAt": "2026-11-26T00:00:00Z"});
        let snap = read(&pull(), &[merged.to_string(), "[]".into()]).unwrap();
        assert_eq!(snap.state, "マージ済み");
        let progress = snap
            .since(&Mark {
                at: "2026-11-26T00:00:00Z".into(),
                state: "オープン".into(),
            })
            .unwrap();
        assert_eq!(progress.was.as_deref(), Some("オープン"));
        assert!(progress.events.is_empty());
        // マージの時刻は 1 つだけ（閉じた時刻を重ねない）
        assert_eq!(snap.events.len(), 1);
        assert_eq!(snap.events[0].what, "マージ");
    }

    #[test]
    fn a_new_link_reports_activity_after_the_last_check() {
        let snap = read(&pull(), &[pull_json(), inline_json()]).unwrap();
        // 前回のチェックがなければ、比べる元を覚えるだけ
        assert_eq!(snap.after(None), None);
        let after = parse_time("2026-11-25T02:30:00Z").unwrap();
        let progress = snap.after(Some(after)).unwrap();
        assert_eq!(progress.events.len(), 1);
        assert_eq!(progress.events[0].what, "コミット");
    }

    #[test]
    fn my_own_activity_is_marked_as_mine() {
        let snap = read(&pull(), &[pull_json(), inline_json()]).unwrap();
        let progress = snap.since(&Mark::default()).unwrap();
        let text = progress.describe(Some("Kanade"), |secs| format!("t{secs}"));
        assert!(text.starts_with("題: ログインを直す\n状態: オープン・修正の依頼あり"));
        assert!(text.contains("自分（@kanade）のレビュー（修正の依頼）: テストを足してください"));
        assert!(text.contains("自分（@kanade）のコードへのコメント（src/login.rs:12）"));
        assert!(text.contains("@alice のコメント: 直りました"));
        assert!(text.contains(&format!("t{}", parse_time("2026-11-25T02:00:00Z").unwrap())));
        // 書きかけのレビューは渡さない
        assert!(!text.contains("書きかけ"));
        // アカウントが分からなければ、名前だけ
        let text = progress.describe(None, |_| String::new());
        assert!(text.contains("@kanade のレビュー") && !text.contains("自分"));
    }

    #[test]
    fn long_activity_drops_the_oldest_first() {
        let comments: Vec<Value> = (0..40)
            .map(|i| {
                json!({"author": {"login": "a"}, "body": format!("{i:02}{}", "あ".repeat(500)),
                       "createdAt": format!("2026-11-25T00:{i:02}:00Z")})
            })
            .collect();
        let v = json!({"title": "t", "state": "OPEN", "comments": comments});
        let snap = read(&issue(), &[v.to_string()]).unwrap();
        let text = snap
            .since(&Mark::default())
            .unwrap()
            .describe(None, |_| String::new());
        assert!(text.chars().count() < MAX_EVENTS_CHARS + 200);
        assert!(text.contains("件は省略"));
        assert!(text.contains("39あ") && !text.contains("- @a のコメント: 00"));
        // 1 つの本文も長すぎれば切る
        assert!(!text.contains(&"あ".repeat(MAX_BODY_CHARS + 1)));
    }

    #[test]
    fn broken_output_is_an_error() {
        assert!(read(&issue(), &[]).is_err());
        assert!(read(&issue(), &["not json".into()]).is_err());
        assert!(read(&issue(), &["[1]".into()]).is_err());
        assert!(read(&pull(), &[pull_json(), "{}".into()]).is_err());
        let snap = read(&issue(), &[r#"{"title":"t","state":"CLOSED"}"#.into()]).unwrap();
        assert_eq!(snap.state, "クローズ");
        assert!(snap.events.is_empty());
    }

    #[test]
    fn marks_round_trip_and_old_values_are_new_links() {
        let mark = Mark {
            at: "2026-11-25T03:00:00Z".into(),
            state: "オープン".into(),
        };
        assert_eq!(Mark::from_json(&mark.to_json()), Some(mark));
        // 前の版は、中身の指紋を文字列で覚えていた
        assert_eq!(Mark::from_json(&json!("0123456789abcdef")), None);
    }

    #[test]
    fn times_are_parsed_and_formatted() {
        for (s, secs) in [
            ("1970-01-01T00:00:00Z", 0),
            ("2026-11-25T10:00:00Z", 1_795_600_800),
            ("2026-11-25T19:00:00+09:00", 1_795_600_800),
            ("2026-11-25T10:00:00.123Z", 1_795_600_800),
            ("2024-02-29T23:59:59Z", 1_709_251_199),
        ] {
            assert_eq!(parse_time(s), Some(secs), "{s}");
        }
        for s in [
            "",
            "2026-11-25",
            "2026-13-01T00:00:00Z",
            "2026-11-25 10:00:00Z",
            "あいう",
        ] {
            assert_eq!(parse_time(s), None, "{s}");
        }
        for secs in [0, 1_795_600_800, 1_709_251_199] {
            assert_eq!(parse_time(&format_time(secs)), Some(secs));
        }
        assert_eq!(format_time(1_795_600_800), "2026-11-25T10:00:00Z");
    }
}
