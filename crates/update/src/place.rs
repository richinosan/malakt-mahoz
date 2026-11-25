//! 置いてある新しい版を、動いている実行ファイル（macOS は `.app`）と入れ替える。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{lock, still_staged, Staged};

/// `.app` の中の実行ファイルの場所。
const BUNDLE_BINARY: &str = "Contents/MacOS/malakt-mahoz";

/// `exe` が `.app` の中の実行ファイルなら、その `.app`。
fn bundle_of(exe: &Path) -> Option<&Path> {
    let bundle = exe.parent()?.parent()?.parent()?;
    let inside = exe.strip_prefix(bundle).ok()?;
    (inside == Path::new(BUNDLE_BINARY) && bundle.extension().is_some_and(|e| e == "app"))
        .then_some(bundle)
}

/// 入れ替えるもの（新しいもの、今のもの）。
fn pair(staged: &Staged, exe: &Path) -> (PathBuf, PathBuf) {
    if !staged.payload.is_dir() {
        return (staged.payload.clone(), exe.to_owned());
    }
    match bundle_of(exe) {
        Some(bundle) => (staged.payload.clone(), bundle.to_owned()),
        // `.app` の外に実行ファイルだけを置いて使っているときは、実行ファイルだけを入れ替える
        None => (staged.payload.join(BUNDLE_BINARY), exe.to_owned()),
    }
}

fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(suffix);
    path.with_file_name(name)
}

fn remove(path: &Path) -> std::io::Result<()> {
    let result = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_symlink() {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(std::fs::read_link(from)?, to);
        #[cfg(not(unix))]
        return std::fs::copy(from, to).map(|_| ());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ())
}

/// 別のドライブへは名前の付け替えができないので、そのときは写す。
fn bring(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to).or_else(|_| copy_all(from, to))
}

/// 新しい版を、今の実行ファイル `exe` と入れ替える。
///
/// 動いている実行ファイルは上書きできない OS があるので、今のものを隣に退けてから、新しい
/// ものを同じ名前で置く。置けなかったら、退けたものを戻す。退けたものは消すが、Windows では
/// 動いている間は消せないので、次の起動で消す（[`clear_leftovers`]）。
///
/// 同じ置き場を使うほかのプロセスが、もう入れ替えた・今入れ替えている・取り直しているなら、
/// 何もせずに `Ok` を返す（入れ替えは 1 回で足りる）。
pub fn install(staged: &Staged, exe: &Path) -> Result<(), String> {
    let (new, current) = pair(staged, exe);
    let lock = lock(&staged.dir).map_err(|e| {
        format!(
            "v{} に更新できませんでした。新しい版を置いた場所（{}）を使えませんでした: {e}",
            staged.version,
            staged.dir.display()
        )
    })?;
    let Some(_lock) = lock else {
        return Ok(());
    };
    if !still_staged(staged) {
        return Ok(());
    }
    let fail = |what: &str, error: std::io::Error| {
        format!(
            "v{} に更新できませんでした。{}{what}: {error}",
            staged.version,
            current.display()
        )
    };
    let incoming = beside(&current, ".new");
    let outgoing = beside(&current, ".old");
    remove(&incoming).map_err(|e| fail(" の隣の古い一時ファイルを消せませんでした", e))?;
    remove(&outgoing).map_err(|e| fail(" の隣の前の版を消せませんでした", e))?;
    if let Err(error) = bring(&new, &incoming) {
        // 写しかけを残さない。消せなければ次の起動で消す
        let _ = remove(&incoming);
        return Err(fail(" の隣に新しい版を置けませんでした", error));
    }
    if let Err(error) = std::fs::rename(&current, &outgoing) {
        let _ = remove(&incoming);
        return Err(fail(" を退けられませんでした", error));
    }
    if let Err(error) = std::fs::rename(&incoming, &current) {
        // 元に戻す。戻せなければ、退けた名前（.old）のまま残る
        let _ = std::fs::rename(&outgoing, &current);
        let _ = remove(&incoming);
        return Err(fail(" に新しい版を置けませんでした", error));
    }
    // 動いている間は消せない OS では、次の起動で消す
    let _ = remove(&outgoing);
    // 置いてあったフォルダー。消せなければ、次の起動で（版が新しくないので）消える
    let _ = std::fs::remove_dir_all(&staged.dir);
    Ok(())
}

/// 前の入れ替えで隣に残ったもの（退けた前の版、写しかけ）を消す。起動のときに呼ぶ。
pub fn clear_leftovers(exe: &Path) {
    for path in [Some(exe), bundle_of(exe)].into_iter().flatten() {
        for suffix in [".old", ".new"] {
            // 消せなければ、次の起動か、次の入れ替えでもう一度消す
            let _ = remove(&beside(path, suffix));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(root: &Path, payload: &str) -> Staged {
        let dir = root.join("update");
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        crate::write_ready(&dir, "0.2.0", payload).unwrap();
        Staged {
            version: "0.2.0".into(),
            payload: dir.join("payload").join(payload),
            dir,
        }
    }

    #[test]
    fn the_binary_is_replaced_and_nothing_is_left_behind() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("malakt-mahoz");
        std::fs::write(&exe, b"old").unwrap();
        let staged = staged(root.path(), "malakt-mahoz");
        std::fs::write(&staged.payload, b"new").unwrap();

        install(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        // 置いてあったものも、退けた前の版も残さない
        assert!(!staged.dir.exists());
        let names: Vec<_> = std::fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["malakt-mahoz"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_new_binary_stays_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("malakt-mahoz");
        std::fs::write(&exe, b"old").unwrap();
        let staged = staged(root.path(), "malakt-mahoz");
        std::fs::write(&staged.payload, b"new").unwrap();
        std::fs::set_permissions(&staged.payload, std::fs::Permissions::from_mode(0o755)).unwrap();
        install(&staged, &exe).unwrap();
        let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn the_whole_app_bundle_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Applications/Malakt Mahoz.app");
        let exe = bundle.join(BUNDLE_BINARY);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), b"old plist").unwrap();
        std::fs::write(bundle.join("Contents/gone-in-the-new-version"), b"x").unwrap();
        let staged = staged(root.path(), "Malakt Mahoz.app");
        std::fs::create_dir_all(staged.payload.join("Contents/MacOS")).unwrap();
        std::fs::write(staged.payload.join(BUNDLE_BINARY), b"new").unwrap();
        std::fs::write(staged.payload.join("Contents/Info.plist"), b"new plist").unwrap();

        install(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(
            std::fs::read(bundle.join("Contents/Info.plist")).unwrap(),
            b"new plist"
        );
        assert!(!bundle.join("Contents/gone-in-the-new-version").exists());
        assert_eq!(
            std::fs::read_dir(root.path().join("Applications"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn a_bare_binary_takes_the_binary_out_of_the_bundle() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("malakt-mahoz");
        std::fs::write(&exe, b"old").unwrap();
        let staged = staged(root.path(), "Malakt Mahoz.app");
        std::fs::create_dir_all(staged.payload.join("Contents/MacOS")).unwrap();
        std::fs::write(staged.payload.join(BUNDLE_BINARY), b"new").unwrap();
        install(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    }

    #[test]
    fn a_failed_install_keeps_the_current_version() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("malakt-mahoz");
        std::fs::write(&exe, b"old").unwrap();
        let staged = staged(root.path(), "malakt-mahoz");
        std::fs::write(&staged.payload, b"new").unwrap();
        // 隣に、消せない形で前の版が残っている（中身のあるフォルダーは、ファイルとしては置き換えられない）
        let moved = root.path().join("elsewhere");
        std::fs::rename(&exe, &moved).unwrap();
        let error = install(&staged, &exe).unwrap_err();
        assert!(error.contains("v0.2.0") && error.contains("malakt-mahoz"));
        // 写しかけは残さない
        assert!(!root.path().join(".malakt-mahoz.new").exists());
        assert!(!exe.exists());
        assert_eq!(std::fs::read(&moved).unwrap(), b"old");
    }

    #[test]
    fn an_update_another_process_already_installed_is_not_installed_twice() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("malakt-mahoz");
        std::fs::write(&exe, b"new").unwrap();
        let staged = staged(root.path(), "malakt-mahoz");
        // もう 1 つのアプリが入れ替えて、置き場を消したあと
        std::fs::remove_dir_all(&staged.dir).unwrap();
        install(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_name() != "update.lock")
                .count(),
            1
        );
    }

    #[test]
    fn installing_waits_out_another_process_using_the_staging_dir() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("malakt-mahoz");
        std::fs::write(&exe, b"old").unwrap();
        let staged = staged(root.path(), "malakt-mahoz");
        std::fs::write(&staged.payload, b"new").unwrap();
        // もう 1 つのアプリが置き場を使っている間は、触らない
        let held = lock(&staged.dir).unwrap().unwrap();
        install(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert_eq!(std::fs::read(&staged.payload).unwrap(), b"new");
        drop(held);
        install(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    }

    #[test]
    fn leftovers_beside_the_binary_and_the_bundle_are_cleared() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Malakt Mahoz.app");
        let exe = bundle.join(BUNDLE_BINARY);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"now").unwrap();
        let old_bundle = root.path().join(".Malakt Mahoz.app.old");
        std::fs::create_dir_all(old_bundle.join("Contents")).unwrap();
        let old_exe = exe.with_file_name(".malakt-mahoz.old");
        std::fs::write(&old_exe, b"before").unwrap();
        clear_leftovers(&exe);
        assert!(!old_bundle.exists() && !old_exe.exists());
        assert_eq!(std::fs::read(&exe).unwrap(), b"now");
    }
}
