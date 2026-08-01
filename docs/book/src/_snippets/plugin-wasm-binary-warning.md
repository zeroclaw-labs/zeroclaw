<!-- Canonical warning about committing WASM artifacts. Edit here; reuse via {{#include}}. -->
> [!IMPORTANT]
> Compiled `.wasm` and `.cwasm` files are binary artifacts, often megabytes
> each. Do not check them into a git source tree without
> [Git LFS](https://git-lfs.com): every rebuild committed as a plain blob
> bloats the repository history permanently, and `git diff`/review tooling
> chokes on them. Treat them like any other build output: add
> `target/` and `*.wasm`/`*.cwasm` to `.gitignore`, and distribute through a
> release artifact or plugin registry archive instead. If an artifact truly
> must live in the tree, track the pattern with LFS
> (`git lfs track "*.wasm"`) before the first commit.
