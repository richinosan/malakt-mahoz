//! アプリロック。
//!
//! ここが持つのは、ロックが有効かどうかだけ。本人確認は OS のパスキー（Windows Hello、
//! Touch ID など）で行い、秘密は何も保存しない。有効・無効を変える前の本人確認は、呼ぶ側が行う。
//!
//! アプリの外のコマンド（MCP サーバーなど）は、アプリがロックを解除している間だけデータを読める。
//! アプリは解除している間、その時刻を 1 分ごとに書き直し、ロックしたときと閉じたときに消す。

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
