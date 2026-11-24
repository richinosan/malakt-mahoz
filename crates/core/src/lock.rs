//! アプリロック。
//!
//! パスフレーズは Argon2id のハッシュだけを保存する。OS の生体認証（Touch ID や
//! Windows Hello）は [`DeviceAuth`] を実装して差し込む。生体認証が使えない環境では
//! パスフレーズで解除する。

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

use crate::{Error, Result, Store};

const KEY: &str = "lock.passphrase_hash";

/// パスフレーズの最小の長さ（文字数）。
pub const MIN_PASSPHRASE_CHARS: usize = 4;

/// OS の本人確認。
pub trait DeviceAuth {
    /// この端末で使えるか。
    fn is_available(&self) -> bool;
    /// 本人確認をする。成功なら `true`。
    fn authenticate(&self, reason: &str) -> bool;
}

/// 生体認証のない環境用。常に使えない。
pub struct NoDeviceAuth;

impl DeviceAuth for NoDeviceAuth {
    fn is_available(&self) -> bool {
        false
    }

    fn authenticate(&self, _reason: &str) -> bool {
        false
    }
}

pub fn is_enabled(store: &Store) -> Result<bool> {
    Ok(store.setting(KEY)?.is_some())
}

/// ロックを有効にする。すでに有効なら `current` に今のパスフレーズが要る。
pub fn set_passphrase(store: &mut Store, current: Option<&str>, new: &str) -> Result<()> {
    check_current(store, current)?;
    if new.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(Error::Invalid(format!(
            "パスフレーズは {MIN_PASSPHRASE_CHARS} 文字以上にしてください"
        )));
    }
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    let hash = Argon2::default()
        .hash_password(new.as_bytes(), &salt)
        .map_err(|e| Error::Invalid(format!("パスフレーズを処理できません: {e}")))?
        .to_string();
    store.set_setting(KEY, Some(&hash))
}

/// ロックを無効にする。今のパスフレーズが要る。
pub fn disable(store: &mut Store, current: &str) -> Result<()> {
    check_current(store, Some(current))?;
    store.set_setting(KEY, None)
}

/// パスフレーズが合っているか。ロックが無効なら常に `true`。
pub fn verify(store: &Store, passphrase: &str) -> Result<bool> {
    let Some(stored) = store.setting(KEY)? else {
        return Ok(true);
    };
    let hash = PasswordHash::new(&stored)
        .map_err(|_| Error::Invalid("保存されたロック情報が壊れています".into()))?;
    Ok(Argon2::default()
        .verify_password(passphrase.as_bytes(), &hash)
        .is_ok())
}

/// 解除を試みる。生体認証が使えればそれを先に試し、だめならパスフレーズで確かめる。
pub fn unlock(store: &Store, device: &dyn DeviceAuth, passphrase: Option<&str>) -> Result<bool> {
    if !is_enabled(store)? {
        return Ok(true);
    }
    if device.is_available() && device.authenticate("malakt-mahoz のロックを解除します") {
        return Ok(true);
    }
    match passphrase {
        Some(p) => verify(store, p),
        None => Ok(false),
    }
}

fn check_current(store: &Store, current: Option<&str>) -> Result<()> {
    if !is_enabled(store)? {
        return Ok(());
    }
    match current {
        Some(c) if verify(store, c)? => Ok(()),
        _ => Err(Error::WrongPassphrase),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        available: bool,
        ok: bool,
    }

    impl DeviceAuth for Fake {
        fn is_available(&self) -> bool {
            self.available
        }

        fn authenticate(&self, _reason: &str) -> bool {
            self.ok
        }
    }

    #[test]
    fn lock_lifecycle() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(!is_enabled(&s).unwrap());
        assert!(verify(&s, "anything").unwrap());
        assert!(unlock(&s, &NoDeviceAuth, None).unwrap());

        assert!(matches!(
            set_passphrase(&mut s, None, "abc"),
            Err(Error::Invalid(_))
        ));
        assert!(!is_enabled(&s).unwrap());
        // 文字数で数える（バイト数ではない）
        set_passphrase(&mut s, None, "あいうえ").unwrap();
        assert!(is_enabled(&s).unwrap());
        assert!(verify(&s, "あいうえ").unwrap());
        assert!(!verify(&s, "あいうお").unwrap());
        // ハッシュだけが保存される
        let stored = s.setting(KEY).unwrap().unwrap();
        assert!(stored.starts_with("$argon2id$"));
        assert!(!stored.contains("あいうえ"));

        // 変更には今のパスフレーズが要る
        assert!(matches!(
            set_passphrase(&mut s, None, "newpass"),
            Err(Error::WrongPassphrase)
        ));
        assert!(matches!(
            set_passphrase(&mut s, Some("wrong"), "newpass"),
            Err(Error::WrongPassphrase)
        ));
        set_passphrase(&mut s, Some("あいうえ"), "newpass").unwrap();
        assert!(verify(&s, "newpass").unwrap());
        assert!(!verify(&s, "あいうえ").unwrap());

        assert!(matches!(
            disable(&mut s, "wrong"),
            Err(Error::WrongPassphrase)
        ));
        assert!(is_enabled(&s).unwrap());
        disable(&mut s, "newpass").unwrap();
        assert!(!is_enabled(&s).unwrap());
    }

    #[test]
    fn unlock_prefers_device_then_passphrase() {
        let mut s = Store::open_in_memory().unwrap();
        set_passphrase(&mut s, None, "pass1234").unwrap();
        let yes = Fake {
            available: true,
            ok: true,
        };
        let no = Fake {
            available: true,
            ok: false,
        };
        let unavailable = Fake {
            available: false,
            ok: true,
        };
        assert!(unlock(&s, &yes, None).unwrap());
        assert!(!unlock(&s, &no, None).unwrap());
        assert!(unlock(&s, &no, Some("pass1234")).unwrap());
        assert!(!unlock(&s, &no, Some("bad")).unwrap());
        // 使えない端末認証は、成功を返す実装でも信用しない
        assert!(!unlock(&s, &unavailable, None).unwrap());
        assert!(unlock(&s, &NoDeviceAuth, Some("pass1234")).unwrap());
        assert!(!NoDeviceAuth.authenticate("x"));
    }

    #[test]
    fn broken_hash_is_an_error() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_setting(KEY, Some("not a hash")).unwrap();
        assert!(matches!(verify(&s, "x"), Err(Error::Invalid(_))));
    }
}
