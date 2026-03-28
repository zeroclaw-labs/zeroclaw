use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::Arc;

/// Maximum XLSX file size (50 MB).
const MAX_XLSX_BYTES: u64 = 50 * 1024 * 1024;
/// Default character limit returned to the LLM.
const DEFAULT_MAX_CHARS: usize = 50_000;
/// Hard ceiling regardless of what the caller requests.
const MAX_OUTPUT_CHARS: usize = 200_000;
/// Upper bound for total uncompressed XML read from sheet files.
const MAX_TOTAL_SHEET_XML_BYTES: u64 = 16 * 1024 * 1024;

/// Extract plain text from an XLSX file in the workspace.
pub struct XlsxReadTool {
    security: Arc<SecurityPolicy>,
}

impl XlsxReadTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

/// Extract plain text from XLSX bytes.
///
/// XLSX is a ZIP archive containing `xl/worksheets/sheet*.xml` with cell data,
/// `xl/sharedStrings.xml` with a string pool, and `xl/workbook.xml` with sheet
/// names. Text cells reference the shared string pool by index; inline and
/// numeric values are taken directly from `<v>` elements.
fn extract_xlsx_text(bytes: &[u8]) -> anyhow::Result<String> {
    extract_xlsx_text_with_limits(bytes, MAX_TOTAL_SHEET_XML_BYTES)
}

fn extract_xlsx_text_with_limits(
    bytes: &[u8],
    max_total_sheet_xml_bytes: u64,
) -> anyhow::Result<String> {
    use std::io::Read;

    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    // 1. Parse shared strings table.
    let shared_strings = parse_shared_strings(&mut archive)?;

    // 2. Parse workbook.xml to get sheet names and rIds.
    let sheet_entries = parse_workbook_sheets(&mut archive)?;

    // 3. Parse workbook.xml.rels to map rId → Target path.
    let rel_targets = parse_workbook_rels(&mut archive)?;

    // 4. Build ordered list of (sheet_name, file_path) pairs.
    let mut ordered_sheets: Vec<(String, String)> = Vec::new();
    for (sheet_name, r_id) in &sheet_entries {
        if let Some(target) = rel_targets.get(r_id) {
            if let Some(normalized) = normalize_sheet_target(target) {
                ordered_sheets.push((sheet_name.clone(), normalized));
            }
        }
    }

    // Fallback: if workbook parsing yielded no sheets, scan ZIP entries directly.
    if ordered_sheets.is_empty() {
        let mut fallback_paths: Vec<String> = (0..archive.len())
            .filter_map(|i| {
                let name = archive.by_index(i).ok()?.name().to_string();
                if name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml") {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        fallback_paths.sort_by(|a, b| {
            let a_idx = sheet_numeric_index(a);
            let b_idx = sheet_numeric_index(b);
            a_idx.cmp(&b_idx).then_with(|| a.cmp(b))
        });

        if fallback_paths.is_empty() {
            anyhow::bail!("Not a valid XLSX (no worksheet XML files found)");
        }

        for (i, path) in fallback_paths.into_iter().enumerate() {
            ordered_sheets.push((format!("Sheet{}", i + 1), path));
        }
    }

    // 5. Extract cell text from each sheet.
    let mut output = String::new();
    let mut total_sheet_xml_bytes = 0u64;
    let multi_sheet = ordered_sheets.len() > 1;

    for (sheet_name, sheet_path) in &ordered_sheets {
        let mut sheet_file = match archive.by_name(sheet_path) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let sheet_xml_size = sheet_file.size();
        total_sheet_xml_bytes = total_sheet_xml_bytes
            .checked_add(sheet_xml_size)
            .ok_or_else(|| anyhow::anyhow!("Sheet XML payload size overflow"))?;
        if total_sheet_xml_bytes > max_total_sheet_xml_bytes {
            anyhow::bail!(
                "Sheet XML payload too large: {} bytes (limit: {} bytes)",
                total_sheet_xml_bytes,
                max_total_sheet_xml_bytes
            );
        }

        let mut xml_content = String::new();
        sheet_file.read_to_string(&mut xml_content)?;

        if multi_sheet {
            if !output.is_empty() {
                output.push('\n');
            }
            use std::fmt::Write as _;
            let _ = writeln!(output, "--- Sheet: {} ---", sheet_name);
        }

        let sheet_text = extract_sheet_cells(&xml_content, &shared_strings)?;
        output.push_str(&sheet_text);
    }

    Ok(output)
}

/// Parse `xl/sharedStrings.xml` into a `Vec<String>` indexed by position.
fn parse_shared_strings<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<Vec<String>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::io::Read;

    let mut xml = String::new();
    match archive.by_name("xl/sharedStrings.xml") {
        Ok(mut f) => {
            f.read_to_string(&mut xml)?;
        }
        Err(zip::result::ZipError::FileNotFound) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    }

    let mut strings = Vec::new();
    let mut reader = Reader::from_str(&xml);
    let mut in_si = false;
    let mut in_t = false;
    let mut current = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if name == b"si" {
                    in_si = true;
                    current.clear();
                } else if in_si && name == b"t" {
                    in_t = true;
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if name == b"t" {
                    in_t = false;
                } else if name == b"si" {
                    in_si = false;
                    strings.push(std::mem::take(&mut current));
                }
            }
            Ok(Event::Text(e)) => {
                if in_t {
                    current.push_str(&e.unescape()?);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    Ok(strings)
}

/// Parse `xl/workbook.xml` → Vec<(sheet_name, rId)>.
fn parse_workbook_sheets<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<Vec<(String, String)>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::io::Read;

    let mut xml = String::new();
    match archive.by_name("xl/workbook.xml") {
        Ok(mut f) => {
            f.read_to_string(&mut xml)?;
        }
        Err(zip::result::ZipError::FileNotFound) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    }

    let mut sheets = Vec::new();
    let mut reader = Reader::from_str(&xml);

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qname = e.name();
                if local_name(qname.as_ref()) == b"sheet" {
                    let mut name = None;
                    let mut r_id = None;
                    for attr in e.attributes().flatten() {
                        let key = attr.key.as_ref();
                        let local = local_name(key);
                        if local == b"name" {
                            name = Some(
                                attr.decode_and_unescape_value(reader.decoder())?
                                    .into_owned(),
                            );
                        } else if key == b"r:id" || local == b"id" {
                            // Accept both r:id and {ns}:id variants.
                            // Only take the relationship id (starts with "rId").
                            let val = attr
                                .decode_and_unescape_value(reader.decoder())?
                                .into_owned();
                            if val.starts_with("rId") {
                                r_id = Some(val);
                            }
                        }
                    }
                    if let (Some(n), Some(r)) = (name, r_id) {
                        sheets.push((n, r));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    Ok(sheets)
}

/// Parse `xl/_rels/workbook.xml.rels` → HashMap<rId, Target>.
fn parse_workbook_rels<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<HashMap<String, String>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::io::Read;

    let mut xml = String::new();
    match archive.by_name("xl/_rels/workbook.xml.rels") {
        Ok(mut f) => {
            f.read_to_string(&mut xml)?;
        }
        Err(zip::result::ZipError::FileNotFound) => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    }

    let mut rels = HashMap::new();
    let mut reader = Reader::from_str(&xml);

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qname = e.name();
                if local_name(qname.as_ref()) == b"Relationship" {
                    let mut rel_id = None;
                    let mut target = None;
                    for attr in e.attributes().flatten() {
                        let key = local_name(attr.key.as_ref());
                        if key.eq_ignore_ascii_case(b"id") {
                            rel_id = Some(
                                attr.decode_and_unescape_value(reader.decoder())?
                                    .into_owned(),
                            );
                        } else if key.eq_ignore_ascii_case(b"target") {
                            target = Some(
                                attr.decode_and_unescape_value(reader.decoder())?
                                    .into_owned(),
                            );
                        }
                    }
                    if let (Some(id), Some(t)) = (rel_id, target) {
                        rels.insert(id, t);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    Ok(rels)
}

/// Extract cell text from a single worksheet XML string.
///
/// Cells are output as tab-separated values per row, newline-separated per row.
fn extract_sheet_cells(xml: &str, shared_strings: &[String]) -> anyhow::Result<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut output = String::new();

    let mut in_row = false;
    let mut in_cell = false;
    let mut in_value = false;
    let mut cell_type = CellType::Number;
    let mut cell_value = String::new();
    let mut row_cells: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                match name {
                    b"row" => {
                        in_row = true;
                        row_cells.clear();
                    }
                    b"c" if in_row => {
                        in_cell = true;
                        cell_type = CellType::Number;
                        cell_value.clear();
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"t" {
                                let val = attr.decode_and_unescape_value(reader.decoder())?;
                                cell_type = match val.as_ref() {
                                    "s" => CellType::SharedString,
                                    "inlineStr" => CellType::InlineString,
                                    "b" => CellType::Boolean,
                                    _ => CellType::Number,
                                };
                            }
                        }
                    }
                    b"v" if in_cell => {
                        in_value = true;
                    }
                    b"t" if in_cell && cell_type == CellType::InlineString => {
                        // Inline string: text is inside <is><t>...</t></is>
                        in_value = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                match name {
                    b"row" => {
                        in_row = false;
                        if !row_cells.is_empty() {
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(&row_cells.join("\t"));
                        }
                    }
                    b"c" if in_cell => {
                        in_cell = false;
                        let resolved = match cell_type {
                            CellType::SharedString => {
                                if let Ok(idx) = cell_value.trim().parse::<usize>() {
                                    shared_strings.get(idx).cloned().unwrap_or_default()
                                } else {
                                    cell_value.clone()
                                }
                            }
                            CellType::Boolean => match cell_value.trim() {
                                "1" => "TRUE".to_string(),
                                "0" => "FALSE".to_string(),
                                other => other.to_string(),
                            },
                            _ => cell_value.clone(),
                        };
                        row_cells.push(resolved);
                    }
                    b"v" => {
                        in_value = false;
                    }
                    b"t" if in_cell => {
                        in_value = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if in_value {
                    cell_value.push_str(&e.unescape()?);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    // Flush last row if not terminated by </row>.
    if in_row && !row_cells.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&row_cells.join("\t"));
    }

    if !output.is_empty() {
        output.push('\n');
    }

    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellType {
    Number,
    SharedString,
    InlineString,
    Boolean,
}

fn sheet_numeric_index(sheet_path: &str) -> Option<u32> {
    let stem = Path::new(sheet_path).file_stem()?.to_string_lossy();
    let digits = stem.strip_prefix("sheet")?;
    digits.parse::<u32>().ok()
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}

fn normalize_sheet_target(target: &str) -> Option<String> {
    if target.contains("://") {
        return None;
    }

    let mut segments = Vec::new();
    for component in Path::new("xl").join(target).components() {
        match component {
            Component::Normal(part) => segments.push(part.to_string_lossy().to_string()),
            Component::ParentDir => {
                segments.pop()?;
            }
            _ => {}
        }
    }

    let normalized = segments.join("/");
    if normalized.starts_with("xl/worksheets/") && normalized.ends_with(".xml") {
        Some(normalized)
    } else {
        None
    }
}

fn parse_max_chars(args: &serde_json::Value) -> anyhow::Result<usize> {
    let Some(value) = args.get("max_chars") else {
        return Ok(DEFAULT_MAX_CHARS);
    };

    let serde_json::Value::Number(number) = value else {
        anyhow::bail!("Invalid 'max_chars': expected a positive integer");
    };
    let Some(raw) = number.as_u64() else {
        anyhow::bail!("Invalid 'max_chars': expected a positive integer");
    };
    if raw == 0 {
        anyhow::bail!("Invalid 'max_chars': must be >= 1");
    }

    Ok(usize::try_from(raw)
        .unwrap_or(MAX_OUTPUT_CHARS)
        .min(MAX_OUTPUT_CHARS))
}

#[async_trait]
impl Tool for XlsxReadTool {
    fn name(&self) -> &str {
        "xlsx_read"
    }

    fn description(&self) -> &str {
        "Extract plain text and numeric data from an XLSX (Excel) file in the workspace. \
         Returns tab-separated cell values per row for each sheet. \
         No formulas, charts, styles, or merged-cell awareness."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the XLSX file. Relative paths resolve from workspace."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 50000, max: 200000)",
                    "minimum": 1,
                    "maximum": 200_000
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let max_chars = match parse_max_chars(&args) {
            Ok(value) => value,
            Err(err) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(err.to_string()),
                })
            }
        };

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let full_path = self.security.resolve_user_supplied_path(path);

        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&resolved_path),
                ),
            });
        }

        tracing::debug!("Reading XLSX: {}", resolved_path.display());

        match tokio::fs::metadata(&resolved_path).await {
            Ok(meta) => {
                if meta.len() > MAX_XLSX_BYTES {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "XLSX too large: {} bytes (limit: {MAX_XLSX_BYTES} bytes)",
                            meta.len()
                        )),
                    });
                }
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file metadata: {e}")),
                });
            }
        }

        let bytes = match tokio::fs::read(&resolved_path).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read XLSX file: {e}")),
                });
            }
        };

        let text = match tokio::task::spawn_blocking(move || extract_xlsx_text(&bytes)).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("XLSX extraction failed: {e}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("XLSX extraction task panicked: {e}")),
                });
            }
        };

        if text.trim().is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "XLSX contains no extractable text".into(),
                error: None,
            });
        }

        let output = if text.chars().count() > max_chars {
            let mut truncated: String = text.chars().take(max_chars).collect();
            use std::fmt::Write as _;
            let _ = write!(truncated, "\n\n... [truncated at {max_chars} chars]");
            truncated
        } else {
            text
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_limit(
        workspace: std::path::PathBuf,
        max_actions: u32,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            max_actions_per_hour: max_actions,
            ..SecurityPolicy::default()
        })
    }

    /// Build a minimal valid XLSX (ZIP) in memory with one sheet containing
    /// the given rows. Each inner `Vec<&str>` is a row of cell values.
    fn minimal_xlsx_bytes(rows: &[Vec<&str>]) -> Vec<u8> {
        use std::io::Write;

        // Build shared strings from all unique cell values.
        let mut all_values: Vec<String> = Vec::new();
        for row in rows {
            for cell in row {
                if !all_values.contains(&cell.to_string()) {
                    all_values.push(cell.to_string());
                }
            }
        }

        let mut ss_entries = String::new();
        for val in &all_values {
            ss_entries.push_str(&format!("<si><t>{val}</t></si>"));
        }
        let shared_strings_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{}" uniqueCount="{}">{ss_entries}</sst>"#,
            all_values.len(),
            all_values.len()
        );

        // Build sheet XML.
        let mut sheet_rows = String::new();
        for (r_idx, row) in rows.iter().enumerate() {
            sheet_rows.push_str(&format!(r#"<row r="{}">"#, r_idx + 1));
            for (c_idx, cell) in row.iter().enumerate() {
                let col_letter = (b'A' + c_idx as u8) as char;
                let cell_ref = format!("{}{}", col_letter, r_idx + 1);
                let ss_idx = all_values.iter().position(|v| v == cell).unwrap();
                sheet_rows.push_str(&format!(r#"<c r="{cell_ref}" t="s"><v>{ss_idx}</v></c>"#));
            }
            sheet_rows.push_str("</row>");
        }
        let sheet_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>{sheet_rows}</sheetData>
</worksheet>"#
        );

        let workbook_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;

        let rels_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("xl/sharedStrings.xml", options).unwrap();
        zip.write_all(shared_strings_xml.as_bytes()).unwrap();

        zip.start_file("xl/workbook.xml", options).unwrap();
        zip.write_all(workbook_xml.as_bytes()).unwrap();

        zip.start_file("xl/_rels/workbook.xml.rels", options)
            .unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();

        zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();

        zip.finish().unwrap().into_inner()
    }

    /// Build an XLSX with two sheets.
    fn two_sheet_xlsx_bytes(
        sheet1_name: &str,
        sheet1_rows: &[Vec<&str>],
        sheet2_name: &str,
        sheet2_rows: &[Vec<&str>],
    ) -> Vec<u8> {
        use std::io::Write;

        // Collect all unique values across both sheets.
        let mut all_values: Vec<String> = Vec::new();
        for rows in [sheet1_rows, sheet2_rows] {
            for row in rows {
                for cell in row {
                    if !all_values.contains(&cell.to_string()) {
                        all_values.push(cell.to_string());
                    }
                }
            }
        }

        let mut ss_entries = String::new();
        for val in &all_values {
            ss_entries.push_str(&format!("<si><t>{val}</t></si>"));
        }
        let shared_strings_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{}" uniqueCount="{}">{ss_entries}</sst>"#,
            all_values.len(),
            all_values.len()
        );

        let build_sheet = |rows: &[Vec<&str>]| -> String {
            let mut sheet_rows = String::new();
            for (r_idx, row) in rows.iter().enumerate() {
                sheet_rows.push_str(&format!(r#"<row r="{}">"#, r_idx + 1));
                for (c_idx, cell) in row.iter().enumerate() {
                    let col_letter = (b'A' + c_idx as u8) as char;
                    let cell_ref = format!("{}{}", col_letter, r_idx + 1);
                    let ss_idx = all_values.iter().position(|v| v == cell).unwrap();
                    sheet_rows.push_str(&format!(r#"<c r="{cell_ref}" t="s"><v>{ss_idx}</v></c>"#));
                }
                sheet_rows.push_str("</row>");
            }
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>{sheet_rows}</sheetData>
</worksheet>"#
            )
        };

        let workbook_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>
<sheet name="{sheet1_name}" sheetId="1" r:id="rId1"/>
<sheet name="{sheet2_name}" sheetId="2" r:id="rId2"/>
</sheets>
</workbook>"#
        );

        let rels_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
</Relationships>"#;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("xl/sharedStrings.xml", options).unwrap();
        zip.write_all(shared_strings_xml.as_bytes()).unwrap();

        zip.start_file("xl/workbook.xml", options).unwrap();
        zip.write_all(workbook_xml.as_bytes()).unwrap();

        zip.start_file("xl/_rels/workbook.xml.rels", options)
            .unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();

        zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
        zip.write_all(build_sheet(sheet1_rows).as_bytes()).unwrap();

        zip.start_file("xl/worksheets/sheet2.xml", options).unwrap();
        zip.write_all(build_sheet(sheet2_rows).as_bytes()).unwrap();

        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn name_is_xlsx_read() {
        let tool = XlsxReadTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "xlsx_read");
    }

    #[test]
    fn description_not_empty() {
        let tool = XlsxReadTool::new(test_security(std::env::temp_dir()));
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_has_path_required() {
        let tool = XlsxReadTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["max_chars"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = XlsxReadTool::new(test_security(std::env::temp_dir()));
        let spec = tool.spec();
        assert_eq!(spec.name, "xlsx_read");
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn missing_path_param_returns_error() {
        let tool = XlsxReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path"));
    }

    #[tokio::test]
    async fn absolute_path_is_blocked() {
        let tool = XlsxReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"path": "/etc/passwd"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    #[tokio::test]
    async fn path_traversal_is_blocked() {
        let tmp = TempDir::new().unwrap();
        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "../../../etc/passwd"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "missing.xlsx"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to resolve"));
    }

    #[tokio::test]
    async fn rate_limit_blocks_request() {
        let tmp = TempDir::new().unwrap();
        let tool = XlsxReadTool::new(test_security_with_limit(tmp.path().to_path_buf(), 0));
        let result = tool.execute(json!({"path": "any.xlsx"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn extracts_text_from_valid_xlsx() {
        let tmp = TempDir::new().unwrap();
        let xlsx_path = tmp.path().join("data.xlsx");
        let rows = vec![vec!["Name", "Age"], vec!["Alice", "30"]];
        tokio::fs::write(&xlsx_path, minimal_xlsx_bytes(&rows))
            .await
            .unwrap();

        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "data.xlsx"})).await.unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(
            result.output.contains("Name"),
            "expected 'Name' in output, got: {}",
            result.output
        );
        assert!(result.output.contains("Age"));
        assert!(result.output.contains("Alice"));
        assert!(result.output.contains("30"));
    }

    #[tokio::test]
    async fn extracts_tab_separated_columns() {
        let tmp = TempDir::new().unwrap();
        let xlsx_path = tmp.path().join("cols.xlsx");
        let rows = vec![vec!["A", "B", "C"]];
        tokio::fs::write(&xlsx_path, minimal_xlsx_bytes(&rows))
            .await
            .unwrap();

        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "cols.xlsx"})).await.unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("A\tB\tC"),
            "expected tab-separated output, got: {:?}",
            result.output
        );
    }

    #[tokio::test]
    async fn extracts_multiple_sheets() {
        let tmp = TempDir::new().unwrap();
        let xlsx_path = tmp.path().join("multi.xlsx");
        let bytes = two_sheet_xlsx_bytes(
            "Sales",
            &[vec!["Product", "Revenue"], vec!["Widget", "1000"]],
            "Costs",
            &[vec!["Item", "Amount"], vec!["Rent", "500"]],
        );
        tokio::fs::write(&xlsx_path, bytes).await.unwrap();

        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "multi.xlsx"})).await.unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(result.output.contains("--- Sheet: Sales ---"));
        assert!(result.output.contains("--- Sheet: Costs ---"));
        assert!(result.output.contains("Widget"));
        assert!(result.output.contains("Rent"));
    }

    #[tokio::test]
    async fn invalid_zip_returns_extraction_error() {
        let tmp = TempDir::new().unwrap();
        let xlsx_path = tmp.path().join("bad.xlsx");
        tokio::fs::write(&xlsx_path, b"this is not a zip file")
            .await
            .unwrap();

        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "bad.xlsx"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("extraction failed"));
    }

    #[tokio::test]
    async fn max_chars_truncates_output() {
        let tmp = TempDir::new().unwrap();
        let long_text = "B".repeat(200);
        let rows = vec![vec![long_text.as_str(); 10]];
        let xlsx_path = tmp.path().join("long.xlsx");
        tokio::fs::write(&xlsx_path, minimal_xlsx_bytes(&rows))
            .await
            .unwrap();

        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "long.xlsx", "max_chars": 50}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("truncated"));
    }

    #[tokio::test]
    async fn invalid_max_chars_returns_tool_error() {
        let tmp = TempDir::new().unwrap();
        let xlsx_path = tmp.path().join("data.xlsx");
        let rows = vec![vec!["Hello"]];
        tokio::fs::write(&xlsx_path, minimal_xlsx_bytes(&rows))
            .await
            .unwrap();

        let tool = XlsxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "data.xlsx", "max_chars": "100"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("max_chars"));
    }

    #[test]
    fn shared_string_reference_resolved() {
        let rows = vec![vec!["Hello", "World"]];
        let bytes = minimal_xlsx_bytes(&rows);
        let text = extract_xlsx_text(&bytes).unwrap();
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn cumulative_sheet_xml_limit_is_enforced() {
        let rows = vec![vec!["Alpha", "Beta"]];
        let bytes = minimal_xlsx_bytes(&rows);
        let error = extract_xlsx_text_with_limits(&bytes, 64).unwrap_err();
        assert!(error.to_string().contains("Sheet XML payload too large"));
    }

    #[test]
    fn numeric_cells_extracted_directly() {
        use std::io::Write;

        // Build a sheet with numeric cells (no t="s" attribute).
        let sheet_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1"><v>42</v></c><c r="B1"><v>3.14</v></c></row>
</sheetData>
</worksheet>"#;

        let workbook_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Numbers" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;

        let rels_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("xl/workbook.xml", options).unwrap();
        zip.write_all(workbook_xml.as_bytes()).unwrap();
        zip.start_file("xl/_rels/workbook.xml.rels", options)
            .unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();
        zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();

        let bytes = zip.finish().unwrap().into_inner();
        let text = extract_xlsx_text(&bytes).unwrap();
        assert!(text.contains("42"), "got: {text}");
        assert!(text.contains("3.14"), "got: {text}");
        assert!(text.contains("42\t3.14"), "got: {text}");
    }

    #[test]
    fn fallback_when_no_workbook() {
        use std::io::Write;

        // ZIP with only sheet files, no workbook.xml.
        let sheet_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1"><v>99</v></c></row>
</sheetData>
</worksheet>"#;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();

        let bytes = zip.finish().unwrap().into_inner();
        let text = extract_xlsx_text(&bytes).unwrap();
        assert!(text.contains("99"), "got: {text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_is_blocked() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        let rows = vec![vec!["secret"]];
        tokio::fs::write(outside.join("secret.xlsx"), minimal_xlsx_bytes(&rows))
            .await
            .unwrap();
        symlink(outside.join("secret.xlsx"), workspace.join("link.xlsx")).unwrap();

        let tool = XlsxReadTool::new(test_security(workspace));
        let result = tool.execute(json!({"path": "link.xlsx"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("escapes workspace"));
    }
}
