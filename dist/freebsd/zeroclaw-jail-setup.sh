#!/bin/sh
#
# Provision a thick FreeBSD jail for ZeroClaw end to end: create the dataset,
# extract a matching base system, register the jail, start it, and install the
# launcher + hardened rc.d script inside it. Installing the zeroclaw binary
# itself is left to you (pkg, or a build copied in) — see the printed next steps.
#
# Run on the HOST as root (or via doas/sudo). It validates its inputs and
# refuses to clobber an existing jail path or an existing jail.conf entry. If a
# run fails partway, it tells you what to remove before retrying.
#
#   doas sh dist/freebsd/zeroclaw-jail-setup.sh
#
# Override the defaults with environment variables, e.g.:
#   JAIL_NAME=zc JAIL_PATH=/jails/zc ZPOOL=tank ZEROCLAW_USER=agent \
#       doas sh dist/freebsd/zeroclaw-jail-setup.sh
#
# See docs/book/src/setup/freebsd.md ("Running in a jail") for the manual steps.

set -eu

JAIL_NAME="${JAIL_NAME:-zeroclaw}"
JAIL_PATH="${JAIL_PATH:-/jails/${JAIL_NAME}}"
ZEROCLAW_USER="${ZEROCLAW_USER:-zeroclaw}"
# ZPOOL: if set, a ZFS dataset is created at <ZPOOL>/jails/<JAIL_NAME>.
# Leave empty to use a plain directory (UFS, or a pre-existing dataset).
ZPOOL="${ZPOOL:-}"

script_dir=$(cd "$(dirname "$0")" && pwd)
launcher_src="${script_dir}/zeroclaw-run.sh"
rcd_src="${script_dir}/zeroclaw-hardened.rc"

base_tmp=""
completed=0
err() { echo "zeroclaw-jail-setup: $*" >&2; exit 1; }
cleanup() {
    [ -n "${base_tmp}" ] && rm -f "${base_tmp}" 2>/dev/null || :
    if [ "${completed}" -ne 1 ]; then
        echo "zeroclaw-jail-setup: did not complete. If a partial jail was" \
             "created at ${JAIL_PATH}, remove it (and any '${JAIL_NAME}'" \
             "jail.conf entry) before retrying." >&2
    fi
}
trap cleanup EXIT INT TERM

# --- 0. Preflight + input validation -----------------------------------------
# These values are written into /etc/jail.conf, rc.conf, and a sed replacement,
# so anything outside a conservative allowlist could corrupt config or inject
# parameters. Validate everything before mutating any host state.
[ "$(id -u)" -eq 0 ] || err "must run as root (try: doas sh $0)"

# Disallow a leading '-' so the name can never be mistaken for an option by
# jexec(8)/service(8) etc.
case "${JAIL_NAME}" in
    ''|[!A-Za-z0-9_]*|*[!A-Za-z0-9_-]*)
        err "JAIL_NAME must match [A-Za-z0-9_][A-Za-z0-9_-]* (got: '${JAIL_NAME}')" ;;
esac
case "${ZEROCLAW_USER}" in
    ''|[!A-Za-z_]*|*[!A-Za-z0-9_-]*)
        err "ZEROCLAW_USER must be a valid username [A-Za-z_][A-Za-z0-9_-]* (got: '${ZEROCLAW_USER}')" ;;
esac
case "${JAIL_PATH}" in
    /*) : ;;
    *) err "JAIL_PATH must be an absolute path (got: '${JAIL_PATH}')" ;;
esac
case "${JAIL_PATH}" in
    *[!A-Za-z0-9_/.-]*) err "JAIL_PATH contains unsafe characters (got: '${JAIL_PATH}')" ;;
esac
if [ -n "${ZPOOL}" ]; then
    case "${ZPOOL}" in
        *[!A-Za-z0-9_/.-]*) err "ZPOOL contains unsafe characters (got: '${ZPOOL}')" ;;
    esac
fi

[ -f "${launcher_src}" ] || err "missing ${launcher_src}"
[ -f "${rcd_src}" ] || err "missing ${rcd_src}"
for _cmd in jail jexec sysrc fetch tar install sed mktemp freebsd-version; do
    command -v "${_cmd}" >/dev/null 2>&1 || err "required command not found: ${_cmd} (is this FreeBSD?)"
done
[ -z "${ZPOOL}" ] || command -v zfs >/dev/null 2>&1 || err "ZPOOL set but zfs(8) not found"

# Refuse to populate anything that already exists — a symlink, a file, or any
# pre-existing directory (even an empty one, which could be a sensitive mount
# point). The jail root must be a fresh path this script creates itself.
if [ -e "${JAIL_PATH}" ] || [ -L "${JAIL_PATH}" ]; then
    err "${JAIL_PATH} already exists; refusing to populate it (remove it or pick a fresh JAIL_PATH)"
fi

# --- 1. Create the jail root --------------------------------------------------
if [ -n "${ZPOOL}" ]; then
    echo "==> Creating ZFS dataset ${ZPOOL}/jails/${JAIL_NAME}"
    zfs create -p -o mountpoint="${JAIL_PATH}" "${ZPOOL}/jails/${JAIL_NAME}"
else
    echo "==> Creating directory ${JAIL_PATH}"
    mkdir -p "${JAIL_PATH}"
fi

# --- 2. Extract a base system matching the host release -----------------------
arch=$(uname -m)
release=$(freebsd-version -u | sed 's/-p[0-9]*$//')
base_url="https://download.freebsd.org/releases/${arch}/${release}/base.txz"
base_tmp=$(mktemp -t zeroclaw-jail-base) || err "mktemp failed"
echo "==> Fetching base.txz for ${arch} ${release}"
fetch -o "${base_tmp}" "${base_url}"
echo "==> Extracting base into ${JAIL_PATH}"
tar -xpf "${base_tmp}" -C "${JAIL_PATH}"
cp /etc/resolv.conf "${JAIL_PATH}/etc/resolv.conf"

# --- 3. Register the jail in /etc/jail.conf -----------------------------------
[ -f /etc/jail.conf ] || : >/etc/jail.conf
# JAIL_NAME is allowlisted above, so it is safe as both a literal and an ERE here.
if grep -qE "^[[:space:]]*${JAIL_NAME}[[:space:]]*\{" /etc/jail.conf; then
    echo "==> /etc/jail.conf already has a '${JAIL_NAME}' entry; leaving it untouched"
else
    echo "==> Appending '${JAIL_NAME}' entry to /etc/jail.conf"
    cat >>/etc/jail.conf <<EOF

${JAIL_NAME} {
    host.hostname = "${JAIL_NAME}";
    path = "${JAIL_PATH}";
    exec.start = "/bin/sh /etc/rc";
    exec.stop  = "/bin/sh /etc/rc.shutdown";
    exec.clean;
    mount.devfs;
    devfs_ruleset = 4;          # devfsrules_jail: restrict device nodes
    persist;
}
EOF
fi

sysrc jail_enable=YES >/dev/null
# Append to jail_list only if not already present (idempotent across retries).
current_list=$(sysrc -n jail_list 2>/dev/null || echo "")
case " ${current_list} " in
    *" ${JAIL_NAME} "*) echo "==> jail_list already contains ${JAIL_NAME}" ;;
    *) sysrc "jail_list+=${JAIL_NAME}" >/dev/null ;;
esac

# --- 4. Start the jail --------------------------------------------------------
echo "==> Starting jail ${JAIL_NAME}"
service jail start "${JAIL_NAME}"

# --- 5. Create the service account + install the service files inside ---------
echo "==> Creating service account '${ZEROCLAW_USER}' inside the jail"
if ! jexec "${JAIL_NAME}" pw usershow "${ZEROCLAW_USER}" >/dev/null 2>&1; then
    jexec "${JAIL_NAME}" pw useradd "${ZEROCLAW_USER}" -m -s /usr/sbin/nologin
fi

echo "==> Installing launcher + hardened rc.d into the jail"
install -d "${JAIL_PATH}/usr/local/libexec"
install -d "${JAIL_PATH}/usr/local/etc/rc.d"
install -m 755 "${launcher_src}" "${JAIL_PATH}/usr/local/libexec/zeroclaw-run.sh"
# ZEROCLAW_USER is allowlisted above (no /, &, backslash, or newline), so it is
# a safe sed replacement.
sed "s/@@ZEROCLAW_USER@@/${ZEROCLAW_USER}/g" "${rcd_src}" \
    >"${JAIL_PATH}/usr/local/etc/rc.d/zeroclaw"
chmod 755 "${JAIL_PATH}/usr/local/etc/rc.d/zeroclaw"
jexec "${JAIL_NAME}" sysrc zeroclaw_enable=YES >/dev/null

completed=1
cat <<EOF

Jail '${JAIL_NAME}' is up at ${JAIL_PATH} with the ZeroClaw service files in place.

Next steps (run inside the jail):

  # 1. Install the zeroclaw binary — either from a package mirror that carries
  #    it, or build it on the host and copy it in. To build inside the jail:
  doas jexec ${JAIL_NAME} pkg install -y rust git
  doas jexec ${JAIL_NAME} /bin/sh -c 'cd /root && git clone \\
      https://github.com/zeroclaw-labs/zeroclaw && cd zeroclaw && \\
      cargo build --release && install -m 755 target/release/zeroclaw \\
      /usr/local/bin/zeroclaw'

  # 2. Set up provider auth for the '${ZEROCLAW_USER}' account, then start it:
  doas jexec ${JAIL_NAME} service zeroclaw start
  doas jexec ${JAIL_NAME} service zeroclaw status

See docs/book/src/setup/freebsd.md for auth, logs, and the gateway bind note.
EOF
