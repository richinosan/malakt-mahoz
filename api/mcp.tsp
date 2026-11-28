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

/** このサーバーからの直前の変更を 1 つ元に戻す。続けて呼ぶと、さらに前の変更を戻す。 */
op undo(): void;

/** undo で戻した変更をやり直す。 */
op redo(): void;
