import "./lib.tsp";

using Malakt;

namespace Malakt.Data;

/** 担当者の役（RACI）。R=実行責任者、A=説明責任者（1 つのタスクに 1 人まで）、C=相談先、I=報告先 */
union Role {
  "R",
  "A",
  "C",
  "I",
}

/** タスクの担当者 1 人と役 */
@closed
model Assignee {
  /** 担当者の ID か名前（敬称は付けない） */
  person: string;

  /**
   * 役。
   * R=実行責任者、A=説明責任者（1 つのタスクに 1 人まで）、C=相談先、I=報告先。
   * 省くと、もう担当の人は今の役のまま、新しい人は R
   */
  roles?: Role[];
}

/** 期限の何分前に知らせるか（時刻つきの期限）。上限は malakt_core::alert::MAX_MINUTES */
@minValue(0)
@maxValue(40320)
scalar AlertMinutes extends int32;

/** 期限の何日前に知らせるか（日付だけの期限）。上限は malakt_core::alert::MAX_DAYS */
@minValue(0)
@maxValue(28)
scalar AlertDays extends int32;

/** 曜日。0 = 月 … 6 = 日 */
@minValue(0)
@maxValue(6)
scalar Weekday extends int32;

/** ポモドーロの段の分数。範囲は malakt_core::pomodoro::MIN_MINUTES〜MAX_MINUTES */
@minValue(1)
@maxValue(180)
scalar PomodoroMinutes extends int32;

/** 作業・休憩を始めたときに、OS で再生中の音楽をどうするか */
union PomodoroMedia {
  "none",
  "play",
  "pause",
}

/** タスクの繰り返し */
model RepeatRule {
  unit: "day" | "week" | "month" | "year";

  /** 何日・何週間・何か月・何年ごとか（標準は 1） */
  @minValue(1) @maxValue(99) every?: int32;

  /** unit が week のときの曜日。0 = 月 … 6 = 日。省くと起点の曜日 */
  weekdays?: Weekday[];

  /** 数え始めの日 YYYY-MM-DD。毎月・毎年の日付になる。省くとタスクの開始日か期限（なければ今日） */
  from?: string;

  /** 次の回を作る時刻 HH:MM。空で 0:00 */
  time?: string;
}

/** 定時タスクを作る時刻の決まり */
model ScheduleRule {
  unit: "day" | "week" | "month" | "year";

  /** 何日・何週間・何か月・何年ごとか（標準は 1） */
  @minValue(1) @maxValue(99) every?: int32;

  /** unit が week のときの曜日。0 = 月 … 6 = 日。省くと起点の曜日 */
  weekdays?: Weekday[];

  /** 数え始めの日 YYYY-MM-DD。毎月・毎年の日付になる。省くと今日（変えるときは今の起点） */
  from?: string;

  /** タスクを作る時刻 HH:MM。空で 0:00 */
  time?: string;
}

/** 所要時間の基準。所要時間はこの始まりから終わりまでの時計の経過 */
@closed
model TimeBasis {
  /** 作った時 / 担当が付いた時 / 進行中にした時 / 開始日 */
  start: "created" | "assigned" | "in_process" | "start_date";

  /** 完了にした時 / 自分の分を完了にした時 */
  end: "completed" | "self_done";
}

/** 期限をいつ知らせるか */
model DueAlerts {
  /** 時刻つきの期限の何分前か。[] で知らせない、null で標準 */
  timed_minutes?: AlertMinutes[] | null;

  /** 日付だけの期限の何日前か。[] で知らせない、null で標準 */
  date_days?: AlertDays[] | null;
}

/** タスクの項目。作るときと書き換えるときで同じ */
model TaskFields {
  title?: string;
  description?: string;

  /** 未着手 / 進行中 / 完了 / 中止 */
  status?: "needs_action" | "in_process" | "completed" | "cancelled";

  priority?: "none" | "high" | "mid" | "low";

  /** 開始日 YYYY-MM-DD。時刻つきなら YYYY-MM-DD HH:MM。null でなくす */
  start?: string | null;

  /** 期限 YYYY-MM-DD。時刻つきなら YYYY-MM-DD HH:MM。null でなくす */
  due?: string | null;

  /** 進捗 */
  @minValue(0) @maxValue(100) percent?: int32;

  /** 親のタスクの ID（サブタスクにする） */
  parent_id?: string | null;

  /** 担当者の ID。足した人の役は R。役も決めるときは assignees を使う */
  assignee_ids?: string[];

  /**
   * 担当者と役（RACI）。
   * 渡すと担当者がこの並びになる（assignee_ids と一緒には渡さない）。
   * person は担当者の ID か名前。
   * 登録されていない名前は、その場で担当者に登録する
   */
  assignees?: Assignee[];

  categories?: string[];

  /** タスクに付ける URL。渡すと URL がこの並びになる（足すだけなら add_urls） */
  urls?: string[];

  /** 今の URL の後ろに足す URL。同じものは足さない */
  add_urls?: string[];

  /** 外す URL */
  remove_urls?: string[];

  /**
   * 予想工数。
   * 月・日・時間・分を組み合わせて書く（「1日3時間」「90分」「1.5h」「2週間」「1か月」「P1DT3H」）。
   * 単位のない数は時間。
   * null か空でなくす
   */
  estimate?: string | null;

  /** このタスクの所要時間の基準。null でワークスペースか全体の標準に合わせる */
  time_basis?: TimeBasis | null;
}

/** 定時タスクの項目。作るときと書き換えるときで同じ */
model ScheduleFields {
  /** 作るタスクのタイトル（200 文字まで） */
  title?: string;

  /** いつタスクを作るか */
  schedule?: ScheduleRule;

  /** 作ったタスクで AI に頼むこと。空で頼まない */
  ai_instruction?: string;

  /** false で止める */
  enabled?: boolean;

  /** 作るタスクの説明。null でワークスペースの既定値 */
  description?: string | null;

  /** 作るタスクの状態。未着手 / 進行中 / 完了 / 中止 */
  status?: "needs_action" | "in_process" | "completed" | "cancelled";

  /** 作るタスクの優先度。null でワークスペースの既定値 */
  priority?: "none" | "high" | "mid" | "low" | null;

  /** 開始日を作った日の何日後にするか（0 で当日）。null で開始日なし */
  @minValue(0) @maxValue(365) start_days?: int32 | null;

  /** 開始日の時刻 HH:MM。null か空で日付だけ */
  start_time?: string | null;

  /** 期限を作った日の何日後にするか（0 で当日）。null でワークスペースの既定値 */
  @minValue(0) @maxValue(365) due_days?: int32 | null;

  /** 期限の時刻 HH:MM。null か空で日付だけ */
  due_time?: string | null;

  /** 作るタスクの、期限をいつ知らせるか（set_due_alerts と同じ）。null で全体の標準 */
  due_alerts?: DueAlerts | null;

  /** 作るタスクの進捗 */
  @minValue(0) @maxValue(100) percent?: int32;

  /** 親のタスクの ID（作るタスクをそのサブタスクにする）。null で親なし */
  parent_id?: string | null;

  /** 作るタスクの担当者の ID。[] でワークスペースの既定値。足した人の役は R。役も決めるときは assignees を使う */
  assignee_ids?: string[];

  /**
   * 作るタスクの担当者と役（RACI）。
   * 渡すと担当者がこの並びになる（assignee_ids と一緒には渡さない）。
   * person は担当者の ID か名前。
   * 登録されていない名前は、その場で担当者に登録する
   */
  assignees?: Assignee[];

  /** 作るタスクのカテゴリ。null でワークスペースの既定値 */
  categories?: string[] | null;

  /** 作るタスクに付ける URL。渡すと URL がこの並びになる（足すだけなら add_urls） */
  urls?: string[];

  /** 今の URL の後ろに足す URL。同じものは足さない */
  add_urls?: string[];

  /** 外す URL */
  remove_urls?: string[];

  /** 作るタスクを、入れるワークスペースのほかにも出すワークスペース（link_task と同じ）。null でワークスペースの既定値 */
  linked_workspace_ids?: string[] | null;

  /** 作るタスクのサブタスクのタイトル（100 件まで）。渡すとこの並びになる */
  subtasks?: string[];

  /**
   * 作るタスクの予想工数。
   * 月・日・時間・分を組み合わせて書く（「1日3時間」「90分」「1.5h」「2週間」「1か月」「P1DT3H」）。
   * 単位のない数は時間。
   * null か空でなくす
   */
  estimate?: string | null;

  /** 作るタスクの所要時間の基準。null でワークスペースか全体の標準に合わせる */
  time_basis?: TimeBasis | null;
}

/** ワークスペース（タスクをまとめる単位。入れ子にできる）の一覧を返す。 */
op list_workspaces(): void;

/**
 * 担当者の一覧を返す。
 * workspace_ids が空の人はすべてのワークスペースで、そうでない人はそのワークスペース（とサブワークスペース）で選べる。
 */
op list_people(): void;

/** タスクの一覧を返す。workspace_id を省くと全部、"unfiled" でどのワークスペースにも入っていないもの。 */
op list_tasks(
  workspace_id?: string,

  /** タイトルと説明の部分一致 */
  query?: string,

  /** 完了・中止も含める（標準は含めない） */
  include_done?: boolean,
): void;

/** タスク 1 件の内容と、コメント、サブタスク、添付、変更履歴を返す。 */
op get_task(id: string): void;

/**
 * ワークスペース 1 つの中身を返す。
 * 上からの道筋、サブワークスペース（入れ子の全部）とそれぞれのタスクの数、サブワークスペースも含めたタスクの一覧。
 * 多いときはタスクの一部だけを返す（tasks_total と tasks_shown を見る）。
 */
op get_workspace(
  id: string,

  /** 完了・中止のタスクも一覧に含める（標準は含めない） */
  include_done?: boolean,
): void;

/** タスクを作る。workspace_id を省くと、どのワークスペースにも入れない。 */
@writes
op create_task(
  title: string,
  ...OmitProperties<TaskFields, "title">,
  workspace_id?: string,
): void;

/** タスクを書き換える。渡した項目だけが変わる。 */
@writes
op update_task(...TaskFields, id: string): void;

/**
 * タスクの担当者 1 人の役（RACI）を決める。
 * 担当でない人は担当に加える。
 * person は担当者の ID か名前。
 * 登録されていない名前は、タスクのワークスペースの担当者としてその場で登録する。
 */
@writes
op set_assignee(
  task_id: string,

  /** 担当者の ID か名前（敬称は付けない） */
  person: string,

  /**
   * 役。
   * R=実行責任者、A=説明責任者（1 つのタスクに 1 人まで）、C=相談先、I=報告先。
   * A を付けると、ほかの人の A は外れる。
   * 空にすると担当から外す
   */
  roles: Role[],
): void;

/**
 * 担当者 1 人の「自分の分」の完了を付ける・外す。
 * 自分の分もほかの人の分も変えられる。
 * 全員の分がそろうとタスクが完了になる。
 * 完了したタスクで誰かの分を外すと、タスクは進行中に戻る。
 */
@writes
op set_assignee_done(
  task_id: string,

  /** タスクの担当者の ID か名前（敬称は付けない） */
  person: string,

  /** true で完了にする、false で完了を外す */
  done: boolean,
): void;

/** タスクを別のワークスペースへ移す。workspace_id が null なら、どこにも入れない。 */
@writes
op move_task(id: string, workspace_id: string | null): void;

/** タスクを削除する。元に戻せないので、実行の前にユーザーに確認が出る。 */
@writes
@confirm
op delete_task(id: string): void;

/**
 * タスクにコメントを書く。
 * 書くのは、あとでタスクを読む人に要る事実だけ（決まったこと、分かったこと、変わった状態、次にすること）。
 * 「確認しました」「レビューしました」のように作業をしたことだけを伝える文と、「まだ〜していません」のようにしていないことは書かない。
 * 書く事実がなければ呼ばない。
 */
@writes
op add_comment(
  task_id: string,
  body: string,

  /** 返信するレビューのスレッドのルートのコメントの ID */
  reply_to?: string,

  /** レビューコメント（返信と解決ができるスレッド）にする */
  review?: boolean,
): void;

/** ワークスペースを作る。parent_id を渡すと、その中に作る。 */
@writes
op create_workspace(name: string, parent_id?: string): void;

/**
 * 担当者を登録する。
 * workspace_id を渡すと、そのワークスペース（のいちばん上の親とサブワークスペース）で選べる担当者になり、省くとすべてのワークスペースで選べる担当者になる。
 */
@writes
op create_person(name: string, workspace_id?: string): void;

/**
 * ワークスペースの名前を変える、別のワークスペースの中へ移す、並び順を変える。
 * parent_id が null なら、いちばん上に出す。
 * index は、同じ親の中で何番目に置くか（0 が先頭。
 * 自分を除いて数え、多すぎると末尾）。
 * 渡した項目だけが変わる。
 */
@writes
op update_workspace(
  id: string,
  name?: string,
  parent_id?: string | null,
  @minValue(0) index?: int32,
): void;

/** ワークスペースを、サブワークスペースとタスクごと削除する。元に戻せないので、実行の前にユーザーに確認が出る。 */
@writes
@confirm
op delete_workspace(id: string): void;

/**
 * 担当者の名前とメモを変える。
 * workspace_ids で使うワークスペースを決める（空の配列で、すべてのワークスペースで使う）。
 * 渡した項目だけが変わる。
 */
@writes
op update_person(
  id: string,
  name?: string,
  note?: string,
  workspace_ids?: string[],
): void;

/** 担当者を削除する（担当から外れるだけで、タスクは残る）。元に戻せないので、実行の前にユーザーに確認が出る。 */
@writes
@confirm
op delete_person(id: string): void;

/** コメントを書き直す、ピン留めする・外す、レビューのスレッドを解決済みにする・戻す。渡した項目だけが変わる。 */
@writes
op update_comment(
  id: string,
  body?: string,
  pinned?: boolean,

  /** レビューのスレッドのルートのコメントだけ */
  resolved?: boolean,
): void;

/** コメントを削除する。元に戻せないので、実行の前にユーザーに確認が出る。 */
@writes
@confirm
op delete_comment(id: string): void;

/**
 * タスクを、入っているワークスペースのほかのワークスペースにも出す（同期する）。
 * linked が false なら、そのワークスペースから外す。
 */
@writes
op link_task(id: string, workspace_id: string, linked: boolean): void;

/** タスクを手動の順（並び順「手動の順」）で、渡した ID の順に並べる。 */
@writes
op reorder_tasks(ids: string[]): void;

/** ユーザーの PC のファイルをタスクに添付する。path はファイルの絶対パス（先頭の ~ はホーム）。1 ファイル 10 MiB まで。 */
@writes
op add_attachment(task_id: string, path: string): void;

/** 添付を削除する。元に戻せないので、実行の前にユーザーに確認が出る。 */
@writes
@confirm
op delete_attachment(id: string): void;

/**
 * タスクの完了チケット（ほかの人が malakt-mahoz redeem で完了を報告するための文字列）を発行して返す。
 * person_id はだれの分として報告するか（省くと自分）。
 */
@writes
op issue_ticket(task_id: string, person_id?: string): void;

/** タスクの、まだ使われていない完了チケットをすべて取り消す。元に戻せないので、実行の前にユーザーに確認が出る。 */
@writes
@confirm
op revoke_tickets(task_id: string): void;

/** タスクに使われているカテゴリの一覧を返す。 */
op list_categories(): void;

/** 通知（ベルの一覧）を新しい順に返す。 */
op list_notices(@minValue(1) @maxValue(200) limit?: int32): void;

/** 通知を既読にする。ids を省くと、すべて既読にする。 */
@writes
op mark_notices_read(ids?: string[]): void;

/** ワークスペースの基準日と、タスクを追加したときに入る値（既定値）を返す。 */
op get_workspace_settings(id: string): void;

/** ワークスペースの基準日と、タスクを追加したときに入る値を変える。渡した項目だけが変わる。 */
@writes
op update_workspace_settings(
  id: string,

  /** 基準日（開催日など）YYYY-MM-DD。null でなくす */
  date?: string | null,

  assignee_ids?: string[],

  /** 期限の決め方。なし / 基準日からの日数 / 決まった日 */
  due?: "none" | "relative" | "fixed",

  /** due が relative のとき、基準日からの日数（負は前） */
  due_days?: int32,

  /** due が fixed のときの日付 YYYY-MM-DD */
  due_date?: string,

  /** 期限の時刻 HH:MM。空で日付だけ */
  due_time?: string,

  /** null で指定なし */
  priority?: "none" | "high" | "mid" | "low" | null,

  categories?: string[],

  /** 説明の雛形 */
  description?: string,

  /** 追加したタスクを一緒に表示するワークスペース */
  link_workspace_ids?: string[],

  /** このワークスペースのタスクでターミナルを開くフォルダー。空で消す */
  terminal_dir?: string,
): void;

/**
 * AI のルール（ユーザーが決めた、AI がいつも従う指示）の一覧を返す。
 * workspace_id を省くと、すべてのワークスペースで使うグローバルのルールと、ワークスペースごとのルールをすべて返す。
 * ワークスペースのルールは、そのワークスペースとサブワークスペースで使う。
 */
op list_ai_rules(workspace_id?: string): void;

/**
 * AI のルールを足す。
 * workspace_id を省くとグローバル（すべてのワークスペース）、渡すとそのワークスペースとサブワークスペースで使う。
 */
@writes
op create_ai_rule(
  /** 名前（100 文字まで） */
  name?: string,

  /** AI への指示（4000 文字まで） */
  body?: string,

  /** false で使わない */
  enabled?: boolean,

  /** 使う場面。いつも / ユーザーとのチャットだけ / 自動の処理（新しいタスクを整える、コメントから更新、GitHub の定期チェック）だけ */
  apply?: "always" | "chat" | "auto",

  workspace_id?: string,
): void;

/** AI のルールを変える。渡した項目だけが変わる。 */
@writes
op update_ai_rule(
  /** 名前（100 文字まで） */
  name?: string,

  /** AI への指示（4000 文字まで） */
  body?: string,

  /** false で使わない */
  enabled?: boolean,

  /** 使う場面。いつも / ユーザーとのチャットだけ / 自動の処理（新しいタスクを整える、コメントから更新、GitHub の定期チェック）だけ */
  apply?: "always" | "chat" | "auto",

  id: string,
): void;

/** AI のルールを削除する。使わないだけなら update_ai_rule で enabled を false にする。 */
@writes
op delete_ai_rule(id: string): void;

/**
 * ポモドーロタイマーの今の状態（段 phase: work / short_break / long_break、run: ready / running / paused、残り秒、作業しているタスク、済んだ作業の数）、設定、今日の作業の合計を返す。
 */
op get_pomodoro(): void;

/**
 * ポモドーロタイマーを操作する。
 * start（始める。一時停止していれば続きから。task_id を渡すと、そのタスクで測る）、pause（一時停止）、stop（止めて最初の作業に戻す）、skip（今の段を飛ばして次へ）、set_task（作業するタスクを変える。task_id を null か省くとタスクなし）、set_settings（渡した項目だけ変える）。
 * 作業の段が終わると、測った時間がタスクの作業の記録に残る。
 * work_media / break_media は、作業 / 休憩を始めたときに OS で再生中の音楽をどうするか（none / play / pause）。
 */
op pomodoro(
  action: "start" | "pause" | "stop" | "skip" | "set_task" | "set_settings",
  task_id?: string | null,
  work_minutes?: PomodoroMinutes,
  short_break_minutes?: PomodoroMinutes,
  long_break_minutes?: PomodoroMinutes,

  /** 長い休憩までの作業の回数 */
  @minValue(1) @maxValue(12) rounds?: int32,

  /** 作業が終わったら休憩をそのまま始める */
  auto_break?: boolean,

  work_media?: PomodoroMedia,
  break_media?: PomodoroMedia,
): void;

/**
 * ポモドーロで測った作業の記録を新しい順に返す。
 * task_id でそのタスクだけ、since（YYYY-MM-DD）でその日以降だけ。
 * 合計の秒数も返す。
 */
op list_work_logs(
  task_id?: string,
  since?: string,
  @minValue(1) @maxValue(500) limit?: int32,
): void;

/**
 * 期限をいつ知らせるか（OS の通知とリマインダー）を返す。
 * task_id を渡すと、そのタスクで使うもの（own が null なら全体の標準のまま）。
 */
op get_due_alerts(task_id?: string): void;

/**
 * 期限をいつ知らせるかを変える。
 * 渡した項目だけが変わる。
 * task_id を省くと全体の標準、渡すとそのタスクだけの上書き。
 * timed_minutes は時刻つきの期限の何分前か（0 で期限ちょうど、例 [0, 5, 60, 1440]、[] で知らせない）。
 * date_days は日付だけの期限の何日前か（0 で当日、1 で前日、[] で知らせない）。
 * タスクで null を渡すと、その側は標準に合わせる。
 * hour は日付だけの期限を知らせる時刻（0〜23 時、全体だけ）。
 */
@writes
op set_due_alerts(
  task_id?: string,
  timed_minutes?: AlertMinutes[] | null,
  date_days?: AlertDays[] | null,
  @minValue(0) @maxValue(23) hour?: int32,
): void;

/**
 * 予想工数と所要時間の統計を返す。
 * workspace_id を渡すとそのワークスペース（サブワークスペースを含む）、省くと全体。
 * period で、その期間に終わったタスクだけを数える。
 * 完了数・所要時間の合計／平均／中央値・予想工数に収まった割合・予想工数に対する所要時間の割合・未完了の予想工数の合計を、全体と担当者ごとに返す。
 * 所要時間は、タスクごとの基準（タスク → ワークスペース → 全体の標準）で数えた時計の経過（ポモドーロの作業時間とは別）。
 */
op get_time_stats(
  workspace_id?: string,

  /** 7 日 / 30 日 / 90 日 / 1 年 / すべて（標準は month） */
  period?: "week" | "month" | "quarter" | "year" | "all",
): void;

/**
 * 所要時間の基準（どこから数え、どこまで数えるか）を変える。
 * workspace_id を省くと全体の標準、渡すとそのワークスペース（サブワークスペースも従う）。
 * ワークスペースで basis に null を渡すと、親のワークスペースか全体の標準に合わせる。
 * タスクごとの基準は update_task の time_basis で変える。
 */
@writes
op set_time_basis(
  workspace_id?: string,

  /** 基準。null はワークスペースだけ */
  basis: TimeBasis | null,
): void;

/**
 * タスクに繰り返しを付ける・変える・外す。
 * 繰り返しのあるタスクは、次の回の時刻になるとアプリがタスクを写して次の回を作り、繰り返しは新しい回へ移る（開始日と期限は回の日に合わせてずれる）。
 * ai_instruction を付けると、回を作ったときにアプリの AI がその指示を実行して、結果をその回のタスクにコメントする（例: 週次の報告の下書き）。
 * アプリが動いている間だけ動く。
 * 渡した項目だけが変わる。
 */
@writes
op set_repeat(
  task_id: string,

  /** null で繰り返しを外す */
  repeat?: RepeatRule | null,

  /** 回を作ったときに AI に頼むこと。空で頼まない */
  ai_instruction?: string,
): void;

/**
 * 定時タスク（決まった時刻が来るたびに、雛形から新しいタスクを 1 件作る設定）の一覧を返す。
 * workspace_id を省くと、ワークスペースの外に作るものと、ワークスペースごとのものをすべて返す。
 */
op list_schedules(workspace_id?: string): void;

/**
 * 定時タスクを足す。
 * 時刻が来るたびに、そのワークスペース（workspace_id を省くとワークスペースの外）に新しいタスクを 1 件作る。
 * 雛形にはタスクと同じ項目（説明・状態・優先度・担当と役・開始日・期限・通知・進捗・カテゴリ・URL・親タスク・ほかのワークスペース・サブタスク）を決められ、決めていない項目にはワークスペースの既定値が入る。
 * 開始日と期限は作った日の何日後か（start_days・due_days）で決める。
 * 添付は add_schedule_attachment。
 * ai_instruction を付けると、作ったタスクでアプリの AI がその指示を実行して、結果をそのタスクにコメントする（例: 週次定例の報告の下書き）。
 * 前のタスクは書き換えない。
 * 1 つのタスクを毎回写すなら set_repeat。
 * アプリが動いている間だけ動く。
 * schedule を省くと今日の曜日の毎週 9:00。
 */
@writes
op create_schedule(
  /** 作るタスクのタイトル（200 文字まで） */
  title: string,

  ...OmitProperties<ScheduleFields, "title">,
  workspace_id?: string,
): void;

/**
 * 定時タスクを変える。
 * 渡した項目だけが変わる（作るタスクの項目も同じ）。
 * schedule を変えたときと、enabled を true にしたときは、次の時刻を今から決め直す。
 */
@writes
op update_schedule(...ScheduleFields, id: string): void;

/**
 * ユーザーの PC のファイルを定時タスクの雛形に添付する。
 * 作るタスクには毎回この添付が付く。
 * path はファイルの絶対パス（先頭の ~ はホーム）。
 * 1 ファイル 10 MiB まで。
 */
@writes
op add_schedule_attachment(schedule_id: string, path: string): void;

/** 定時タスクの雛形から添付を外す。作ったタスクの添付は残る。 */
@writes
op delete_schedule_attachment(id: string): void;

/** 定時タスクを削除する。作ったタスクは残る。止めるだけなら update_schedule で enabled を false にする。 */
@writes
op delete_schedule(id: string): void;

/** ほかの人が完了チケットで完了を報告したときに、通知を作るかの設定を返す。 */
op get_done_notices(): void;

/**
 * ほかの人が完了を報告したときに、通知を作るかを変える。
 * 渡した項目だけが変わる。
 * enabled は全体のオン・オフ。
 * when は each（1 人が自分の分を完了にするたび）か finished（タスクが完了したときだけ）。
 * roles は、完了にした人がこのどれかの役なら知らせる（R・A・C・I、1 つ以上）。
 * muted_people は知らせない担当者（ID か名前）、muted_workspaces は知らせないワークスペースの ID（サブワークスペースも知らせない）。
 * どちらも渡すとその並びになり、[] で全員・全部を知らせる。
 */
@writes
op set_done_notices(
  enabled?: boolean,
  when?: "each" | "finished",
  roles?: Role[],
  muted_people?: string[],
  muted_workspaces?: string[],
): void;

/** GitHub のリンク（issue、プルリクエスト、リポジトリ、ファイル、コミット）の中身を gh コマンドで取ってくる。 */
@inAppOnly
op github_fetch(url: string): void;

/** Slack のメッセージへのリンク（https://〜.slack.com/archives/…）のメッセージと、そのスレッドの返信を Slack CLI で取ってくる。 */
@inAppOnly
op slack_fetch(url: string): void;

/**
 * ユーザーの PC でコマンドを 1 つ実行し、終了コードと出力を返す。
 * 動くシェル（PowerShell、bash、WSL など）は、ユーザーのメッセージの最後にアプリが書く。
 * そのシェルの書き方で書く。
 * 実行の前にユーザーに確認が出る。
 */
@confirm
@inAppOnly
op run_shell(command: string): void;
