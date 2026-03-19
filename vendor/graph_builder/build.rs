use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::str;

// Checks if the maybe_uninit_write_slice feature can be enabled
const MAYBE_UNINIT_WRITE_SLICE_PROBE: &str = r#"
#![feature(maybe_uninit_write_slice)]
#![allow(dead_code)]

use std::mem::MaybeUninit;

pub fn test() {
    MaybeUninit::write_slice(
        [MaybeUninit::<usize>::uninit()].as_mut_slice(),
        [1_usize].as_slice()
    );
}
"#;

// Checks if the new_uninit feature can be enabled
const NEW_UNINIT_PROBE: &str = r#"
#![feature(new_uninit)]
#![allow(dead_code)]

use std::mem::MaybeUninit;

pub fn test() -> Box<[MaybeUninit<usize>]> {
    Box::<[usize]>::new_uninit_slice(42)
}
"#;

// Checks if the slice_partition_dedup feature can be enabled
const SLICE_PARTITION_DEDUP_PROBE: &str = r#"
#![feature(slice_partition_dedup)]
#![allow(dead_code)]

pub fn test() {
    let mut slice = [1, 2, 2, 3, 3, 2, 1, 1];
    let (dedup, duplicates) = slice.partition_dedup();

    assert_eq!(dedup, [1, 2, 3, 2, 1]);
    assert_eq!(duplicates, [2, 3, 1]);
}
"#;

// Checks if the doc_cfg feature can be enabled
const DOC_CFG_PROBE: &str = r#"
#![feature(doc_cfg)]
#![allow(dead_code)]

#[doc(cfg(feature = "some_feature"))]
pub struct Foobar;
"#;

fn main() {
    let force_fallback_impl = env::var_os("CARGO_FEATURE_FORCE_FALLBACK_IMPL").is_some();

    if !force_fallback_impl {
        test_for_feature("maybe_uninit_write_slice", MAYBE_UNINIT_WRITE_SLICE_PROBE);
        test_for_feature("new_uninit", NEW_UNINIT_PROBE);
        test_for_feature("slice_partition_dedup", SLICE_PARTITION_DEDUP_PROBE);
        test_for_feature("doc_cfg", DOC_CFG_PROBE);
    }
}

fn test_for_feature(feature_name: &str, probe: &str) {
    match compile_probe(probe) {
        Some(status) if status.success() => enable_feature(feature_name),
        _ => {}
    }
}

fn enable_feature(feature_name: &str) {
    println!("cargo:rustc-cfg=has_{feature_name}");
}

// Checks if some code can be compiled with the current toolchain
fn compile_probe(probe: &str) -> Option<ExitStatus> {
    let rustc = env::var_os("RUSTC")?;
    let out_dir = env::var_os("OUT_DIR")?;
    let probefile = Path::new(&out_dir).join("probe.rs");
    fs::write(&probefile, probe).ok()?;
    Command::new(rustc)
        .arg("--edition=2021")
        .arg("--crate-name=graph_build")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(out_dir)
        .arg(probefile)
        .status()
        .ok()
}
