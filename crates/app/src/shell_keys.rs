//! シェルの場所（AI のタブ、ターミナル、ブラウザ）のキー。
//!
//! どこにキーの入力先があるかで、同じキーでも働きが変わる。ブラウザを見ているときは Chrome や
//! Edge と同じキー（Ctrl+R で読み込み直す、Ctrl+L で URL の欄など）、ターミナルでは Windows
//! Terminal や macOS のターミナルと同じキー（Ctrl+Shift+T で新しいタブなど）、それ以外はアプリのキー。
//!
//! ブラウザのページ（OS の部品）にキーの入力先があると、キーはアプリに届かない。そこでページの中の
//! スクリプト（`webview.rs`）が [`page_keys`] のキーだけをアプリに知らせ、アプリはそのキーを
//! ブラウザの画面で押したものとして扱う（[`crate::browser`]）。

use gpui_kit::*;

use crate::secondary;

actions!(
    malakt,
    [
        BrowserReload,
        BrowserBack,
        BrowserForward,
        FocusAddress,
        NewBrowserTab,
        ReopenTab,
        FindInPage,
        FindNext,
        FindPrevious,
        CloseFind,
        PrintPage,
        NextTab,
        PreviousTab,
        CloseTab,
        NewTerminal,
        BrowserZoomIn,
        BrowserZoomOut,
        BrowserZoomReset,
        ClosePrint,
        SubmitPrint,
        ShowDownloads,
        CloseDownloads
    ]
);

/// タブの列の何番目を見るか。`9` は最後のタブ（Chrome と同じ）。
#[derive(Clone, PartialEq, Eq, Debug, Action)]
#[action(namespace = malakt, no_json)]
pub struct SelectTab(pub usize);

const MAC: bool = cfg!(target_os = "macos");

/// ブラウザの画面の場所の名前。
pub const BROWSER: &str = "Browser";
/// ページの中を探す欄の場所の名前。
pub const FIND: &str = "BrowserFind";
/// 印刷のダイアログの場所の名前。
pub const PRINT: &str = "BrowserPrint";
/// ブラウザのダウンロードの一覧
pub const DOWNLOADS: &str = "BrowserDownloads";

/// 印刷のキー。ページの window.print も、このキーとしてアプリに知らせる
pub const PRINT_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-p"
} else {
    "ctrl-p"
};
/// シェルの場所（タブの列の下すべて）の名前。
pub const SHELL: &str = "AiShell";
/// ターミナルの画面の場所の名前。
pub const TERMINAL: &str = "Terminal";

/// 1 から 9 までの、タブを選ぶキー。
fn digits(keys: [&'static str; 9], context: &'static str) -> Vec<KeyBinding> {
    keys.iter()
        .enumerate()
        .map(|(at, key)| KeyBinding::new(key, SelectTab(at + 1), Some(context)))
        .collect()
}

const CMD_DIGITS: [&str; 9] = [
    "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
];
const CTRL_DIGITS: [&str; 9] = [
    "ctrl-1", "ctrl-2", "ctrl-3", "ctrl-4", "ctrl-5", "ctrl-6", "ctrl-7", "ctrl-8", "ctrl-9",
];
const CTRL_ALT_DIGITS: [&str; 9] = [
    "ctrl-alt-1",
    "ctrl-alt-2",
    "ctrl-alt-3",
    "ctrl-alt-4",
    "ctrl-alt-5",
    "ctrl-alt-6",
    "ctrl-alt-7",
    "ctrl-alt-8",
    "ctrl-alt-9",
];

/// キーを割り当てる。場所を決めていない割り当て（ロックなど）は深さによらず同じ強さなので、
/// それより後に呼ぶ（後に割り当てたものが勝つ）。
pub fn bind(cx: &mut App) {
    let browser = Some(BROWSER);
    let shell = Some(SHELL);
    let terminal = Some(TERMINAL);
    let mut keys = vec![
        // タブを切り替える。AI のタブ・ターミナル・ブラウザのどこでも
        KeyBinding::new("ctrl-tab", NextTab, shell),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, shell),
        // ブラウザ
        KeyBinding::new("f5", BrowserReload, browser),
        KeyBinding::new(secondary!("r"), BrowserReload, browser),
        KeyBinding::new(secondary!("shift-r"), BrowserReload, browser),
        KeyBinding::new(secondary!("l"), FocusAddress, browser),
        KeyBinding::new(secondary!("t"), NewBrowserTab, browser),
        KeyBinding::new(secondary!("w"), CloseTab, browser),
        KeyBinding::new(secondary!("shift-t"), ReopenTab, browser),
        KeyBinding::new(secondary!("f"), FindInPage, browser),
        KeyBinding::new(secondary!("g"), FindNext, browser),
        KeyBinding::new(secondary!("shift-g"), FindPrevious, browser),
        KeyBinding::new(secondary!("p"), PrintPage, browser),
        // ページの上の拡大縮小はページのスクリプトが受ける。これは URL の欄などにいるとき
        KeyBinding::new(secondary!("="), BrowserZoomIn, browser),
        KeyBinding::new(secondary!("+"), BrowserZoomIn, browser),
        KeyBinding::new(secondary!("-"), BrowserZoomOut, browser),
        KeyBinding::new(secondary!("0"), BrowserZoomReset, browser),
        KeyBinding::new("ctrl-pagedown", NextTab, browser),
        KeyBinding::new("ctrl-pageup", PreviousTab, browser),
        // ページの中を探す欄。Enter / Shift+Enter は、欄の Enter の知らせで受ける
        KeyBinding::new("escape", CloseFind, Some(FIND)),
        // 印刷のダイアログ。ファイル名の欄の Enter は、欄の Enter の知らせで受ける
        KeyBinding::new("escape", ClosePrint, Some(PRINT)),
        KeyBinding::new("enter", SubmitPrint, Some(PRINT)),
        KeyBinding::new("escape", CloseDownloads, Some(DOWNLOADS)),
    ];
    keys.extend(digits(if MAC { CMD_DIGITS } else { CTRL_DIGITS }, SHELL));
    if MAC {
        keys.extend([
            KeyBinding::new("cmd-[", BrowserBack, browser),
            KeyBinding::new("cmd-]", BrowserForward, browser),
            KeyBinding::new("cmd-shift-]", NextTab, shell),
            KeyBinding::new("cmd-shift-[", PreviousTab, shell),
            KeyBinding::new("cmd-alt-right", NextTab, browser),
            KeyBinding::new("cmd-alt-left", PreviousTab, browser),
            KeyBinding::new("cmd-t", NewTerminal, terminal),
            KeyBinding::new("cmd-w", CloseTab, terminal),
            // ダウンロードの一覧（Chrome の macOS と同じ）
            KeyBinding::new("cmd-alt-l", ShowDownloads, browser),
            KeyBinding::new("cmd-shift-j", ShowDownloads, browser),
        ]);
    } else {
        keys.extend([
            KeyBinding::new("shift-f5", BrowserReload, browser),
            KeyBinding::new("ctrl-f5", BrowserReload, browser),
            KeyBinding::new("alt-left", BrowserBack, browser),
            KeyBinding::new("alt-right", BrowserForward, browser),
            KeyBinding::new("alt-d", FocusAddress, browser),
            KeyBinding::new("f6", FocusAddress, browser),
            KeyBinding::new("ctrl-f4", CloseTab, browser),
            KeyBinding::new("f3", FindNext, browser),
            KeyBinding::new("shift-f3", FindPrevious, browser),
            KeyBinding::new("ctrl-shift-t", NewTerminal, terminal),
            KeyBinding::new("ctrl-shift-w", CloseTab, terminal),
            // ダウンロードの一覧（Chrome と同じ）。ブラウザの外の Ctrl+J は AI チャット
            KeyBinding::new("ctrl-j", ShowDownloads, browser),
        ]);
        keys.extend(digits(CTRL_ALT_DIGITS, TERMINAL));
    }
    cx.bind_keys(keys);
}

/// ページの中で押したときに、アプリへ知らせるキー（gpui の書き方）。
///
/// `take` はページより先に受けるキー（タブやウィンドウの操作、アプリのキー）。ページには届かない。
/// `soft` はページが使わなかったとき（`preventDefault` しなかったとき）だけ受けるキー
/// （読み込み直す、戻る・進む、探す、印刷。Chrome でもページが先に使える）。
pub struct PageKeys {
    pub take: Vec<String>,
    pub soft: Vec<String>,
}

pub fn page_keys() -> PageKeys {
    let s = |keys: &[&str]| -> Vec<String> { keys.iter().map(|k| (*k).to_owned()).collect() };
    let mut take = if MAC {
        s(&[
            "cmd-l",
            "cmd-t",
            "cmd-w",
            "cmd-shift-t",
            "cmd-shift-]",
            "cmd-shift-[",
            "cmd-alt-right",
            "cmd-alt-left",
            "ctrl-tab",
            "ctrl-shift-tab",
            "cmd-alt-l",
            "cmd-shift-j",
            // アプリのキー
            "ctrl-cmd-f",
            "cmd-j",
            "cmd-q",
            "cmd-,",
            "cmd-shift-n",
            "f1",
        ])
    } else {
        s(&[
            "ctrl-l",
            "alt-d",
            "f6",
            "ctrl-t",
            "ctrl-w",
            "ctrl-f4",
            "ctrl-shift-t",
            "ctrl-tab",
            "ctrl-shift-tab",
            "ctrl-pagedown",
            "ctrl-pageup",
            // アプリのキー
            "f11",
            "ctrl-j",
            "ctrl-q",
            "ctrl-,",
            "ctrl-shift-n",
            "f1",
        ])
    };
    let modifier = if MAC { "cmd" } else { "ctrl" };
    take.extend((1..=9).map(|n| format!("{modifier}-{n}")));
    let soft = if MAC {
        s(&[
            "cmd-r",
            "cmd-shift-r",
            "cmd-[",
            "cmd-]",
            "cmd-f",
            "cmd-g",
            "cmd-shift-g",
            "cmd-p",
            // キー操作の一覧（ページが自分のキーの一覧に使っていなければ）
            "cmd-/",
            "cmd-shift-/",
        ])
    } else {
        s(&[
            "f5",
            "shift-f5",
            "ctrl-f5",
            "ctrl-r",
            "ctrl-shift-r",
            "alt-left",
            "alt-right",
            "ctrl-f",
            "f3",
            "shift-f3",
            "ctrl-g",
            "ctrl-shift-g",
            "ctrl-p",
            // キー操作の一覧（ページが自分のキーの一覧に使っていなければ）
            "ctrl-/",
            "ctrl-shift-/",
        ])
    };
    let script_order = |keys: Vec<String>| keys.iter().map(|k| in_script_order(k)).collect();
    PageKeys {
        take: script_order(take),
        soft: script_order(soft),
    }
}

/// ページのスクリプト（`webview.rs` の KEY_SCRIPT）が組み立てる順に、修飾キーを並べ直す。
/// スクリプトは ctrl・alt・shift・cmd の順に書くので、`cmd-shift-t` は `shift-cmd-t` になる。
fn in_script_order(key: &str) -> String {
    const ORDER: [&str; 4] = ["ctrl", "alt", "shift", "cmd"];
    let mut parts: Vec<&str> = key.split('-').collect();
    let last = parts.pop().unwrap_or_default();
    parts.sort_by_key(|part| ORDER.iter().position(|m| m == part));
    parts.push(last);
    parts.join("-")
}

/// ページから届いたキーか（[`page_keys`] にあるか）。ページのスクリプトはほかのキーも送れるので、確かめる。
pub fn is_page_key(key: &str) -> bool {
    let keys = page_keys();
    let key = in_script_order(key);
    keys.take.iter().chain(&keys.soft).any(|k| *k == key)
}

/// ページから届いたキーを、gpui のキーにする。
pub fn keystroke(key: &str) -> Option<Keystroke> {
    let os = if MAC {
        OperatingSystem::Mac
    } else if cfg!(windows) {
        OperatingSystem::Windows
    } else {
        OperatingSystem::Linux
    };
    Keystroke::parse_with_os(key, os).ok()
}

fn pick(windows: &'static str, mac: &'static str) -> &'static str {
    if MAC {
        mac
    } else {
        windows
    }
}

/// キーの一覧に出す、ブラウザのキー。
pub fn browser_help() -> Vec<(&'static str, &'static str)> {
    vec![
        (pick("Ctrl+R / F5", "⌘R"), "読み込み直す"),
        (pick("Alt+← / Alt+→", "⌘[ / ⌘]"), "戻る / 進む"),
        (pick("Ctrl+L / Alt+D / F6", "⌘L"), "URL の欄"),
        (pick("Ctrl+T", "⌘T"), "新しいタブ"),
        (pick("Ctrl+W", "⌘W"), "タブを閉じる"),
        (pick("Ctrl+Shift+T", "⇧⌘T"), "閉じたタブを開き直す"),
        (
            pick("Ctrl+Tab / Ctrl+Shift+Tab", "⌃Tab / ⌃⇧Tab"),
            "次 / 前のタブ",
        ),
        (
            pick("Ctrl+1 〜 Ctrl+9", "⌘1 〜 ⌘9"),
            "その番号のタブ（9 は最後）",
        ),
        (pick("Ctrl+F", "⌘F"), "ページ内を検索"),
        (pick("Enter / F3", "Enter / ⌘G"), "次を検索"),
        (pick("Shift+Enter / Shift+F3", "⇧Enter / ⇧⌘G"), "前を検索"),
        (
            pick("Ctrl++ / Ctrl+- / Ctrl+0", "⌘+ / ⌘- / ⌘0"),
            "拡大 / 縮小 / 100%",
        ),
        (pick("Ctrl+P", "⌘P"), "印刷・PDF に保存"),
        (pick("Ctrl+J", "⌥⌘L"), "ダウンロード"),
        (pick("F11", "⌃⌘F"), "全画面"),
    ]
}

/// キーの一覧に出す、ターミナルのキー。
pub fn terminal_help() -> Vec<(&'static str, &'static str)> {
    vec![
        (pick("Ctrl+Shift+C", "⌘C"), "コピー"),
        (pick("Ctrl+Shift+V / Ctrl+V", "⌘V"), "貼り付け"),
        (pick("Ctrl+C", "⌃C"), "選んでいればコピー、なければ中断"),
        (pick("Ctrl+Shift+A", "⌘A"), "すべて選ぶ"),
        (pick("Ctrl+Shift+T", "⌘T"), "新しいターミナル"),
        (pick("Ctrl+Shift+W", "⌘W"), "タブを閉じる"),
        (
            pick("Ctrl+Tab / Ctrl+Shift+Tab", "⌃Tab / ⌃⇧Tab"),
            "次 / 前のタブ",
        ),
        (
            pick("Ctrl+Alt+1 〜 9", "⌘1 〜 ⌘9"),
            "その番号のタブ（9 は最後）",
        ),
        ("Shift+PageUp / PageDown", "出力をさかのぼる"),
        (pick("Ctrl+クリック", "⌘クリック"), "リンクをブラウザで開く"),
        (pick("F11", "⌃⌘F"), "全画面"),
    ]
}

#[cfg(test)]
mod tests {
    use super::{in_script_order, is_page_key, keystroke, page_keys};

    /// ページから届くキーは、どれも gpui のキーとして読め、同じキーが 2 回並ばない。
    #[test]
    fn page_keys_parse_and_are_unique() {
        let keys = page_keys();
        let all: Vec<&String> = keys.take.iter().chain(&keys.soft).collect();
        for key in &all {
            let parsed = keystroke(key).unwrap_or_else(|| panic!("{key} を読めない"));
            assert!(!parsed.key.is_empty(), "{key}");
            assert!(is_page_key(key));
        }
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "同じキーが並んでいる");
        assert!(!is_page_key("ctrl-a"), "ページの文字の操作はページのまま");
        assert!(!is_page_key("ctrl-c"));
    }

    /// ページのスクリプトと同じ順（ctrl・alt・shift・cmd）に並べる。macOS の ⌘⇧T や ⌘⌥→ も届く。
    #[test]
    fn page_keys_follow_the_script_order() {
        assert_eq!(in_script_order("cmd-shift-t"), "shift-cmd-t");
        assert_eq!(in_script_order("cmd-alt-right"), "alt-cmd-right");
        assert_eq!(in_script_order("cmd-shift-]"), "shift-cmd-]");
        assert_eq!(in_script_order("ctrl-cmd-f"), "ctrl-cmd-f");
        assert_eq!(in_script_order("ctrl-shift-tab"), "ctrl-shift-tab");
        assert_eq!(in_script_order("f11"), "f11");
        assert_eq!(in_script_order("ctrl-,"), "ctrl-,");
        if cfg!(target_os = "macos") {
            assert!(is_page_key("shift-cmd-t") && is_page_key("cmd-shift-t"));
        }
        let keys = page_keys();
        for key in keys.take.iter().chain(&keys.soft) {
            assert_eq!(&in_script_order(key), key);
        }
    }
}
