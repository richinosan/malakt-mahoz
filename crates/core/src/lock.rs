//! アプリロック。
//!
//! ここが持つのは、ロックが有効かどうかだけ。本人確認は OS のパスキー（Windows Hello、
//! Touch ID など）で行い、秘密は何も保存しない。有効・無効を変える前の本人確認は、呼ぶ側が行う。
//!
//! アプリの外のコマンド（MCP サーバーなど）は、アプリがロックを解除している間だけデータを読める。
//! アプリは解除している間、その時刻を 1 分ごとに書き直し、ロックしたときと閉じたときに消す。
//!
//! 操作がないまま決めた時間が過ぎたら、アプリは自分でロックする（[`auto_after`]）。
//! ロック中も OS の通知は出し、中身をどこまで出すかを選べる（[`locked_notices`]）。

use crate::{Result, Store};

const KEY: &str = "lock.enabled";
/// パスキーにする前の版が保存していた設定。残っていたら、ロックは有効として扱う。
const OLD_KEY: &str = "lock.passphrase_hash";

/// アプリがロックを解除している時刻（UNIX 秒）を置く設定。
const OPEN_KEY: &str = "lock.open_at";
/// 解除の印が有効な秒数。アプリが落ちて印が残っても、これを過ぎると読めなくなる。
pub const OPEN_FOR: i64 = 150;

/// アプリがロックを解除していることを記録する。解除している間、繰り返し呼ぶ。
pub fn mark_open(store: &mut Store, now: i64) -> Result<()> {
    store.set_setting(OPEN_KEY, Some(&now.to_string()))
}

/// 解除の印を消す。ロックしたときと、アプリを閉じるときに呼ぶ。
pub fn mark_closed(store: &mut Store) -> Result<()> {
    store.set_setting(OPEN_KEY, None)
}

/// アプリの外のコマンドが、今データを読み書きしてよいか。
/// ロックが無効ならいつでもよい。有効なら、アプリが解除している間だけ。
pub fn allows_outside(store: &Store, now: i64) -> Result<bool> {
    if !is_enabled(store)? {
        return Ok(true);
    }
    let open_at = store.setting(OPEN_KEY)?.and_then(|v| v.parse::<i64>().ok());
    Ok(open_at.is_some_and(|at| (now - at).abs() <= OPEN_FOR))
}

/// 操作がないまま自動でロックするまでの分数を置く設定。ないときは自動でロックしない。
const AUTO_KEY: &str = "lock.auto_minutes";

/// 自動でロックするまでの時間として選べる分数。
pub const AUTO_CHOICES: [u32; 7] = [1, 5, 10, 15, 30, 60, 120];

/// 操作がないまま自動でロックするまでの分数。自動でロックしないなら `None`。
/// 選べない値が入っていたら（手で書き換えたなど）、自動でロックしない。
pub fn auto_after(store: &Store) -> Result<Option<u32>> {
    Ok(store
        .setting(AUTO_KEY)?
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|m| AUTO_CHOICES.contains(m)))
}

/// 自動でロックするまでの分数を変える。`None` で自動でロックしない。
/// 選べる分数（[`AUTO_CHOICES`]）でなければ、何も変えずにエラーにする。
pub fn set_auto_after(store: &mut Store, minutes: Option<u32>) -> Result<()> {
    match minutes {
        None => store.set_setting(AUTO_KEY, None),
        Some(m) if AUTO_CHOICES.contains(&m) => store.set_setting(AUTO_KEY, Some(&m.to_string())),
        Some(m) => Err(crate::Error::Invalid(format!(
            "自動でロックするまでの時間は {} 分のどれかです: {m}",
            AUTO_CHOICES.map(|c| c.to_string()).join(" / ")
        ))),
    }
}

/// 最後の操作が `last_input`（UNIX 秒）のとき、`now` に自動でロックする時間か。
/// 時計が戻ったとき（`now` が `last_input` より前）はロックしない。
pub fn idle_long_enough(last_input: i64, now: i64, minutes: u32) -> bool {
    now - last_input >= i64::from(minutes) * 60
}

/// ロック中の OS の通知に、何を出すか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LockedNotices {
    /// 通知の 1 行（タスクのタイトルを含む）を出す。コメントの本文は出さない
    #[default]
    Title,
    /// 「新しい通知があります」だけを出す
    Hidden,
}

impl LockedNotices {
    pub const ALL: [LockedNotices; 2] = [LockedNotices::Title, LockedNotices::Hidden];

    pub fn code(self) -> &'static str {
        match self {
            LockedNotices::Title => "title",
            LockedNotices::Hidden => "hidden",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|n| n.code() == code)
    }

    pub fn label(self) -> &'static str {
        match self {
            LockedNotices::Title => "タイトルを出す",
            LockedNotices::Hidden => "タイトルを出さない",
        }
    }
}

/// ロック中の通知に何を出すかを置く設定。ないときは [`LockedNotices::Title`]。
const NOTICES_KEY: &str = "lock.notices";

pub fn locked_notices(store: &Store) -> Result<LockedNotices> {
    Ok(store
        .setting(NOTICES_KEY)?
        .and_then(|v| LockedNotices::parse(&v))
        .unwrap_or_default())
}

pub fn set_locked_notices(store: &mut Store, notices: LockedNotices) -> Result<()> {
    match notices {
        LockedNotices::Title => store.set_setting(NOTICES_KEY, None),
        other => store.set_setting(NOTICES_KEY, Some(other.code())),
    }
}

pub fn is_enabled(store: &Store) -> Result<bool> {
    Ok(store.setting(KEY)?.is_some() || store.setting(OLD_KEY)?.is_some())
}

pub fn enable(store: &mut Store) -> Result<()> {
    store.set_setting(KEY, Some("1"))?;
    store.set_setting(OLD_KEY, None)
}

pub fn disable(store: &mut Store) -> Result<()> {
    store.set_setting(KEY, None)?;
    store.set_setting(OLD_KEY, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_lifecycle() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(!is_enabled(&s).unwrap());
        enable(&mut s).unwrap();
        assert!(is_enabled(&s).unwrap());
        enable(&mut s).unwrap();
        assert!(is_enabled(&s).unwrap());
        disable(&mut s).unwrap();
        assert!(!is_enabled(&s).unwrap());
    }

    #[test]
    fn outside_commands_read_only_while_the_app_is_unlocked() {
        let mut s = Store::open_in_memory().unwrap();
        // ロックが無効なら、アプリを開いていなくても読める
        assert!(allows_outside(&s, 1_000).unwrap());
        enable(&mut s).unwrap();
        assert!(!allows_outside(&s, 1_000).unwrap());
        mark_open(&mut s, 1_000).unwrap();
        assert!(allows_outside(&s, 1_000).unwrap());
        assert!(allows_outside(&s, 1_000 + OPEN_FOR).unwrap());
        // 印が古くなったら（アプリが落ちたあとなど）読めない
        assert!(!allows_outside(&s, 1_001 + OPEN_FOR).unwrap());
        // 時計が大きく戻っていても読めない
        assert!(!allows_outside(&s, 999 - OPEN_FOR).unwrap());
        mark_open(&mut s, 2_000).unwrap();
        mark_closed(&mut s).unwrap();
        assert!(!allows_outside(&s, 2_000).unwrap());
        // 読めない値は、印がないのと同じ
        s.set_setting(OPEN_KEY, Some("soon")).unwrap();
        assert!(!allows_outside(&s, 2_000).unwrap());
        disable(&mut s).unwrap();
        assert!(allows_outside(&s, 2_000).unwrap());
    }

    #[test]
    fn auto_lock_minutes() {
        let mut s = Store::open_in_memory().unwrap();
        // 最初は自動でロックしない
        assert_eq!(auto_after(&s).unwrap(), None);
        set_auto_after(&mut s, Some(15)).unwrap();
        assert_eq!(auto_after(&s).unwrap(), Some(15));
        // 選べない分数は、保存せずにエラーにする
        assert!(set_auto_after(&mut s, Some(7)).is_err());
        assert!(set_auto_after(&mut s, Some(0)).is_err());
        assert_eq!(auto_after(&s).unwrap(), Some(15));
        set_auto_after(&mut s, None).unwrap();
        assert_eq!(auto_after(&s).unwrap(), None);
        // 手で書き換えた値は、自動でロックしないのと同じ
        s.set_setting(AUTO_KEY, Some("7")).unwrap();
        assert_eq!(auto_after(&s).unwrap(), None);
        s.set_setting(AUTO_KEY, Some("soon")).unwrap();
        assert_eq!(auto_after(&s).unwrap(), None);
    }

    #[test]
    fn idle_time() {
        assert!(!idle_long_enough(1_000, 1_000, 1));
        assert!(!idle_long_enough(1_000, 1_059, 1));
        assert!(idle_long_enough(1_000, 1_060, 1));
        assert!(!idle_long_enough(1_000, 1_000 + 30 * 60 - 1, 30));
        assert!(idle_long_enough(1_000, 1_000 + 30 * 60, 30));
        // 時計が戻ったらロックしない
        assert!(!idle_long_enough(1_000, 0, 1));
    }

    #[test]
    fn what_locked_notices_show() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(locked_notices(&s).unwrap(), LockedNotices::Title);
        set_locked_notices(&mut s, LockedNotices::Hidden).unwrap();
        assert_eq!(locked_notices(&s).unwrap(), LockedNotices::Hidden);
        set_locked_notices(&mut s, LockedNotices::Title).unwrap();
        assert_eq!(locked_notices(&s).unwrap(), LockedNotices::Title);
        assert!(s.setting(NOTICES_KEY).unwrap().is_none());
        s.set_setting(NOTICES_KEY, Some("all")).unwrap();
        assert_eq!(locked_notices(&s).unwrap(), LockedNotices::Title);
        for n in LockedNotices::ALL {
            assert_eq!(LockedNotices::parse(n.code()), Some(n));
        }
    }

    #[test]
    fn a_lock_from_the_old_version_stays_locked() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_setting(OLD_KEY, Some("$argon2id$old")).unwrap();
        assert!(is_enabled(&s).unwrap());
        enable(&mut s).unwrap();
        assert!(s.setting(OLD_KEY).unwrap().is_none());
        assert!(is_enabled(&s).unwrap());

        s.set_setting(OLD_KEY, Some("$argon2id$old")).unwrap();
        disable(&mut s).unwrap();
        assert!(!is_enabled(&s).unwrap());
    }
}
