#!/bin/sh
# Simulate a daemon resource independently of the CLI's lifetime. No Docker
# daemon, global environment changes, or executable rewrites are needed.
set -eu
state=${ZEROCLAW_DOCKER_TEST_STATE:?}

if [ -f "$state/record-env" ]; then
    printf '%s\n%s\n%s\n' "${DOCKER_HOST-unset}" "${DOCKER_CONTEXT-unset}" \
        "${ZC_CLEANUP_DAEMON_ONLY-unset}" > "$state/$1-env"
fi

case "$1" in
    run)
        [ "$2" = "--name" ]
        name=$3
        printf '%s\n' "$$" > "$state/launcher-pid"
        if [ -f "$state/fail-launch" ]; then
            echo 'cannot start fixture container' >&2
            exit 125
        fi
        : > "$state/launcher-ready"
        while [ -f "$state/defer-create" ]; do /bin/sleep 0.02; done
        /bin/mkdir "$state/$name"
        printf '%s' "$name" > "$state/created"
        if [ -f "$state/succeed" ]; then
            /bin/rmdir "$state/$name"
            echo completed
            echo notice >&2
            exit 0
        fi
        while [ -d "$state/$name" ]; do /bin/sleep 0.02; done
        exit 137
        ;;
    rm)
        [ "$2" = "--force" ]
        [ "$3" = "--" ]
        name=$4
        printf '%s' "$name" > "$state/cleanup-target"
        if [ -f "$state/fail-cleanup" ]; then
            echo 'daemon unavailable' >&2
            exit 1
        fi
        while [ -f "$state/hang-cleanup" ]; do /bin/sleep 0.02; done
        if [ -d "$state/$name" ]; then
            /bin/rmdir "$state/$name"
            exit 0
        fi
        if [ -f "$state/create-after-miss" ]; then
            # This response observed absence, but creation and launcher exit
            # happen before the caller receives it. That absence is stale.
            /bin/rm "$state/create-after-miss"
            /bin/mkdir "$state/$name"
            printf '%s' "$name" > "$state/created"
            read -r launcher < "$state/launcher-pid"
            /bin/kill -KILL "$launcher"
            /bin/sleep 0.05
        fi
        echo "Error response from daemon: No such container: $name" >&2
        exit 1
        ;;
    *) exit 2 ;;
esac
