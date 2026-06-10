#!/bin/sh
#
# Launcher for the zeroclaw FreeBSD rc.d service.
#
# daemon(8) starts its child with a minimal environment, so we export a full
# PATH here: FreeBSD keeps git, python3, etc. under /usr/local/bin, which is NOT
# on the default service PATH.
#
# The rc.d script runs this through `daemon -u <user>`, so this launcher already
# executes AS the service account. HOME is taken from that account's passwd
# entry (via tilde expansion) rather than hardcoded to /home/<user>, so accounts
# whose home is elsewhere — and rc.conf run-as overrides — work unchanged.
#
# Install as /usr/local/libexec/zeroclaw-run.sh (see dist/freebsd/README.md).

: "${HOME:=$(eval echo ~)}"
export HOME
export PATH="/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:${HOME}/bin"
exec /usr/local/bin/zeroclaw daemon --config-dir "${HOME}/.zeroclaw"
