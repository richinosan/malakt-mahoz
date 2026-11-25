//! GitHub Releases から新しい版を探し、取ってきて確かめ、取り出して置く。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{lock, newer, staged, write_ready, Error, Release, Source, Staged};

const SUMS: &str = "SHA256SUMS";
/// アーカイブの大きさの上限。これを超えるものは取り込まない。
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
/// アーカイブの中に入っている、入れ替えるもの。
const PAYLOADS: [&str; 3] = ["Malakt Mahoz.app", "malakt-mahoz.exe", "malakt-mahoz"];

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        // GitHub の API は、クライアントが名乗ることを求めている
        .user_agent(concat!("malakt-mahoz/", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                // 会社のプロキシなどの証明書も通るように、OS の証明書を使う
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

fn get(url: &str, timeout: Duration) -> Result<ureq::http::Response<ureq::Body>, Error> {
    agent(timeout)
        .get(url)
        .header("accept", "application/vnd.github+json")
        .call()
        .map_err(|e| Error::Offline(format!("{url} に接続できませんでした: {e}")))
}

fn text(url: &str) -> Result<(u16, String), Error> {
    let mut response = get(url, Duration::from_secs(30))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::Offline(format!("{url} の返事を読めませんでした: {e}")))?;
    Ok((status, body))
}

/// アーカイブの名前。
fn archive_name(version: &str, platform: &str) -> String {
    let extension = if platform.starts_with("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("malakt-mahoz-v{version}-{platform}.{extension}")
}

/// 最新のリリースが `current` より新しく、`platform` 用のアーカイブがあれば返す。
///
/// リリースがまだない、新しくない、この OS 用のファイルがない、下書きや先行版、のときは
/// `Ok(None)`。つながらない・返事が読めないときは [`Error::Offline`]。
pub fn latest(source: &Source, current: &str, platform: &str) -> Result<Option<Release>, Error> {
    let (status, body) = text(&source.latest)?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        // 回数の制限など。次の確認でやり直す
        return Err(Error::Offline(format!(
            "{} が {status} を返しました",
            source.latest
        )));
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Offline(format!("{} の返事が読めません: {e}", source.latest)))?;
    if value["draft"] == true || value["prerelease"] == true {
        return Ok(None);
    }
    let Some(tag) = value["tag_name"].as_str() else {
        return Ok(None);
    };
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if !newer(version, current) {
        return Ok(None);
    }
    let archive = archive_name(version, platform);
    let url_of = |name: &str| {
        value["assets"]
            .as_array()?
            .iter()
            .find(|asset| asset["name"] == name)?["browser_download_url"]
            .as_str()
            // リリースの置き場のほかからは取らない
            .filter(|url| url.starts_with(&source.downloads))
            .map(str::to_owned)
    };
    Ok(match (url_of(&archive), url_of(SUMS)) {
        (Some(archive_url), Some(sums_url)) => Some(Release {
            version: version.to_owned(),
            archive,
            archive_url,
            sums_url,
        }),
        _ => None,
    })
}

/// 新しい版を探し、あれば取ってきて `dir` に置く。前に置いたものがあれば、それを返す。
///
/// `skipped` は、入れ替えに失敗して見送っている版。同じ版は取ってこない。
/// ほかのプロセスが `dir` で取ってきている・入れ替えている最中なら、何も触らずに `Ok(None)`。
pub fn check(
    source: &Source,
    current: &str,
    platform: &str,
    skipped: Option<&str>,
    dir: &Path,
) -> Result<Option<Staged>, Error> {
    let lock = lock(dir).map_err(|e| {
        Error::Failed(format!(
            "新しい版を置く場所（{}）を使えませんでした: {e}",
            dir.display()
        ))
    })?;
    let Some(_lock) = lock else {
        return Ok(None);
    };
    if let Some(found) = staged(dir, current) {
        return Ok(Some(found));
    }
    match latest(source, current, platform)? {
        Some(release) if skipped != Some(release.version.as_str()) => {
            stage(&release, dir).map(Some)
        }
        _ => Ok(None),
    }
}

/// `SHA256SUMS` から、`name` の行のハッシュを読む。
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// アーカイブを `to` に書きながらハッシュを計算する。
fn download(url: &str, to: &Path) -> Result<String, Error> {
    let mut response = get(url, Duration::from_secs(30 * 60))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Offline(format!("{url} が {status} を返しました")));
    }
    let disk = |e: std::io::Error| {
        Error::Failed(format!(
            "新しい版を {} に保存できませんでした: {e}",
            to.display()
        ))
    };
    let mut file = std::fs::File::create(to).map_err(disk)?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_ARCHIVE)
        .reader();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| Error::Offline(format!("{url} を最後まで読めませんでした: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n]).map_err(disk)?;
    }
    file.flush().map_err(disk)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn unpack(archive: &Path, name: &str, into: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    if name.ends_with(".zip") {
        zip::ZipArchive::new(file)
            .and_then(|mut zip| zip.extract(into))
            .map_err(|e| e.to_string())
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(file))
            .unpack(into)
            .map_err(|e| e.to_string())
    }
}

/// 取り出した中から、入れ替えるものを探す。アーカイブは、フォルダー 1 つに全部が入っている。
fn find_payload(unpacked: &Path) -> Option<PathBuf> {
    let tops = std::fs::read_dir(unpacked).ok()?;
    tops.flatten().find_map(|top| {
        PAYLOADS
            .iter()
            .map(|name| top.path().join(name))
            .find(|path| path.exists())
    })
}

/// 新しい版を取ってきて、`SHA256SUMS` と合うことを確かめ、`dir` に取り出して置く。
///
/// `dir` の中身は先に消す（前に取ったものを残さない）。失敗したら `dir` ごと消す。
/// 置いたあとに `dir` に残るのは、入れ替えるものと目印だけ。
pub fn stage(release: &Release, dir: &Path) -> Result<Staged, Error> {
    let result = stage_into(release, dir);
    if result.is_err() {
        // 消せなければ、次の起動で消す
        let _ = std::fs::remove_dir_all(dir);
    }
    result
}

fn stage_into(release: &Release, dir: &Path) -> Result<Staged, Error> {
    let disk = |what: &str, e: std::io::Error| {
        Error::Failed(format!(
            "v{} を入れる準備ができませんでした。{} {what}: {e}",
            release.version,
            dir.display()
        ))
    };
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| disk("を空にできませんでした", e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| disk("を作れませんでした", e))?;

    let (status, sums) = text(&release.sums_url)?;
    if !(200..300).contains(&status) {
        return Err(Error::Offline(format!(
            "{} が {status} を返しました",
            release.sums_url
        )));
    }
    let expected = expected_hash(&sums, &release.archive).ok_or_else(|| {
        Error::Failed(format!(
            "v{} の {SUMS} に {} のハッシュがありません。",
            release.version, release.archive
        ))
    })?;
    let archive = dir.join("archive");
    let actual = download(&release.archive_url, &archive)?;
    if actual != expected {
        return Err(Error::Failed(format!(
            "v{} の {} が {SUMS} と合いません。更新を取りやめました。",
            release.version, release.archive
        )));
    }

    let unpacked = dir.join("unpacked");
    unpack(&archive, &release.archive, &unpacked).map_err(|e| {
        Error::Failed(format!(
            "v{} の {} を取り出せませんでした: {e}",
            release.version, release.archive
        ))
    })?;
    let found = find_payload(&unpacked).ok_or_else(|| {
        Error::Failed(format!(
            "v{} の {} にアプリが入っていません。",
            release.version, release.archive
        ))
    })?;
    let name = found
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let payload = dir.join("payload").join(&name);
    std::fs::create_dir_all(dir.join("payload")).map_err(|e| disk("に置けませんでした", e))?;
    std::fs::rename(&found, &payload).map_err(|e| disk("に置けませんでした", e))?;
    // アーカイブと、使わないファイル（README など）は残さない
    std::fs::remove_file(&archive).map_err(|e| disk("のアーカイブを消せませんでした", e))?;
    std::fs::remove_dir_all(&unpacked).map_err(|e| disk("を片付けられませんでした", e))?;
    // 目印は最後に書く。ここまで来なかったものは、次の起動で消える
    write_ready(dir, &release.version, &name).map_err(|e| disk("に目印を書けませんでした", e))?;
    Ok(Staged {
        version: release.version.clone(),
        dir: dir.to_owned(),
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;

    /// 手元の HTTP サーバー。パスごとに決めた中身を返し、ないパスには 404 を返す。
    fn serve_on(listener: TcpListener, routes: HashMap<String, Vec<u8>>) {
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(n) if n > 0 => request.extend_from_slice(&buffer[..n]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&request);
                let path = head.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = match routes.get(path) {
                    Some(body) => ("200 OK", body.clone()),
                    None => ("404 Not Found", b"{}".to_vec()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
    }

    fn serve(routes: HashMap<String, Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        serve_on(listener, routes);
        base
    }

    fn tar_gz(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, data, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn zipped(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (path, data) in files {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn sha256(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `version` のリリースを 1 つ持つサーバー。`sums` を渡すと、SHA256SUMS をそれに差し替える。
    fn release_server(version: &str, archive: &str, data: Vec<u8>, sums: Option<String>) -> Source {
        let sums = sums.unwrap_or_else(|| {
            format!(
                "{}  other.tar.gz\n{}  {archive}\n",
                "0".repeat(64),
                sha256(&data)
            )
        });
        // 自分の住所を返事に書くので、先に場所を決めてから中身を作る
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let downloads = format!("{base}/download/");
        let json = serde_json::json!({
            "tag_name": format!("v{version}"),
            "draft": false,
            "prerelease": false,
            "assets": [
                { "name": archive, "browser_download_url": format!("{downloads}v{version}/{archive}") },
                { "name": "SHA256SUMS", "browser_download_url": format!("{downloads}v{version}/SHA256SUMS") },
            ],
        });
        let mut routes = HashMap::new();
        routes.insert("/latest".to_owned(), json.to_string().into_bytes());
        routes.insert(format!("/download/v{version}/{archive}"), data);
        routes.insert(
            format!("/download/v{version}/SHA256SUMS"),
            sums.into_bytes(),
        );
        serve_on(listener, routes);
        Source {
            latest: format!("{base}/latest"),
            downloads,
        }
    }

    const LINUX: &str = "malakt-mahoz-v0.2.0-linux-x86_64.tar.gz";

    fn linux_archive() -> Vec<u8> {
        tar_gz(&[
            (
                "malakt-mahoz-v0.2.0-linux-x86_64/README.md",
                b"readme",
                0o644,
            ),
            (
                "malakt-mahoz-v0.2.0-linux-x86_64/malakt-mahoz",
                b"new binary",
                0o755,
            ),
        ])
    }

    #[test]
    fn a_newer_release_is_downloaded_checked_and_unpacked() {
        let source = release_server("0.2.0", LINUX, linux_archive(), None);
        let release = latest(&source, "0.1.0", "linux-x86_64").unwrap().unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.archive, LINUX);

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        // 前に取ったものは残さない
        std::fs::create_dir_all(dir.join("payload/older")).unwrap();
        let got = stage(&release, &dir).unwrap();
        assert_eq!(got.version, "0.2.0");
        assert_eq!(std::fs::read(&got.payload).unwrap(), b"new binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&got.payload)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        // ディスクに残るのは、入れ替えるものと目印だけ
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["payload", "ready.json"]);
        assert_eq!(std::fs::read_dir(dir.join("payload")).unwrap().count(), 1);
        // 次の起動でも、置いてあるものが見つかる
        assert_eq!(staged(&dir, "0.1.0"), Some(got));
    }

    #[test]
    fn checking_stages_once_and_then_reuses_what_is_there() {
        let source = release_server("0.2.0", LINUX, linux_archive(), None);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        let first = check(&source, "0.1.0", "linux-x86_64", None, &dir)
            .unwrap()
            .unwrap();
        assert_eq!(first.version, "0.2.0");
        // 2 回目（もう 1 つのアプリ）は、置いてあるものをそのまま使う
        let marker = std::fs::metadata(dir.join("ready.json"))
            .unwrap()
            .modified()
            .unwrap();
        let second = check(&source, "0.1.0", "linux-x86_64", None, &dir).unwrap();
        assert_eq!(second, Some(first));
        assert_eq!(
            std::fs::metadata(dir.join("ready.json"))
                .unwrap()
                .modified()
                .unwrap(),
            marker
        );
        // 見送っている版は取ってこない
        let other = root.path().join("other");
        assert_eq!(
            check(&source, "0.1.0", "linux-x86_64", Some("0.2.0"), &other),
            Ok(None)
        );
        assert!(!other.exists());
    }

    #[test]
    fn another_process_downloading_is_left_alone() {
        let source = release_server("0.2.0", LINUX, linux_archive(), None);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        // もう 1 つのアプリが取ってきている途中（鍵を持っていて、目印はまだない）
        let held = lock(&dir).unwrap().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("archive"), b"half").unwrap();
        assert_eq!(
            check(&source, "0.1.0", "linux-x86_64", None, &dir),
            Ok(None)
        );
        assert_eq!(std::fs::read(dir.join("archive")).unwrap(), b"half");
        // 終わったら（落ちたら）、残りを片付けて取り直す
        drop(held);
        let got = check(&source, "0.1.0", "linux-x86_64", None, &dir)
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(&got.payload).unwrap(), b"new binary");
        assert!(!dir.join("archive").exists());
    }

    #[test]
    fn the_same_or_an_older_release_is_ignored() {
        let source = release_server("0.2.0", LINUX, linux_archive(), None);
        assert_eq!(latest(&source, "0.2.0", "linux-x86_64"), Ok(None));
        assert_eq!(latest(&source, "0.3.0", "linux-x86_64"), Ok(None));
        // この OS 用のファイルがないリリース
        assert_eq!(latest(&source, "0.1.0", "macos-aarch64"), Ok(None));
    }

    #[test]
    fn no_release_yet_is_not_an_error() {
        let base = serve(HashMap::new());
        let source = Source {
            latest: format!("{base}/latest"),
            downloads: format!("{base}/download/"),
        };
        assert_eq!(latest(&source, "0.1.0", "linux-x86_64"), Ok(None));
    }

    #[test]
    fn being_offline_is_quiet() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let source = Source {
            latest: format!("http://127.0.0.1:{port}/latest"),
            downloads: format!("http://127.0.0.1:{port}/download/"),
        };
        assert!(matches!(
            latest(&source, "0.1.0", "linux-x86_64"),
            Err(Error::Offline(_))
        ));
    }

    #[test]
    fn drafts_prereleases_and_files_from_elsewhere_are_ignored() {
        let asset = |name: &str, url: &str| serde_json::json!({ "name": name, "browser_download_url": url });
        let check = |json: serde_json::Value| {
            let mut routes = HashMap::new();
            routes.insert("/latest".to_owned(), json.to_string().into_bytes());
            let base = serve(routes);
            let source = Source {
                latest: format!("{base}/latest"),
                downloads: "https://releases.example/download/".into(),
            };
            latest(&source, "0.1.0", "linux-x86_64").unwrap()
        };
        let good = [
            asset(LINUX, "https://releases.example/download/v0.2.0/a.tar.gz"),
            asset(SUMS, "https://releases.example/download/v0.2.0/SHA256SUMS"),
        ];
        assert!(check(serde_json::json!({ "tag_name": "v0.2.0", "assets": good })).is_some());
        assert!(check(
            serde_json::json!({ "tag_name": "v0.2.0", "prerelease": true, "assets": good })
        )
        .is_none());
        assert!(
            check(serde_json::json!({ "tag_name": "v0.2.0", "draft": true, "assets": good }))
                .is_none()
        );
        // リリースの置き場ではない URL
        let elsewhere = [
            asset(LINUX, "https://evil.example/a.tar.gz"),
            asset(SUMS, "https://releases.example/download/v0.2.0/SHA256SUMS"),
        ];
        assert!(check(serde_json::json!({ "tag_name": "v0.2.0", "assets": elsewhere })).is_none());
        // SHA256SUMS のないリリース
        assert!(
            check(serde_json::json!({ "tag_name": "v0.2.0", "assets": [good[0].clone()] }))
                .is_none()
        );
    }

    #[test]
    fn an_archive_that_does_not_match_its_hash_is_thrown_away() {
        let wrong = format!("{}  {LINUX}\n", "a".repeat(64));
        let source = release_server("0.2.0", LINUX, linux_archive(), Some(wrong));
        let release = latest(&source, "0.1.0", "linux-x86_64").unwrap().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        let Err(Error::Failed(text)) = stage(&release, &dir) else {
            panic!("ハッシュが合わないのに置いた");
        };
        assert!(text.contains("v0.2.0") && text.contains(LINUX) && text.contains("SHA256SUMS"));
        assert!(!dir.exists());

        // 自分の行がない SHA256SUMS
        let missing = format!("{}  other.tar.gz\n", "a".repeat(64));
        let source = release_server("0.2.0", LINUX, linux_archive(), Some(missing));
        let release = latest(&source, "0.1.0", "linux-x86_64").unwrap().unwrap();
        assert!(matches!(stage(&release, &dir), Err(Error::Failed(_))));
        assert!(!dir.exists());
    }

    #[test]
    fn an_archive_without_the_app_is_refused() {
        let data = tar_gz(&[(
            "malakt-mahoz-v0.2.0-linux-x86_64/README.md",
            b"readme",
            0o644,
        )]);
        let source = release_server("0.2.0", LINUX, data, None);
        let release = latest(&source, "0.1.0", "linux-x86_64").unwrap().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        let Err(Error::Failed(text)) = stage(&release, &dir) else {
            panic!("アプリがないのに置いた");
        };
        assert!(text.contains("アプリが入っていません"));
        assert!(!dir.exists());

        // 壊れたアーカイブ
        let source = release_server("0.2.0", LINUX, b"not a tarball".to_vec(), None);
        let release = latest(&source, "0.1.0", "linux-x86_64").unwrap().unwrap();
        assert!(matches!(stage(&release, &dir), Err(Error::Failed(_))));
        assert!(!dir.exists());
    }

    #[test]
    fn windows_zips_and_mac_bundles_are_unpacked() {
        let name = "malakt-mahoz-v0.2.0-windows-x86_64.zip";
        let data = zipped(&[
            ("malakt-mahoz-v0.2.0-windows-x86_64/LICENSE", b"license"),
            (
                "malakt-mahoz-v0.2.0-windows-x86_64/malakt-mahoz.exe",
                b"new exe",
            ),
        ]);
        let source = release_server("0.2.0", name, data, None);
        let release = latest(&source, "0.1.0", "windows-x86_64").unwrap().unwrap();
        let root = tempfile::tempdir().unwrap();
        let got = stage(&release, &root.path().join("win")).unwrap();
        assert!(got.payload.ends_with("payload/malakt-mahoz.exe"));
        assert_eq!(std::fs::read(&got.payload).unwrap(), b"new exe");

        let name = "malakt-mahoz-v0.2.0-macos-aarch64.tar.gz";
        let top = "malakt-mahoz-v0.2.0-macos-aarch64/Malakt Mahoz.app/Contents";
        let data = tar_gz(&[
            (&format!("{top}/Info.plist"), b"plist", 0o644),
            (&format!("{top}/MacOS/malakt-mahoz"), b"new mac", 0o755),
        ]);
        let source = release_server("0.2.0", name, data, None);
        let release = latest(&source, "0.1.0", "macos-aarch64").unwrap().unwrap();
        let got = stage(&release, &root.path().join("mac")).unwrap();
        assert!(got.payload.ends_with("payload/Malakt Mahoz.app"));
        assert_eq!(
            std::fs::read(got.payload.join("Contents/MacOS/malakt-mahoz")).unwrap(),
            b"new mac"
        );
    }

    #[test]
    fn hashes_are_read_from_either_sha256sum_format() {
        let hash = "A".repeat(64);
        let sums = format!("{hash} *a.zip\n{}  b.tar.gz\nshort  c\n", "b".repeat(64));
        assert_eq!(expected_hash(&sums, "a.zip"), Some("a".repeat(64)));
        assert_eq!(expected_hash(&sums, "b.tar.gz"), Some("b".repeat(64)));
        assert_eq!(expected_hash(&sums, "c"), None);
        assert_eq!(expected_hash(&sums, "d"), None);
    }
}
