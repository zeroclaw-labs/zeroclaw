tool-backup = ワークスペースバックアップの作成、一覧表示、検証、復元
tool-browser = プラグイン可能なバックエンド（agent-browser、rust-native、computer_use）を使用したWeb/ブラウザオートメーション。DOMアクションに加えて、オプションのOSレベルアクション（mouse_move、mouse_click、mouse_drag、key_type、key_press、screen_capture）をコンピュータユースサイドカー経由でサポート。'snapshot'を使用して対話的要素をref（@e1、@e2）にマップします。openアクション向けにbrowser.allowed_domainsを強制します。
tool-browser-delegate = ブラウザ対応CLIへのブラウザベースのタスクの委譲。Teams、Outlook、Jira、Confluenceなどのウェブアプリケーションと相互作用するため
tool-browser-open = 承認されたHTTPS URLをシステムブラウザで開く。セキュリティ制約：許可リストのみのドメイン、ローカル/プライベートホストなし、スクレイピングなし。
tool-cloud-ops = クラウド変換アドバイザリーツール。IaCプランを分析し、マイグレーションパスを評価し、コストをレビューし、Well-Architected Frameworkの柱に対してアーキテクチャをチェックします。読み取り専用：クラウドリソースを作成または変更しません。
tool-cloud-patterns = クラウドパターンライブラリ。ワークロード説明を指定すると、適用可能なクラウドネイティブアーキテクチャパターン（コンテナ化、サーバーレス、データベース現代化など）を提案します。
tool-composio = Composio経由で1000以上のアプリ（Gmail、Notion、GitHub、Slack等）でアクションを実行します。action='list'で利用可能なアクション（パラメータ名を含む）を確認します。action='execute'でaction_name/tool_slugとparamsを指定して実行します。正確なparamsが不確実な場合は、'text'に自然言語の説明を記述してください（Composioが正しいパラメータをNLPで解決します）。action='list_accounts'またはaction='connected_accounts'でOAuth接続アカウントを一覧表示します。action='connect'でapp/auth_config_idを指定するとOAuth URLが取得できます。connected_account_idは省略すると自動解決されます。
tool-content-search = ワークスペース内のregexパターンでファイルコンテンツを検索します。ripgrep（rg）をサポート、フォールバックとしてgrepを使用。出力モード：'content'（マッチ行とコンテキスト）、'files_with_matches'（ファイルパスのみ）、'count'（ファイルごとのマッチ数）。例：pattern='fn main'、include='*.rs'、output_mode='content'。
tool-cron-add = cron/at/everyスケジュール付きのスケジュール済みcronジョブ（シェルまたはエージェント）を作成します。job_type='agent'でPromptを使用してAIエージェントをスケジュール実行します。出力をチャネル（Discord、Telegram、Slack、Mattermost、Matrix）に配信するには、delivery={"mode":"announce","channel":"discord","to":"<channel_id_or_chat_id>"}を設定します。これは、チャネル経由でユーザーにスケジュール/遅延メッセージを送信するための推奨ツールです。
tool-cron-list = すべてのスケジュール済みcronジョブを一覧表示
tool-cron-remove = IDでcronジョブを削除
tool-cron-run = cronジョブを即座に強制実行し、実行履歴を記録
tool-cron-runs = cronジョブの最近の実行履歴を一覧表示
tool-cron-update = 既存のcronジョブにパッチを適用（スケジュール、コマンド、プロンプト、有効、配信、モデル等）
tool-data-management = ワークスペースデータ保持、削除、ストレージ統計
tool-delegate = 特殊なエージェントへの小タスクの委譲。用途：異なるモデルから利益を得られるタスク（例：高速要約、深い推論、コード生成）。サブエージェントはデフォルトで単一のプロンプトを実行します。agentic=trueでは、フィルタ済みツール呼び出しループで反復できます。
tool-file-edit = 完全一致する文字列を新しいコンテンツに置き換えてファイルを編集
tool-file-read = 行番号付きのファイルコンテンツを読み込み。offsetとlimitによる部分読み込みをサポート。PDFからテキストを抽出します。その他のバイナリファイルはロッシーUTF-8変換で読み込まれます。
tool-file-write = ワークスペース内のファイルにコンテンツを書き込み
tool-git-operations = 構造化されたGit操作（status、diff、log、branch、commit、add、checkout、stash）を実行。解析されたJSON出力を提供し、自律性制御のためのセキュリティポリシーと統合します。
tool-glob-search = ワークスペース内でglobパターンにマッチするファイルを検索。ワークスペースルートに対する相対パスの、ソート済みマッチングファイルパスのリストを返します。例：'**/*.rs'（すべてのRustファイル）、'src/**/mod.rs'（src内のすべてのmod.rs）。
tool-google-workspace = gws CLIを経由してGoogle Workspaceサービス（Drive、Gmail、Calendar、Sheets、Docs等）と相互作用します。gwsがインストール済みで認証されている必要があります。
tool-hardware-board-info = 接続されたハードウェアの完全なボード情報（チップ、アーキテクチャ、メモリマップ）を返す。用途：ユーザーが「board info」、「what board do I have」、「connected hardware」、「chip info」、「what hardware」、または「memory map」を尋ねる場合。
tool-hardware-memory-map = 接続されたハードウェアのメモリマップ（フラッシュとRAMアドレス範囲）を返す。用途：ユーザーが「upper and lower memory addresses」、「memory map」、「address space」、または「readable addresses」を尋ねる場合。データシートからフラッシュ/RAMの範囲を返します。
tool-hardware-memory-read = USBを経由してNucleoから実際のメモリ/レジスタ値を読み込む。用途：ユーザーが「read register values」、「read memory at address」、「dump memory」、「lower memory 0-126」、または「give address and value」を尋ねる場合。16進ダンプを返します。NucleoがUSBに接続されている必要があり、probeフィーチャが必要です。パラメータ：address（16進、例：RAMスタート用の0x20000000）、length（バイト、デフォルト128）。
tool-http-request = 外部APIにHTTPリクエストを送信します。GET、POST、PUT、DELETE、PATCH、HEAD、OPTIONSメソッドをサポート。セキュリティ制約：許可リストのみのドメイン、ローカル/プライベートホストなし、設定可能なタイムアウトとレスポンスサイズ制限。
tool-image-info = イメージファイルメタデータ（フォーマット、寸法、サイズ）を読み込み、オプションで基数64エンコード済みデータを返す。
tool-jira = Jiraと相互作用：設定可能な詳細レベルでチケットを取得、JQLで問題を検索、メンション書式付きコメントを追加。
tool-knowledge = アーキテクチャ決定、ソリューションパターン、経験、専門家の知識グラフを管理します。アクション：capture、search、relate、suggest、expert_find、lessons_extract、graph_stats。
tool-linkedin = LinkedInを管理：投稿作成、投稿一覧、コメント、リアクション、投稿削除、エンゲージメント表示、プロフィール情報取得、設定されたコンテンツ戦略の読み込み。.envファイルにLINKEDIN_*認証情報が必要。
tool-discord-search = discord.dbに保存されたDiscordメッセージ履歴を検索します。過去のメッセージを検索したり、チャネルアクティビティを要約したり、ユーザーが何を言ったかを調べるために使用します。キーワード検索とオプショナルフィルタをサポート：channel_id、since、until。
tool-memory-forget = キーでメモリを削除します。古い情報や機密データを削除する場合に使用。メモリが見つかったかどうか、削除されたかどうかを返します。
tool-memory-recall = 長期記憶で関連する情報、好み、またはコンテキストを検索します。関連性でランク付けされたスコア結果を返します。クエリを省略するか * のみを指定すると、最近の記憶を返します。
tool-memory-store = 事実、好み、またはノートを長期記憶に保存します。永続的な事実にはカテゴリ「core」を、セッションノートには「daily」、チャットコンテキストには「conversation」、またはカスタムカテゴリ名を使用します。
tool-microsoft365 = Microsoft 365統合：Microsoft Graph APIを経由してOutlookメール、Teamsメッセージ、カレンダーイベント、OneDriveファイル、SharePoint検索を管理
tool-model-routing-config = デフォルトモデル設定、シナリオベースのプロバイダー/モデルルート、分類ルール、委譲サブエージェントプロフィールを管理
tool-notion = Notionと相互作用：データベースをクエリ、ページを読み込み/作成/更新、ワークスペースを検索。
tool-pdf-read = ワークスペース内のPDFファイルから平文テキストを抽出します。読み取り可能なすべてのテキストを返します。画像のみまたは暗号化されたPDFは空の結果を返します。'rag-pdf'ビルドフィーチャが必要。
tool-project-intel = プロジェクト配信インテリジェンス：ステータスレポートを生成、リスクを検出、クライアント更新をドラフト、スプリントを要約、作業量を推定。読み取り専用分析ツール。
tool-proxy-config = ZeroClawプロキシ設定を管理（スコープ：environment | zeroclaw | services）。ランタイムおよびプロセス環境アプリケーション含む
tool-pushover = Pushover通知をデバイスに送信します。.envファイルにPUSHOVER_TOKENおよびPUSHOVER_USER_KEYが必要。
tool-schedule = スケジュール済みシェルのみのタスクを管理します。アクション：create/add/once/list/get/cancel/remove/pause/resume。警告：このツールは、出力がログに記録されるのみで、チャネルに配信されないシェルジョブを作成します。Discord/Telegram/Slack/Matrixにスケジュール済みメッセージを送信するには、job_type='agent'とdelivery配信設定（例：{"mode":"announce","channel":"discord","to":"<channel_id>"}）付きのcron_addツールを使用してください。
tool-screenshot = 現在の画面のスクリーンショットをキャプチャします。ファイルパスと基数64エンコード済みPNGデータを返します。
tool-security-ops = 管理型サイバーセキュリティサービス向けセキュリティ操作ツール。アクション：triage_alert（アラートの分類/優先順位付け）、run_playbook（インシデント対応ステップの実行）、parse_vulnerability（スキャン結果の解析）、generate_report（セキュリティ態勢レポートの作成）、list_playbooks（利用可能なプレイブックをリスト）、alert_stats（アラートメトリクスを要約）。
tool-shell = ワークスペースディレクトリ内でシェルコマンドを実行
tool-sop-advance = 現在のSOPステップの結果を報告し、次のステップに進む。run_id、ステップが成功したか失敗したか、簡潔な出力要約を指定します。
tool-sop-approve = オペレータ承認を待つ保留中のSOPステップを承認します。実行するステップ命令を返します。sop_statusを使用して、どの実行が待機中かを確認します。
tool-sop-execute = Standard Operating Procedure（SOP）を名前で手動トリガーします。実行IDと最初のステップ命令を返します。sop_listを使用して、利用可能なSOPを確認します。
tool-sop-list = ロードされたすべてのStandard Operating Procedure（SOP）をトリガー、優先度、ステップ数、アクティブ実行数と共に一覧表示します。オプションで名前または優先度でフィルタ。
tool-sop-status = SOP実行状態をクエリします。特定の実行にはrun_idを、そのSOPの実行をリストするにはsop_nameを指定してください。引数がない場合、すべてのアクティブな実行を表示します。
tool-tool-search = 遅延MCPツールの完全なスキーマ定義を取得して、呼び出すことができます。"select:name1,name2"で完全一致を指定するか、キーワードで検索します。
tool-web-fetch = ウェブページを取得してその内容をクリーンなプレーンテキストとして返します。HTMLページは自動的に読みやすいテキストに変換されます。JSONおよびプレーンテキストのレスポンスはそのまま返されます。GETリクエストのみ。リダイレクトに従います。セキュリティ：ホワイトリストオンリードメイン、ローカル/プライベートホストなし。
tool-web-search-tool = ウェブで情報を検索します。タイトル、URL、説明付きの関連検索結果を返します。最新情報、ニュース、または研究トピックを見つけるために使用します。
tool-workspace = マルチクライアントワークスペースを管理します。サブコマンド：list、switch、create、info、export。各ワークスペースは分離されたメモリ、監査、シークレット、ツール制限を提供します。
tool-weather = 世界中のあらゆる場所の現在の天気条件と予報を取得します。都市名（任意の言語またはスクリプト）、IAIAエアポートコード（例：'LAX'）、GPS座標（例：'51.5,-0.1'）、郵便番号、ドメインベースのジオロケーションをサポートしています。気温、体感温度、湿度、風速/風向、降水量、視程、気圧、紫外線指数、雲量を返します。オプションで0～3日間の予報と時間単位の詳細データがあります。単位はデフォルトでメートル法（°C、km/h、mm）ですが、リクエストに応じてヤード・ポンド法（°F、mph、インチ）に設定できます。APIキーは不要です。
