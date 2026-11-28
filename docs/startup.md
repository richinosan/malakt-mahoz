# スタートアップ（ログインしたときに起動）

「設定」の「スタートアップ」の「ログインしたときに起動」をオンにすると、PC にログインしたときにアプリが起動します。標準はオフです。

| OS | 登録する場所 |
| --- | --- |
| Windows | レジストリの `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` の `Malakt Mahoz` |
| macOS | `~/Library/LaunchAgents/io.github.richinosan.malakt-mahoz.plist`（`.app` を `open -a` で開く） |
| Linux | `~/.config/autostart/io.github.richinosan.malakt-mahoz.desktop` |

- 管理者の権限は要りません。自分のユーザーにだけ登録します
- オンかオフかは OS の登録をそのまま出します。Windows のタスク マネージャーの「スタートアップ アプリ」で無効にしたときや、登録を消したときは、アプリの設定でもオフになります。アプリでオンにすると、タスク マネージャーで無効にしたものも有効に戻ります
- 登録するのは、動いているアプリ（macOS は `.app`）の場所です。自動アップデートは同じ場所で新しい版に入れ替えるので、更新したあとも同じ登録で起動します
- アプリを別のフォルダーへ動かしたときは、動かした先で 1 回起動すると、その場所に登録し直します（登録してある場所にアプリがもうないときだけ）
- macOS で、ダウンロードしたフォルダーから動かしているときは登録できません。「アプリケーション」フォルダーへ移してからオンにしてください
- ログインしたときに起動するのはアプリだけです。MCP サーバー（`malakt-mahoz mcp`）はこれまでどおりエージェントが起動し、アプリを閉じると一緒に終わります（[docs/mcp.md](mcp.md)）

## AI と MCP から

- アプリの AI bot: 「スタートアップに登録して」のように頼むと、`set_app_options` の `autostart` で切り替えます
- MCP: `get_autostart` で読み、`set_autostart`（`{"enabled": true}`）で切り替えます。`malakt-mahoz call set_autostart '{"enabled":true}'` でも同じです
