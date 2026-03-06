use std::io::Write;

/// Shell completion targets supported by ZeroClaw.
#[derive(Copy, Clone, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum CompletionShell {
    #[value(name = "bash")]
    Bash,
    #[value(name = "fish")]
    Fish,
    #[value(name = "zsh")]
    Zsh,
    #[value(name = "powershell")]
    PowerShell,
    #[value(name = "elvish")]
    Elvish,
}

/// Write a shell completion script for `zeroclaw` to the given writer.
///
/// The script is printed to stdout so it can be sourced directly:
///
/// # Examples
///
/// ```no_run
/// use zeroclaw::cli::completions::{CompletionShell, write_shell_completion};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut stdout = std::io::stdout().lock();
/// write_shell_completion(CompletionShell::Bash, &mut stdout)?;
/// # Ok(())
/// # }
/// ```
///
/// # Command-line Examples
///
/// ```bash
/// # Source directly:
/// source <(zeroclaw completions bash)
///
/// # Or write to a file:
/// zeroclaw completions zsh > ~/.zfunc/_zeroclaw
/// ```
pub fn write_shell_completion<W: Write>(
    shell: CompletionShell,
    writer: &mut W,
) -> Result<(), std::io::Error> {
    use clap_complete::generate;
    use clap_complete::shells;

    // We need to build the Cli struct here for clap_complete
    // Import it from the binary to keep a single source of truth
    let mut cmd = crate::Cli::command();
    let bin_name = cmd.get_name().to_string();

    match shell {
        CompletionShell::Bash => generate(shells::Bash, &mut cmd, bin_name.clone(), writer),
        CompletionShell::Fish => generate(shells::Fish, &mut cmd, bin_name.clone(), writer),
        CompletionShell::Zsh => generate(shells::Zsh, &mut cmd, bin_name.clone(), writer),
        CompletionShell::PowerShell => {
            generate(shells::PowerShell, &mut cmd, bin_name.clone(), writer);
        }
        CompletionShell::Elvish => generate(shells::Elvish, &mut cmd, bin_name, writer),
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_shell_variants_exist() {
        let variants = vec![
            CompletionShell::Bash,
            CompletionShell::Fish,
            CompletionShell::Zsh,
            CompletionShell::PowerShell,
            CompletionShell::Elvish,
        ];

        for variant in variants {
            // Verify each variant can be created
            let _ = format!("{:?}", variant);
        }
    }

    #[test]
    fn write_completion_to_buffer() {
        let mut buffer = Vec::new();
        let result = write_shell_completion(CompletionShell::Bash, &mut buffer);

        assert!(result.is_ok());
        assert!(!buffer.is_empty());
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("zeroclaw"));
    }
}
