//! プラグインを探す。標準のフォルダー、設定で足したフォルダー、環境変数のフォルダーを合わせて 1 つの一覧にする。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::{self, Manifest};

/// フォルダーを足す環境変数。OS のパスの区切り（`:` / `;`）で複数。
pub const ENV_DIRS: &str = "MALAKT_MAHOZ_PLUGINS";

/// 標準のフォルダー。どの OS でもホームの下の `.malakt-mahoz/plugins`（データベースの場所とは分ける）。
pub fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".malakt-mahoz").join("plugins"))
}

/// 読むフォルダーの並び。前にあるものほど優先する（同じ ID なら前のものを使う）。
/// 並びは、標準（`use_default` のとき）、設定で足したもの、環境変数のもの。同じフォルダーは 1 回だけ。
pub fn folders(use_default: bool, extra: &[PathBuf], env: Option<&std::ffi::OsStr>) -> Vec<Folder> {
    let mut list: Vec<Folder> = Vec::new();
    let mut push = |path: PathBuf, from: Source| {
        if !path.as_os_str().is_empty() && !list.iter().any(|f| f.path == path) {
            list.push(Folder { path, from });
        }
    };
    if use_default {
        if let Some(dir) = default_dir() {
            push(dir, Source::Default);
        }
    }
    for dir in extra {
        push(dir.clone(), Source::Setting);
    }
    if let Some(env) = env {
        for dir in std::env::split_paths(env) {
            push(dir, Source::Env);
        }
    }
    list
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Default,
    Setting,
    Env,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    pub from: Source,
}

/// 見つけたプラグイン 1 つ。
#[derive(Debug, Clone)]
pub struct Found {
    pub manifest: Manifest,
    /// `plugin.toml` のあるフォルダー
    pub dir: PathBuf,
    /// `plugin.toml` と WASM の中身のハッシュ。承認したときのものと違えば、もう一度承認を求める
    pub hash: String,
    /// 同じ ID で、優先度が低いために使わないもののフォルダー
    pub shadowed: Vec<PathBuf>,
}

impl Found {
    pub fn wasm_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.wasm)
    }
}

/// 読めなかったもの。どのフォルダーの何かを書く。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub plugins: Vec<Found>,
    pub problems: Vec<Problem>,
}

/// フォルダーを読む。フォルダーの直下の各フォルダーが、プラグイン 1 つ（`plugin.toml` があるもの）。
/// 標準のフォルダーがまだないのは問題にしない。ほかのフォルダーがないときは問題に挙げる。
pub fn discover(folders: &[Folder]) -> Discovery {
    let mut found = Discovery::default();
    for folder in folders {
        let entries = match std::fs::read_dir(&folder.path) {
            Ok(entries) => entries,
            Err(error) => {
                let missing = error.kind() == std::io::ErrorKind::NotFound;
                if !(missing && folder.from == Source::Default) {
                    found.problems.push(Problem {
                        path: folder.path.clone(),
                        text: format!("フォルダーを読めませんでした: {error}"),
                    });
                }
                continue;
            }
        };
        let mut dirs: Vec<PathBuf> = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path.join(manifest::FILE).is_file() {
                        dirs.push(path);
                    }
                }
                Err(error) => found.problems.push(Problem {
                    path: folder.path.clone(),
                    text: format!("フォルダーを読めませんでした: {error}"),
                }),
            }
        }
        // 読む順を OS によらずそろえる
        dirs.sort();
        for dir in dirs {
            match read_one(&dir) {
                Ok((manifest, hash)) => {
                    if let Some(first) = found
                        .plugins
                        .iter_mut()
                        .find(|p| p.manifest.id == manifest.id)
                    {
                        first.shadowed.push(dir);
                    } else {
                        found.plugins.push(Found {
                            manifest,
                            dir,
                            hash,
                            shadowed: Vec::new(),
                        });
                    }
                }
                Err(text) => found.problems.push(Problem { path: dir, text }),
            }
        }
    }
    found
}

fn read_one(dir: &Path) -> Result<(Manifest, String), String> {
    let path = dir.join(manifest::FILE);
    let text =
        std::fs::read(&path).map_err(|e| format!("{} を読めませんでした: {e}", manifest::FILE))?;
    let source = String::from_utf8(text.clone())
        .map_err(|_| format!("{} が UTF-8 ではありません", manifest::FILE))?;
    let manifest = Manifest::parse(&source)
        .map_err(|e| format!("{} が正しくありません: {e}", manifest::FILE))?;
    let wasm = std::fs::read(dir.join(&manifest.wasm))
        .map_err(|e| format!("{} を読めませんでした: {e}", manifest.wasm))?;
    Ok((manifest, hash(&text, &wasm)))
}

/// マニフェストと WASM のハッシュ（SHA-256 の 16 進）。
pub fn hash(manifest: &[u8], wasm: &[u8]) -> String {
    let mut h = Sha256::new();
    for part in [manifest, wasm] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(root: &Path, folder: &str, id: &str) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(manifest::FILE),
            format!("id = \"{id}\"\nname = \"{folder}\"\nversion = \"1\"\napi = 1\n"),
        )
        .unwrap();
        std::fs::write(dir.join(manifest::DEFAULT_WASM), b"\0asm").unwrap();
        dir
    }

    fn setting(path: &Path) -> Folder {
        Folder {
            path: path.to_owned(),
            from: Source::Setting,
        }
    }

    #[test]
    fn folders_are_merged_in_order_and_the_first_id_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        plugin(a.path(), "one", "one");
        let first = plugin(a.path(), "two", "two");
        let second = plugin(b.path(), "two-copy", "two");
        plugin(b.path(), "three", "three");
        std::fs::create_dir(b.path().join("not-a-plugin")).unwrap();
        let found = discover(&[setting(a.path()), setting(b.path())]);
        let ids: Vec<_> = found
            .plugins
            .iter()
            .map(|p| p.manifest.id.as_str())
            .collect();
        assert_eq!(ids, ["one", "two", "three"]);
        let two = &found.plugins[1];
        assert_eq!(two.dir, first);
        assert_eq!(two.shadowed, [second]);
        assert!(found.problems.is_empty(), "{:?}", found.problems);
    }

    #[test]
    fn broken_plugins_and_missing_folders_are_reported() {
        let a = tempfile::tempdir().unwrap();
        let bad = plugin(a.path(), "bad", "bad");
        std::fs::write(bad.join(manifest::FILE), "id = 1").unwrap();
        let no_wasm = plugin(a.path(), "no-wasm", "no-wasm");
        std::fs::remove_file(no_wasm.join(manifest::DEFAULT_WASM)).unwrap();
        let missing = a.path().join("missing");
        let found = discover(&[setting(a.path()), setting(&missing)]);
        assert!(found.plugins.is_empty());
        let paths: Vec<_> = found.problems.iter().map(|p| p.path.clone()).collect();
        assert_eq!(paths, [bad, no_wasm, missing.clone()]);
        // 標準のフォルダーがまだないのは問題にしない
        let quiet = discover(&[Folder {
            path: missing,
            from: Source::Default,
        }]);
        assert!(quiet.problems.is_empty());
    }

    #[test]
    fn the_hash_follows_both_files() {
        let a = tempfile::tempdir().unwrap();
        let dir = plugin(a.path(), "one", "one");
        let before = discover(&[setting(a.path())]).plugins[0].hash.clone();
        std::fs::write(dir.join(manifest::DEFAULT_WASM), b"\0asm2").unwrap();
        let after = discover(&[setting(a.path())]).plugins[0].hash.clone();
        assert_ne!(before, after);
        assert_ne!(hash(b"ab", b"c"), hash(b"a", b"bc"));
    }

    #[test]
    fn folder_order_and_duplicates() {
        let extra = [
            PathBuf::from("/x"),
            PathBuf::from("/y"),
            PathBuf::from("/x"),
        ];
        let env = std::env::join_paths([PathBuf::from("/y"), PathBuf::from("/z")]).unwrap();
        let list = folders(false, &extra, Some(&env));
        let paths: Vec<_> = list.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("/x"),
                PathBuf::from("/y"),
                PathBuf::from("/z")
            ]
        );
        assert_eq!(list[2].from, Source::Env);
        let with_default = folders(true, &[], None);
        assert_eq!(with_default.first().map(|f| f.from), Some(Source::Default));
    }
}
