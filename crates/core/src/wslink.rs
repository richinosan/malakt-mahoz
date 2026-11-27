//! ワークスペースへのリンク。AI チャットの入力欄に貼ると、そのワークスペースを添える。

/// リンクの頭。この後ろにワークスペースの ID が続く。
pub const PREFIX: &str = "malakt-mahoz://workspace/";

/// ワークスペースへのリンク。
pub fn url(workspace_id: &str) -> String {
    format!("{PREFIX}{workspace_id}")
}

/// 文章の中のリンクが指すワークスペースの ID。出てきた順で、同じものは 1 回だけ。
pub fn find(text: &str) -> Vec<&str> {
    crate::tasklink::find_ids(PREFIX, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_found_in_order_without_duplicates() {
        let a = url("01ABC");
        let b = url("01XYZ");
        assert_eq!(a, "malakt-mahoz://workspace/01ABC");
        let text = format!("{a}\n{b} と {a}、タスクは {}", crate::tasklink::url("01T"));
        assert_eq!(find(&text), ["01ABC", "01XYZ"]);
        // タスクのリンクとは混ざらない
        assert_eq!(crate::tasklink::find(&text), ["01T"]);
        assert!(find("malakt-mahoz://workspace/ だけ").is_empty());
    }
}
