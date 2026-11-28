import "./lib.tsp";

using Malakt;

namespace Malakt.Mcp;

/**
 * OS にログインしたときに malakt-mahoz のアプリを起動するか（スタートアップ）を読む。
 * available が false なら、この OS では登録できない。
 */
op get_autostart(): void;

/**
 * OS にログインしたときに malakt-mahoz のアプリを起動するか（スタートアップ）を変える。
 * enabled が true で登録、false で外す。
 * undo では戻らないので、戻すときはもう一度呼ぶ。
 */
op set_autostart(enabled: boolean): void;

/**
 * アプリのウィンドウを常に最前面に表示しているかを読む。
 * task はメインのウィンドウ（最初に開いた 1 枚）、pomodoro はポモドーロ、timer はタイマー、chat は AI チャットだけのウィンドウ、shell はシェルだけのウィンドウ。
 * 開いていないウィンドウは、次に開いたときの状態。
 */
op get_on_top(): void;

/**
 * アプリのウィンドウを常に最前面に表示する（on が true）、または元に戻す（false）。
 * window は get_on_top と同じ（標準は task）。
 * アプリが 2 秒以内に切り替える。
 * 開いていないウィンドウは、次に開いたときに最前面に出す。
 * undo では戻らないので、戻すときはもう一度呼ぶ。
 */
op set_on_top(on: boolean, window?: Malakt.App.OnTopWindow): void;

/** このサーバーからの直前の変更を 1 つ元に戻す。続けて呼ぶと、さらに前の変更を戻す。 */
op undo(): void;

/** undo で戻した変更をやり直す。 */
op redo(): void;
