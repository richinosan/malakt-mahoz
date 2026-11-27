//! シナリオテストのキーが、どの OS でも同じ意味になることを確かめる。
//!
//! 全選択・コピー・元に戻すなどは、macOS では Cmd、ほかでは Ctrl で押す。
//! テストに `ctrl-a` と書くと、macOS では「行の先頭へ」になり、macOS のテストだけが失敗する。
//! こういうキーは `secondary-` で書く。コピーの Ctrl+C も、macOS では ⌘C なので `secondary-c` で書く。

/// どの OS でも Ctrl で押すキー（端末に届く中断・画面のクリア・行の消去・入力の終わり、タブの切り替えなど）
/// と、どの OS でもどこにも割り当てていないキー（受けないことを確かめる）
const SAME_ON_EVERY_OS: [&str; 8] = [
    "\"ctrl-c\"",
    "\"ctrl-l\"",
    "\"ctrl-u\"",
    "\"ctrl-d\"",
    "\"ctrl-j\"",
    "\"ctrl-tab\"",
    "\"ctrl-shift-tab\"",
    "\"ctrl-shift-alt-x\"",
];

/// `ctrl-c` のあとこの行数の中でクリップボードを読んでいたら、コピーのつもりで押している
const COPY_LOOKAHEAD: usize = 4;
/// どの OS でも Ctrl+C でコピーする所（シェルの出力）で押すとき、前の行のコメントに書く印
const COPY_ON_EVERY_OS: &str = "どの OS も Ctrl+C でコピー";

#[test]
fn scenario_keys_mean_the_same_on_every_os() {
    // 新しいテストのファイルも漏れないように、tests の下の .rs を全部読む
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("tests のフォルダーを読める")
        .map(|entry| entry.expect("tests の中を読める").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter(|path| !path.ends_with("keys.rs"))
        .collect();
    files.sort();
    assert!(files.len() > 5, "テストのファイルが見つからない: {files:?}");
    let mut wrong = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("テストのファイルを読める");
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        wrong.extend(check(&file, &source));
    }
    assert!(wrong.is_empty(), "secondary- で書く:\n{}", wrong.join("\n"));
}

/// OS で意味が変わるキーを、1 つの OS のキーで書いている行を返す。
fn check(file: &str, source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut wrong = Vec::new();
    for (number, line) in lines.iter().enumerate() {
        let previous = number.checked_sub(1).map_or("", |n| lines[n]);
        let fixed = line.contains("\"ctrl-") || line.contains("\"cmd-");
        // `#[cfg(target_os = ...)]` や `key(ほか, macOS)` で OS ごとに書き分けた行は、そのままでよい
        let per_os = previous.trim_start().starts_with("#[cfg(") || line.contains("key(\"");
        if !fixed || per_os {
            continue;
        }
        let at = format!("{file}:{}: {}", number + 1, line.trim());
        if !SAME_ON_EVERY_OS.iter().any(|key| line.contains(key)) {
            wrong.push(at);
            continue;
        }
        // Ctrl+C は中断なら全 OS で同じだが、コピーは macOS では ⌘C（secondary-c）
        let copies = line.contains("\"ctrl-c\"")
            && lines[number + 1..]
                .iter()
                .take(COPY_LOOKAHEAD)
                .any(|next| next.contains("clipboard"));
        let marked = line.contains(COPY_ON_EVERY_OS) || previous.contains(COPY_ON_EVERY_OS);
        if copies && !marked {
            wrong.push(format!("{at}（コピーなら secondary-c）"));
        }
    }
    wrong
}

/// 検査そのものが、macOS で落ちた書き方を見つけられること。
#[test]
fn copy_with_ctrl_c_is_caught() {
    let copy = "    window.press(\"ctrl-c\", cx);\n    let copied = cx.read_from_clipboard();\n";
    assert_eq!(check("a.rs", copy).len(), 1);
    let stop = "    window.press(\"ctrl-c\", cx);\n    wait(cx, \"止まる\");\n";
    assert!(check("a.rs", stop).is_empty());
    let marked = format!("    // {COPY_ON_EVERY_OS}きる\n{copy}");
    assert!(check("a.rs", &marked).is_empty());
    assert_eq!(
        check("a.rs", "    window.press(\"ctrl-a\", cx);\n").len(),
        1
    );
    assert!(check(
        "a.rs",
        "    window.press(key(\"ctrl-r\", \"cmd-r\"), cx);\n"
    )
    .is_empty());
}
