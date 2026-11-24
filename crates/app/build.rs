//! Windows では、実行ファイルにアイコンを埋め込む。

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile(
            "assets/icon/icon.rc",
            embed_resource::ParamsIncludeDirs(["assets/icon"]),
        )
        .manifest_optional()
        .unwrap();
    }
}
