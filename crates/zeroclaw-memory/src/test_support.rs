//! Unix-only test scaffolding shared by the Lucid connector tests: writes
//! executable `/bin/sh` stand-ins for the `lucid` binary. This one writer
//! owns the dispatch skeleton and chmod mechanics; callers supply the
//! per-script `store`/`context` branch bodies (each must `exit` itself on
//! success) and an optional prelude that runs before dispatch (e.g.
//! invocation logging). Any other subcommand exits 1.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(crate) fn write_lucid_script(
    dir: &Path,
    file_name: &str,
    prelude: &str,
    store_body: &str,
    context_body: &str,
) -> String {
    let script_path = dir.join(file_name);
    let script = format!(
        r#"#!/bin/sh
set -eu
{prelude}
if [ "${{1:-}}" = "store" ]; then
{store_body}
fi

if [ "${{1:-}}" = "context" ]; then
{context_body}
fi

echo "unsupported command" >&2
exit 1
"#
    );

    fs::write(&script_path, script).unwrap();
    let mut permissions = fs::metadata(&script_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).unwrap();
    script_path.display().to_string()
}
