#!/usr/bin/env bash
set -euo pipefail

# Test-only immutable command fixtures from the officially qualified old
# Stable producer (blob below). Neither production workflow consumes them.
# Full body hashes bind the reference without relying on depth-1 CI retaining
# old Git objects. Only the three qualified matrix substitutions follow.
archive_test_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
archive_baseline_blob="6ca82a0ffe0d3973dac3f8b4061403a5cb63e4cc"
archive_packager="$archive_test_root/scripts/ci/package_release_archive.sh"
archive_verifier="$archive_test_root/scripts/ci/verify_release_archive.py"

fail() {
  echo "archive baseline: $*" >&2
  exit 1
}

archive_python=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 \
    && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 8))' 2>/dev/null; then
    archive_python="$candidate"
    break
  fi
done
[[ -n "$archive_python" ]] || fail "Python 3.8 or newer is required"

legacy_unix_body="$(cat <<'OLD_UNIX'
mkdir -p staging/web
cp target/${{ matrix.target }}/release/${{ matrix.artifact }} staging/
extra=""
for b in zerocode zerorelay; do
  if [ -f target/${{ matrix.target }}/release/$b ]; then
    cp target/${{ matrix.target }}/release/$b staging/
    extra="$extra $b"
  fi
done
cp -r web/dist staging/web/dist
cd staging
tar czf ../zeroclaw-${{ matrix.target }}.${{ matrix.ext }} ${{ matrix.artifact }} $extra web/dist
OLD_UNIX
)"
legacy_windows_body="$(cat <<'OLD_WINDOWS'
mkdir -p staging/web
cp target/${{ matrix.target }}/release/${{ matrix.artifact }} staging/
extra=""
for b in zerocode.exe zerorelay.exe; do
  if [ -f target/${{ matrix.target }}/release/$b ]; then
    cp target/${{ matrix.target }}/release/$b staging/
    extra="$extra $b"
  fi
done
cp -r web/dist staging/web/dist
cd staging
7z a ../zeroclaw-${{ matrix.target }}.${{ matrix.ext }} ${{ matrix.artifact }} $extra web/dist
OLD_WINDOWS
)"
body_digest() {
  printf '%s\n' "$1" | "$archive_python" -c \
    'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}
[[ "$(body_digest "$legacy_unix_body")" == c6e48d5d8b2c40dbd8a818287ca64e111ba750896889e7629ff89bae96645779 ]] ||
  fail "immutable old Unix fixture drifted"
[[ "$(body_digest "$legacy_windows_body")" == e91bb9f7a6126f6d64cbdb2f68a0017445cacf3352316585f12503a36a11b2b0 ]] ||
  fail "immutable old Windows fixture drifted"

case "${RUNNER_OS:-$(uname -s)}" in
  Windows|MINGW*|MSYS*|CYGWIN*)
    package_step="Package (Windows)"
    package_ext="zip"
    main_name="zeroclaw.exe"
    code_name="zerocode.exe"
    relay_name="zerorelay.exe"
    archive_tool="7z"
    package_os="Windows"
    baseline_body="$legacy_windows_body"
    ;;
  Linux|Darwin|macOS)
    package_step="Package (Unix)"
    package_ext="tar.gz"
    main_name="zeroclaw"
    code_name="zerocode"
    relay_name="zerorelay"
    archive_tool="tar"
    package_os="Linux"
    baseline_body="$legacy_unix_body"
    ;;
  *) fail "unsupported baseline runner" ;;
esac
command -v "$archive_tool" >/dev/null || fail "$archive_tool is required"

# Only GitHub's three fixed matrix placeholders become fixture constants.
# Shell commands, member selection, copy order and archiver flags stay intact.
fixture_commands="$(
  printf '%s\n' "$baseline_body" |
    sed -e 's/[$][{][{] matrix.target [}][}]/fixture-target/g' \
      -e "s/[$][{][{] matrix.artifact [}][}]/$main_name/g" \
      -e "s/[$][{][{] matrix.ext [}][}]/$package_ext/g"
)"
[[ "$fixture_commands" != *'${{'* ]] || fail "unresolved matrix placeholder"

archive_test_tmp="$(mktemp -d "${TMPDIR:-/tmp}/zeroclaw-archive-baseline.XXXXXX")"
# This directory was created exclusively by this invocation; no repository or
# inherited staging directory is adopted or removed.
trap 'rm -rf -- "$archive_test_tmp"' EXIT
commands_file="$archive_test_tmp/package.sh"
printf '%s\n' "$fixture_commands" > "$commands_file"
case_count=0

seed_fixture() {
  local fixture="$1" companions="$2" omitted="${3:-none}"
  mkdir -p "$fixture/target/fixture-target/release"
  if [[ "$omitted" != main ]]; then
    printf 'dummy main payload\nnot executed\n' > "$fixture/target/fixture-target/release/$main_name"
    chmod 751 "$fixture/target/fixture-target/release/$main_name"
  fi
  if [[ "$companions" == both || "$companions" == code ]]; then
    printf 'dummy code companion\n' > "$fixture/target/fixture-target/release/$code_name"
  fi
  if [[ "$companions" == both || "$companions" == relay ]]; then
    printf 'dummy relay companion\n' > "$fixture/target/fixture-target/release/$relay_name"
  fi
  # A nonmember in the build directory must never enter the product archive.
  printf 'not a release member\n' > "$fixture/target/fixture-target/release/unrelated.txt"
  if [[ "$omitted" != web ]]; then
    mkdir -p "$fixture/web/dist/assets" "$fixture/web/dist/empty"
    printf '<html>dummy frontend</html>\n' > "$fixture/web/dist/index.html"
    printf 'dummy frontend asset\n' > "$fixture/web/dist/assets/app.js"
    printf 'space-containing asset\n' > "$fixture/web/dist/assets/with space.txt"
    printf 'hidden frontend payload\n' > "$fixture/web/dist/.metadata"
  fi
}

run_packager() {
  local producer="$1" fixture="$2"
  # GitHub's packaging shell exits on a command failure. Run the unmodified
  # body in a child shell so negative-case conditionals cannot disable that.
  if [[ "$producer" == old ]]; then
    (cd "$fixture" && bash --noprofile --norc -e -o pipefail "$commands_file")
  else
    (cd "$fixture" && bash "$archive_packager" fixture-target "$main_name" "$package_ext" "$package_os")
  fi
}

assert_roundtrip() {
  local fixture="$1" companions="$2"
  local archive="$fixture/zeroclaw-fixture-target.$package_ext"
  local extracted="$fixture/extracted"
  [[ -f "$archive" ]] || fail "canonical archive filename was not produced"
  mkdir "$extracted"
  if [[ "$package_ext" == zip ]]; then
    7z x "-o$extracted" "$archive" > "$fixture/extract.log"
  else
    tar -xzf "$archive" -C "$extracted"
  fi

  # Exact expected members are independent assertions over controlled fixture
  # inputs. No filesystem staging or product packaging is implemented here.
  {
    printf '%s\n' "$main_name"
    if [[ "$companions" == both || "$companions" == code ]]; then
      printf '%s\n' "$code_name"
    fi
    if [[ "$companions" == both || "$companions" == relay ]]; then
      printf '%s\n' "$relay_name"
    fi
    printf '%s\n' 'web/dist/index.html' 'web/dist/assets/app.js' \
      'web/dist/assets/with space.txt' 'web/dist/.metadata'
  } | LC_ALL=C sort > "$fixture/expected-members.txt"
  (cd "$extracted" && find . -type f | sed 's#^\./##' | LC_ALL=C sort) > "$fixture/actual-members.txt"
  cmp "$fixture/expected-members.txt" "$fixture/actual-members.txt" ||
    fail "archive member layout differs for companions: $companions"
  [[ -d "$extracted/web/dist/empty" ]] || fail "empty frontend directory was lost"
  cmp "$fixture/target/fixture-target/release/$main_name" "$extracted/$main_name" ||
    fail "main payload changed during packaging/extraction"
  for member in "$code_name" "$relay_name"; do
    if [[ -f "$fixture/target/fixture-target/release/$member" ]]; then
      cmp "$fixture/target/fixture-target/release/$member" "$extracted/$member" ||
        fail "companion payload changed: $member"
    else
      [[ ! -e "$extracted/$member" ]] || fail "absent optional companion was added: $member"
    fi
  done
  for member in index.html assets/app.js 'assets/with space.txt' .metadata; do
    cmp "$fixture/web/dist/$member" "$extracted/web/dist/$member" ||
      fail "frontend payload changed: $member"
  done
  if [[ "$package_ext" == tar.gz ]]; then
    [[ -x "$extracted/$main_name" ]] || fail "Unix main executable mode was lost"
  fi
}

positive_case() {
  local companions="$1" producer fixture
  for producer in old shared; do
    fixture="$archive_test_tmp/positive $companions/$producer"
    seed_fixture "$fixture" "$companions"
    if ! run_packager "$producer" "$fixture" > "$fixture/package.log" 2>&1; then
      cat "$fixture/package.log" >&2
      fail "$producer packaging failed for optional companions: $companions"
    fi
    assert_roundtrip "$fixture" "$companions"
  done
  cmp "$archive_test_tmp/positive $companions/old/actual-members.txt" \
    "$archive_test_tmp/positive $companions/shared/actual-members.txt" || fail "old/new inventory differs"
  case_count=$((case_count + 1))
  echo "PASS archive baseline: $package_step / optional companions $companions"
}

missing_input_case() {
  local omitted="$1"
  local fixture producer status old_status=""
  for producer in old shared; do
    fixture="$archive_test_tmp/missing $omitted/$producer"
    seed_fixture "$fixture" both "$omitted"
    if run_packager "$producer" "$fixture" > "$fixture/package.log" 2>&1; then
      fail "$producer missing $omitted unexpectedly packaged successfully"
    else
      status=$?
    fi
    if [[ "$producer" == old ]]; then old_status="$status"; else
      [[ "$status" -eq "$old_status" ]] || fail "old/new missing-input status differs"
    fi
    [[ ! -e "$fixture/zeroclaw-fixture-target.$package_ext" ]] || fail "missing $omitted produced an archive"
  done
  case_count=$((case_count + 1))
  echo "PASS archive baseline: $package_step / missing $omitted refuses"
}

archiver_failure_case() {
  local fixture producer status
  for producer in old shared; do
    fixture="$archive_test_tmp/archiver failure/$producer"
    seed_fixture "$fixture" both
    mkdir "$fixture/failing-bin"
    printf '#!/usr/bin/env bash\nexit 83\n' > "$fixture/failing-bin/$archive_tool"
    chmod +x "$fixture/failing-bin/$archive_tool"
    if PATH="$fixture/failing-bin:$PATH" run_packager "$producer" "$fixture" > "$fixture/package.log" 2>&1; then
      fail "$producer archiver failure unexpectedly succeeded"
    else
      status=$?
    fi
    [[ "$status" -eq 83 ]] || fail "archiver status was not propagated: $status"
    [[ ! -e "$fixture/zeroclaw-fixture-target.$package_ext" ]] || fail "a failing archiver produced an archive"
  done
  case_count=$((case_count + 1))
  echo "PASS archive baseline: $package_step / archiver error propagates"
}

for companions in both none code relay; do
  positive_case "$companions"
done
missing_input_case main
missing_input_case web
archiver_failure_case
[[ "$case_count" -eq 7 ]] || fail "not all seven baseline cases ran"
printf 'PASS archive baseline: %s / %s / 7 cases / source blob %s\n' \
  "$package_step" "$package_ext" "$archive_baseline_blob"

verification_fixture="$archive_test_tmp/positive both/shared"
verification_archive="$verification_fixture/zeroclaw-fixture-target.$package_ext"
verification_manifest="$verification_fixture/input-manifest.json"
manifest_args=(manifest --binary "$verification_fixture/target/fixture-target/release/$main_name"
  --companion "$verification_fixture/target/fixture-target/release/$code_name"
  --companion "$verification_fixture/target/fixture-target/release/$relay_name"
  --web-dist "$verification_fixture/web/dist" --source fixture-source
  --target fixture-target --run 101 --attempt 2 --features fixture-features --output "$verification_manifest")
"$archive_python" "$archive_verifier" "${manifest_args[@]}"
"$archive_python" "$archive_verifier" verify --manifest "$verification_manifest" \
  --archive "$verification_archive" --extract-dir "$verification_fixture/verified" \
  --expected-source fixture-source --expected-target fixture-target \
  --expected-run 101 --expected-attempt 2 \
  --evidence "$verification_fixture/evidence.json"
cmp "$verification_fixture/target/fixture-target/release/$main_name" \
  "$verification_fixture/verified/$main_name" || fail "verified main differs from actual build input"
echo "PASS archive verification: actual input manifest and extracted payloads"

expect_verification_failure() {
  local label="$1" diagnostic="$2"
  shift 2
  if "$archive_python" "$archive_verifier" "$@" > "$archive_test_tmp/negative.log" 2>&1; then
    fail "verification unexpectedly accepted $label"
  fi
  grep -Fq "$diagnostic" "$archive_test_tmp/negative.log" || {
    cat "$archive_test_tmp/negative.log" >&2
    fail "$label failed for an unrelated reason"
  }
  echo "PASS archive verification: $label refuses"
}
expect_verification_failure 'missing eligible companion' 'input file is not regular' "${manifest_args[@]}" \
  --companion "$verification_fixture/target/fixture-target/release/missing-companion"
expect_verification_failure 'missing main input' 'input file is not regular' "${manifest_args[@]}" \
  --binary "$verification_fixture/target/fixture-target/release/missing-main"
expect_verification_failure 'missing web input' 'web input directory is missing' "${manifest_args[@]}" \
  --web-dist "$verification_fixture/missing-web"

# Malformed archives below are test-only data. The verifier never stages or
# packages a release; every real product archive is written by the helper.
"$archive_python" - "$verification_archive" "$verification_fixture" "$main_name" <<'PY'
import io
import pathlib
import stat
import sys
import tarfile
import zipfile

archive, destination, main = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
records = []
if archive.suffix == ".zip":
    with zipfile.ZipFile(str(archive)) as source:
        for member in source.infolist():
            records.append((member.filename, source.read(member), member.is_dir()))
else:
    with tarfile.open(str(archive), "r:gz") as source:
        for member in source.getmembers():
            reader = source.extractfile(member) if member.isfile() else None
            records.append((member.name, reader.read() if reader else b"", member.isdir()))

for variant in ["missing", "directory", "extra", "duplicate", "traversal", "link", "payload"]:
    items = list(records)
    if variant == "missing":
        items = [item for item in items if item[0] != main]
    elif variant == "directory":
        items = [item for item in items if item[0].rstrip("/") != "web/dist/empty"]
    elif variant == "extra":
        items.append(("unexpected.txt", b"extra", False))
    elif variant == "duplicate":
        items.append(next(item for item in items if item[0] == main))
    elif variant == "traversal":
        items.append(("../escape", b"unsafe", False))
    elif variant == "payload":
        items = [(name, b"X" + data[1:] if name == main else data, directory)
                 for name, data, directory in items]
    path = destination / (variant + (".zip" if archive.suffix == ".zip" else ".tar.gz"))
    if archive.suffix == ".zip":
        with zipfile.ZipFile(str(path), "w") as output:
            for name, data, directory in items:
                output.writestr(name.rstrip("/") + ("/" if directory else ""), data)
            if variant == "link":
                member = zipfile.ZipInfo("unsafe-link")
                member.create_system = 3
                member.external_attr = (stat.S_IFLNK | 0o777) << 16
                output.writestr(member, b"../outside")
    else:
        with tarfile.open(str(path), "w:gz") as output:
            for name, data, directory in items:
                member = tarfile.TarInfo(name)
                member.type = tarfile.DIRTYPE if directory else tarfile.REGTYPE
                member.size = 0 if directory else len(data)
                output.addfile(member, None if directory else io.BytesIO(data))
            if variant == "link":
                member = tarfile.TarInfo("unsafe-link")
                member.type = tarfile.SYMTYPE
                member.linkname = "../outside"
                output.addfile(member)
PY
for variant in missing directory extra duplicate traversal link payload; do
  case "$variant" in
    missing) diagnostic='missing archive files' ;;
    directory) diagnostic='missing archive directories' ;;
    extra) diagnostic='unexpected archive files' ;;
    duplicate) diagnostic='duplicate archive member' ;;
    traversal) diagnostic='unsafe member path' ;;
    link) diagnostic='nonregular archive member' ;;
    payload) diagnostic='payload digest differs' ;;
  esac
  expect_verification_failure "$variant archive" "$diagnostic" verify --manifest "$verification_manifest" \
    --archive "$verification_fixture/$variant.$package_ext" \
    --extract-dir "$verification_fixture/rejected-$variant" \
    --expected-source fixture-source --expected-target fixture-target \
    --expected-run 101 --expected-attempt 2
done
expect_verification_failure 'wrong source' 'source identity differs' verify --manifest "$verification_manifest" \
  --archive "$verification_archive" --extract-dir "$verification_fixture/wrong-source" \
  --expected-source another-source --expected-target fixture-target --expected-run 101 --expected-attempt 2
expect_verification_failure 'wrong target' 'target identity differs' verify --manifest "$verification_manifest" \
  --archive "$verification_archive" --extract-dir "$verification_fixture/wrong-target" \
  --expected-source fixture-source --expected-target another-target --expected-run 101 --expected-attempt 2
expect_verification_failure 'wrong run' 'run identity differs' verify --manifest "$verification_manifest" \
  --archive "$verification_archive" --extract-dir "$verification_fixture/wrong-run" \
  --expected-source fixture-source --expected-target fixture-target --expected-run 102 --expected-attempt 2
expect_verification_failure 'wrong attempt' 'attempt identity differs' verify --manifest "$verification_manifest" \
  --archive "$verification_archive" --extract-dir "$verification_fixture/wrong-attempt" \
  --expected-source fixture-source --expected-target fixture-target --expected-run 101 --expected-attempt 3
expect_verification_failure 'nonempty extraction directory' 'extraction directory is not empty' verify --manifest "$verification_manifest" \
  --archive "$verification_archive" --extract-dir "$verification_fixture/extracted" \
  --expected-source fixture-source --expected-target fixture-target --expected-run 101 --expected-attempt 2
