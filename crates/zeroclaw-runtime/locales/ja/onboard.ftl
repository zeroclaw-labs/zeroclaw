# オンボーディングフロー — インターフェースに依存しない文字列。
#
# すべてのインターフェース（CLI、RPC、ウェブ）に提供されます。フローは
# メッセージ ID と引数をデータとして運び、利用する側がアクティブな
# ロケールに従って解決します。

# ロケールセレクター — フローの最初のステップ。
onboard-flow-locale-prompt = 言語を選択してください
onboard-flow-locale-confirmed = 言語を {$label} に設定しました。

# セクションウォークの結果。
onboard-flow-completed = {$items} を設定しました。
onboard-flow-cancelled = オンボーディングをキャンセルしました。変更はありません。
onboard-flow-failed = {$layer}:{$instance} を設定できませんでした: {$reason}

# エラー。
onboard-flow-no-fields = セクション {$section} に設定可能なフィールドがありません。
