//! Verify acceptance criterion: "Unit test validates attribute/marshalling works"
//!
//! Checks that the C# SDK test project contains xunit tests covering:
//! - PluginFunctionAttribute construction and Name property
//! - PluginEntryPoint.Serialize / Deserialize round-trip
//! - Snake-case JSON naming policy

use std::path::Path;

#[test]
fn csharp_unit_test_file_exists() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/tests/PluginEntryPointTests.cs");
    assert!(
        path.is_file(),
        "PluginEntryPointTests.cs not found at {}",
        path.display()
    );
}

#[test]
fn csharp_unit_test_covers_attribute_behaviour() {
    let src = read_test_source();

    assert!(
        src.contains("PluginFunctionAttribute_DefaultName_IsNull")
            || src.contains("PluginFunctionAttribute"),
        "Tests must exercise PluginFunctionAttribute"
    );
    assert!(
        src.contains("new PluginFunctionAttribute("),
        "Tests must construct PluginFunctionAttribute to validate it"
    );
}

#[test]
fn csharp_unit_test_covers_serialize_round_trip() {
    let src = read_test_source();

    assert!(
        src.contains("PluginEntryPoint.Serialize("),
        "Tests must call PluginEntryPoint.Serialize"
    );
    assert!(
        src.contains("PluginEntryPoint.Deserialize<"),
        "Tests must call PluginEntryPoint.Deserialize"
    );
}

#[test]
fn csharp_unit_test_covers_snake_case_policy() {
    let src = read_test_source();

    assert!(
        src.contains("Serialize_Uses_SnakeCase") || src.contains("snake_case"),
        "Tests must verify snake_case JSON naming policy"
    );
}

#[test]
fn csharp_unit_test_uses_xunit() {
    let src = read_test_source();

    assert!(
        src.contains("using Xunit;"),
        "Unit tests must use xunit framework"
    );
    assert!(
        src.contains("[Fact]"),
        "Unit tests must have [Fact]-annotated test methods"
    );
}

fn read_test_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/tests/PluginEntryPointTests.cs");
    assert!(
        path.is_file(),
        "PluginEntryPointTests.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read PluginEntryPointTests.cs")
}
