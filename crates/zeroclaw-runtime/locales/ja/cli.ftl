cli-about = 最速で最小のAIアシスタント。
cli-no-command-provided = コマンドが指定されていません。
cli-try-quickstart = `zeroclaw quickstart` を試して、最初のエージェントを作成してください。
cli-quickstart-about = 最初のエージェントをエンドツーエンドで作成
cli-agent-about = AIエージェントループを開始
cli-gateway-about = ゲートウェイサーバー (ウェブフック、ウェブソケット) を管理
cli-acp-about = ACPサーバーを起動 (JSON-RPC 2.0 over stdio)
cli-daemon-about = 長時間実行自動デーモンを開始
cli-service-about = OSサービスライフサイクルを管理 (launchd/systemd ユーザーサービス)
cli-doctor-about = デーモン/スケジューラー/チャネル鮮度の診断を実行
cli-status-about = システムステータスを表示 (詳細)
cli-estop-about = エマージェンシーストップ状態を開始・検査・再開
cli-cron-about = スケジュール済みタスクを設定・管理
cli-models-about = プロバイダーモデルカタログを管理
cli-providers-about = サポートされているAIプロバイダーをリスト表示
cli-channel-about = 通信チャネルを管理
cli-integrations-about = 50以上の統合を参照
cli-skills-about = スキル (ユーザー定義機能) を管理
cli-sop-about = 標準操作手順 (SOP) を管理
cli-migrate-about = 他のエージェントランタイムからデータを移行
cli-auth-about = プロバイダー サブスクリプション認証プロファイルを管理
cli-hardware-about = USBハードウェアを発見・内省
cli-peripheral-about = ハードウェアペリフェラルを管理
cli-memory-about = エージェントメモリエントリを管理
cli-config-about = ZeroClaw設定を管理
cli-update-about = ZeroClaw更新を確認・適用
cli-self-test-about = 診断自己テストを実行
cli-completions-about = シェル補完スクリプトを生成
cli-desktop-about = ZeroClawコンパニオンデスクトップアプリを起動
cli-config-schema-about = 完全な設定JSONスキーマをstdoutにダンプ
cli-config-list-about = すべての設定プロパティを現在の値とともにリスト表示
cli-config-get-about = 設定プロパティ値を取得
cli-config-set-about = 設定プロパティを設定 (シークレットフィールドはマスク入力で自動プロンプト)
cli-config-init-about = 未設定セクションをデフォルト (enabled=false) で初期化
cli-config-migrate-about = config.tomlを現在のスキーマバージョンにディスク上で移行 (コメント保持)
cli-service-install-about = 自動開始と再開のためのデーモンサービスユニットをインストール
cli-service-start-about = デーモンサービスを開始
cli-service-stop-about = デーモンサービスを停止
cli-service-restart-about = 最新設定を適用するためデーモンサービスを再開
cli-service-status-about = デーモンサービスステータスを確認
cli-service-uninstall-about = デーモンサービスユニットをアンインストール
cli-service-logs-about = デーモンサービスログをテール表示
cli-channel-list-about = すべての設定済みチャネルをリスト表示
cli-channel-start-about = すべての設定済みチャネルを開始
cli-channel-doctor-about = 設定済みチャネルのヘルスチェックを実行
cli-channel-add-about = 新しいチャネル設定を追加
cli-channel-remove-about = チャネル設定を削除
cli-channel-send-about = 設定済みチャネルに1回限りのメッセージを送信
cli-wechat-pairing-required = 🔐 WeChatのペアリングが必要です。ワンタイムバインドコード: {$code}
cli-wechat-send-bind-command = WeChatから `{$command} <code>` を送信してください。
cli-wechat-qr-login = 📱 WeChat QRログイン（{$attempt}/{$max}）
cli-wechat-scan-to-connect = WeChatでスキャンして接続してください。
cli-wechat-qr-url = QR URL: {$url}
cli-wechat-qr-expired-giving-up = WeChat QRコードが {$max} 回期限切れになったため、中止します。
cli-wechat-qr-fetch-failed = WeChat QRコードの取得に失敗しました。
cli-wechat-qr-fetch-status-failed = WeChat QRコードの取得に失敗しました（{$status}）: {$body}
cli-wechat-missing-response-field = WeChatの応答に {$field} がありません。
cli-wechat-scanned-confirm = 👀 スキャンされました！スマートフォンで確認してください...
cli-wechat-qr-expired-refreshing = ⏳ QRコードの期限が切れました。更新中...
cli-wechat-login-confirmed-missing-field = ログインは確認されましたが、{$field} がありません。
cli-wechat-connected = ✅ WeChat に接続しました！
cli-wechat-bound-success = ✅ WeChatアカウントが正常にバインドされました。これで ZeroClaw と会話できます。
cli-wechat-invalid-bind-code = ❌ 無効なバインドコードです。もう一度お試しください。
cli-skills-list-about = すべてのインストール済みスキルをリスト表示
cli-skills-audit-about = スキルソースディレクトリまたはインストール済みスキル名を監査
cli-skills-install-about = URLまたはローカルパスから新しいスキルをインストール
cli-skills-remove-about = インストール済みスキルを削除
cli-skills-test-about = スキル (またはすべてのスキル) の TEST.sh 検証を実行
cli-skills-review-summary = { "  " }💾 スキルレビュー: {$summary}
cli-skills-install-start = スキルをインストール中: {$source}
cli-skills-install-resolving-registry = { "  " }スキルレジストリから '{$source}' を解決中...
cli-skills-install-installed-audited = { "  " }{$status} スキルがインストールされ、監査されました: {$path}（{$files} ファイルをスキャン）
cli-skills-install-security-audit-completed = { "  " }セキュリティ監査が正常に完了しました。
cli-skills-install-tier-official = {$name} v{$version} をインストール中 — 公式（zeroclaw-labs 管理）
cli-skills-install-tier-community =
    {$name} v{$version} をインストール中 — コミュニティ提出
    このスキルは ZeroClaw による監査を受けていません。スキルの内容を確認し、
    権限を付与したり本番環境で実行したりする前に `zeroclaw skills audit {$name}` を
    実行してください。
cli-skills-add-scaffolded = スキル {$target} を {$dir} にスキャフォールドしました
cli-skills-bundle-add-prompt =
    ディレクトリ '{$dir}' でskill-bundle '{$alias}' を作成するには、次を実行してください:
    zeroclaw config map-key skill-bundles {$alias}
    zeroclaw config set skill-bundles.{$alias}.directory {$dir}

    （`zeroclaw skills bundle add` による直接のバンドル作成は、config変更面を重複させてしまいます。）
cli-skills-bundle-remove-prompt =
    skill-bundle '{$alias}' を削除するには、次を実行してください:
    zeroclaw config map-key-delete skill-bundles {$alias}

    （configエントリを削除します。ディスク上のバンドルのディレクトリはそのまま残ります。）
cli-skills-bundle-list-empty =
    スキルバンドルが設定されていません。
    作成するには: zeroclaw config set skill-bundles.default.directory shared/skills/default
cli-skills-bundle-list-header = スキルバンドル ({$count}):
cli-skills-bundle-entry = {$alias} -> {$dir}
cli-skills-bundle-include = 含む: {$values}
cli-skills-bundle-exclude = 除外: {$values}
cli-skills-bundle-show-no-skills = （スキルがインストールされていません）
cli-skills-bundle-show-skills-header = スキル ({$count}):
cli-skills-bundle-show-skill = {$name}: {$description}
cli-cron-list-about = すべてのスケジュールタスクを一覧表示
cli-cron-add-about = 新しい定期スケジュールタスクを追加
cli-cron-add-at-about = 特定の UTC タイムスタンプで発火するワンショットタスクを追加
cli-cron-add-every-about = 固定間隔で繰り返すタスクを追加
cli-cron-once-about = 現在から遅延後に発火するワンショットタスクを追加
cli-cron-remove-about = スケジュールタスクを削除
cli-cron-update-about = 既存のスケジュールタスクの 1 つ以上のフィールドを更新
cli-cron-pause-about = スケジュールタスクを一時停止
cli-cron-resume-about = 一時停止したタスクを再開
cli-auth-login-about = OAuth でログイン (OpenAI Codex または Gemini)
cli-auth-refresh-about = リフレッシュトークンを使用して OpenAI Codex アクセストークンをリフレッシュ
cli-auth-logout-about = 認証プロファイルを削除
cli-auth-use-about = プロバイダーのアクティブなプロファイルを設定
cli-auth-list-about = 認証プロファイルを一覧表示
cli-auth-status-about = アクティブなプロファイルとトークン有効期限情報を表示
cli-memory-list-about = オプションのフィルター付きでメモリエントリを一覧表示
cli-memory-get-about = キーで特定のメモリエントリを取得
cli-memory-stats-about = メモリバックエンド統計とヘルスを表示
cli-memory-clear-about = カテゴリ別、キー別、またはすべてをクリアしてメモリをクリア
cli-memory-clear-unsupported-backend = memory clear は追記専用バックエンド '{$backend}' ではサポートされていません。削除可能なバックエンド（sqlite、lucid、またはpostgres）に切り替えてください
cli-estop-status-about = 現在の estop ステータスを表示
cli-estop-resume-about = エンゲージされた estop レベルから再開
cli-models-refresh-about = プロバイダーモデルをリフレッシュしてキャッシュ
cli-models-list-about = プロバイダーのキャッシュされたモデルを一覧表示
cli-models-set-about = 設定でデフォルトモデルを設定
cli-models-status-about = 現在のモデル設定とキャッシュステータスを表示
cli-doctor-models-about = プロバイダー全体のモデルカタログをプローブして可用性を報告
cli-doctor-traces-about = ランタイムトレースイベント (ツール診断とモデル応答) をクエリ
cli-hardware-discover-about = USB デバイスを列挙して既知のボードを表示
cli-hardware-introspect-about = デバイスをそのシリアル番号またはデバイスパスで内省
cli-hardware-info-about = ST-Link 経由 probe-rs を使用して USB でチップ情報を取得
cli-peripheral-list-about = 設定されたペリフェラルを一覧表示
cli-peripheral-add-about = ボードタイプとトランスポートパスでペリフェラルを追加
cli-peripheral-flash-about = Arduino ボードに ZeroClaw ファームウェアをフラッシュ
cli-sop-list-about = ロードされた SOP を一覧表示
cli-sop-validate-about = SOP 定義を検証
cli-sop-show-about = SOP の詳細を表示
cli-migrate-openclaw-about = OpenClaw ワークスペースからこの ZeroClaw ワークスペースにメモリをインポート
cli-agent-long-about =
    AI エージェントループを起動します。

    設定された AI プロバイダーでインタラクティブなチャットセッションを起動します。単一ショットクエリの場合は --message を使用し、インタラクティブモードに入りません。

    例:
    zeroclaw agent                              # インタラクティブセッション
    zeroclaw agent -m "Summarize today's logs"  # 単一メッセージ
    zeroclaw agent -p anthropic --model claude-sonnet-4-20250514
    zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
cli-gateway-long-about =
    ゲートウェイサーバー（webhook、websocket）を管理します。

    受信 webhook イベントと WebSocket 接続を受け入れる HTTP/WebSocket ゲートウェイを起動、再起動、または検査します。

    例:
    zeroclaw gateway start              # ゲートウェイを起動
    zeroclaw gateway restart            # ゲートウェイを再起動
    zeroclaw gateway get-paircode       # ペアリングコードを表示
cli-acp-long-about =
    ACP サーバーを起動します（stdio 上の JSON-RPC 2.0）。

    IDE とツール統合用に stdin/stdout で JSON-RPC 2.0 サーバーを起動します。セッション管理と通知としてのストリーミングエージェント応答に対応しています。

    メソッド: initialize、session/new、session/prompt、session/stop。

    例:
    zeroclaw acp                        # ACP サーバーを起動
    zeroclaw acp --max-sessions 5       # 同時セッション数を制限
cli-daemon-long-about =
    長時間実行の自律型デーモンを起動します。

    完全な ZeroClaw ランタイムを起動します: ゲートウェイサーバー、すべての設定されたチャネル（Telegram、Discord、Slack など）、ハートビートモニター、および cron スケジューラー。これは本番環境またはオンアシスタントとして ZeroClaw を実行する推奨方法です。

    デーモンを OS サービス（systemd/launchd）として登録し、ブート時に自動起動するには「zeroclaw service install」を使用してください。

    例:
    zeroclaw daemon                   # 設定デフォルトを使用
    zeroclaw daemon -p 9090           # ポート 9090 のゲートウェイ
    zeroclaw daemon --host 127.0.0.1  # ローカルホストのみ
cli-cron-long-about =
    スケジュール済みタスクを設定および管理します。

    cron 式、RFC 3339 タイムスタンプ、期間、または固定間隔を使用して、定期的、ワンショット、または間隔ベースのタスクをスケジュールします。

    Cron 式は標準 5 フィールド形式を使用します: 「min hour day month weekday」。タイムゾーンはデフォルトで UTC です。--tz と IANA タイムゾーン名で上書きしてください。

    例:
    zeroclaw cron list
    zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York --agent
    zeroclaw cron add '*/30 * * * *' 'Check system health' --agent
    zeroclaw cron add '*/5 * * * *' 'echo ok'
    zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder' --agent
    zeroclaw cron add-every 60000 'Ping heartbeat'
    zeroclaw cron once 30m 'Run backup in 30 minutes' --agent
    zeroclaw cron pause TASK_ID
    zeroclaw cron update TASK_ID --expression '0 8 * * *' --tz Europe/London
cli-channel-long-about =
    通信チャネルを管理します。

    ZeroClaw をメッセージングプラットフォームに接続するチャネルを追加、削除、一覧表示、送信、およびヘルスチェックします。サポートされるチャネルタイプ: telegram、discord、slack、whatsapp、matrix、imessage、email。

    例:
    zeroclaw channel list
    zeroclaw channel doctor
    zeroclaw channel add telegram '{ "{" }"bot_token":"..."、"name":"my-bot"{ "}" }'
    zeroclaw channel remove my-bot
    zeroclaw channel bind-telegram zeroclaw_user
    zeroclaw channel send 'Alert!' --channel-id telegram --recipient 123456789
cli-hardware-long-about =
    USB ハードウェアを検出して内省します。

    接続されている USB デバイスを列挙し、既知の開発ボード（STM32 Nucleo、Arduino、ESP32）を特定し、probe-rs/ST-Link 経由でチップ情報を取得します。

    例:
    zeroclaw hardware discover
    zeroclaw hardware introspect /dev/ttyACM0
    zeroclaw hardware info --chip STM32F401RETx
cli-peripheral-long-about =
    ハードウェアペリフェラルを管理します。

    エージェントにツール（GPIO、センサー、アクチュエーター）を公開するハードウェアボードを追加、一覧表示、フラッシュ、および設定します。サポートされるボード: nucleo-f401re、rpi-gpio、esp32、arduino-uno。

    例:
    zeroclaw peripheral list
    zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
    zeroclaw peripheral add rpi-gpio native
    zeroclaw peripheral flash --port /dev/cu.usbmodem12345
    zeroclaw peripheral flash-nucleo
cli-memory-long-about =
    エージェントメモリエントリを管理します。

    エージェントが保存したメモリエントリを一覧表示、検査、クリアします。カテゴリとセッション別のフィルタリング、ページネーション、および確認付きバッククリアをサポートしています。

    例:
    zeroclaw memory stats
    zeroclaw memory list
    zeroclaw memory list --category core --limit 10
    zeroclaw memory get KEY
    zeroclaw memory clear --category conversation --yes
cli-config-long-about =
    ZeroClaw 設定を管理します。

    ドット記法で設定プロパティを表示、設定、または初期化します。「schema」を使用して、設定ファイルの完全な JSON スキーマをダンプします。

    プロパティはドット記法でアドレス指定されます（例: channels.matrix.mention-only）。
    シークレットフィールド（API キー、トークン）は自動的にマスクされた入力を使用します。
    列挙フィールドは、値が省略された場合、インタラクティブ選択を提供します。

    例:
    zeroclaw config list                                  # すべてのプロパティを一覧表示
    zeroclaw config list --secrets                        # シークレットのみを一覧表示
    zeroclaw config list --filter channels.matrix         # プレフィックスでフィルタリング
    zeroclaw config get channels.matrix.mention-only      # 値を取得
    zeroclaw config set channels.matrix.mention-only true # 値を設定
    zeroclaw config set channels.matrix.access-token      # シークレット: マスクされた入力
    zeroclaw config set channels.matrix.stream-mode       # 列挙: インタラクティブ選択
    zeroclaw config init channels.matrix                  # デフォルト値でセクションを初期化
    zeroclaw config schema                                # JSON Schema を stdout に出力
    zeroclaw config schema > schema.json

    プロパティパスタブ補完は `zeroclaw completions <shell>` に自動的に含まれます。
cli-update-long-about =
    ZeroClaw 更新を確認して適用します。

    デフォルトでは、6 段階のパイプライン（プリフライト、ダウンロード、バックアップ、検証、スワップ、スモークテスト）で最新リリースをダウンロードしてインストールします。失敗時に自動ロールバックします。

    更新を確認するだけでインストールしない場合は --check を使用してください。
    インストール確認プロンプトをスキップするには --force を使用してください。
    最新ではなく特定のリリースをターゲットにするには --version を使用してください。

    例:
    zeroclaw update                      # 最新をダウンロードしてインストール
    zeroclaw update --check              # チェックのみ、インストールしない
    zeroclaw update --force              # 確認なしでインストール
    zeroclaw update --version 0.6.0      # 特定のバージョンをインストール
cli-self-test-long-about =
    診断自己テストを実行して ZeroClaw インストールを検証します。

    デフォルトでは、ネットワークチェック（ゲートウェイヘルス、メモリラウンドトリップ）を含む完全なテストスイートを実行します。--quick を使用して、ネットワークチェックをスキップしてより高速なオフライン検証を実行してください。

    例:
    zeroclaw self-test             # 完全なスイート
    zeroclaw self-test --quick     # 高速チェックのみ（ネットワークなし）
cli-skills-install-suggestion =
    このリクエストには `{$name}` スキルが必要なようですが、インストールされていません。

    一致した機能: {$matched}
    次: `{$install_command}` を実行してインストールしてください。
cli-completions-long-about =
    `zeroclaw` のシェル補完スクリプトを生成します。

    スクリプトは stdout に出力されるため、直接ソースできます:

    例:
    source <(zeroclaw completions bash)
    zeroclaw completions zsh > ~/.zfunc/_zeroclaw
    zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish
cli-desktop-long-about =
    ZeroClaw コンパニオンデスクトップアプリを起動します。

    コンパニオンアプリは、CLI と同じゲートウェイに接続する軽量のメニューバー/システムトレイアプリケーションです。ダッシュボードへのクイックアクセス、ステータス監視、およびデバイスペアリングを提供します。

    --install を使用して、プラットフォーム用の事前ビルドコンパニオンアプリをダウンロードしてください。

    例:
    zeroclaw desktop              # コンパニオンアプリを起動
    zeroclaw desktop --install    # ダウンロードしてインストール
channel-needs-quickstart-reply = このエージェントはまだ完全にセットアップされていません。返信する前に、オペレーターがQuickstartを実行する必要があります。
channel-whatsapp-web-feature-missing-warning = ⚠ WhatsApp Web は設定されていますが、'whatsapp-web' 機能がコンパイルされていません。
channel-whatsapp-web-feature-missing-build = ビルド/実行: cargo build --features whatsapp-web
channel-whatsapp-web-feature-missing-install = PATHにインストールされている場合は、次のコマンドで再インストールしてください: cargo install --path . --force --locked --features whatsapp-web
channel-whatsapp-web-feature-missing-error = WhatsApp Web チャネルには 'whatsapp-web' 機能が必要です。有効にするには: cargo build --features whatsapp-web（または、PATHにインストールされている場合: cargo install --path . --force --locked --features whatsapp-web）
channel-wecom-ws-stream-bootstrap = 処理中です。お待ちください。
channel-wecom-ws-stop-ack = 現在のメッセージを停止しました。
channel-wecom-ws-voice-unavailable = 現在、音声メッセージを処理できません {$emoji}
channel-wecom-ws-unsupported-message = このメッセージタイプはまだサポートされていません。
channel-wecom-ws-welcome = こんにちは、チャットへようこそ {$emoji}
channel-wecom-ws-supplemental-message =
    {"["}補足メッセージ]
    {$extra}
channel-wecom-ws-group-allowlist-missing =
    WeComの許可リストが設定されていないため、このボットはグループメッセージを受け付けていません。

    グループのchatid: {$chatid}
    送信者のuserid: {$userid}

    {$allowed_groups_path} または {$allowed_users_path} に許可エントリを追加してください。テスト用に一時的に ["*"] に設定することもできます。
channel-wecom-ws-group-access-denied =
    このグループはこのボットの使用を許可されていません。

    グループのchatid: {$chatid}
    送信者のuserid: {$userid}

    管理者にこのグループを {$allowed_groups_path} に追加するよう依頼するか、あなたのuseridを {$allowed_users_path} に追加してください。
channel-wecom-ws-dm-allowlist-missing =
    WeComの許可リストが設定されていないため、このボットはメッセージを受け付けていません。

    あなたのuserid: {$userid}

    {$allowed_users_path} に許可エントリを追加してください。テスト用に一時的に ["*"] に設定することもできます。
channel-wecom-ws-dm-access-denied =
    このボットを使用する権限がありません。

    あなたのユーザーID: {$userid}

    管理者に、あなたのユーザーIDを {$allowed_users_path} に追加するよう依頼してください。
channel-discord-interaction-unauthorized = このコマンドをここで使用する権限がありません。
channel-discord-interaction-malformed = 不明または不正なコマンドです。
channel-discord-interaction-unavailable = このコマンドは利用できなくなったか、入力が空でした。
channel-discord-delivery-failure-note-one = （注意：{$count}個のファイルを配信できませんでした。）
channel-discord-delivery-failure-note-many = （注意：{$count}個のファイルを配信できませんでした。）
channel-whatsapp-web-delivery-failure-note-one = （注意：{$count}件のWhatsAppメディア添付ファイルを配信できませんでした。）
channel-whatsapp-web-delivery-failure-note-many = （注意：{$count}件のWhatsAppメディア添付ファイルを配信できませんでした。）
onboard-openai-auth-note =
    OpenAI認証:
    • APIキー — platform.openai.com 経由の標準APIアクセス (sk-...)
    • Codexサブスクリプション — ChatGPT Plus/Proアカウントを使用 (APIキー不要)
onboard-openai-auth-prompt = 認証
onboard-openai-auth-api-key = APIキー
onboard-openai-auth-codex = Codexサブスクリプション
onboard-openai-codex-followup =
    Codexサブスクリプションの認証はChatGPTアカウントを使用します。
    エージェントを起動する前に `zeroclaw auth login --provider openai-codex` を実行して認証してください。
cli-web-dist-dir-reason-tilde = 展開されない `~` で始まっています
cli-web-dist-dir-reason-dollar = 展開されない `$` が含まれています
cli-doctor-web-dist-dir-expansion-warning = gateway.web_dist_dir = "{$path}" — {$reason}。gateway.web_dist_dir はそのまま読み込まれるため、値を自分で展開してください（例: 絶対パス）
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = 未設定（自動検出を使用）
cli-self-test-web-dist-dir-pass-literal = {$path}（リテラルパス）
cli-self-test-web-dist-dir-fail-expansion = 警告: {$path} — {$reason}。gateway.web_dist_dir はそのまま読み込まれるため、値を自分で展開してください（例: 絶対パス）
cli-peripherals-none = 周辺機器が設定されていません。
cli-peripherals-add-hint = 次のコマンドで追加します: zeroclaw peripheral add <board> <path>
cli-peripherals-add-example = {"  "}例: zeroclaw peripheral add nucleo-f401re <serial-path>
cli-peripherals-config-hint = または config.toml に追加します:
cli-peripherals-configured = 設定済みの周辺機器:
cli-peripherals-already-configured = ボード {$board} ({$path}) は既に設定されています。
cli-peripherals-added = {$board} を {$path} に追加しました。適用するにはデーモンを再起動してください。
cli-peripherals-flash-needs-hardware = Arduino のフラッシュには 'hardware' 機能が必要です。
cli-peripherals-unoq-needs-hardware = Uno Q のセットアップには 'hardware' 機能が必要です。
cli-peripherals-nucleo-needs-hardware = Nucleo のフラッシュには 'hardware' 機能が必要です。
cli-skills-none-installed = スキルがインストールされていません。
cli-skills-create-hint = {"  "}作成: mkdir -p ~/.zeroclaw/workspace/skills/my-skill
cli-skills-install-hint = {"  "}またはインストール: zeroclaw skills install <source>
cli-skills-installed-header = インストール済みのスキル ({$count}):
cli-skills-tags = タグ:  {$tags}
cli-sop-none = SOP が見つかりません。
cli-sop-create-hint = {"  "}作成: mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}その後 SOP.toml と SOP.md を追加します
cli-sop-loaded-header = 読み込み済みの SOP ({$count}):
cli-sop-none-to-validate = 検証する SOP が見つかりません。
cli-sop-valid = ✅ {$name} — 有効
cli-sop-warnings = ⚠️  {$name} — {$count} 件の警告:
cli-sop-all-passed = すべての SOP が検証に合格しました。
cli-sop-priority = {"  "}優先度:       {$value}
cli-sop-execution-mode = {"  "}実行モード: {$value}
cli-sop-deterministic = {"  "}決定論的:  {$value}
cli-sop-cooldown = {"  "}クールダウン:       {$value}秒
cli-sop-max-concurrent = {"  "}最大同時実行数: {$value}
cli-sop-location = {"  "}場所:       {$value}
cli-sop-triggers = {"  "}トリガー:
cli-sop-steps = {"  "}ステップ:
cli-sop-step-tools = ツール: {$tools}
cli-memory-reindexing = メモリバックエンドを再インデックス中...
cli-memory-none = メモリエントリが見つかりません。
cli-memory-none-at-offset = オフセット {$offset} にエントリがありません (合計: {$total})。
cli-memory-next-page = 次のページを表示するには --offset {$offset} を使用してください。
cli-memory-key-not-found = キーに該当するメモリエントリが見つかりません: {$key}
cli-memory-prefix-matched = プレフィックス '{$key}' が {$n} 件のエントリに一致しました:
cli-memory-narrow-prefix = 一致を絞り込むには、より長いプレフィックスを指定してください。
cli-memory-key = キー:       {$value}
cli-memory-category = カテゴリ:  {$value}
cli-memory-timestamp = タイムスタンプ: {$value}
cli-memory-session = セッション:   {$value}
cli-memory-stats-header = メモリ統計:
cli-memory-backend = {"  "}バックエンド:  {$value}
cli-memory-total = {"  "}合計:    {$value}
cli-memory-by-category = {"  "}カテゴリ別:
cli-memory-none-to-clear = クリアするエントリがありません。
cli-memory-found-in-scope = '{$scope}' に {$count} 件のエントリが見つかりました。
cli-memory-aborted = 中止しました。
cli-memory-deleted-key = 削除されたキー: {$key}
cli-cron-none = スケジュールされたタスクはまだありません。
cli-cron-usage = 使用方法:
cli-cron-jobs-header = 🕒 スケジュールされたジョブ ({$count}):
cli-cron-list-cmd = {"    "}cmd: {$cmd}
cli-cron-list-prompt = {"    "}prompt: {$prompt}
cli-cron-added-agent = ✅ エージェントcronジョブ {$id} を追加しました
cli-cron-added = ✅ cronジョブ {$id} を追加しました
cli-cron-added-oneshot-agent = ✅ ワンショットエージェントcronジョブ {$id} を追加しました
cli-cron-added-oneshot = ✅ ワンショットcronジョブ {$id} を追加しました
cli-cron-added-interval-agent = ✅ インターバルエージェントcronジョブ {$id} を追加しました
cli-cron-added-interval = ✅ インターバルcronジョブ {$id} を追加しました
cli-cron-updated = ✅ cronジョブ {$id} を更新しました
cli-cron-removed = ✅ cronジョブ {$id} を削除しました
cli-cron-paused = ⏸️  cronジョブ {$id} を一時停止しました
cli-cron-resumed = ▶️  cronジョブ {$id} を再開しました
cli-cron-expr = {"  "}Expr  : {$v}
cli-cron-expr2 = {"  "}Expr: {$v}
cli-cron-next = {"  "}Next  : {$v}
cli-cron-next2 = {"  "}Next: {$v}
cli-cron-next3 = {"  "}Next     : {$v}
cli-cron-prompt = {"  "}Prompt: {$v}
cli-cron-prompt3 = {"  "}Prompt   : {$v}
cli-cron-cmd = {"  "}Cmd : {$v}
cli-cron-cmd3 = {"  "}Cmd      : {$v}
cli-cron-at = {"  "}At    : {$v}
cli-cron-at2 = {"  "}At  : {$v}
cli-cron-every = {"  "}Every(ms): {$v}
cli-no-command = コマンドが指定されていません。
cli-press-enter = 終了するにはEnterキーを押してください...
cli-quickstart-title = クイックスタート — 1つの動作するエージェントをエンドツーエンドで作成します。
cli-quickstart-needs-tty = クイックスタートは対話式で、stdin と stderr にターミナルが必要です。対話式シェルから実行するか、ヘッドレス設定には `zeroclaw config set <path> <value>` を使用してください。
cli-quickstart-cancelled = クイックスタートをキャンセルしました。設定は書き込まれていません。
cli-quickstart-incomplete = {"  "}すべてのセレクターがまだ入力されていません。
cli-quickstart-create-agent = ── エージェントを作成
cli-quickstart-create-agent-locked = ── エージェントを作成（ロック中 — 先にすべてのセレクターを入力してください）
cli-quickstart-open-selector-prompt = セレクターを開く（Enter）か、作成を選択します。Escで終了。
cli-quickstart-use-existing = 既存を使用
cli-quickstart-create-new = 新規作成
cli-quickstart-model-provider-prompt = モデルプロバイダー
cli-quickstart-pick-configured-provider = 設定済みプロバイダーを選択
cli-quickstart-row-model-provider = {$glyph} モデルプロバイダー — {$summary}
cli-quickstart-row-risk-profile = {$glyph} リスクプロファイル — {$summary}
cli-quickstart-row-memory = {$glyph} メモリ              — {$summary}
cli-quickstart-row-channels = {$glyph} チャンネル (0..N) — {$summary}
cli-quickstart-row-peer-groups = {$glyph} ピアグループ      — {$summary}
cli-quickstart-row-agent-identity = {$glyph} エージェントID   — {$summary}
cli-quickstart-summary-not-yet-chosen = 未選択
cli-quickstart-summary-not-yet-visited = 未訪問
cli-quickstart-summary-not-yet-named = 未命名
cli-quickstart-summary-provider-fresh = {$name} (エイリアス: {$alias}, モデル: {$model})
cli-quickstart-summary-use-existing = 既存の {$reference} を使用
cli-quickstart-summary-preset-fresh = プリセット: {$name}
cli-quickstart-summary-channels-none = なし (`zeroclaw agent` のみでチャット)
cli-quickstart-summary-agent = エイリアス: {$alias}, システムプロンプト: {$chars} 文字, 人格ファイル {$files} 件
cli-quickstart-summary-peer-groups-none = なし — チャンネルはピアを受け付けません
cli-quickstart-channel-remove-row = {"  "}{$reference} (削除)
cli-quickstart-peer-group-row = {$channel} → {$name} (ピア {$count} 件)
cli-quickstart-provider-local-label = {$name} (ローカル)
cli-quickstart-provider-type-prompt = プロバイダータイプ
cli-quickstart-alias-for = {$name} のエイリアス
cli-quickstart-model-field-missing-warning = 警告: スキーマが `{$provider}` の `model` フィールドを生成しませんでした — 手動入力にフォールバックします。報告してください。
cli-quickstart-model-id-for = {$name} のモデルID
cli-quickstart-risk-profile-prompt = リスクプロファイル
cli-quickstart-memory-backend-prompt = メモリバックエンド
cli-quickstart-add-channel = + チャンネルを追加
cli-quickstart-channels-done = 完了 (チャンネルセレクターを訪問済みにします)
cli-quickstart-channels-prompt = チャンネル (任意, 0..N)
cli-quickstart-channel-source-prompt = チャンネルソース
cli-quickstart-all-channels-bound = {"  "}設定済みのすべてのチャンネルは既にエージェントに割り当てられています。ここで再利用する前に `zeroclaw config set agents.<alias>.channels ...` で解放してください。
cli-quickstart-pick-configured-channel = 設定済みチャンネルを選択
cli-quickstart-channel-type-prompt = チャンネルタイプ
cli-quickstart-add-peer-group = + ピアグループを追加
cli-quickstart-done = 完了
cli-quickstart-peer-groups-prompt = ピアグループ (行でEnterを押すと削除、+ 追加で作成)
cli-quickstart-channel-to-authorize-prompt = 許可するチャンネル
cli-quickstart-external-peers-prompt = 外部ピア (カンマまたは改行区切り、なしなら空欄)
cli-quickstart-agent-alias-prompt = エージェントエイリアス
cli-quickstart-edit-system-prompt = $EDITOR でシステムプロンプトを編集しますか？ (空欄ならスキップ)
cli-quickstart-personality-start-template = テンプレートから開始 ($EDITOR で開く)
cli-quickstart-personality-start-current = 現在の内容から開始 ($EDITOR で開く)
cli-quickstart-personality-start-scratch = 空の状態から開始 ($EDITOR で開く)
cli-quickstart-personality-skip = スキップ
cli-quickstart-esc-go-back = {" "}(Escで戻る)
cli-quickstart-esc-return-checklist = {" "}(Escでチェックリストに戻る)
cli-quickstart-personality-file-prompt = {$filename}{$position} — 次は?{$back_hint}
cli-quickstart-next-agent-command = {"  "}zeroclaw agent -a {$alias}  # ターミナルでこのエージェントとチャット
cli-quickstart-fix-and-rerun = 既存の設定は変更されていません。次を修正してから quickstart を再実行してください:
cli-quickstart-could-not-finish = quickstart を完了できませんでした: 修正が必要な問題 {$count} 件
cli-quickstart-pick-preset = プリセットを選択
cli-quickstart-pick-existing-prompt = 既存の {$prompt} を選択
cli-quickstart-pick-preset-prompt = {$prompt} プリセットを選択
cli-quickstart-step-model-provider = モデルプロバイダー
cli-quickstart-step-risk-profile = リスクプロファイル
cli-quickstart-step-runtime-profile = ランタイムプロファイル
cli-quickstart-step-memory = メモリ
cli-quickstart-step-channels = チャンネル
cli-quickstart-step-peer-groups = ピアグループ
cli-quickstart-step-agent = エージェント
cli-quickstart-error-internal-no-result = 内部エラー: 検証エラーがないのに apply_into が結果を返しませんでした
cli-quickstart-error-completion-flag = quickstart-completed の切り替えに失敗しました: {$err}
cli-quickstart-error-persist-config = 設定の保存に失敗しました: {$err}
cli-quickstart-error-not-type-alias-ref = `{$reference}` は `<type>.<alias>` 参照ではありません
cli-quickstart-error-no-configured-path = `{$path}` は設定されていません
cli-quickstart-error-provider-required = プロバイダータイプ、エイリアス、モデルが必要です
cli-quickstart-error-unknown-provider-type = 不明なモデルプロバイダータイプ `{$provider}` — プロバイダー一覧から選択してください
cli-quickstart-error-alias-exists = エイリアス `{$alias}` は既に存在します
cli-quickstart-error-no-profile = プロファイル `{$alias}` は設定されていません
cli-quickstart-error-unknown-risk-preset = 不明なリスクプリセット `{$preset}`
cli-quickstart-error-unknown-runtime-preset = 不明なランタイムプリセット `{$preset}`
cli-quickstart-error-channel-bound = チャンネル `{$reference}` は既にエージェント `{$owner}` に割り当てられています
cli-quickstart-error-channel-required = チャンネルタイプとエイリアスが必要です
cli-quickstart-error-peer-group-name-required = ピアグループ名が必要です
cli-quickstart-error-peer-group-channel-required = ピアグループのチャンネル参照が必要です
cli-quickstart-error-peer-group-unknown-channel = ピアグループ `{$name}` が不明なチャンネル `{$channel}` を参照しています
cli-quickstart-error-peer-group-exists = ピアグループ `{$name}` は既に存在します
cli-quickstart-error-personality-workspace = エージェントワークスペースを作成できませんでした: {$err}
cli-quickstart-error-personality-filename-required = ファイル名が必要です
cli-quickstart-error-personality-not-editable = `{$filename}` は編集可能な人格ファイルではありません
cli-quickstart-error-personality-too-large = 内容が {$limit} 文字の制限を超えています
cli-quickstart-error-personality-stage-failed = {$filename} のステージに失敗しました: {$err}
cli-quickstart-error-personality-write-failed = {$path} の書き込みに失敗しました: {$err}
cli-quickstart-error-agent-name-required = エージェント名が必要です
cli-quickstart-error-agent-exists = エージェント `{$name}` は既に存在します
cli-no-channels-compiled = {"  "}このバイナリにコンパイルされているチャンネルタイプはありません。
cli-quickstart-complete = クイックスタートが完了しました。エージェント `{$alias}` を作成しました。
cli-next-steps = 次のステップ:
cli-agent-not-created = エージェントは作成されませんでした — ディスク上の変更はありません。
cli-onboard-deprecated = `zeroclaw onboard` は非推奨です — `zeroclaw quickstart` を使用してください。
cli-otp-initialized = ZeroClaw用のOTPシークレットを初期化しました。
cli-otp-enrollment-uri = 登録URI: {$uri}
cli-pairing-enabled = 🔐 ゲートウェイのペアリングが有効です。
cli-pairing-use-code = {"  "}このワンタイムコードを使って新しいデバイスをペアリングしてください:
cli-pairing-post = {"    "}POST /pair にヘッダー X-Pairing-Code: {$code} を付けて送信
cli-pairing-restart = {"   "}新しいペアリングコードを生成するにはゲートウェイを再起動してください。
cli-pairing-disabled = ⚠️  ゲートウェイのペアリングは設定で無効になっています。
cli-gateway-running-q = {"   "}ゲートウェイは実行中ですか？次のコマンドで起動してください:
cli-status-title = 🦀 ZeroClaw ステータス
cli-security-status-title = ZeroClaw セキュリティステータス
cli-security-status-source = ソース:      {$v}
cli-security-status-agent = エージェント:       {$v}
cli-security-status-agent-enabled = エージェント有効: {$enabled}
cli-security-status-risk-profile = リスクプロファイル: {$v}
cli-security-status-autonomy = 自律性:   {$v}
cli-security-status-approvals = 承認:  中リスクの承認が必要: {$medium}、高リスクのコマンドはブロック済み: {$high}
cli-security-status-sandbox = サンドボックス:    要求済み {$requested}、アクティブ {$active} ({$description})
cli-security-status-workspace = ワークスペース:  {$dir}; ワークスペース限定: {$workspace_only}; 読み書きルート: {$read_write_roots}; 読み取り専用ルート: {$read_only_roots}; 書き込み専用ルート: {$write_only_roots}; 環境変数パススルー: {$env_passthrough}
cli-security-status-credentials = 認証情報: 暗号化: {$encryption}; シークレット設定数: {$secrets_set}/{$secrets_total}; 分類されたフィールド数: {$classified_total}; クラス: {$classification_summary}
cli-security-status-credentials-classes-none = なし
cli-security-status-gateway = ゲートウェイ:    {$host}:{$port}; ペアリング必須: {$pairing}; パブリックバインド: {$public_bind}; TLS: {$tls}
cli-security-status-warnings = 警告:   {$v}
cli-security-status-warnings-none = 警告:   なし
cli-security-status-warning-agent-disabled = エージェントが無効です
cli-security-status-warning-sandbox-disabled = このエージェントのリスクプロファイルではサンドボックス化が無効です
cli-security-status-warning-sandbox-none = アクティブなサンドボックスはアプリケーション層のみです
cli-security-status-warning-sandbox-fallback = 要求されたサンドボックスバックエンド `{$requested}` は `{$active}` にフォールバックしました
cli-security-status-warning-workspace-not-restricted = ワークスペース限定のファイルシステムポリシーが無効です
cli-security-status-warning-shell-env-passthrough = {$count} 個のシェル環境変数がパススルーされています
cli-security-status-warning-secrets-unencrypted = 設定シークレットの暗号化が無効です
cli-security-status-warning-credential-follow-up = 一部の認証情報形式の設定項目はまだフォローアップが必要です
cli-security-status-warning-pairing-disabled = ゲートウェイのペアリングが必須になっていません
cli-security-status-warning-public-bind-no-tls = ゲートウェイが TLS を有効にせずにパブリックバインドを許可しています
cli-status-provider-none = 🤖 ModelProvider:      (設定なし)
cli-status-agents-none = 🛡️  エージェント:        (設定なし)
cli-status-service-running = 🟢 サービス:       実行中
cli-status-service-stopped = 🔴 サービス:       停止
cli-status-channels = チャンネル:
cli-status-cli-always = {"  "}CLI:      ✅ 常時
cli-status-peripherals = 周辺機器:
cli-desktop-download = ZeroClaw コンパニオンアプリをダウンロード:
cli-desktop-homebrew = または Homebrew でインストール(近日対応予定):
cli-desktop-linux-pkg = {"  "}お使いのアーキテクチャ用の .deb または .AppImage をダウンロードしてください。
cli-desktop-launching = ZeroClaw コンパニオンアプリを起動中...
cli-status-version = バージョン:     {$v}
cli-status-workspace = ワークスペース:   {$v}
cli-status-config = 設定:      {$v}
cli-status-provider-indent = {"   "}ModelProvider:      {$family}.{$alias}
cli-status-provider = 🤖 ModelProvider:      {$family}.{$alias}
cli-status-model = {"   "}モデル:         {$model}
cli-status-observability = 📊 可観測性:  {$v}
cli-status-trace-storage = 🧾 トレースストレージ:  {$mode} ({$path})
cli-status-agents = 🛡️  エージェント:        {$v}
cli-status-runtime = ⚙️  ランタイム:       {$v}
cli-status-heartbeat = 💓 ハートビート:      {$v}
cli-status-heartbeat-every-minutes = {$minutes}分ごと
cli-status-memory = 🧠 メモリ:         {$backend} (自動保存: {$auto_save})
cli-status-security-noprofile = セキュリティ ({$alias}): <risk_profile なし>
cli-status-security = セキュリティ ({$alias}):
cli-status-workspace-only = {"  "}ワークスペースのみ:    {$v}
cli-status-allowed-roots = {"  "}許可されたルート:     {$v}
cli-status-allowed-commands = {"  "}許可されたコマンド:  {$v}
cli-status-max-actions = {"  "}最大アクション/時:  {$v}
cli-status-cost-tracking = {"  "}コスト追跡:     {$v}
cli-status-max-cost-day = {"  "}最大コスト/日:      ${$v}
cli-status-max-cost-month = {"  "}最大コスト/月:    ${$v}
cli-status-spent-today = {"  "}本日の支出:       ${$spent} / ${$limit}
cli-status-spent-month = {"  "}今月の支出:  ${$spent} / ${$limit}
cli-status-otp = {"  "}OTP 有効:       {$v}
cli-status-estop = {"  "}E-stop 有効:    {$v}
cli-status-peripherals-enabled = {"  "}有効:   {$v}
cli-status-boards = {"  "}ボード:    {$v}
cli-status-channel-not-compiled = 🚫 設定済み、未コンパイル
cli-status-word-enabled = 有効
cli-status-word-disabled = 無効
cli-status-word-yes = はい
cli-status-word-no = いいえ
cli-status-word-on = オン
cli-status-word-off = オフ
cli-status-word-none = (なし)
cli-status-word-configured = 設定済み
cli-status-word-not-configured = 未設定
cli-desktop-not-installed = ZeroClaw コンパニオンアプリがインストールされていません。
cli-desktop-blurb1 = コンパニオンアプリは軽量なメニューバーアプリで、
cli-desktop-blurb2 = CLI と同じゲートウェイに接続します。
cli-config-all-configured = すべてのセクションは既に設定済みです。
cli-config-schema-current = 設定は既に現在のスキーマバージョンです。
cli-config-applied-ops = {$count} 件の操作を適用しました:
cli-plugins-none = インストールされているプラグインはありません。
cli-plugins-installed = インストール済みプラグイン:
cli-plugin-installed-from = プラグインを {$source} からインストールしました
cli-plugin-removed = プラグイン '{$name}' を削除しました。
cli-plugin-not-found = プラグイン '{$name}' が見つかりません。
cli-plugin-legacy-detected = 注意: レガシーな場所 ({$path}) にあるプラグインはエージェントに読み込まれません。`zeroclaw plugin migrate` を実行して {$target} に移動してください。
cli-plugin-migrated = {$count} 個のプラグインを {$path} から {$target} に移動しました。
cli-plugin-migrate-none = 移行する項目はありません。
cli-estop-resume-done = Estop の再開が完了しました。
cli-estop-engaged = Estop を作動させました。
cli-estop-status = Estop ステータス:
cli-auth-none = 認証プロファイルが設定されていません。
cli-auth-active = アクティブなプロファイル:
cli-warn-crypto-provider = 警告: デフォルトの暗号プロバイダーのインストールに失敗しました: {$err}
cli-error-label = {"   "}エラー: {$err}
cli-warn-cost-usage = {"  "}⚠ コスト使用状況を読み込めませんでした: {$err}
cli-warn-cost-tracker = {"  "}⚠ コストトラッカーを初期化できませんでした: {$err}
cli-desktop-download-at = {"  "}ダウンロード先: {$url}
cli-config-legend = 凡例: 💉 env で上書き  🔒 シークレット
cli-config-secret-set = {$path} は設定されています(暗号化されたシークレット — 値は表示されません)
cli-config-secret-unset = {$path} は設定されていません(暗号化されたシークレット)
cli-config-updated = {$path} を更新しました。
cli-config-review-hint = `zeroclaw config list` を実行して確認し、必須フィールドを設定してください。
cli-config-backed-up = {$path} にバックアップしました
cli-plugin-name-version = プラグイン: {$name} v{$version}
cli-plugin-description = 説明: {$desc}
cli-plugin-capabilities = 機能: {$v}
cli-plugin-permissions = 権限: {$v}
cli-plugin-wasm = WASM: {$path}
cli-plugin-wasm-none = WASM: (スキルのみのプラグイン)
cli-estop-domains-none = {"  "}domain_blocks:  (なし)
cli-estop-domains = {"  "}domain_blocks:  {$v}
cli-estop-tools-none = {"  "}tool_freeze:    (なし)
cli-estop-tools = {"  "}tool_freeze:    {$v}
cli-estop-updated-at = {"  "}updated_at:     {$v}
cli-auth-saved = プロファイル {$profile} を保存しました
cli-auth-active-for = {$provider} のアクティブなプロファイル: {$profile}
cli-auth-refresh-ok = ✓ トークンの更新に成功しました (プロファイル {$profile})
cli-auth-removed = 認証プロファイル {$provider}:{$profile} を削除しました
cli-auth-not-found = 認証プロファイルが見つかりません: {$provider}:{$profile}
cli-locales-fetched = {"  "}{$name} を取得しました -> {$path}
cli-locales-skipped = {"  "}{$name} をスキップしました: アップストリームに存在しません（{$path}; 試行: {$refs}）
cli-locales-installed = {$dir} 配下に '{$locale}' 用のカタログを {$count} 件インストールしました
cli-browse-header = {$path} ({$count} 件のエントリ)
cli-browse-empty = (空)
cli-browse-file-bytes = {$name} ({$bytes} バイト)
cli-hardware-feature-required = ハードウェア検出には 'hardware' 機能が必要です。
cli-hardware-feature-build = ビルド方法: cargo build --features hardware
cli-hardware-unsupported-platform = このプラットフォームではハードウェア USB 検出はサポートされていません。
cli-hardware-supported-platforms = 対応プラットフォーム: Linux、macOS、Windows。
cli-update-already-current = すでに最新です (v{$version})。
cli-update-success = v{$version} に正常に更新しました！
cli-update-prebuilt-channel-note = ビルド済み更新は軽量なデフォルトチャンネルバンドルを使います。Slack、Discord、その他の非デフォルトチャンネルを使うには、`./install.sh --source --preset full`、`--features channels-full`、または特定の `channel-*` 機能でソースからビルドしてください。
cli-selftest-all-passed = {$total} 件すべてのチェックに合格しました。
cli-selftest-some-failed = {$failed}/{$total} 件のチェックが失敗しました。
cli-selftest-channel-config-uncompiled = コンパイル済みチャンネル種別 {$compiled} 件、コンパイル済みかつ設定済み {$configured} 件。設定済みですが未コンパイル: {$names}。ソースから `./install.sh --source --preset full`、`--features channels-full`、または特定の `channel-*` 機能でビルドしてください。
cli-channels-header = チャンネル:
cli-channels-cli-always = {"  "}✅ CLI (常に利用可能)
cli-channels-notion = {"  "}{$status} Notion
cli-channels-not-compiled-header = {"  "}設定済みですが、このバイナリにはコンパイルされていません:
cli-channels-not-compiled-entry = {"  "}🚫 {$name} (設定済み、未コンパイル)
cli-channels-build-hint = {"  "}ソースから `./install.sh --source --preset full`、`--features channels-full`、または特定の `channel-*` 機能でビルドしてください。
cli-channels-start-hint = チャンネルを開始するには: zeroclaw channel start
cli-channels-doctor-hint = 状態を確認するには:    zeroclaw channel doctor
cli-channels-configure-hint = 設定するには:      zeroclaw config set channels.<name>.<field>=<value>
turn-interrupted-by-user = [ユーザーによって中断されました]
turn-cancelled-client-rpc = [クライアント経由でターンがキャンセルされました]
turn-stream-interrupted = [ストリームが中断されました]
turn-tool-interrupted-before-result = [このツールが結果を生成する前にユーザーによって中断されました]
channel-runtime-malformed-tool-output = 内部ツール呼び出し形式のエラーが発生し、このリクエストを完了できませんでした。もう一度お試しください。
cli-alias-list-empty = ({$section} の下にエントリがありません)
cli-alias-created = {$section}.{$alias} を作成しました
cli-alias-exists = {$section}.{$alias} は既に存在します（変更なし）
cli-alias-impact-scrub-header = {$section}.{$alias} を削除すると {$count} 件の参照が除去されます:
cli-alias-impact-blocked-header = {$section}.{$alias} の削除は {$count} 件のハード参照によってブロックされています:
cli-alias-impact-blocker = ✗ {$path}（ハード参照）
cli-alias-impact-scrub = • {$path}（除去されます）
cli-alias-no-changes = 変更は行われませんでした。適用するには --yes を付けて再実行してください（プレビューには --dry-run）。
cli-alias-warn-workspace-archive = 警告: ワークスペースのアーカイブに失敗しました: {$error}
cli-alias-owned-cascaded = 所有状態をカスケードしました: memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions} → {$archive}
cli-alias-owned-repointed = 所有状態を再ポイントしました: memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions}
cli-alias-warn-workspace-move = 警告: ワークスペースの移動に失敗しました: {$error}
cli-alias-warn = 警告: {$warning}
cli-alias-deleted = {$section}.{$alias} を削除しました（{$count} 件の参照を除去しました）
cli-alias-delete-refused-header = 拒否されました: {$count} 件のハード参照が削除をブロックしています:
cli-alias-delete-refused-hint = 削除が拒否されました — 先にハード参照を解決してください
cli-alias-not-configured = {$path} は設定されていません
cli-alias-delete-failed = 削除に失敗しました: {$error}
cli-alias-delete-reserved-default = `default` エージェントは予約されており、削除できません
cli-alias-renamed = {$section}.{$from} → {$section}.{$to} にリネームしました（{$count} 件の参照パスを書き換えました）
cli-alias-rename-invalid = 無効な新しいエイリアス: {$message}
cli-alias-rename-reserved = エイリアス `{$alias}` は予約されており、リネームできません
cli-alias-rename-postcondition = リネームカスケードの事後条件が失敗しました: {$message}
cli-alias-unknown-provider-category = 不明なプロバイダーカテゴリ `{$category}`（期待される値: models | tts | transcription）
cli-alias-no-such-section = そのような設定セクションはありません: {$section}
cli-alias-live-acp-sessions = `{$alias}` のアクティブな ACP セッションが {$count} 件あります — 先に終了してください
cli-alias-owned-state-unavailable = 注意: 設定参照は更新されましたが、エージェントの所有状態（memory 行、ワークスペースディレクトリ、cron/acp/session 行）はこの CLI によってまだカスケードされていません — 完全な所有状態カスケードにはゲートウェイ API を使用してください。
cli-bundle-not-configured = スキルバンドル '{$alias}' は設定されていません
cli-bundle-rename-failed = リネームに失敗しました: {$error}
cli-bundle-exists = スキルバンドル '{$alias}' は既に存在します（変更なし）
cli-bundle-created = skill_bundles.{$alias} を作成しました（dir: {$dir}）
cli-bundle-created-warn = skill_bundles.{$alias} を作成しました（警告: dir の解決に失敗しました: {$error}）
cli-bundle-impact-header = skill_bundles.{$alias} を削除すると {$count} 件のエージェント参照から除去されます:
cli-bundle-no-changes = 変更は行われませんでした。適用するには --yes を付けて再実行してください。
cli-bundle-archived = バンドルディレクトリをアーカイブしました → {$path}
cli-bundle-warn-archive = 警告: バンドルディレクトリのアーカイブに失敗しました: {$error}
cli-bundle-deleted = skill_bundles.{$alias} を削除しました（{$count} 件のエージェントから除去しました）
cli-bundle-warn-move = 警告: バンドルディレクトリの移動に失敗しました: {$error}
cli-bundle-renamed = skill_bundles.{$from} → skill_bundles.{$to} にリネームしました

# ── Context window (doctor update-context-windows, agent interactive) ──
cli-agent-context-bar = ctx: {used} / {max}  {bar}  {pct}%
cli-agent-context-bar-unknown = ctx: 不明 / {max}
cli-doctor-ctxwin-already-set = {provider_ref}: 既に context_window = {ctx} が設定されています
cli-doctor-ctxwin-no-model = {provider_ref}: モデルが未設定のためスキップ
cli-doctor-ctxwin-would-set = {provider_ref}: context_window = {ctx} を設定します (ドライラン)
cli-doctor-ctxwin-set = {provider_ref}: context_window = {ctx} を設定しました
cli-doctor-ctxwin-not-found = {provider_ref}: 更新対象のエントリが見つかりません
cli-doctor-ctxwin-fetch-failed = {provider_ref}: コンテキストウィンドウを公開していないか、取得に失敗しました
cli-doctor-ctxwin-saved = config.toml に {updated} 件の更新を保存しました
cli-doctor-ctxwin-dry-run = ドライラン完了 — 変更は書き込まれません。--dry-run なしで実行して適用してください。
cli-doctor-ctxwin-none = 更新は必要ありません。
cli-onboard-about = ワークスペースと設定を初期化
