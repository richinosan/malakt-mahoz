//! シナリオテストのキーが、どの OS でも同じ意味になることを確かめる。
//!
//! 全選択・コピー・元に戻すなどは、macOS では Cmd、ほかでは Ctrl で押す。
//! テストに `ctrl-a` と書くと、macOS では「行の先頭へ」になり、macOS のテストだけが失敗する。
//! こういうキーは `secondary-` で書く。

/// どの OS でも Ctrl で押すキー（端末に届く中断・画面のクリア・行の消去・入力の終わり、タブの切り替えなど）
const SAME_ON_EVERY_OS: [&str; 7] = [
    "\"ctrl-c\"",
    "\"ctrl-l\"",
    "\"ctrl-u\"",
    "\"ctrl-d\"",
    "\"ctrl-j\"",
    "\"ctrl-tab\"",
    "\"ctrl-shift-tab\"",
];

#[test]
fn scenario_keys_mean_the_same_on_every_os() {
    let sources = [
        ("ui.rs", include_str!("ui.rs")),
        ("ai.rs", include_str!("ai.rs")),
        ("buttons.rs", include_str!("buttons.rs")),
        ("term.rs", include_str!("term.rs")),
        ("drag.rs", include_str!("drag.rs")),
    ];
    let mut wrong = Vec::new();
    for (file, source) in sources {
        let mut previous = "";
        for (number, line) in source.lines().enumerate() {
            let fixed = line.contains("\"ctrl-") || line.contains("\"cmd-");
            // `#[cfg(target_os = ...)]` で OS ごとに書き分けた行は、そのままでよい
            let per_os = previous.trim_start().starts_with("#[cfg(");
            previous = line;
            if fixed && !per_os && !SAME_ON_EVERY_OS.iter().any(|key| line.contains(key)) {
                wrong.push(format!("{file}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "secondary- で書く:\n{}", wrong.join("\n"));
}
