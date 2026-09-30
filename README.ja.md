<p align="center">
  <img src="assets/termior-logo.svg" width="128" height="128" alt="Termior logo">
</p>

# Termior

[English](README.md#english) · [简体中文](README.md#简体中文) · [繁體中文](README.zh-TW.md) · 日本語 · [한국어](README.ko.md) · [Español](README.es.md) · [Deutsch](README.de.md)

---

## 日本語

> オープンソース・クロスプラットフォーム・ターミナルファーストの AI ネイティブ開発環境（ADE）。BYOK・ローカルファースト・アカウント不要・テレメトリなし。

Termior は、本物の PTY ターミナル、軽量コードエディター、ファイルエクスプローラー、Git、Web プレビュー、そしてツール呼び出し・承認・変更レビュー・復旧に対応した内蔵/外部エージェントを、単一のネイティブウィンドウに統合します。製品およびエンジニアリング仕様は [docs/termior-spec.md](docs/termior-spec.md) を参照してください。

### コアバリュー

1. **ネイティブの低レイテンシ** — コア UI に JS ランタイムや IPC シリアライゼーション境界はありません。ターミナルとエディターは GPU 上で直接レンダリングし、通常時 60 fps、高リフレッシュレート環境では 120 fps を目標とします。
2. **ターミナルと AI の深い連携** — 本物の PTY、リアルタイムの cwd/バッファコンテキスト、ツール呼び出し、承認、hunk 単位の diff レビューが同じワークスペースに集約されています。
3. **ローカルファーストとユーザー主導** — BYOK、API キーは OS キーチェーンにのみ保存、テレメトリなし、アカウント不要。LM Studio、MLX、Ollama などのローカル推論サーバーに対応します。
4. **明確なセキュリティ境界** — 内蔵エージェントによるディスク・シェル・シークレット・ネットワーク操作はすべて、ワークスペース承認、承認フロー、deny-list、SSRF ポリシーゲートを経由します。外部エージェントは、そのバックエンド、実行環境、実際の隔離能力と未知点を明示し、バックエンド自身の操作を内蔵ゲートで保護されているかのように偽装しません。

### 現在のデスクトップマイルストーン

ワークスペースは純粋なロジックプロトタイプから、ビルド可能な GPUI デスクトップワークベンチへと進んでいます。現時点で含まれるもの：

- 永続ワークスペース、8 種類のタブ、分割ペイン、Explorer / Source Control / History サイドバー、ステータスバー、独立した設定ウィンドウ；
- portable-pty + alacritty_terminal による本物のターミナル：シェル統合（bash/zsh/fish/PowerShell）、OSC 7/133/777、IME、検索、スクロールバック、URL/localhost 検出、Windows Job Objects。設定でのデフォルトシェル選択（システム既定 / 検出されたシェル（Git Bash や WSL ディストリビューションを含む） / 手動パス）と、新規ターミナル作成時のシェル選択メニュー（オプション）。コピー/貼り付け/すべて選択のコンテキストメニュー；
- [SSH セッションと SFTP](docs/ssh.md)：接続プロファイルとサイドバーからのクイック接続、OS 認証情報ストア、パスワード/MFA/鍵/エージェント認証、ジャンプホスト、ホスト鍵検証、再開対応のファイル/ディレクトリ転送。双 pane の SFTP ブラウザーと、アクティブなリモートタブに追従するファイルエクスプローラー；
- Rope 編集バッファ、仮想化ビューポート、tree-sitter のインクリメンタルハイライト、検索、アンドゥ/リドゥ、インライン補完のゴーストテキスト、10 種類のエディターテーマ、Vim 操作レイヤー；
- ファイルインデックス（浅いインデックスとデバウンス付き監視）、gitignore 対応、ファジー検索、ストリーミング バックグラウンド grep、キーボードによるツリーナビゲーションと完全なコンテキストメニュー。タブごとのプロジェクトフォルダーと `cd` インジェクション、サイドバーの追従；
- Git 専用の diff / history / commit-file タブ、ファイルおよび hunk 単位の stage/unstage、確認付き discard、commit、branch、fetch/pull/push、コミットグラフ。ステータス更新は UI スレッドから分離；
- Web プレビュー：localhost URL 検出、URL 検証、システムブラウザーへの引き継ぎ（WebView を埋め込まない — [ADR 0002](docs/adr/0002-remove-embedded-webview.md) 参照）。共有編集バッファと同期するネイティブ Markdown プレビュー；
- 12 種類のアプリテーマとカスタムテーマモデル、GPU 描画のウィンドウ背景ぼかし；
- OpenAI、Anthropic、Gemini、Groq、xAI、Cerebras、OpenRouter、DeepSeek、Mistral、OpenAI 互換、LM Studio、MLX、Ollama の実際の HTTP/SSE アダプター；
- OS キーチェーン、セッション/プロジェクトメモリー、ファイル/画像/クリップボード/`@path` 添付、スニペット/TODO、Auto / Plan / Yolo の 3 段階送信モード；
- Composer コマンド面：`/` スラッシュコマンドパレット（新規セッション、Auto/Plan/Yolo、エージェント切り替え、停止、添付、ドックおよびインスペクターパネル）はキーマップを再利用し、動作を fork しません。`#handle` スニペット補完は `/snippet` と `/snippets` を基盤とし、`/todos` パネルは `todo_read` / `todo_write` 経由でエージェントと共有されるアプリ内 TODO リストの上に構築；
- ターミナル選択範囲をコマンド来歴付きで Composer に添付：OSC 133 C/D 境界を各コマンドの正確なバイトに合わせるため、添付にはコマンド id、cwd、コマンドライン、終了コードが含まれ、コンテキストメニューから直近の失敗コマンドの出力を添付できます；
- 直列化可能な Task/Turn、予算とキャンセル、実際のツール実行、承認カード、コマンドタイムアウト、永続シェル、バックグラウンドプロセス、そして `write_file → AI diff → hunk ごとの決定 → 原子的書き込み` の安全ループ；
- 内蔵エージェントと Codex app-server を Composer 内で切り替え可能。スタンドアロンの `termior-agent-host` が構造化バックエンドコントラクト、Codex/ACP アダプター、能力ネゴシエーション、実行環境の記述を提供；
- `AGENTS.md` の階層ルール、Context Inspector、Agent Skills、stdio と Streamable HTTP の MCP、制限付き Hooks、レビュー可能な Memory、カスタムエージェント、サブタスクオーケストレーション；
- journal/snapshot による復旧、Recovery Center、コンテンツアドレス方式のチェックポイント、direct/worktree/sandboxed 実行環境、タスクツリーと自動化キューのエントリーポイント；
- 内蔵/ターミナルエージェントの統一通知ルーティング：対象が可視の間は抑制、非表示時はテーマ対応 toast、ウィンドウ非フォーカス時はシステム通知、ヘッダーのベルに現在の状態を列挙；
- Claude Code OSC hooks の安全で冪等なインストールとアンインストール。

これはまだ段階的なマイルストーンであり、仕様全体の最終受領ではありません。ベースラインとなるデスクトップ範囲は [docs/desktop-milestone.md](docs/desktop-milestone.md)、Agent ステージ A–E の実装証拠とプラットフォームサンドボックス、リモート MCP OAuth、実際の ACP クライアント、バックグラウンド自動実行の境界は [docs/ai-agent-implementation-status.md](docs/ai-agent-implementation-status.md) に記録されています。

最新のリリースタグは `v0.2.1`、ワークスペースのバージョンは [Cargo.toml](Cargo.toml) で定義されます。

### ワークスペース

```text
crates/
  termior                  GPUI エントリーポイント、アプリ起動、デスクトップ配線
  termior-ui               GPUI ビューレイヤー、タブ/サイドバー/ワークスペースの永続状態
  termior-ui-kit           共有コントロール、ペインレイアウト、検索モデル
  termior-i18n             コンパイル時埋め込み翻訳テーブルとロケールフォールバック
  termior-terminal         PTY セッション、プロセスライフサイクル、バイトブリッジ
  termior-ssh              OpenSSH 接続プロファイル、認証オプション、SFTP 転送コマンド
  termior-terminal-core    OSC、シェル統合、検索
  termior-editor           Rope、tree-sitter、Vim、補完状態
  termior-explorer         ファイルインデックス、ツリー、検索、ウォッチャー
  termior-explorer-core    純粋ロジックのファジーマッチングと glob
  termior-vcs              Git ステータス、diff、履歴、リモート操作
  termior-preview          localhost 検出とプレビュー状態
  termior-ai               プロバイダー、タスクランタイム、ツール、コンテキスト、オーケストレーション
  termior-agent-host       内蔵/外部エージェントのバックエンドコントラクト、Codex、ACP、MCP
  termior-diff             hunk diff と受け入れセットの適用
  termior-security         認可、deny-list、SSRF、ツールゲート
  termior-store            原子的 JSON、マイグレーション、設定、タスク journal、チェックポイント
  termior-theme            中央セマンティックパレットとテーマライブラリ
  termior-hooks            Claude Code hooks
  termior-platform         システム通知、外部 URL、自動更新のプラットフォーム境界
  termior-bench            コールドスタート、メモリー、フレームレート、PTY スループットのゲート
```

### ビルドと検証

stable Rust、プラットフォームネイティブのビルドツールチェーン、GPUI のシステム依存が必要です。

#### Windows：まず MSVC 環境を有効化

このプロジェクトは `libgit2-sys` や `libz-sys` などの C コードをコンパイルするため、`cl.exe` が C 標準ライブラリのヘッダー（例：`time.h`）を検出できる必要があります。Git Bash や通常のターミナルから直接 `cargo build` を実行すると `INCLUDE`、`LIB`、`VCINSTALLDIR` が未設定のままとなり、`cl.exe` は `fatal error C1083: Cannot open include file: 'time.h'` で失敗します（cc-rs のログでは `command did not execute successfully (status code: exit code: 2)` と表示）。ビルド前に MSVC 環境を有効化してください。

以下のいずれかで `INCLUDE` / `LIB` / `VCINSTALLDIR` が空でない状態にします：

- **x64 Native Tools Command Prompt for VS 2022**（最も簡単）：スタートメニューから開けば変数は設定済みなので、そのまま下のコマンドを実行します。
- **PowerShell / cmd**：先に `vcvars64.bat` を実行してからビルド：
  ```bat
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
  cargo build
  ```
- **Git Bash**：cmd でラップして `.bat` が正しいインタープリターで動くようにします：
  ```bash
  cmd //c '"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" && cargo build'
  ```

環境の確認：

```bat
echo %INCLUDE%   &  rem MSVC ヘッダーと Windows SDK の ucrt/shared/um ディレクトリを指すべき
echo %LIB%       &  rem 対応するライブラリディレクトリを指すべき
echo %VCINSTALLDIR%
```

#### 検証

以下のコマンドは CI の core/desktop 分割に対応します：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --exclude termior -- -D warnings
cargo test --workspace --exclude termior --all-targets
cargo test -p termior-ui-kit --no-default-features
cargo check -p termior
cargo clippy -p termior --all-targets -- -D warnings
cargo test -p termior --all-targets
cargo build -p termior --release
cargo llvm-cov --workspace --exclude termior --tests --fail-under-lines 80
cargo bench -p termior-editor --bench large_file -- --quick
cargo bench -p termior-bench --bench pty_throughput -- --quick
```

CI は同じ分割を実装します：`ci.yml` はアイコン生成チェック、3 プラットフォームの core ジョブ、desktop ジョブ（check、clippy、テスト、リリースビルド、バイナリサイズゲート、Windows スモークフレーム、分割ペインスモーク、NFR プロセスゲート）、行カバレッジ 80% の coverage ジョブ、PTY スループットゲート付き benchmark ジョブで構成されます。`audit.yml` は push ごとと毎週 `cargo-deny` を実行し、ライセンスと CVE のドリフトを検出します。

#### アプリアイコン

アプリアイコンの唯一のソースは `assets/termior-logo.svg` です。SVG を編集した後、Node.js 22+ で Windows ICO、macOS ICNS、汎用 PNG、Linux hicolor アセットを再生成します：

```bash
npm ci
npm run icons
npm run icons:check
```

#### デスクトップアプリの実行

```bash
cargo run -p termior
```

#### リリース

リリースタグ（`v<workspace-version>`）は 3 プラットフォームのリリースビルドをトリガーし、SHA-256 チェックサム付きのポータブル ZIP/TAR アーカイブと、各プラットフォームのインストーラー（Windows は Inno Setup の `*-setup.exe`、macOS はドラッグでインストールする `.dmg`、Linux は `cargo-deb` による `.deb`）を生成します。パイプラインは Spec NFR-05 に従い、単一バイナリを 60 MiB、アーカイブ/インストーラーを 100 MiB に制限します。ローカルでは `scripts/check-release-binary.ps1` と `scripts/package-release.ps1` で同じチェックとパッケージングを再現できます（Windows インストーラーには Inno Setup 6 が必要）。

リリースサイズの大部分は、GPUI のネイティブレンダリングスタック、ターミナル/VTE 層、言語ごとの tree-sitter グラマー、そして TLS・キーチェーン・プロバイダークライアントです。Web プレビューは WebView を埋め込まずシステムブラウザーに委譲するため（ADR 0002）、依存面はプラットフォーム間で同一です。PNG デコードと tree-sitter 言語は明示的な最小 feature を使用しており、新しい UI・プレビュー・グラマー機能を追加する際は CI の依存ツリーとサイズレポートで評価する必要があります。

プロバイダーのエンドポイント、モデル、有効化状態は `Termior-settings.json` に保存されます。API キーは設定ウィンドウ経由でのみ OS キーチェーンに書き込まれ、設定ファイルやセッションファイルに直列化されることはありません。

#### 自動更新

**設定 → About** に自動更新トグル（既定で有効）、手動チェック、リリースノートがあります。アプリは起動 15 秒後と以降 6 時間ごとに GitHub の安定リリースを確認し、OS と CPU アーキテクチャに合うインストーラーをダウンロードして SHA-256 を検証します。ダウンロード完了するとタイトルバーに **Update available** と表示され、About を開いて **Install update…** を選ぶとシステムインストーラーが起動します。インストールを完了するには、アプリを終了する前にターミナルタスクを終了または停止してください。アップデーターがアプリやターミナルを自ら閉じることはありません。自動更新をオフにしても手動チェックは引き続き機能します。

Windows は Inno Setup、macOS は DMG を開いてアプリをドラッグで置き換え、Linux は DEB インストーラーを使用します。ポータブルビルド、DEB 非対応ディストリビューション、該当アーキテクチャのパッケージがない場合は、リリースのダウンロードページから手動で更新してください。ネットワークやチェックサムの失敗はエラーを表示し現在のバージョンを維持します。システムインストーラーに渡されたファイルはシステムの一時ディレクトリに置かれ、インストール後に削除できます。チェックサムはファイル整合性の検証のみに使われます。現在の Windows と macOS のインストーラーは未署名です。

### プライバシーとセキュリティ

- テレメトリなし、アカウントなし、自動アップロードなし；
- ローカルプロバイダーは完全オフラインで利用可能；
- `.env`、`.ssh`、credentials などの機密パスは canonicalize 後に双方向で拒否；
- クラウドプロバイダーの外向き通信は常に URL と名前解決後 IP の SSRF 検査を通過；
- モデルがファイルを直接ディスクに書き込むことはなく、ツール承認と hunk レビューを必須とします。

### 国際化（i18n）

インターフェースは 7 言語を同梱しています：English、简体中文、繁體中文、日本語、한국어、Español、Deutsch。**設定 → General → Language** で即時切り替え（メインウィンドウはデバウンス付き保存に追従）。既定の「システムに従う」は OS のロケールを追跡します。翻訳は `crates/termior-i18n/locales/` にコンパイル時埋め込みのフラット JSON テーブルで、`en.json` が信頼できる唯一のソースです。全ロケールは同一のキーセットを維持する必要があり、CI テストがキーの一致と空でない値を強制します。設計は [ADR 0007](docs/adr/0007-i18n-embedded-json-tables.md) を参照してください。

### ライセンス

プロジェクト自体は [MIT License](LICENSE) の下で提供されます。サードパーティ依存のライセンスは `cargo deny --exclude termior check` で監査されます。[LICENSE-APACHE](LICENSE-APACHE) は vendored `gpui_windows` の上流 Apache-2.0 条項を維持するためだけに存在し、GPUI 上流の依存ツリーは別途検証されます。
