//! GitHub のリンクの検出と、中身を取りに行くコマンド。

use serde_json::Value;

use crate::shell::Spec;

/// 1 回のメッセージで取りに行くリンクの上限。
pub const MAX_LINKS: usize = 5;

const HOST: &str = "https://github.com/";

/// リポジトリではない、GitHub のページの先頭の名前。
const RESERVED: [&str; 14] = [
    "orgs",
    "settings",
    "features",
    "marketplace",
    "sponsors",
    "topics",
    "apps",
    "notifications",
    "login",
    "about",
    "pricing",
    "search",
    "users",
    "enterprise",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Repo,
    Issue(u64),
    Pull(u64),
    /// ブランチやタグなどの名前と、ファイルのパス
    File {
        reference: String,
        path: String,
    },
    Commit(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub owner: String,
    pub repo: String,
    pub kind: Kind,
}

fn is_name(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || extra.contains(&c))
}

/// GitHub のリンク 1 つを読む。GitHub のリンクでなければ `None`。
pub fn parse_link(url: &str) -> Option<Link> {
    let rest = url.strip_prefix(HOST)?;
    let rest = rest.split(['?', '#']).next()?;
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if !is_name(owner, &[]) || !is_name(repo, &['.', '_']) || RESERVED.contains(&owner) {
        return None;
    }
    let kind = match (parts.next(), parts.next()) {
        (Some("issues"), Some(n)) => Kind::Issue(n.parse().ok()?),
        (Some("pull"), Some(n)) => Kind::Pull(n.parse().ok()?),
        (Some("commit"), Some(sha))
            if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            Kind::Commit(sha.to_owned())
        }
        (Some("blob"), Some(reference)) => {
            let path: Vec<&str> = parts.collect();
            if path.is_empty() || path.contains(&"..") {
                return None;
            }
            Kind::File {
                reference: reference.to_owned(),
                path: path.join("/"),
            }
        }
        _ => Kind::Repo,
    };
    Some(Link {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        kind,
    })
}

/// 文章の中の GitHub のリンクを、出てきた順に、重複なしで返す（[`MAX_LINKS`] 個まで）。
pub fn find_links(text: &str) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(HOST) {
        let candidate = &rest[start..];
        // リンクは ASCII だけ。日本語の文に続けて書かれていても、そこで切る
        let end = candidate
            .find(|c: char| !c.is_ascii_graphic() || "<>\"'()[]{}`".contains(c))
            .unwrap_or(candidate.len());
        let url = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!']);
        if let Some(link) = parse_link(url) {
            if !links.contains(&link) && links.len() < MAX_LINKS {
                links.push(link);
            }
        }
        rest = &candidate[end.max(HOST.len())..];
    }
    links
}

impl Link {
    fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    pub fn url(&self) -> String {
        let base = format!("{HOST}{}", self.slug());
        match &self.kind {
            Kind::Repo => base,
            Kind::Issue(n) => format!("{base}/issues/{n}"),
            Kind::Pull(n) => format!("{base}/pull/{n}"),
            Kind::File { reference, path } => format!("{base}/blob/{reference}/{path}"),
            Kind::Commit(sha) => format!("{base}/commit/{sha}"),
        }
    }

    /// REST API のパスと、Accept に入れる形式。
    fn api(&self) -> (String, &'static str) {
        let base = format!("repos/{}", self.slug());
        match &self.kind {
            Kind::Repo => (base, "application/vnd.github+json"),
            // プルリクエストも issue として読める
            Kind::Issue(n) | Kind::Pull(n) => {
                (format!("{base}/issues/{n}"), "application/vnd.github+json")
            }
            Kind::File { reference, path } => (
                format!("{base}/contents/{path}?ref={reference}"),
                "application/vnd.github.raw",
            ),
            Kind::Commit(sha) => (
                format!("{base}/commits/{sha}"),
                "application/vnd.github.diff",
            ),
        }
    }

    /// `gh` で取りに行くコマンド。シェルを通さず、引数はこちらで組み立てる。
    pub fn gh(&self) -> Spec {
        let slug = self.slug();
        let args: Vec<String> = match &self.kind {
            Kind::Repo => vec!["repo".into(), "view".into(), slug],
            Kind::Issue(n) => vec![
                "issue".into(),
                "view".into(),
                n.to_string(),
                "-R".into(),
                slug,
                "--comments".into(),
            ],
            Kind::Pull(n) => vec![
                "pr".into(),
                "view".into(),
                n.to_string(),
                "-R".into(),
                slug,
                "--comments".into(),
            ],
            Kind::File { .. } | Kind::Commit(_) => {
                let (path, accept) = self.api();
                vec!["api".into(), "-H".into(), format!("Accept: {accept}"), path]
            }
        };
        let mut spec = Spec::direct("gh", args);
        // 入力を待って止まらないようにする
        spec.env = vec![
            ("GH_PROMPT_DISABLED".into(), "1".into()),
            ("NO_COLOR".into(), "1".into()),
        ];
        spec
    }

    /// `gh` がないときに読む URL と Accept（公開のリポジトリだけ読める）。
    pub fn api_request(&self) -> (String, &'static str) {
        let (path, accept) = self.api();
        (format!("https://api.github.com/{path}"), accept)
    }

    /// API の返事を、AI に渡す文章にする。
    pub fn format_api_response(&self, body: &str) -> String {
        let field = |v: &Value, key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match (&self.kind, serde_json::from_str::<Value>(body)) {
            (Kind::Issue(_) | Kind::Pull(_), Ok(v)) => format!(
                "title: {}\nstate: {}\nauthor: {}\n\n{}",
                field(&v, "title"),
                field(&v, "state"),
                v.get("user").map(|u| field(u, "login")).unwrap_or_default(),
                field(&v, "body"),
            ),
            (Kind::Repo, Ok(v)) => format!(
                "name: {}\ndescription: {}\ndefault branch: {}\nopen issues: {}",
                field(&v, "full_name"),
                field(&v, "description"),
                field(&v, "default_branch"),
                v.get("open_issues_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
            _ => body.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(owner: &str, repo: &str, kind: Kind) -> Link {
        Link {
            owner: owner.into(),
            repo: repo.into(),
            kind,
        }
    }

    #[test]
    fn each_kind_of_link_is_recognized() {
        let parse = |path: &str| parse_link(&format!("https://github.com/{path}"));
        assert_eq!(parse("o/r"), Some(link("o", "r", Kind::Repo)));
        assert_eq!(parse("o/r.git"), Some(link("o", "r", Kind::Repo)));
        assert_eq!(
            parse("o/my.repo_1/"),
            Some(link("o", "my.repo_1", Kind::Repo))
        );
        assert_eq!(
            parse("o/r/issues/12"),
            Some(link("o", "r", Kind::Issue(12)))
        );
        assert_eq!(
            parse("o/r/issues/12#issuecomment-1"),
            Some(link("o", "r", Kind::Issue(12)))
        );
        assert_eq!(
            parse("o/r/pull/7/files"),
            Some(link("o", "r", Kind::Pull(7)))
        );
        assert_eq!(
            parse("o/r/commit/0123abc"),
            Some(link("o", "r", Kind::Commit("0123abc".into())))
        );
        assert_eq!(
            parse("o/r/blob/main/src/lib.rs?plain=1"),
            Some(link(
                "o",
                "r",
                Kind::File {
                    reference: "main".into(),
                    path: "src/lib.rs".into()
                }
            ))
        );
        // 一覧のページなどは、リポジトリとして読む
        assert_eq!(parse("o/r/issues"), Some(link("o", "r", Kind::Repo)));
        assert_eq!(parse("o/r/tree/main"), Some(link("o", "r", Kind::Repo)));
    }

    #[test]
    fn things_that_are_not_repositories_are_ignored() {
        for url in [
            "https://github.com/",
            "https://github.com/o",
            "https://github.com/settings/profile",
            "https://github.com/orgs/o/people",
            "https://github.com/o/r/issues/abc",
            "https://github.com/o/r/blob/main/../secret",
            "https://github.com/o/..",
            "https://github.com/o$/r",
            "https://github.com/o/r;rm",
            "https://example.com/o/r",
            "http://github.com/o/r",
        ] {
            assert_eq!(parse_link(url), None, "{url}");
        }
    }

    #[test]
    fn links_are_found_inside_japanese_text() {
        let text =
            "これ見て https://github.com/o/r/issues/3、あと(https://github.com/o/r/pull/4)。\
                    同じもの https://github.com/o/r/issues/3 と https://github.com/a-b/c.d.";
        assert_eq!(
            find_links(text),
            vec![
                link("o", "r", Kind::Issue(3)),
                link("o", "r", Kind::Pull(4)),
                link("a-b", "c.d", Kind::Repo),
            ]
        );
        assert!(find_links("リンクなし github.com/o/r").is_empty());
        assert!(find_links("https://github.com/ だけ").is_empty());
    }

    #[test]
    fn at_most_five_links_are_fetched() {
        let text: String = (0..9)
            .map(|i| format!("https://github.com/o/r/issues/{i} "))
            .collect();
        assert_eq!(find_links(&text).len(), MAX_LINKS);
    }

    #[test]
    fn gh_commands_are_built_without_a_shell() {
        let issue = link("o", "r", Kind::Issue(3)).gh();
        assert_eq!(issue.program, "gh");
        assert_eq!(
            issue.args,
            ["issue", "view", "3", "-R", "o/r", "--comments"]
        );
        assert_eq!(issue.display, "gh issue view 3 -R o/r --comments");
        assert!(issue
            .env
            .contains(&("GH_PROMPT_DISABLED".into(), "1".into())));
        assert_eq!(
            link("o", "r", Kind::Pull(4)).gh().args,
            ["pr", "view", "4", "-R", "o/r", "--comments"]
        );
        assert_eq!(
            link("o", "r", Kind::Repo).gh().args,
            ["repo", "view", "o/r"]
        );
        let file = link(
            "o",
            "r",
            Kind::File {
                reference: "main".into(),
                path: "a/b.rs".into(),
            },
        );
        assert_eq!(
            file.gh().args,
            [
                "api",
                "-H",
                "Accept: application/vnd.github.raw",
                "repos/o/r/contents/a/b.rs?ref=main"
            ]
        );
        assert_eq!(
            link("o", "r", Kind::Commit("abc1234".into())).gh().args[3],
            "repos/o/r/commits/abc1234"
        );
    }

    #[test]
    fn urls_round_trip_and_map_to_the_api() {
        for url in [
            "https://github.com/o/r",
            "https://github.com/o/r/issues/3",
            "https://github.com/o/r/pull/4",
            "https://github.com/o/r/blob/main/a/b.rs",
            "https://github.com/o/r/commit/abc1234",
        ] {
            assert_eq!(parse_link(url).unwrap().url(), url);
        }
        assert_eq!(
            link("o", "r", Kind::Pull(4)).api_request(),
            (
                "https://api.github.com/repos/o/r/issues/4".into(),
                "application/vnd.github+json"
            )
        );
    }

    #[test]
    fn api_responses_become_readable_text() {
        let issue = link("o", "r", Kind::Issue(3));
        let body =
            r#"{"title":"落ちる","state":"open","user":{"login":"kanade"},"body":"起動で落ちる"}"#;
        assert_eq!(
            issue.format_api_response(body),
            "title: 落ちる\nstate: open\nauthor: kanade\n\n起動で落ちる"
        );
        let repo = link("o", "r", Kind::Repo);
        let text = repo.format_api_response(
            r#"{"full_name":"o/r","description":"説明","default_branch":"main","open_issues_count":2}"#,
        );
        assert!(text.contains("description: 説明") && text.contains("open issues: 2"));
        // ファイルの中身や、JSON でない返事はそのまま
        assert_eq!(issue.format_api_response("not json"), "not json");
        let file = link(
            "o",
            "r",
            Kind::File {
                reference: "main".into(),
                path: "a".into(),
            },
        );
        assert_eq!(file.format_api_response("{\"a\":1}"), "{\"a\":1}");
    }
}
