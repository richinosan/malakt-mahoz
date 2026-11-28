// malakt-mahoz の API。アプリの中の AI、MCP のサーバー、コマンド（malakt-mahoz call）、
// プラグインが使うツールを、ここで 1 か所に定義する。
//
// - Malakt.Data: データベースの操作。どこからでも呼べる
// - Malakt.App: 画面と設定の操作。アプリの中の AI だけが呼べる
// - Malakt.Mcp: MCP のサーバーだけが持つ操作
//
// ツールの一覧（generated/tools.json）は `mise run api` で作り直す。
import "./lib.tsp";
import "./data.tsp";
import "./app.tsp";
import "./mcp.tsp";
