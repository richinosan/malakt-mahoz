//! シナリオテストのキーが、どの OS でも同じ意味になることを確かめる。
//!
//! 全選択・コピー・元に戻すなどは、macOS では Cmd、ほかでは Ctrl で押す。
//! テストに `ctrl-a` と書くと、macOS では「行の先頭へ」になり、macOS のテストだけが失敗する。
//! こういうキーは `secondary-` で書く。

/// どの OS でも Ctrl で押すキー（端末の中断と画面のクリア）
const SAME_ON_EVERY_OS: [&str; 2] = ["\"ctrl-c\"", "\"ctrl-l\""];

#[test]
fn scenario_keys_mean_the_same_on_every_os() {
    let sources = [
        ("ui.rs", include_str!("ui.rs")),
        ("ai.rs", include_str!("ai.rs")),
        ("buttons.rs", include_str!("buttons.rs")),
    ];
    let mut wrong = Vec::new();
    for (file, source) in sources {
        for (number, line) in source.lines().enumerate() {
            let fixed = line.contains("\"ctrl-") || line.contains("\"cmd-");
            if fixed && !SAME_ON_EVERY_OS.iter().any(|key| line.contains(key)) {
                wrong.push(format!("{file}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "secondary- で書く:\n{}", wrong.join("\n"));
}
