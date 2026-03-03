use super::traits::{Tool, ToolResult};
use crate::runtime::RuntimeAdapter;
use crate::security::SecurityPolicy;
use crate::security::SyscallAnomalyDetector;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Maximum shell command execution time before kill.
const SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];
fn truncate_utf8_to_max_bytes(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut cutoff = max_bytes;
    while cutoff > 0 && !text.is_char_boundary(cutoff) {
        cutoff -= 1;
    }
    text.truncate(cutoff);
}

/// Get exit code explanation
fn get_exit_code_explanation(exit_code: i32) -> Option<String> {
    if std::env::consts::OS == "windows" {
        match exit_code {
            -65536 => Some("PowerShell internal error: usually due to execution policy restrictions, syntax errors, or module loading failures".to_string()),
            -196608 => Some("PowerShell startup failure: configuration file corruption or permission issues".to_string()),
            1 => Some("Command execution failed: usually due to syntax errors or command not found".to_string()),
            -1 => Some("Process terminated abnormally: may have been terminated by system or encountered fatal error".to_string()),
            code if code < 0 => Some(format!("PowerShell internal error code: {} (possibly memory access error or system-level error)", code)),
            _ => None,
        }
    } else {
        match exit_code {
            1 => Some("Command execution failed".to_string()),
            2 => Some("Command not found or insufficient permissions".to_string()),
            126 => Some("Command exists but cannot execute (permission issue)".to_string()),
            127 => Some("Command not found".to_string()),
            128..=255 => Some(format!("Command terminated by signal: signal number {}", exit_code - 128)),
            _ => None,
        }
    }
}

/// Decode Windows command output, handle Chinese character encoding issues
fn decode_windows_output(output: &[u8]) -> String {
    // First try UTF-8 decoding
    if let Ok(utf8_str) = String::from_utf8(output.to_vec()) {
        return utf8_str;
    }
    
    // Detect possible encoding types
    let (encoding_type, _confidence) = detect_encoding(output);
    
    match encoding_type {
        EncodingType::Utf8 => String::from_utf8_lossy(output).to_string(),
        EncodingType::Utf16Le => decode_utf16le(output),
        EncodingType::Utf16Be => decode_utf16be(output),
        EncodingType::Gbk => decode_gbk_simple(output),
        EncodingType::Latin1 => output.iter().map(|&b| b as char).collect(),
        EncodingType::Binary => format!("[Binary data: {} bytes]", output.len()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EncodingType {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gbk,
    Latin1,
    Binary,
}

/// Simple encoding detection
fn detect_encoding(data: &[u8]) -> (EncodingType, f32) {
    if data.is_empty() {
        return (EncodingType::Utf8, 1.0);
    }
    
    // Check UTF-16 LE BOM (common in PowerShell)
    if data.starts_with(b"\xFF\xFE") {
        return (EncodingType::Utf16Le, 1.0);
    }
    
    // Check UTF-16 BE BOM
    if data.starts_with(b"\xFE\xFF") {
        return (EncodingType::Utf16Be, 1.0);
    }
    
    // Check UTF-8 BOM
    if data.starts_with(b"\xEF\xBB\xBF") {
        return (EncodingType::Utf8, 1.0);
    }
    
    // Detect UTF-16 LE pattern (ASCII characters + 00 bytes)
    let utf16le_patterns = detect_utf16le_patterns(data);
    if utf16le_patterns > data.len() / 4 {
        return (EncodingType::Utf16Le, 0.9);
    }
    
    // Check if valid UTF-8
    let mut valid_utf8_count = 0;
    let mut total_chars = 0;
    let mut i = 0;
    
    while i < data.len() {
        if data[i] < 0x80 {
            valid_utf8_count += 1;
            total_chars += 1;
            i += 1;
        } else if i + 1 < data.len() && (data[i] & 0xE0) == 0xC0 {
            // 2-byte UTF-8
            if (data[i + 1] & 0xC0) == 0x80 {
                valid_utf8_count += 1;
                total_chars += 1;
                i += 2;
            } else {
                i += 1;
            }
        } else if i + 2 < data.len() && (data[i] & 0xF0) == 0xE0 {
            // 3-byte UTF-8
            if (data[i + 1] & 0xC0) == 0x80 && (data[i + 2] & 0xC0) == 0x80 {
                valid_utf8_count += 1;
                total_chars += 1;
                i += 3;
            } else {
                i += 1;
            }
        } else if i + 3 < data.len() && (data[i] & 0xF8) == 0xF0 {
            // 4-byte UTF-8
            if (data[i + 1] & 0xC0) == 0x80 && (data[i + 2] & 0xC0) == 0x80 && (data[i + 3] & 0xC0) == 0x80 {
                valid_utf8_count += 1;
                total_chars += 1;
                i += 4;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    
    let utf8_ratio = if total_chars > 0 { valid_utf8_count as f32 / total_chars as f32 } else { 0.0 };
    
    if utf8_ratio > 0.9 {
        return (EncodingType::Utf8, utf8_ratio);
    }
    
    // Detect GBK pattern (common Chinese encoding)
    let gbk_patterns = count_gbk_patterns(data);
    if gbk_patterns > data.len() / 4 {
        return (EncodingType::Gbk, 0.8);
    }
    
    // Check if mainly printable ASCII
    let printable_ascii = data.iter().filter(|&&b| b >= 0x20 && b <= 0x7E).count();
    let printable_ratio = printable_ascii as f32 / data.len() as f32;
    
    if printable_ratio > 0.7 {
        return (EncodingType::Latin1, printable_ratio);
    }
    
    // Detect binary data
    let null_bytes = data.iter().filter(|&&b| b == 0).count();
    let high_bytes = data.iter().filter(|&&b| b > 0x80).count();
    
    if null_bytes > data.len() / 10 || high_bytes > data.len() / 2 {
        return (EncodingType::Binary, 0.9);
    }
    
    // Default to Latin1
    (EncodingType::Latin1, 0.5)
}

/// Detect UTF-16 LE pattern (ASCII characters + 00 bytes)
fn detect_utf16le_patterns(data: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    
    while i + 1 < data.len() {
        // ASCII character + 00 byte pattern
        if data[i] >= 0x20 && data[i] <= 0x7E && data[i + 1] == 0x00 {
            count += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    
    count
}

/// Decode UTF-16 LE encoding
fn decode_utf16le(data: &[u8]) -> String {
    if data.len() % 2 != 0 {
        // If not even length, try removing the last byte first
        return decode_utf16le(&data[..data.len() - 1]);
    }
    
    let u16_data: Vec<u16> = data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    
    String::from_utf16(&u16_data).unwrap_or_else(|_| {
        // If UTF-16 decoding fails, fall back to displaying readable characters
        let mut result = String::new();
        for chunk in data.chunks_exact(2) {
            let code_unit = u16::from_le_bytes([chunk[0], chunk[1]]);
            if code_unit < 0x80 && code_unit >= 0x20 {
                result.push(code_unit as u8 as char);
            } else if code_unit == 0 {
                // Null character, skip
                continue;
            } else {
                result.push_str(&format!("[U+{:04X}]", code_unit));
            }
        }
        result
    })
}

/// Decode UTF-16 BE encoding
fn decode_utf16be(data: &[u8]) -> String {
    if data.len() % 2 != 0 {
        // If not even length, try removing the last byte first
        return decode_utf16be(&data[..data.len() - 1]);
    }
    
    let u16_data: Vec<u16> = data
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();
    
    String::from_utf16(&u16_data).unwrap_or_else(|_| {
        // If UTF-16 decoding fails, fall back to displaying readable characters
        let mut result = String::new();
        for chunk in data.chunks_exact(2) {
            let code_unit = u16::from_be_bytes([chunk[0], chunk[1]]);
            if code_unit < 0x80 && code_unit >= 0x20 {
                result.push(code_unit as u8 as char);
            } else if code_unit == 0 {
                // Null character, skip
                continue;
            } else {
                result.push_str(&format!("[U+{:04X}]", code_unit));
            }
        }
        result
    })
}

/// Simple GBK pattern detection
fn count_gbk_patterns(data: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    
    while i < data.len().saturating_sub(1) {
        let high = data[i];
        let low = data[i + 1];
        
        // GBK double-byte character range
        if (high >= 0x81 && high <= 0xFE) && (low >= 0x40 && low <= 0xFE && low != 0x7F) {
            count += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    
    count
}

/// Improved GBK decoding, preserve more readable information
fn decode_gbk_simple(data: &[u8]) -> String {
    let mut result = String::new();
    let mut i = 0;
    let mut gbk_chars = 0;
    let mut total_processed = 0;
    
    while i < data.len() {
        if data[i] < 0x80 {
            // ASCII character, add directly
            result.push(data[i] as char);
            i += 1;
            total_processed += 1;
        } else if i + 1 < data.len() {
            let high = data[i];
            let low = data[i + 1];
            
            if (high >= 0x81 && high <= 0xFE) && (low >= 0x40 && low <= 0xFE && low != 0x7F) {
                // GBK double-byte character
                gbk_chars += 1;
                
                // Try to identify common PowerShell error patterns
                if is_likely_powershell_error(high, low) {
                    result.push_str("[PS-ERROR]");
                } else if is_likely_chinese_char(high, low) {
                    // Chinese character, describe in Chinese
                    result.push_str("[中文]");
                } else {
                    // Other GBK characters, show encoding info
                    result.push_str(&format!("[GBK:{:02X}{:02X}]", high, low));
                }
                i += 2;
                total_processed += 2;
            } else {
                // Unrecognized double-byte, show raw info
                result.push_str(&format!("[{:02X}{:02X}]", high, low));
                i += 1;
                total_processed += 1;
            }
        } else {
            // Remaining single byte
            result.push_str(&format!("[{:02X}]", data[i]));
            i += 1;
            total_processed += 1;
        }
    }
    
    // If too many GBK characters, add statistics
    if gbk_chars > 5 {
        result.push_str(&format!("\n[Encoding info: {} GBK characters, total bytes:{}]", gbk_chars, total_processed));
    }
    
    result
}

/// Detect if likely PowerShell error information
fn is_likely_powershell_error(high: u8, low: u8) -> bool {
    // Common PowerShell error byte patterns (based on experience)
    matches!((high, low), 
        (0x57, 0x69) | // "Wi" (Windows)
        (0x50, 0x6F) | // "Po" (PowerShell)
        (0x45, 0x72) | // "Er" (Error)
        (0x65, 0x78) | // "ex" (exception)
        (0x43, 0x6F) | // "Co" (Console)
        (0x4F, 0x75) | // "Ou" (Output)
        (0x45, 0x6E) | // "En" (Encoding)
        (0x54, 0x65) | // "Te" (Text)
        (0x46, 0x61) | // "Fa" (Failed)
        (0x4E, 0x6F) | // "No" (Not)
        (0x43, 0x61) | // "Ca" (Cannot)
        (0x55, 0x6E) | // "Un" (Unknown)
        (0x52, 0x65) | // "Re" (Required)
        (0x4D, 0x65) | // "Me" (Message)
        (0x53, 0x79) | // "Sy" (System)
        (0x54, 0x79) | // "Ty" (Type)
        (0x46, 0x6F) | // "Fo" (For)
        (0x49, 0x6E) | // "In" (In)
        (0x41, 0x74) | // "At" (At)
        (0x42, 0x79) | // "By" (By)
        (0x54, 0x68) | // "Th" (The)
        (0x4F, 0x66) | // "Of" (Of)
        (0x41, 0x6E) | // "An" (And)
        (0x4F, 0x72) | // "Or" (Or)
        (0x49, 0x73) | // "Is" (Is)
        (0x49, 0x74) | // "It" (It)
        (0x54, 0x6F)   // "To" (To)
    )
}

/// Detect if likely Chinese character
fn is_likely_chinese_char(high: u8, low: u8) -> bool {
    // GB2312 Chinese character range
    (high >= 0xB0 && high <= 0xF7) && (low >= 0xA1 && low <= 0xFE)
}

/// Shell command execution tool with sandboxing
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self::new_with_syscall_detector(security, runtime, None)
    }

    pub fn new_with_syscall_detector(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
    ) -> Self {
        Self {
            security,
            runtime,
            syscall_detector,
        }
    }
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(super) fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

fn extract_command_argument(args: &serde_json::Value) -> Option<String> {
    if let Some(command) = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
    {
        return Some(command.to_string());
    }

    for alias in [
        "cmd",
        "script",
        "shell_command",
        "command_line",
        "bash",
        "sh",
        "input",
    ] {
        if let Some(command) = args
            .get(alias)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|cmd| !cmd.is_empty())
        {
            return Some(command.to_string());
        }
    }

    args.as_str()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
        .map(ToString::to_string)
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        let current_os = std::env::consts::OS;
        if current_os == "windows" {
            "Execute PowerShell commands on Windows with UTF-8 encoding support for Chinese characters. \
            Supports file operations, system management, web requests, and PowerShell-specific cmdlets. \
            Automatically handles encoding issues for better Chinese character display."
        } else {
            "Execute bash/sh commands on Linux/macOS. Supports standard Unix utilities, file operations, \
            process management, and shell scripting. Uses the system default shell interpreter."
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let current_os = std::env::consts::OS;
        let (shell_name, examples) = if current_os == "windows" {
            (
                "PowerShell",
                vec![
                    "Get-ChildItem",
                    "Get-Content file.txt",
                    "Invoke-WebRequest -Uri 'https://api.example.com' -UseBasicParsing",
                    "Get-Process | Where-Object {$_.CPU -gt 100}",
                    "$PSVersionTable",
                    "Get-ExecutionPolicy"
                ]
            )
        } else {
            (
                "bash/sh",
                vec![
                    "ls -la",
                    "cat file.txt",
                    "curl https://api.example.com",
                    "ps aux | grep process",
                    "echo $SHELL",
                    "which python3"
                ]
            )
        };
        
        let description = format!(
            "The {} command to execute. \
            \\n\\nPlatform: {} \
            \\n\\nExamples:\\n{} \
            \\n\\nNote: On Windows, this tool uses PowerShell with UTF-8 encoding support for Chinese characters. \
            On Linux/macOS, it uses bash/sh.",
            shell_name,
            current_os,
            examples.iter().map(|ex| format!("  - {}", ex)).collect::<Vec<_>>().join("\\n")
        );
        
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": description
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk commands in supervised mode",
                    "default": false
                }
            },
            "required": ["command"]
        })
    }

    #[allow(clippy::incompatible_msrv)]
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = extract_command_argument(&args)
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;
        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        match self.security.validate_command_execution(&command, approved) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        if let Some(path) = self.security.forbidden_path_argument(&command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Execute with timeout to prevent hanging commands.
        // Clear the environment to prevent leaking API keys and other secrets
        // (CWE-200), then re-add only safe, functional variables.
        let mut cmd = match self
            .runtime
            .build_shell_command(&command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                println!("\nFailed to build runtime command: {e}\n");
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };
        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        let result =
            tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                // Choose appropriate encoding handling based on operating system
                let (mut stdout, mut stderr) = if std::env::consts::OS == "windows" {
                    // Try multiple encoding methods on Windows
                    let stdout = decode_windows_output(&output.stdout);
                    let stderr = decode_windows_output(&output.stderr);
                    (stdout, stderr)
                } else {
                    // Unix/Linux systems use UTF-8
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    (stdout, stderr)
                };

                // Truncate output to prevent OOM
                if stdout.len() > MAX_OUTPUT_BYTES {
                    truncate_utf8_to_max_bytes(&mut stdout, MAX_OUTPUT_BYTES);
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    truncate_utf8_to_max_bytes(&mut stderr, MAX_OUTPUT_BYTES);
                    stderr.push_str("\n... [stderr truncated at 1MB]");
                }
                if let Some(detector) = &self.syscall_detector {
                    let _ = detector.inspect_command_output(
                        &command,
                        &stdout,
                        &stderr,
                        output.status.code(),
                    );
                }

                // Detailed logging of command execution results for debugging
                let exit_code = output.status.code().unwrap_or(-1);
                
                // Detect encoding issues
                let has_encoding_issues = stderr.contains('■') || stderr.contains('�') || 
                                          stderr.chars().any(|c| c as u32 > 0xFFFD);
                
                if has_encoding_issues {
                    println!("Command '{}' produced output with potential encoding issues (exit code: {})", command, exit_code);
                    println!("Warning: Command output contains encoding artifacts. Raw stderr (hex): {:02x?}", &output.stderr[..output.stderr.len().min(100)]);
                }
                
                println!(
                    "Shell command executed: '{}' | Exit code: {} | stdout: {} bytes | stderr: {} bytes | encoding_issues: {}",
                    command, exit_code, stdout.len(), stderr.len(), has_encoding_issues
                );
                
                // Display exit code explanation
                if let Some(explanation) = get_exit_code_explanation(exit_code) {
                    println!("Exit code explanation: {}", explanation);
                }
                
                if !stderr.is_empty() {
                    println!("Command produced stderr output: {}", &stderr);
                }
                
                if !output.status.success() && stderr.is_empty() {
                    println!("Command failed without stderr output: '{}' (exit code: {})", command, exit_code);
                    
                    // Provide suggestions for PowerShell-specific errors
                    if std::env::consts::OS == "windows" && exit_code == -65536 {
                        println!("PowerShell -65536 error suggestions:");
                        println!("  1. Check execution policy: Get-ExecutionPolicy");
                        println!("  2. Try running as administrator");
                        println!("  3. Check PowerShell version: $PSVersionTable");
                        println!("  4. Verify command syntax is correct");
                        println!("  5. Check if antivirus software is blocking execution");
                    }
                }

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => {
                println!("\nFailed to execute command: {e}\n");
                return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            })},
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Command timed out after {SHELL_TIMEOUT_SECS}s and was killed"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuditConfig, SyscallAnomalyConfig};
    use crate::runtime::{NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy, SyscallAnomalyDetector};
    use tempfile::TempDir;

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    fn test_syscall_detector(tmp: &TempDir) -> Arc<SyscallAnomalyDetector> {
        let log_path = tmp.path().join("shell-syscall-anomalies.log");
        let cfg = SyscallAnomalyConfig {
            baseline_syscalls: vec!["read".into(), "write".into()],
            log_path: log_path.to_string_lossy().to_string(),
            alert_cooldown_secs: 1,
            max_alerts_per_minute: 50,
            ..SyscallAnomalyConfig::default()
        };
        let audit = AuditConfig {
            enabled: false,
            ..AuditConfig::default()
        };
        Arc::new(SyscallAnomalyDetector::new(cfg, tmp.path(), audit))
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(schema["required"]
            .as_array()
            .expect("schema required field should be an array")
            .contains(&json!("command")));
        assert!(schema["properties"]["approved"].is_object());
    }

    #[test]
    fn extract_command_argument_supports_aliases() {
        assert_eq!(
            extract_command_argument(&json!({"cmd": "echo from-cmd"})).as_deref(),
            Some("echo from-cmd")
        );
        assert_eq!(
            extract_command_argument(&json!({"script": "echo from-script"})).as_deref(),
            Some("echo from-script")
        );
        assert_eq!(
            extract_command_argument(&json!("echo from-string")).as_deref(),
            Some("echo from-string")
        );
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn shell_executes_command_from_cmd_alias() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"cmd": "echo alias"}))
            .await
            .expect("cmd alias execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("alias"));
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "rm -rf /"}))
            .await
            .expect("disallowed command execution should return a result");
        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("not allowed") || error.contains("high-risk"));
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool
            .execute(json!({"command": "ls"}))
            .await
            .expect("readonly command execution should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_ref()
            .expect("error field should be present for blocked command")
            .contains("not allowed"));
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .expect("command with nonexistent path should return a result");
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_blocks_absolute_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat /etc/passwd"}))
            .await
            .expect("absolute path argument should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_option_assignment_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "grep --file=/etc/passwd root ./src"}))
            .await
            .expect("option-assigned forbidden path should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_short_option_attached_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "grep -f/etc/passwd root ./src"}))
            .await
            .expect("short option attached forbidden path should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_tilde_user_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat ~root/.ssh/id_rsa"}))
            .await
            .expect("tilde-user path should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_input_redirection_path_bypass() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat </etc/passwd"}))
            .await
            .expect("input redirection bypass should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into(), "echo".into()],
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => std::env::set_var(self.key, val),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("ZEROCLAW_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "ZEROCLAW_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home_for_env_command() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command should succeed");
        assert!(result.success);
        assert!(
            result.output.contains("HOME="),
            "HOME should be available in shell environment"
        );
        assert!(
            result.output.contains("PATH="),
            "PATH should be available in shell environment"
        );
    }

    #[tokio::test]
    async fn shell_blocks_plain_variable_expansion() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .expect("plain variable expansion should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("ZEROCLAW_TEST_PASSTHROUGH", "db://unit-test");
        let tool = ShellTool::new(
            test_security_with_env_passthrough(&["ZEROCLAW_TEST_PASSTHROUGH"]),
            test_runtime(),
        );

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(result
            .output
            .contains("ZEROCLAW_TEST_PASSTHROUGH=db://unit-test"));
    }

    #[test]
    fn invalid_shell_env_passthrough_names_are_filtered() {
        let security = SecurityPolicy {
            shell_env_passthrough: vec![
                "VALID_NAME".into(),
                "BAD-NAME".into(),
                "1NOPE".into(),
                "ALSO_VALID".into(),
            ],
            ..SecurityPolicy::default()
        };
        let vars = collect_allowed_shell_env_vars(&security);
        assert!(vars.contains(&"VALID_NAME".to_string()));
        assert!(vars.contains(&"ALSO_VALID".to_string()));
        assert!(!vars.contains(&"BAD-NAME".to_string()));
        assert!(!vars.contains(&"1NOPE".to_string()));
    }

    #[tokio::test]
    async fn shell_requires_approval_for_medium_risk_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["touch".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security.clone(), test_runtime());
        let denied = tool
            .execute(json!({"command": "touch zeroclaw_shell_approval_test"}))
            .await
            .expect("unapproved command should return a result");
        assert!(!denied.success);
        assert!(denied
            .error
            .as_deref()
            .unwrap_or("")
            .contains("explicit approval"));

        let allowed = tool
            .execute(json!({
                "command": "touch zeroclaw_shell_approval_test",
                "approved": true
            }))
            .await
            .expect("approved command execution should succeed");
        assert!(allowed.success);

        let _ =
            tokio::fs::remove_file(std::env::temp_dir().join("zeroclaw_shell_approval_test")).await;
    }

    // ── §5.2 Shell timeout enforcement tests ─────────────────

    #[test]
    fn shell_timeout_constant_is_reasonable() {
        assert_eq!(SHELL_TIMEOUT_SECS, 60, "shell timeout must be 60 seconds");
    }

    #[test]
    fn shell_output_limit_is_1mb() {
        assert_eq!(
            MAX_OUTPUT_BYTES, 1_048_576,
            "max output must be 1 MB to prevent OOM"
        );
    }

    // ── §5.3 Non-UTF8 binary output tests ────────────────────

    #[test]
    fn shell_safe_env_vars_excludes_secrets() {
        for var in SAFE_ENV_VARS {
            let lower = var.to_lowercase();
            assert!(
                !lower.contains("key") && !lower.contains("secret") && !lower.contains("token"),
                "SAFE_ENV_VARS must not include sensitive variable: {var}"
            );
        }
    }

    #[test]
    fn shell_safe_env_vars_includes_essentials() {
        assert!(
            SAFE_ENV_VARS.contains(&"PATH"),
            "PATH must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"HOME"),
            "HOME must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"TERM"),
            "TERM must be in safe env vars"
        );
    }

    #[tokio::test]
    async fn shell_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "echo test"}))
            .await
            .expect("rate-limited command should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn shell_handles_nonexistent_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime());
        let result = tool
            .execute(json!({"command": "cat __nonexistent_stderr_capture_file__"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|msg| !msg.trim().is_empty()),
            "expected non-empty stderr in error field"
        );
    }

    #[tokio::test]
    async fn shell_record_action_budget_exhaustion() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 1,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let r1 = tool
            .execute(json!({"command": "echo first"}))
            .await
            .unwrap();
        assert!(r1.success);

        let r2 = tool
            .execute(json!({"command": "echo second"}))
            .await
            .unwrap();
        assert!(!r2.success);
        assert!(
            r2.error.as_deref().unwrap_or("").contains("Rate limit")
                || r2.error.as_deref().unwrap_or("").contains("budget")
        );
    }

    #[tokio::test]
    async fn shell_syscall_detector_writes_anomaly_log() {
        let tmp = tempfile::tempdir().expect("temp dir should be created");
        let log_path = tmp.path().join("shell-syscall-anomalies.log");
        let detector = test_syscall_detector(&tmp);
        let tool = ShellTool::new_with_syscall_detector(
            test_security(AutonomyLevel::Full),
            test_runtime(),
            Some(detector),
        );

        let result = tool
            .execute(json!({"command": "echo seccomp denied syscall=openat"}))
            .await
            .expect("command execution should return result");
        assert!(result.success);
        assert!(result.output.contains("openat"));

        let log = tokio::fs::read_to_string(&log_path)
            .await
            .expect("syscall anomaly log should be written");
        assert!(log.contains("\"kind\":\"unknown_syscall\""));
        assert!(log.contains("\"syscall\":\"openat\""));
    }
}
