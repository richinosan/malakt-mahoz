//! API キーの保存。OS のキーチェーンに入れ、データベースやエクスポートには入れない。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::provider::Provider;

/// キーチェーンでの、このアプリの名前。
const SERVICE: &str = "malakt-mahoz";

pub trait KeyStore: Send + Sync {
    fn get(&self, provider: Provider) -> Result<Option<String>, String>;
    fn set(&self, provider: Provider, key: &str) -> Result<(), String>;
    fn delete(&self, provider: Provider) -> Result<(), String>;
}

/// OS のキーチェーン（macOS のキーチェーン、Windows の資格情報マネージャー、Linux の Secret Service）。
pub struct Keychain;

impl Keychain {
    fn entry(provider: Provider) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, &format!("api-key-{}", provider.as_str()))
            .map_err(|e| format!("OS のキーチェーンを開けませんでした: {e}"))
    }
}

impl KeyStore for Keychain {
    fn get(&self, provider: Provider) -> Result<Option<String>, String> {
        match Self::entry(provider)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!(
                "OS のキーチェーンから {} の API キーを読めませんでした: {e}",
                provider.label()
            )),
        }
    }

    fn set(&self, provider: Provider, key: &str) -> Result<(), String> {
        Self::entry(provider)?.set_password(key).map_err(|e| {
            format!(
                "OS のキーチェーンに {} の API キーを保存できませんでした: {e}",
                provider.label()
            )
        })
    }

    fn delete(&self, provider: Provider) -> Result<(), String> {
        match Self::entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!(
                "OS のキーチェーンから {} の API キーを削除できませんでした: {e}",
                provider.label()
            )),
        }
    }
}

/// メモリの中だけに持つ。テスト用。
#[derive(Default)]
pub struct MemoryKeys(Mutex<HashMap<Provider, String>>);

impl MemoryKeys {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<Provider, String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl KeyStore for MemoryKeys {
    fn get(&self, provider: Provider) -> Result<Option<String>, String> {
        Ok(self.map().get(&provider).cloned())
    }

    fn set(&self, provider: Provider, key: &str) -> Result<(), String> {
        self.map().insert(provider, key.to_owned());
        Ok(())
    }

    fn delete(&self, provider: Provider) -> Result<(), String> {
        self.map().remove(&provider);
        Ok(())
    }
}

/// キーがどこにあったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Keychain,
    /// 環境変数（`ANTHROPIC_API_KEY` など）
    Env,
    /// キーを入れていない。キーが要らないサービス（ローカルの AI）だけ
    None,
}

/// キーを探す。キーチェーンが先で、なければ環境変数。
///
/// `env` は環境変数を読む関数（テストで差し替える）。
pub fn lookup(
    store: &dyn KeyStore,
    provider: Provider,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<(String, Source)>, String> {
    let from_env = || {
        env(provider.env_var())
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty())
            .map(|key| (key, Source::Env))
    };
    let found = match store.get(provider) {
        Ok(Some(key)) => Ok(Some((key, Source::Keychain))),
        Ok(None) => Ok(from_env()),
        // キーチェーンがない環境でも、環境変数のキーで使える
        Err(error) => from_env().map(Some).ok_or(error),
    };
    if provider.needs_key() {
        return found;
    }
    match found {
        Ok(Some(found)) => Ok(Some(found)),
        // キーが要らないサービスは、キーなしで送る。キーチェーンが読めなくても止めない
        // （キーが要るサーバーなら、送ったときに 401 のエラーが画面に出る）
        Ok(None) | Err(_) => Ok(Some((String::new(), Source::None))),
    }
}

/// 画面に出す、伏せたキー。末尾の 4 文字だけ見せる。
pub fn masked(key: &str) -> String {
    let count = key.chars().count();
    if count <= 8 {
        return "••••".into();
    }
    let tail: String = key.chars().skip(count - 4).collect();
    format!("••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Broken;

    impl KeyStore for Broken {
        fn get(&self, _: Provider) -> Result<Option<String>, String> {
            Err("キーチェーンがありません".into())
        }
        fn set(&self, _: Provider, _: &str) -> Result<(), String> {
            Err("キーチェーンがありません".into())
        }
        fn delete(&self, _: Provider) -> Result<(), String> {
            Err("キーチェーンがありません".into())
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn keys_are_kept_per_service() {
        let keys = MemoryKeys::default();
        assert_eq!(lookup(&keys, Provider::Claude, no_env), Ok(None));
        keys.set(Provider::Claude, "sk-ant-1").unwrap();
        keys.set(Provider::Cursor, "cur-1").unwrap();
        assert_eq!(
            lookup(&keys, Provider::Claude, no_env),
            Ok(Some(("sk-ant-1".into(), Source::Keychain)))
        );
        assert_eq!(lookup(&keys, Provider::OpenAi, no_env), Ok(None));
        keys.delete(Provider::Claude).unwrap();
        assert_eq!(lookup(&keys, Provider::Claude, no_env), Ok(None));
        assert!(lookup(&keys, Provider::Cursor, no_env).unwrap().is_some());
    }

    #[test]
    fn the_environment_is_used_only_when_the_keychain_has_no_key() {
        let env = |name: &str| (name == "OPENAI_API_KEY").then(|| " sk-env \n".to_owned());
        let keys = MemoryKeys::default();
        assert_eq!(
            lookup(&keys, Provider::OpenAi, env),
            Ok(Some(("sk-env".into(), Source::Env)))
        );
        assert_eq!(lookup(&keys, Provider::Claude, env), Ok(None));
        keys.set(Provider::OpenAi, "sk-chain").unwrap();
        assert_eq!(
            lookup(&keys, Provider::OpenAi, env).unwrap().unwrap().1,
            Source::Keychain
        );
        let empty = |_: &str| Some("  ".to_owned());
        assert_eq!(
            lookup(&MemoryKeys::default(), Provider::Claude, empty),
            Ok(None)
        );
    }

    #[test]
    fn local_ai_works_without_a_key() {
        let keys = MemoryKeys::default();
        let none = Ok(Some((String::new(), Source::None)));
        assert_eq!(lookup(&keys, Provider::Local, no_env), none);
        // キーチェーンが読めなくても、キーなしで送る
        assert_eq!(lookup(&Broken, Provider::Local, no_env), none);
        keys.set(Provider::Local, "jan-key").unwrap();
        assert_eq!(
            lookup(&keys, Provider::Local, no_env),
            Ok(Some(("jan-key".into(), Source::Keychain)))
        );
    }

    #[test]
    fn a_broken_keychain_is_reported_unless_the_environment_has_a_key() {
        assert!(lookup(&Broken, Provider::Claude, no_env)
            .unwrap_err()
            .contains("キーチェーン"));
        let env = |_: &str| Some("sk-env".to_owned());
        assert_eq!(
            lookup(&Broken, Provider::Claude, env),
            Ok(Some(("sk-env".into(), Source::Env)))
        );
    }

    #[test]
    fn masked_keys_show_only_the_tail() {
        assert_eq!(masked("sk-ant-api03-abcdWXYZ"), "••••WXYZ");
        assert_eq!(masked("short"), "••••");
        assert_eq!(masked("あいうえおかきくけこ"), "••••きくけこ");
    }
}
