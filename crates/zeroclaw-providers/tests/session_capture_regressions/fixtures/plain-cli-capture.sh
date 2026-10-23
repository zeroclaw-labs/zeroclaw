#!/bin/sh
set -eu
fixture_root=$(dirname "$0")
printf '%s\n' "$$" > "$fixture_root/pid"
IFS= read -r mode < "$fixture_root/mode"
if [ "$mode" = early_exit ]; then
    printf 'CLI_PARTIAL'
    exit 7
fi
if [ "$mode" = blocked_stdin ]; then
    head -c 131072 /dev/zero | tr '\000' x
fi
cat >/dev/null
printf 'CLI_PARTIAL'
case "$mode" in
    fail)
        printf 'fixture process error api_key=fixture-secret' >&2
        exit 7
        ;;
    signal)
        kill -TERM "$$"
        ;;
    invalid_utf8)
        printf '\377'
        ;;
    truncated_utf8)
        printf '\344\270'
        ;;
    large_stderr)
        head -c 1048576 /dev/zero | tr '\000' x >&2
        exit 9
        ;;
    live)
        while [ ! -f "$fixture_root/release" ]; do sleep 0.01; done
        ;;
    split_credentials)
        printf ' api_'
        sleep 0.05
        printf 'key="fixture'
        sleep 0.05
        printf '%s' '-secret" http://user:fixture'
        sleep 0.05
        printf '%s' '-secret@example.invalid/path '
        printf 'sk'
        sleep 0.05
        printf '%s' '-fixture-secret '
        ;;
    sensitive_context_limit)
        printf ' api_key="'
        head -c 73728 /dev/zero | tr '\000' x
        exec sleep 60
        ;;
esac
printf ' \344\270\255\346\226\207 CLI_FINAL\n'
