//! タスクへのリンク。コメントや説明に貼ると、そのタスクのカードになる。

/// リンクの頭。この後ろにタスクの ID が続く。
pub const PREFIX: &str = "malakt-mahoz://task/";

/// タスクへのリンク。
pub fn url(task_id: &str) -> String {
    format!("{PREFIX}{task_id}")
}

/// 文章の中のリンクが指すタスクの ID。出てきた順で、同じものは 1 回だけ。
pub fn find(text: &str) -> Vec<&str> {
    find_ids(PREFIX, text)
}

/// 文章の中の、頭が `prefix` のリンクが指す ID。出てきた順で、同じものは 1 回だけ。
pub(crate) fn find_ids<'a>(prefix: &str, text: &'a str) -> Vec<&'a str> {
    let mut ids: Vec<&str> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        let after = &rest[at + prefix.len()..];
        let end = after
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(after.len());
        let id = &after[..end];
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
        rest = &after[end..];
    }
    ids
}

/// 文章からリンクを抜いたもの。リンクをカードで見せるときに、同じものを 2 回出さない。
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let end = after
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(after.len());
        out.push_str(&rest[..at]);
        if end == 0 {
            // ID のないものはリンクではないので、そのまま残す
            out.push_str(PREFIX);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_found_in_order_without_duplicates() {
        let a = url("01ABC");
        let b = url("01XYZ");
        assert_eq!(a, "malakt-mahoz://task/01ABC");
        let text = format!("先にこれ {a}、次に\n{b} と、もう一度 {a}。");
        assert_eq!(find(&text), ["01ABC", "01XYZ"]);
        assert_eq!(strip(&text), "先にこれ 、次に\n と、もう一度 。");
        assert_eq!(strip(&a), "");
        assert!(find("リンクなし").is_empty());
        // ID のないものはリンクとして扱わない
        assert!(find("malakt-mahoz://task/ だけ").is_empty());
        assert_eq!(
            strip("malakt-mahoz://task/ だけ"),
            "malakt-mahoz://task/ だけ"
        );
    }
}
