//! Verify that the C# SDK entry-point marker (PluginFunctionAttribute)
//! and PluginEntryPoint class handle JSON marshalling correctly.
//!
//! Checks that PluginFunctionAttribute.cs contains:
//! - The [PluginFunction] attribute targeting methods
//! - PluginEntryPoint with snake_case JSON serialization
//! - Generic Invoke wrappers that deserialize input and serialize output
//! - Serialize / Deserialize helper methods using System.Text.Json

use std::path::Path;

#[test]
fn csharp_entry_point_defines_plugin_function_attribute() {
    let src = read_entry_point_source();

    assert!(
        src.contains("class PluginFunctionAttribute : Attribute"),
        "PluginFunctionAttribute class not found"
    );
    assert!(
        src.contains("AttributeTargets.Method"),
        "PluginFunctionAttribute must target methods"
    );
}

#[test]
fn csharp_entry_point_uses_snake_case_json_policy() {
    let src = read_entry_point_source();

    assert!(
        src.contains("JsonNamingPolicy.SnakeCaseLower"),
        "PluginEntryPoint must use SnakeCaseLower naming policy for JSON marshalling"
    );
    assert!(
        src.contains("PropertyNameCaseInsensitive = true"),
        "PluginEntryPoint must enable case-insensitive deserialization"
    );
}

#[test]
fn csharp_entry_point_invoke_handles_typed_input_output() {
    let src = read_entry_point_source();

    assert!(
        src.contains("Invoke<TInput, TOutput>(Func<TInput, TOutput>"),
        "PluginEntryPoint must provide Invoke<TInput, TOutput> wrapper"
    );
    assert!(
        src.contains("Deserialize<TInput>(raw)"),
        "Invoke must deserialize input from Extism host"
    );
    assert!(
        src.contains("Serialize(result)"),
        "Invoke must serialize the handler result"
    );
}

#[test]
fn csharp_entry_point_provides_invoke_overloads() {
    let src = read_entry_point_source();

    assert!(
        src.contains("Invoke<TInput>(Action<TInput>"),
        "PluginEntryPoint must provide input-only Invoke overload"
    );
    assert!(
        src.contains("InvokeNoInput<TOutput>(Func<TOutput>"),
        "PluginEntryPoint must provide InvokeNoInput overload"
    );
}

#[test]
fn csharp_entry_point_serialize_deserialize_use_system_text_json() {
    let src = read_entry_point_source();

    assert!(
        src.contains("using System.Text.Json"),
        "PluginEntryPoint must use System.Text.Json"
    );
    assert!(
        src.contains("JsonSerializer.Deserialize<T>("),
        "Deserialize must call JsonSerializer.Deserialize"
    );
    assert!(
        src.contains("JsonSerializer.SerializeToUtf8Bytes("),
        "Serialize must call JsonSerializer.SerializeToUtf8Bytes"
    );
}

fn read_entry_point_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdks/csharp/src/PluginFunctionAttribute.cs");
    assert!(
        path.is_file(),
        "PluginFunctionAttribute.cs not found at {}",
        path.display()
    );
    std::fs::read_to_string(&path).expect("failed to read PluginFunctionAttribute.cs")
}
