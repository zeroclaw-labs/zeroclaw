#!/usr/bin/env python3
"""Opt-in Linux/ext4 VM power-loss proof for native session-event stores.

Build the infra test executable first, then pass it with --binary. Requires
QEMU, cpio, mkfs.ext4, a matching Linux kernel with built-in virtio-blk/ext4,
and busybox. No root access, guest networking, host mounts or KVM is needed.
Use --work-dir on the filesystem whose durability boundary is being tested.
"""

import argparse
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import tempfile
import time


CASES = (
    ("sqlite", "sqlite_before_commit", False),
    ("sqlite", "sqlite_after_commit", True),
    ("jsonl", "jsonl_segment_published", False),
    ("jsonl", "jsonl_commit_published", False),
    ("jsonl", "jsonl_root_published", True),
)


def checked(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def install_binary(source, destination, root):
    target = root / destination.lstrip("/")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)
    # Guest PID 1 already runs as root. Do not copy host setuid/setgid bits
    # whose owner in this unprivileged archive would be the host user.
    target.chmod(0o755)
    checked(["strip", str(target)], stdout=subprocess.DEVNULL)
    dependencies = checked(["ldd", str(source)], capture_output=True, text=True).stdout
    for line in dependencies.splitlines():
        parts = line.strip().split()
        library = next((part for part in parts if part.startswith("/")), None)
        if library:
            output = root / library.lstrip("/")
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(library, output)


def initramfs(directory, binary, busybox):
    root = directory / "initramfs"
    for name in ("bin", "dev", "proc", "sys", "data", "tmp"):
        (root / name).mkdir(parents=True, exist_ok=True)
    install_binary(busybox, "/bin/busybox", root)
    # Distribution initcpio busybox builds can omit the mount applet.
    mount = shutil.which("mount")
    if mount is None:
        raise RuntimeError("util-linux mount is required")
    install_binary(Path(mount), "/bin/mount", root)
    install_binary(binary, "/bin/session-tests", root)
    (root / "bin/sh").symlink_to("busybox")
    init = root / "init"
    init.write_text('''#!/bin/sh
/bin/mount -t devtmpfs devtmpfs /dev
/bin/mount -t proc proc /proc
/bin/mount -t sysfs sysfs /sys
for argument in $(/bin/busybox cat /proc/cmdline); do
    case "$argument" in
        zc_stage=*) export ZEROCLAW_SESSION_POWER_LOSS_STAGE="${argument#*=}" ;;
        zc_backend=*) export ZEROCLAW_SESSION_CRASH_BACKEND="${argument#*=}" ;;
        zc_point=*) export ZEROCLAW_SESSION_CRASH_POINT="${argument#*=}" ;;
        zc_visible=*) export ZEROCLAW_SESSION_EXPECT_VISIBLE="${argument#*=}" ;;
    esac
done
if [ "$ZEROCLAW_SESSION_POWER_LOSS_STAGE" != append ]; then
    unset ZEROCLAW_SESSION_CRASH_POINT
fi
if ! /bin/mount -t ext4 /dev/vda /data; then
    echo ZEROCLAW_POWER_LOSS_GUEST_FAILED=mount
    /bin/busybox poweroff -f
fi
export ZEROCLAW_SESSION_CRASH_DIRECTORY=/data/fixture
export ZEROCLAW_SESSION_CRASH_ACTION=power_loss
/bin/session-tests --exact session_events::crash_tests::power_loss_worker --nocapture
result=$?
if [ "$result" -ne 0 ]; then echo ZEROCLAW_POWER_LOSS_GUEST_FAILED=test; fi
/bin/busybox sync
/bin/busybox poweroff -f
''')
    init.chmod(0o755)
    names = "\n".join(str(p.relative_to(root)) for p in sorted(root.rglob("*"))) + "\n"
    archive = directory / "initramfs.cpio"
    with archive.open("wb") as output:
        checked(["cpio", "-o", "-H", "newc"], cwd=root, input=names.encode(), stdout=output, stderr=subprocess.PIPE)
    return archive


def boot(kernel, archive, image, stage, backend, point, visible, logfile, timeout_secs):
    cmdline = (
        "console=ttyS0 rdinit=/init panic=-1 quiet noapic "
        f"zc_stage={stage} zc_backend={backend} zc_point={point} "
        f"zc_visible={str(visible).lower()}"
    )
    command = [
        "qemu-system-x86_64", "-accel", "tcg", "-m", "768", "-smp", "1",
        "-kernel", str(kernel), "-initrd", str(archive), "-append", cmdline,
        "-drive", f"file={image},format=raw,if=virtio,cache=none",
        "-nic", "none", "-display", "none", "-serial", "stdio",
        "-monitor", "none", "-no-reboot",
    ]
    marker = (f"ZEROCLAW_POWER_LOSS_BOUNDARY={point}" if stage == "append"
              else f"ZEROCLAW_POWER_LOSS_STAGE_OK={stage}").encode()
    output = bytearray()
    killed_at_boundary = False
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + timeout_secs
            while process.poll() is None:
                if time.monotonic() > deadline:
                    raise TimeoutError(f"guest deadline: {backend}/{point}/{stage}")
                if not selector.select(timeout=1):
                    continue
                chunk = os.read(process.stdout.fileno(), 65536)
                output.extend(chunk)
                logfile.write_bytes(output)
                if stage == "append" and marker in output:
                    # No guest sync/unmount/shutdown: discard kernel dirty pages.
                    process.kill()
                    killed_at_boundary = True
                    break
        tail, _ = process.communicate(timeout=10)
        output.extend(tail)
        if marker not in output or b"ZEROCLAW_POWER_LOSS_GUEST_FAILED=" in output:
            raise RuntimeError(f"guest proof failed: {backend}/{point}/{stage}; see {logfile}")
        if stage == "append":
            assert killed_at_boundary and process.returncode == -9
        elif process.returncode != 0:
            raise RuntimeError(f"unexpected QEMU exit {process.returncode}; see {logfile}")
    finally:
        if process.poll() is None:
            process.kill()
            tail, _ = process.communicate()
            output.extend(tail)
        logfile.write_bytes(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--busybox", type=Path, default=Path("/usr/lib/initcpio/busybox"))
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--guest-timeout-secs", type=int, default=180,
                        help="Per-boot deadline; increase on heavily loaded hosts")
    args = parser.parse_args()
    if args.guest_timeout_secs <= 0:
        parser.error("--guest-timeout-secs must be positive")
    args.work_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="session-power-loss-", dir=args.work_dir) as temporary:
        directory = Path(temporary)
        archive = initramfs(directory, args.binary.resolve(), args.busybox.resolve())
        for backend, point, visible in CASES:
            image = directory / f"{point}.ext4"
            with image.open("wb") as output:
                output.truncate(256 * 1024 * 1024)
            checked(["mkfs.ext4", "-q", "-F", str(image)])
            for stage in ("prepare", "append", "recover"):
                logfile = args.work_dir / f"{point}-{stage}.log"
                boot(args.kernel, archive, image, stage, backend, point, visible, logfile, args.guest_timeout_secs)
            print(f"PASS {backend}/{point}: visible={visible}, atomic events/payload/receipt, retry deduplicated", flush=True)
    print("PASS all 5 VM interruption boundaries (Linux/ext4, QEMU cache=none)")


if __name__ == "__main__":
    main()
