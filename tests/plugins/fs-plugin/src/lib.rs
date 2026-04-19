use extism_pdk::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Deserialize)]
struct ReadInput {
    path: String,
}

#[derive(Serialize)]
struct ReadOutput {
    contents: String,
}

#[plugin_fn]
pub fn tool_read_file(input: String) -> FnResult<String> {
    let params: ReadInput = serde_json::from_str(&input)?;
    let contents = fs::read_to_string(&params.path)?;
    let result = ReadOutput { contents };
    Ok(serde_json::to_string(&result)?)
}

#[derive(Deserialize)]
struct WriteInput {
    path: String,
    contents: String,
}

#[derive(Serialize)]
struct WriteOutput {
    bytes_written: usize,
}

#[plugin_fn]
pub fn tool_write_file(input: String) -> FnResult<String> {
    let params: WriteInput = serde_json::from_str(&input)?;
    let len = params.contents.len();
    fs::write(&params.path, &params.contents)?;
    let result = WriteOutput {
        bytes_written: len,
    };
    Ok(serde_json::to_string(&result)?)
}

#[derive(Serialize)]
struct TransformOutput {
    success: bool,
    input_path: String,
    output_path: String,
    original_len: usize,
    transformed_len: usize,
}

#[derive(Serialize)]
struct TransformError {
    success: bool,
    error: String,
}

/// Reads /input/data.txt, transforms contents to uppercase, writes to /output/result.txt.
#[plugin_fn]
pub fn tool_transform_file(_input: String) -> FnResult<String> {
    let input_path = "/input/data.txt";
    let output_path = "/output/result.txt";

    let contents = match fs::read_to_string(input_path) {
        Ok(c) => c,
        Err(e) => {
            let err = TransformError {
                success: false,
                error: format!("failed to read {input_path}: {e}"),
            };
            return Ok(serde_json::to_string(&err)?);
        }
    };

    let transformed = contents.to_uppercase();

    if let Err(e) = fs::write(output_path, &transformed) {
        let err = TransformError {
            success: false,
            error: format!("failed to write {output_path}: {e}"),
        };
        return Ok(serde_json::to_string(&err)?);
    }

    let result = TransformOutput {
        success: true,
        input_path: input_path.to_string(),
        output_path: output_path.to_string(),
        original_len: contents.len(),
        transformed_len: transformed.len(),
    };
    Ok(serde_json::to_string(&result)?)
}
