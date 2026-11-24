// Windows のリリースビルドでは、コンソールの窓を出さない
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    let path = malakt_mahoz::database_path();
    // 引数があれば、ウィンドウを開かずにコマンドとして動く（完了チケットの引き換えなど）
    let args: Vec<String> = std::env::args().skip(1).collect();
    if malakt_mahoz::cli::is_command(&args) {
        let code = malakt_mahoz::cli::run(
            &args,
            || malakt_core::Store::open(&path),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        );
        std::process::exit(code);
    }
    let store = match malakt_core::Store::open(&path) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "データベースを開けませんでした（{}）: {error}",
                path.display()
            );
            std::process::exit(1);
        }
    };
    gpui_kit::application()
        .with_assets(malakt_mahoz::Assets)
        .run(move |cx| {
            malakt_mahoz::db::clear_stale_opened();
            malakt_mahoz::init(store, cx);
            malakt_mahoz::db::watch(cx);
            // 最後のウィンドウを閉じたら終了する
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            malakt_mahoz::open_main_window(cx).expect("ウィンドウを開けませんでした");
            cx.activate(true);
        });
}
