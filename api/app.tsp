import "./lib.tsp";

using Malakt;

namespace Malakt.App;

// 下の候補は、アプリのコードの表（ai_tools.rs の SORTS・COLUMNS・MODES・DENSITIES・FEELS・DUE_FORMATS）と
// 同じ並び。テスト（app_enums_match_the_code）で確かめる

/** 一覧の並び順 */
union Sort {
  "due",
  "priority",
  "updated",
  "created",
  "title",
  "status",
  "assignee",
  "estimate",
  "elapsed",
  "manual",
}

/** 表の列 */
union Column {
  "status",
  "priority",
  "assignee",
  "due",
  "updated",
  "estimate",
  "elapsed",
}

/** ライト / ダーク */
union Mode {
  "system",
  "light",
  "dark",
}

/** 詰め具合 */
union Density {
  "roomy",
  "standard",
  "compact",
}

/** 見た目の質感 */
union Feel {
  "standard",
  "glass",
  "crisp",
  "tint",
}

/** 日付の出し方 */
union DueFormat {
  "date",
  "relative",
}

/** 最前面に表示できるウィンドウ。下の候補は on_top.rs の Kind と同じ並び */
union OnTopWindow {
  "task",
  "pomodoro",
  "timer",
  "chat",
  "shell",
}

/** 表の列 1 つの設定 */
@closed
model ColumnSetting {
  column: Column;
  shown?: boolean;
  width?: float64 | null;
}

/**
 * 今の画面の状態を返す。
 * 開いている場所、開いているタスク、一覧の絞り込みと並び順、表の列、左のメニューと詳細の幅、AI チャットとシェル、ペイン、ロック・リマインダー・自動アップデートの設定、開けるシェル、最前面に表示しているウィンドウ（on_top）。
 */
@view
op get_screen(): void;

/** 画面を開く。to は home（ホーム）、all（すべてのタスク）、unfiled（未所属のタスク）、またはワークスペースの ID。 */
@view
op navigate(to: string): void;

/** タスクの詳細を開く。 */
@view
op open_task(id: string): void;

/** 開いているタスクの詳細を閉じる。 */
@view
op close_task(): void;

/** タスクの一覧の絞り込みと並び順を変える。渡した項目だけが変わる。clear_filters で絞り込みを外す（タブと並び順は残る）。 */
@view
op set_list_view(
  /** 未完了 / 完了 */
  tab?: "open" | "closed",

  /** キーワード。空で外す */
  query?: string,

  status?: "needs_action" | "in_process" | "completed" | "cancelled" | null,

  /** me（自分）、unassigned（未割り当て）、担当者の ID。null ですべて */
  assignee?: string | null,

  priority?: "none" | "high" | "mid" | "low" | null,
  category?: string | null,

  /** 予想工数あり / 予想工数なし / 所要時間が予想工数を超えた。null で外す */
  time?: "estimated" | "not_estimated" | "over" | null,

  /** 期限が近い順 / 優先度が高い順 / 更新が新しい順 / 作成が新しい順 / タイトル順 / 状態順 / 担当順 / 予想工数が短い順 / 所要時間が短い順 / 手動の順 */
  sort?: Sort,

  /** 並び順を逆にする */
  descending?: boolean,

  clear_filters?: boolean,

  /** ホームの「期限が近いタスク」を自分の担当だけにする */
  home_mine_only?: boolean,
): void;

/**
 * タスクの表の列を出す・隠す、幅を変える、並べ替える。
 * order は列の並び（全部の列でなくてよい。
 * 書いた列がその順で先頭に来る）。
 * width は 56〜480、null で最初の幅。
 */
@view
op set_columns(columns?: ColumnSetting[], order?: Column[]): void;

/**
 * 画面の部分を出す・隠す、幅を変える。
 * sidebar は左のメニュー、chat は AI チャット、shell はシェルの場所（AI のタブ、ターミナル、ブラウザのタブ）。
 * 幅は null で最初の幅。
 * expanded は左のメニューのワークスペースを開く・閉じる（ID と true / false）。
 */
@view
op set_panels(
  sidebar?: boolean,

  /** 200〜480 */
  sidebar_width?: float64 | null,

  /** タスクの詳細の幅 400〜1600 */
  detail_width?: float64 | null,

  chat?: boolean,
  shell?: boolean,
  expanded?: Record<boolean>,
): void;

/**
 * 画面を分けたペインを操作する。
 * split_right / split_down（分ける）、close（今のペインを閉じる）、zoom（今のペインだけを大きく出す・戻す）、focus_next / focus_prev、focus_left / focus_right / focus_up / focus_down、swap_left / swap_right / swap_up / swap_down、duplicate（複製）、pop_out（別ウィンドウに出す）、save_layout（今の配置を保存）、load_layout（name の配置にする。
 * 最初からある配置か、保存した配置）、delete_layout（保存した配置 name を削除）。
 */
@view
op pane(
  action:
    | "split_right"
    | "split_down"
    | "close"
    | "zoom"
    | "focus_next"
    | "focus_prev"
    | "focus_left"
    | "focus_right"
    | "focus_up"
    | "focus_down"
    | "swap_left"
    | "swap_right"
    | "swap_up"
    | "swap_down"
    | "duplicate"
    | "pop_out"
    | "save_layout"
    | "load_layout"
    | "delete_layout",

  /** load_layout と delete_layout の配置の名前（get_screen の pane_layouts） */
  name?: string,
): void;

/**
 * ダイアログを開く。
 * people（担当者）、customize（カスタマイズ）、lock（ロックの設定）、notices（通知）、keys（キー操作の一覧）、workspace_settings（ワークスペースの設定。
 * workspace_id が要る）、workspace_icon（ワークスペースのアイコン。
 * workspace_id が要る）、history（開いているタスクの変更履歴）、ticket（完了チケット。
 * task_id が要る）。
 */
@view
op open_dialog(
  name:
    | "people"
    | "customize"
    | "notices"
    | "keys"
    | "workspace_settings"
    | "workspace_icon"
    | "history"
    | "ticket",
  workspace_id?: string,
  task_id?: string,
): void;

/**
 * 新しいウィンドウを開く。
 * kind は main（タスクの画面をもう 1 枚。標準）、pomodoro（ポモドーロタイマー。開いていれば手前に出す。タイマーの操作は pomodoro ツール）、timer（カウントダウンとストップウォッチのタイマー。開いていれば手前に出す。タイマーの操作は timer ツール）。
 */
@view
op open_window(kind?: "main" | "pomodoro" | "timer"): void;

/**
 * ウィンドウを常に最前面に表示する（on が true）、または元に戻す（false）。
 * window は task（操作しているタスクのウィンドウ。標準）、pomodoro（ポモドーロ）、timer（タイマー）、chat（AI チャットだけのウィンドウ）、shell（シェルだけのウィンドウ）。
 * 開いていないウィンドウは、次に開いたときに最前面に出す。
 * 今の状態は get_screen の on_top にある。
 */
@view
op set_on_top(on: boolean, window?: OnTopWindow): void;

/**
 * ターミナルを開く。
 * task_id を渡すと、そのタスクのワークスペースのフォルダーで開く。
 * shell は get_screen の terminal_shells の id（選んだシェルは次からも使う）。
 */
@view
op open_terminal(task_id?: string, shell?: string): void;

/**
 * アプリ内ブラウザ（シェルの場所のタブ）を操作する。
 * open（url を新しいタブで開いて見せる。
 * 同じ場所のタブがあればそれを見せる）、list（開いているタブの一覧）、show（tab を見せる）、navigate（tab で url を開く）、back、forward、reload、close（tab を閉じる）、open_external（url か tab のページを OS のブラウザで開く）、zoom_in / zoom_out（ページの拡大率を 1 段変える）、zoom_reset（100% に戻す）。
 * tab を省くと、見ているタブ。
 * ページの文字は read_browser で読む。
 */
@view
op browser(
  action:
    | "open"
    | "list"
    | "show"
    | "navigate"
    | "back"
    | "forward"
    | "reload"
    | "close"
    | "open_external"
    | "zoom_in"
    | "zoom_out"
    | "zoom_reset",
  url?: string,
  tab?: int32,
): void;

/**
 * アプリ内ブラウザのページの文字（url、title、ユーザーが選んでいる文字 selection、本文 text）を読む。
 * tab を省くと、ユーザーが見ているタブ。
 * ログインが要るページも、ユーザーのログインのまま読める。
 */
op read_browser(
  tab?: int32,

  /** 本文の最大の文字数（500〜20000、標準 20000） */
  max_chars?: int32,
): void;

/**
 * ロック。
 * lock_now（今すぐロック）、enable（ロックを有効にする）、disable（ロックを無効にする）、set_options（ロックの設定を変える。
 * 渡した項目だけが変わる）。
 * 有効と無効は、OS の認証（パスキー）が出て、ユーザーが通したときだけ変わる。
 * auto_lock_minutes は、操作がないまま自動でロックするまでの分数（0 で自動でロックしない）。
 * locked_notices は、ロック中の OS の通知に出すもの（title: 通知の 1 行を出す、hidden: 「新しい通知があります」だけ）。
 * 今の値は get_screen の lock にある。
 */
@view
op lock(
  action: "lock_now" | "enable" | "disable" | "set_options",
  auto_lock_minutes?: 0 | 1 | 5 | 10 | 15 | 30 | 60 | 120,
  locked_notices?: "title" | "hidden",
): void;

/**
 * アプリの設定を変える。
 * 渡した項目だけが変わる。
 * reminders は OS のリマインダーとの連携、autostart は OS にログインしたときにアプリを起動するか（スタートアップ）、auto_update は自動アップデート、pane_keys はペインのキー操作（herdr / tmux）。
 */
@view
op set_app_options(
  reminders?: boolean,
  autostart?: boolean,
  auto_update?: boolean,
  pane_keys?: "herdr" | "tmux",
): void;

/** 取ってきてある新しい版に入れ替えて、アプリを起動し直す。 */
@view
op update_now(): void;

/**
 * 全部のデータ（タスク、ワークスペース、担当者、コメント、添付）を zip に書き出す。
 * path は書き出すファイルの場所（先頭の ~ はホーム）。
 * 省くと書類のフォルダー。
 * 同じ名前のファイルがあると、上書きの前にユーザーに確認が出る。
 */
@view
op export_data(path?: string): void;

/** エクスポートした zip を取り込む。ID が同じものは上書きするので、実行の前にユーザーに確認が出る。 */
@confirm
@view
op import_data(path: string): void;

/** 直前の変更を 1 つ元に戻す（Ctrl+Z と同じ）。AI の変更も、ツールの呼び出し 1 回が 1 つの段になる。 */
@view
op undo(): void;

/** 元に戻した変更をやり直す。 */
@view
op redo(): void;

/** ワークスペースに付けられるアイコンを探す。query は英語か日本語の言葉。 */
@view
op find_icons(query: string, @minValue(1) @maxValue(100) limit?: int32): void;

/**
 * ワークスペースのアイコンと色を変える。
 * icon は find_icons で見つけた名前（null で最初のアイコン）、image_path は icon の代わりに使う画像ファイル（PNG・JPEG・SVG）の絶対パス（先頭の ~ はホーム）、color は red / orange / yellow / green / teal / blue / purple / pink（null で文字と同じ色）。
 * 渡した項目だけが変わる。
 */
@view
op set_workspace_icon(
  id: string,
  icon?: string | null,
  image_path?: string,
  color?: string | null,
): void;

/** カスタマイズ（見た目）の今の値と、選べるテーマ・レイアウト・値を返す。 */
@view
op get_customization(): void;

/** この PC で使える書体の名前を返す。query で絞り込む。 */
@view
op list_fonts(query?: string): void;

/** テーマ（おすすめのテーマか、自分で保存したテーマ）を名前で選んで使う。 */
@view
op apply_theme(name: string): void;

/** 今の見た目を、自分のテーマとして名前を付けて保存する。 */
@view
op save_theme(name: string): void;

/** 自分で保存したテーマの名前を変える。 */
@view
op rename_theme(name: string, new_name: string): void;

/** 自分で保存したテーマを削除する（今の見た目は変わらない）。元に戻せないので、実行の前にユーザーに確認が出る。 */
@confirm
@view
op delete_theme(name: string): void;

/** レイアウト（重ねる詳細 / 一覧と詳細 / 3 カラム / カード / 浮いたパネル）を名前で選んで使う。 */
@view
op apply_layout(name: string): void;

/**
 * 見た目を 1 つずつ変える。
 * 渡した項目だけが変わる。
 * mode はライト・ダーク、scene は背景の画像（none / sky / forest / dusk か、画像ファイルの絶対パス）、clear は面の透明度 0〜70、blur は背景のぼかし 0〜100、cards は一覧をカードで出す、floating はパネルを浮かせる、overlay は詳細を一覧の上に重ねる、density は行の詰め方、feel はホバーと押したときの描き方、due_format は一覧の期限の出し方（date は「11/24～11/27」、relative は「明日」「あと 3 日」「2 日前」）、fonts は画面の書体の優先順（名前の配列。
 * 文字ごとに上から順に、その文字を持つ書体で描く。
 * null で標準の LINE Seed JP）、mono_fonts はターミナルとコードの等幅の書体の優先順（null で標準）、font は書体を 1 つだけにする、accent / light_background / dark_background は #rrggbb の色（null で標準の色）。
 */
@view
op set_appearance(
  mode?: Mode,
  scene?: string,
  @minValue(0) @maxValue(70) clear?: int32,
  @minValue(0) @maxValue(100) blur?: int32,
  cards?: boolean,
  floating?: boolean,
  overlay?: boolean,

  /** ゆったり / 標準 / 詰める */
  density?: Density,

  /** 標準 / ガラス / くっきり / 色づく */
  feel?: Feel,

  /** 日付 / 相対 */
  due_format?: DueFormat,

  font?: string | null,
  fonts?: string[] | null,
  mono_fonts?: string[] | null,
  accent?: string | null,
  light_background?: string | null,
  dark_background?: string | null,
): void;

/**
 * AI チャットの設定（サービス、モデル、effort、振り分けのモデル、新しいタスクを整える、コメントから進捗を更新する、GitHub の定期チェック、オート実行）を返す。
 * AI のルールは list_ai_rules で読む。
 */
@session
op get_ai_settings(): void;

/**
 * AI チャットの設定を変える。
 * 渡した項目だけが変わり、次のメッセージから使う。
 * provider は claude / chatgpt / opencode / opencode_go / cursor。
 * effort はモデルごとに覚え、model を変えるとそのモデルで前に選んだ effort に戻る（model と effort を一緒に渡すと、新しいモデルの effort になる）。
 * routing_model は空で今のモデルを使う。
 * watch_minutes は 60 / 180 / 360 / 720 / 1440（分）。
 * auto_run をオンにするときは、実行の前にユーザーに確認が出る。
 * API キーはここでは変えられない（チャットの「設定」で入れる）。
 * AI のルールは create_ai_rule / update_ai_rule で変える。
 */
@session
op set_ai_settings(
  provider?: string,
  `model`?: string,
  effort?: string,
  routing_model?: string,

  /** 新しいタスクを AI が整える */
  arrange?: boolean,

  /** コメントから AI が進捗などを更新する */
  track?: boolean,

  /** GitHub の定期チェック */
  watch_enabled?: boolean,

  watch_minutes?: int32,
  watch_instruction?: string,

  /** コマンドの実行と削除を確認なしで行う */
  auto_run?: boolean,
): void;

/** GitHub の定期チェックを今すぐ 1 回行う。 */
@session
op check_github(): void;

/**
 * プラグインの一覧を返す（フォルダーを読み直す）。
 * state は on（有効）、off（無効）、changed（有効にしたあとでファイルが変わった。
 * もう一度有効にするまで動かない）。
 * folders は読むフォルダー（from は default・setting・env。
 * 前にあるものほど優先する）、problems は読めなかったプラグインとフォルダー。
 */
@view
op list_plugins(): void;

/**
 * 有効なプラグインのコマンドを実行する。
 * on が task のコマンドには task_id、workspace のコマンドには workspace_id を渡す。
 * 返すのはプラグインの結果（notice と、クリップボードに入れた文字 copied）。
 */
op run_plugin_command(plugin_id: string, command_id: string, task_id?: string, workspace_id?: string): void;

/**
 * プラグインを有効・無効にする。
 * 有効にするときは、設定の「プラグイン」に承認の画面を出す。
 * ユーザーが「有効にする」を押すと有効になる。
 */
@view
op set_plugin_enabled(plugin_id: string, enabled: boolean): void;

/**
 * プラグインを読むフォルダーを足す（add）・外す（remove）。
 * path はフォルダーの絶対パス（先頭の ~ はホーム）。
 * 外せるのは足したフォルダーだけ。
 */
@view
op plugin_folder(action: "add" | "remove", path: string): void;

/** 標準のフォルダー（~/.malakt-mahoz/plugins）を読むかどうかを決める。 */
@view
op set_plugin_default_folder(enabled: boolean): void;
