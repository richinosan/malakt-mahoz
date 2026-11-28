// ツールに付ける印。中身は lib.js、ツールの一覧への載せ方は build.mjs。
import "./lib.js";

using TypeSpec.Reflection;

namespace Malakt;

/** データベースを書き換えるツール。1 回の呼び出しが、元に戻す 1 つの段になる。 */
extern dec writes(target: Operation);

/** アプリの中の AI が呼ぶときに、実行の前にユーザーの確認を出すツール。 */
extern dec confirm(target: Operation);

/** アプリの中の AI にだけ渡すツール。MCP とコマンドには出さない（呼ぶ側が自分で持っている）。 */
extern dec inAppOnly(target: Operation);

/** 画面を動かすツール。メインのウィンドウで実行する。 */
extern dec view(target: Operation);

/** AI の設定を変えるツール。チャットで実行する。 */
extern dec session(target: Operation);

/** ユーザーの PC のファイルを読むツール。プラグインからは呼べない。 */
extern dec localFiles(target: Operation);

/** 書いていない項目を受け付けないオブジェクト（additionalProperties: false）。 */
extern dec closed(target: Model);
