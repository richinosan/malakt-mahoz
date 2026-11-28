//! malakt-mahoz のデータとロジック。UI には依存しない。
//!
//! - `Store`: SQLite への保存と読み出し
//! - `access`: アクセスキー（外のエージェントに渡す合い言葉。キーごとに読み取りのみ・ロック中も使えるかを選ぶ）
//! - `actor`: 誰が変えたか（自分・AI・外のエージェントなど）
//! - `alert`: 期限の通知のタイミング（何分前・何日前に知らせるか）
//! - `archive`: zip へのエクスポート / インポート
//! - `attachment`: 添付ファイル（重複なし・圧縮つきで保存）
//! - `bulk`: 複数のタスクをまとめて変える操作と、一覧での選択
//! - `calendar`: 日付を選ぶカレンダー（月の並び・日本の祝日・よく使う日・入力の解釈）
//! - `estimate`: 予想工数（月・日・時間・分の組み合わせ）の読み書き
//! - `diff`: 文字列の差分（変更履歴の赤と緑）
//! - `done_notice`: ほかの人が完了にしたときに知らせるか（役・担当者・ワークスペースで選ぶ）
//! - `defaults`: ワークスペースごとのタスク追加時の既定値と基準日
//! - `intake`: ほかのツール（Notion・Redmine・Backlog・ClickUp・Slack）からの取り込み
//! - `history`: 変更履歴を、1 回の変更ごとの差分と、コメント欄に挟む出来事にする
//! - `links`: 1 つのタスクを複数のワークスペースに表示する
//! - `notice`: 通知（期限が近い、ほかの人からの変更、タイマーとポモドーロの終わり）
//! - `pomodoro`: ポモドーロタイマー（作業と休憩）と、タスクごとの作業の記録
//! - `remind`: OS のリマインダーとの連携（何を作り・直し・消すかの判断）
//! - `repeat`: 繰り返すタスク（決まった時刻に次の回を作り、AI に頼むこともできる）
//! - `rules`: AI のルール（グローバルとワークスペースごと）
//! - `schedule`: 定時タスク（決まった時刻に雛形から新しいタスクを作り、AI に頼むこともできる）
//! - `timer`: 単純なタイマー（カウントダウンとストップウォッチ）
//! - `timing`: 所要時間（開始の基準から完了の基準まで）と、その統計
//! - `ticket`: 完了チケット（画面で発行し、コマンドで完了報告する）
//! - `undo`: 元に戻す・やり直し（上限つきの記録）
//! - `lock`: アプリロック
//! - `markdown`: 本文の Markdown を画面に出す形にそろえる（改行をそのまま見せる）
//! - `mention`: コメントの `@名前`
//! - `view`: 一覧の絞り込みや並べ替え
//! - `when`: 期限や開始の入力（いろいろな書き方）を読む

#![cfg_attr(test, allow(clippy::cloned_ref_to_slice_refs))]

pub mod access;
pub mod actor;
pub mod alert;
pub mod archive;
pub mod attachment;
pub mod bots;
pub mod bulk;
pub mod caldav;
pub mod calendar;
pub mod defaults;
pub mod diff;
pub mod done_notice;
pub mod error;
pub mod estimate;
pub mod history;
pub mod icon;
pub mod intake;
pub mod links;
pub mod lock;
pub mod markdown;
pub mod mention;
pub mod model;
pub mod notice;
pub mod panes;
pub mod pomodoro;
pub mod remind;
pub mod repeat;
pub mod rules;
pub mod schedule;
mod schema;
pub mod store;
pub mod tasklink;
pub mod text;
pub mod ticket;
pub mod timer;
pub mod timing;
pub mod undo;
pub mod view;
pub mod weblink;
pub mod when;
pub mod wslink;

pub use actor::Actor;
pub use error::{Error, Result};
pub use model::*;
pub use store::{Store, Usage};
