# Contributing

I would appreciate any contributions to this crate. However, some things are handy to know.

## Code Style

### Import Order

All imports are semantically grouped and ordered. The order is:

- standard library (`use std::...`)
- external crates (`use rand::...`)
- current crate (`use crate::...`)
- parent module (`use super::..`)
- current module (`use self::...`)
- module declaration (`mod ...`)

There must be an empty line between groups. An example:

```rust
use crossterm_utils::{csi, write_cout, Result};

use crate::sys::{get_cursor_position, show_cursor};

use super::Cursor;
```

#### CLion Tips

The CLion IDE does this for you (_Menu_ -> _Code_ -> _Optimize Imports_). Be aware that the CLion sorts
imports in a group in a different way when compared to the `rustfmt`. It's effectively two steps operation
to get proper grouping & sorting:

* _Menu_ -> _Code_ -> _Optimize Imports_ - group & semantically order imports
* `cargo fmt` - fix ordering within the group

Second step can be automated via _CLion_ -> _Preferences_ ->
_Languages & Frameworks_ -> _Rust_ -> _Rustfmt_ -> _Run rustfmt on save_.  

### Max Line Length

| Type                 | Max line length |
|:---------------------|----------------:|
| Code                 |             100 |
| Comments in the code |             120 |
| Documentation        |             120 |

100 is the [`max_width`](https://github.com/rust-lang/rustfmt/blob/master/Configurations.md#max_width)
default value.

120 is because of the GitHub. The editor & viewer width there is +- 123 characters. 

### Warnings

The code must be warning free. It's quite hard to find an error if the build logs are polluted with warnings.
If you decide to silent a warning with (`#[allow(...)]`), please add a comment why it's required.

Always consult the [Travis CI](https://travis-ci.org/crossterm-rs/crossterm/pull_requests) build logs.

### Forbidden Warnings

Search for `#![deny(...)]` in the code:

* `unused_must_use`
* `unused_imports`
