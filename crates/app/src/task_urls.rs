//! タスクの詳細で、タイトルの下に並べる URL の見せ方。

use malakt_ai::{github, slack};

/// URL を並べるときの印と、短い名前。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// 同梱のアイコンの名前。
    pub icon: &'static str,
    pub label: String,
}

/// 名前に出す文字数。長いものは「…」で切る（全体はホバーで出す）。
const MAX_LABEL_CHARS: usize = 48;

/// GitHub は「owner/repo#12」、Slack はワークスペースの名前、そのほかはホストとパス。
pub fn shown(url: &str) -> Shown {
    if let Some(link) = github::parse_link(url) {
        let repo = format!("{}/{}", link.owner, link.repo);
        let label = match &link.kind {
            github::Kind::Repo => repo,
            github::Kind::Issue(n) | github::Kind::Pull(n) => format!("{repo}#{n}"),
            github::Kind::Commit(sha) => format!("{repo}@{}", &sha[..sha.len().min(7)]),
            github::Kind::File { path, .. } => {
                format!("{repo}/{}", path.rsplit('/').next().unwrap_or(path))
            }
        };
        return Shown {
            icon: "github",
            label: clip(&label),
        };
    }
    if let Some(link) = slack::parse_link(url) {
        let name = link.host.strip_suffix(".slack.com").unwrap_or(&link.host);
        return Shown {
            icon: "message-square",
            label: clip(&format!("Slack {name}")),
        };
    }
    if url.starts_with(malakt_core::tasklink::PREFIX) {
        return Shown {
            icon: "link",
            label: "タスク".into(),
        };
    }
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    // ユーザー名は出さない（パスの中の @ はそのまま）
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let rest = match rest[..host_end].rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    Shown {
        icon: "globe",
        label: clip(rest),
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_LABEL_CHARS {
        return text.to_owned();
    }
    let head: String = text.chars().take(MAX_LABEL_CHARS - 1).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(url: &str) -> (&'static str, String) {
        let s = shown(url);
        (s.icon, s.label)
    }

    #[test]
    fn github_links_read_like_references() {
        assert_eq!(
            label("https://github.com/acme/app/pull/12"),
            ("github", "acme/app#12".into())
        );
        assert_eq!(
            label("https://github.com/acme/app/issues/3#issuecomment-1"),
            ("github", "acme/app#3".into())
        );
        assert_eq!(
            label("https://github.com/acme/app"),
            ("github", "acme/app".into())
        );
        assert_eq!(
            label("https://github.com/acme/app/commit/0123456789abcdef"),
            ("github", "acme/app@0123456".into())
        );
        assert_eq!(
            label("https://github.com/acme/app/blob/main/docs/ai.md"),
            ("github", "acme/app/ai.md".into())
        );
    }

    #[test]
    fn slack_and_task_links_get_their_own_mark() {
        assert_eq!(
            label("https://acme.slack.com/archives/C0123ABCD/p1700000000123456"),
            ("message-square", "Slack acme".into())
        );
        assert_eq!(
            label(&malakt_core::tasklink::url("01ABC")),
            ("link", "タスク".into())
        );
    }

    #[test]
    fn other_links_show_host_and_path() {
        assert_eq!(
            label("https://www.example.com/docs/"),
            ("globe", "example.com/docs".into())
        );
        assert_eq!(
            label("https://user@example.com/a?b=1"),
            ("globe", "example.com/a?b=1".into())
        );
        assert_eq!(
            label("https://medium.com/@kanade/post"),
            ("globe", "medium.com/@kanade/post".into())
        );
        let long = format!("https://example.com/{}", "あ".repeat(80));
        let got = label(&long).1;
        assert_eq!(got.chars().count(), MAX_LABEL_CHARS);
        assert!(got.ends_with('…'));
    }
}
