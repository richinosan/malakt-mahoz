// Windows のリリースビルドでは、コンソールの窓を出さない
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    let path = malakt_mahoz::database_path();
    // 引数があれば、ウィンドウを開かずにコマンドとして動く（MCP サーバー、完了チケットの引き換えなど）
    let args: Vec<String> = std::env::args().skip(1).collect();
    if malakt_mahoz::cli::is_command(&args) {
        let code = malakt_mahoz::cli::run(
            &args,
            || malakt_core::Store::open(&path),
            &mut std::io::stdin().lock(),
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
            // ターミナルで動かす Claude Code から、このアプリのタスクを読み書きできるようにする
            malakt_mahoz::term::shared(cx).update(cx, |terms, _| terms.register_mcp = true);
            malakt_mahoz::reminders::connect_system(cx);
            malakt_mahoz::update::connect_system(cx);
            malakt_mahoz::db::watch(cx);
            malakt_mahoz::quit_when_main_closes(cx);
            let (window, view) =
                malakt_mahoz::open_saved_windows(cx).expect("ウィンドウを開けませんでした");
            // ロックしてあるなら、起動してすぐパスキーを求める
            window
                .update(cx, |_, window, cx| {
                    view.update(cx, |view, cx| view.request_unlock(window, cx))
                })
                .expect("開いたばかりのウィンドウがありません");
            cx.activate(true);
        });
}
