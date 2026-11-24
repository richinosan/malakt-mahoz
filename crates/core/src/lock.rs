//! アプリロック。
//!
//! ここが持つのは、ロックが有効かどうかだけ。本人確認は OS のパスキー（Windows Hello、
//! Touch ID など）で行い、秘密は何も保存しない。有効・無効を変える前の本人確認は、呼ぶ側が行う。

use crate::{Result, Store};

const KEY: &str = "lock.enabled";
/// パスキーにする前の版が保存していた設定。残っていたら、ロックは有効として扱う。
const OLD_KEY: &str = "lock.passphrase_hash";

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
