//! Windows では、実行ファイルにアイコンを埋め込み、メインスレッドのスタックを 8MB にする。

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon/icon.ico");
    // 画面はメインスレッドで描く。macOS と Linux のメインスレッドは 8MB だが、Windows は
    // 1MB しかなく、デバッグビルドで設定の画面を開くとあふれる。ほかの OS と同じにする
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:8388608"),
            _ => println!("cargo:rustc-link-arg-bins=-Wl,--stack,8388608"),
        }
    }
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
