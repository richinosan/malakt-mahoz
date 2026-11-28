// API の定義（*.tsp）で使う印。build.mjs が読み、ツールの一覧（generated/tools.json）に載せる。

/** 印は namespace Malakt に置く。 */
export const namespace = "Malakt";

const state = (name) => Symbol.for(`malakt-mahoz.${name}`);

const marks = {
  writes: state("writes"),
  confirm: state("confirm"),
  inAppOnly: state("inAppOnly"),
  view: state("view"),
  session: state("session"),
  closed: state("closed"),
};

/** データベースを書き換えるツール。1 回の呼び出しが、元に戻す 1 つの段になる。 */
export function $writes(context, target) {
  context.program.stateSet(marks.writes).add(target);
}

/** アプリの中の AI が呼ぶときに、実行の前にユーザーの確認を出すツール。 */
export function $confirm(context, target) {
  context.program.stateSet(marks.confirm).add(target);
}

/** アプリの中の AI にだけ渡すツール。MCP とコマンドには出さない（呼ぶ側が自分で持っている）。 */
export function $inAppOnly(context, target) {
  context.program.stateSet(marks.inAppOnly).add(target);
}

/** 画面を動かすツール。メインのウィンドウで実行する。 */
export function $view(context, target) {
  context.program.stateSet(marks.view).add(target);
}

/** AI の設定を変えるツール。チャットで実行する。 */
export function $session(context, target) {
  context.program.stateSet(marks.session).add(target);
}

/** 書いていない項目を受け付けないオブジェクト（additionalProperties: false）。 */
export function $closed(context, target) {
  context.program.stateSet(marks.closed).add(target);
}

export function has(program, mark, target) {
  return program.stateSet(marks[mark]).has(target);
}
