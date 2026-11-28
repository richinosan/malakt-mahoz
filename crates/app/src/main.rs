// Windows のリリースビルドでは、コンソールの窓を出さない
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    let path = malakt_mahoz::database_path();
    // 引数があれば、ウィンドウを開かずにコマンドとして動く（MCP サーバー、完了チケットの引き換えなど）
    let args: Vec<String> = std::env::args().skip(1).collect();
    // スタートアップの登録は、画面からも MCP サーバーからも、本物の OS に書く
    malakt_mahoz::autostart::use_system();
    if malakt_mahoz::cli::is_command(&args) {
        // Windows のリリース版はコンソールを持たないので、呼び出したターミナルに出力をつなぐ
        malakt_term::console::attach();
        if args == ["mcp"] {
            // MCP サーバーは、アプリが動いている間だけ動かす。動いたままだと実行ファイルをつかんだままになる
            if !malakt_mahoz::mcp::app_running(&path) {
                eprintln!("malakt-mahoz のアプリが起動していません。アプリを起動してから、もう一度つないでください。");
                std::process::exit(1);
            }
            malakt_mahoz::mcp::exit_when_app_closes(path.clone());
        }
        let key = std::env::var(malakt_mahoz::mcp::ENV_KEY).ok();
        let code = malakt_mahoz::cli::run(
            &args,
            key,
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
            // アプリ内ブラウザは、OS のブラウザの部品で開く
            malakt_mahoz::browser::use_system_browser(cx);
            // 動いている印を付ける。MCP サーバーはこれを見て、アプリを閉じると一緒に終わる
            if let Err(error) = malakt_mahoz::mcp::hold_running(&malakt_mahoz::database_path()) {
                eprintln!("アプリが動いている印を付けられませんでした: {error}");
            }
            // ターミナルで動かす Claude Code から、このアプリのタスクを読み書きできるようにする
            malakt_mahoz::term::shared(cx).update(cx, |terms, _| {
                terms.register_mcp = true;
                terms.cursor_home = dirs::home_dir();
            });
            // GitHub のアカウントは、gh にログインしているものを使う
            malakt_mahoz::ai::shared(cx).update(cx, |ai, cx| ai.read_github(cx));
            malakt_mahoz::reminders::connect_system(cx);
            malakt_mahoz::update::connect_system(cx);
            let autostart = malakt_mahoz::autostart::connect_system(cx);
            malakt_mahoz::db::watch(cx);
            malakt_mahoz::pomodoro::run(cx);
            malakt_mahoz::quit_when_main_closes(cx);
            let (window, view) =
                malakt_mahoz::open_saved_windows(cx).expect("ウィンドウを開けませんでした");
            // ロックしてあるなら、起動してすぐパスキーを求める
            window
                .update(cx, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        view.request_unlock(window, cx);
                        if let Err(error) = &autostart {
                            view.error = Some(error.clone().into());
                        }
                        // AI を使っていなければ、AI のおすすめを出す
                        view.offer_ai(cx);
                    })
                })
                .expect("開いたばかりのウィンドウがありません");
            malakt_mahoz::pomodoro::reopen(cx);
            cx.activate(true);
        });
}
