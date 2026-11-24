// Windows のリリースビルドでは、コンソールの窓を出さない
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    let path = malakt_mahoz::database_path();
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
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            malakt_mahoz::init(store, cx);
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
