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

/// `gh` にログインしているアカウントの名前を調べるコマンド。上から順に試し、最初に読めたものを使う。
/// `network` が `false` なら、`gh` の設定を読むだけで、GitHub には通信しない。
pub fn whoami(network: bool) -> Vec<Spec> {
    whoami_for(network, cfg!(windows))
}

fn whoami_for(network: bool, windows: bool) -> Vec<Spec> {
    let mut commands = vec!["gh config get user -h github.com"];
    if network {
        // 環境変数のトークンでログインしていると、設定には名前がない
        commands.push("gh api user --jq .login");
    }
    let mut specs = Vec::new();
    for command in commands {
        let args = command.split(' ').skip(1).map(str::to_owned).collect();
        let mut spec = Spec::direct("gh", args);
        spec.env = quiet_env();
        specs.push(spec);
        if windows {
            // gh を WSL の中にだけ入れている。ログインシェルに PATH を読ませる
            let mut spec = Spec::direct(
                "wsl.exe",
                vec!["-e".into(), "sh".into(), "-lc".into(), command.into()],
            );
            spec.env = quiet_env();
            specs.push(spec);
        }
    }
    specs
}

/// [`whoami`] のコマンドの出力から、アカウントの名前を読む。
pub fn parse_login(output: &str) -> Option<String> {
    let line = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    (is_name(line, &[]) && line.len() <= 39).then(|| line.to_owned())
}

/// ユーザーの GitHub のアカウントを、AI に伝える文。
pub fn account_note(login: &str) -> String {
    format!(
        "ユーザーの GitHub アカウントは {login} です。GitHub で {login} が担当やレビューを頼まれているものは、ユーザー自身（担当者の「自分」）のものです。"
    )
}

/// 入力を待って止まらないようにする。
pub(crate) fn quiet_env() -> Vec<(String, String)> {
    vec![
        ("GH_PROMPT_DISABLED".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
    ]
}

/// `gh issue view --json` で受け取る項目。
const ISSUE_FIELDS: &str =
    "number,title,state,author,body,labels,assignees,milestone,closedAt,url,comments";
/// `gh pr view --json` で受け取る項目。
const PULL_FIELDS: &str = "number,title,state,isDraft,author,body,labels,assignees,baseRefName,headRefName,reviewDecision,mergedAt,closedAt,additions,deletions,changedFiles,url,comments,reviews";

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
            // `--comments` は、端末でないときはコメントだけを出す（コメントがなければ空になる）。
            // 題、本文、状態、コメントを必ず取れるように、項目を決めて JSON で受け取る
            Kind::Issue(n) => vec![
                "issue".into(),
                "view".into(),
                n.to_string(),
                "-R".into(),
                slug,
                "--json".into(),
                ISSUE_FIELDS.into(),
            ],
            Kind::Pull(n) => vec![
                "pr".into(),
                "view".into(),
                n.to_string(),
                "-R".into(),
                slug,
                "--json".into(),
                PULL_FIELDS.into(),
            ],
            Kind::File { .. } | Kind::Commit(_) => {
                let (path, accept) = self.api();
                vec!["api".into(), "-H".into(), format!("Accept: {accept}"), path]
            }
        };
        let mut spec = Spec::direct("gh", args);
        spec.env = quiet_env();
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

/// PR の時刻（所要時間の基準）を読むときに受け取る項目。
const PR_TIME_FIELDS: &str = "author,createdAt,mergedAt,reviews";

/// PR の時刻（UNIX 時刻、ミリ秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrTimes {
    /// 作った時
    pub opened: i64,
    /// 作った人以外が初めてレビューした時（承認・変更の依頼・コメントのどれでも）
    pub reviewed: Option<i64>,
    /// マージした時
    pub merged: Option<i64>,
}

impl Link {
    /// PR の時刻を読むコマンド。PR のリンクでなければ `None`。
    pub fn pr_times_command(&self) -> Option<Spec> {
        let Kind::Pull(n) = self.kind else {
            return None;
        };
        let mut spec = Spec::direct(
            "gh",
            vec![
                "pr".into(),
                "view".into(),
                n.to_string(),
                "-R".into(),
                self.slug(),
                "--json".into(),
                PR_TIME_FIELDS.into(),
            ],
        );
        spec.env = quiet_env();
        Some(spec)
    }
}

/// [`Link::pr_times_command`] の出力を読む。
pub fn parse_pr_times(output: &str) -> Result<PrTimes, String> {
    let v: Value = serde_json::from_str(output.trim())
        .map_err(|e| format!("gh の出力を JSON として読めませんでした: {e}"))?;
    let time = |v: &Value, key: &str| -> Result<Option<i64>, String> {
        match v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => crate::watch::parse_time(s)
                .map(|secs| Some(secs * 1000))
                .ok_or_else(|| format!("{key} の時刻を読めませんでした: {s}")),
        }
    };
    let opened = time(&v, "createdAt")?.ok_or("PR を作った時刻（createdAt）がありません")?;
    let login = |v: &Value| {
        v.get("author")
            .and_then(|a| a.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let author = login(&v);
    let mut reviewed: Option<i64> = None;
    for review in v
        .get("reviews")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let state = review.get("state").and_then(Value::as_str).unwrap_or("");
        // 下書きのレビューと、作った人が自分の PR に付けたものは数えない
        if state == "PENDING" || (author.is_some() && login(review) == author) {
            continue;
        }
        if let Some(at) = time(review, "submittedAt")? {
            reviewed = Some(reviewed.map_or(at, |r| r.min(at)));
        }
    }
    Ok(PrTimes {
        opened,
        reviewed,
        merged: time(&v, "mergedAt")?,
    })
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
            ["issue", "view", "3", "-R", "o/r", "--json", ISSUE_FIELDS]
        );
        // コメントがなくても、題と本文は必ず出る形で取る
        assert!(ISSUE_FIELDS.contains("title") && ISSUE_FIELDS.contains("body"));
        assert!(PULL_FIELDS.contains("title") && PULL_FIELDS.contains("body"));
        assert!(issue
            .env
            .contains(&("GH_PROMPT_DISABLED".into(), "1".into())));
        assert_eq!(
            link("o", "r", Kind::Pull(4)).gh().args,
            ["pr", "view", "4", "-R", "o/r", "--json", PULL_FIELDS]
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
    fn the_account_is_read_from_the_gh_settings_first() {
        let display =
            |specs: Vec<Spec>| -> Vec<String> { specs.into_iter().map(|s| s.display).collect() };
        // 通信しないときは、設定を読むだけ
        assert_eq!(
            display(whoami_for(false, false)),
            ["gh config get user -h github.com"]
        );
        assert_eq!(
            display(whoami_for(true, false)),
            [
                "gh config get user -h github.com",
                "gh api user --jq .login"
            ]
        );
        // Windows では、gh が WSL の中にだけあるときも読む
        let windows = whoami_for(true, true);
        assert_eq!(
            display(windows.clone()),
            [
                "gh config get user -h github.com",
                "wsl.exe -e sh -lc gh config get user -h github.com",
                "gh api user --jq .login",
                "wsl.exe -e sh -lc gh api user --jq .login",
            ]
        );
        assert_eq!(windows[1].args[3], "gh config get user -h github.com");
        for spec in windows {
            assert!(spec
                .env
                .contains(&("GH_PROMPT_DISABLED".into(), "1".into())));
        }
    }

    #[test]
    fn the_login_is_the_last_line_of_the_output() {
        assert_eq!(parse_login("octo-cat\n"), Some("octo-cat".into()));
        assert_eq!(
            parse_login("bash: no job control in this shell\r\nkanade\r\n\r\n"),
            Some("kanade".into())
        );
        for output in [
            "",
            "\n",
            "could not find key \"user\"",
            "a b",
            "名前",
            &"a".repeat(40),
        ] {
            assert_eq!(parse_login(output), None, "{output}");
        }
        assert!(account_note("octo").contains("GitHub アカウントは octo です"));
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

    #[test]
    fn pr_times_are_read_from_gh() {
        let link = parse_link("https://github.com/o/r/pull/12").unwrap();
        let spec = link.pr_times_command().unwrap();
        assert_eq!(
            spec.args,
            [
                "pr",
                "view",
                "12",
                "-R",
                "o/r",
                "--json",
                "author,createdAt,mergedAt,reviews"
            ]
        );
        assert!(parse_link("https://github.com/o/r/issues/12")
            .unwrap()
            .pr_times_command()
            .is_none());
        let out = r#"{"author":{"login":"me"},"createdAt":"2026-11-27T01:00:00Z","mergedAt":"2026-11-27T05:00:00Z",
            "reviews":[{"author":{"login":"me"},"state":"COMMENTED","submittedAt":"2026-11-27T01:30:00Z"},
                       {"author":{"login":"you"},"state":"PENDING","submittedAt":""},
                       {"author":{"login":"you"},"state":"APPROVED","submittedAt":"2026-11-27T03:00:00Z"},
                       {"author":{"login":"bot"},"state":"COMMENTED","submittedAt":"2026-11-27T02:00:00Z"}]}"#;
        let t = parse_pr_times(out).unwrap();
        let at = |s: &str| crate::watch::parse_time(s).unwrap() * 1000;
        assert_eq!(t.opened, at("2026-11-27T01:00:00Z"));
        assert_eq!(t.reviewed, Some(at("2026-11-27T02:00:00Z")));
        assert_eq!(t.merged, Some(at("2026-11-27T05:00:00Z")));
        // まだのものは None。読めないものはエラー
        let t = parse_pr_times(r#"{"author":{"login":"me"},"createdAt":"2026-11-27T01:00:00Z","mergedAt":null,"reviews":[]}"#).unwrap();
        assert_eq!((t.reviewed, t.merged), (None, None));
        assert!(parse_pr_times("not json").is_err());
        assert!(parse_pr_times(r#"{"reviews":[]}"#).is_err());
    }
}
