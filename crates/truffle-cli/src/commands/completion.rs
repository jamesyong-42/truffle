//! `truffle completion` -- generate shell completions.
//!
//! Uses `clap_complete` to generate completion scripts for bash, zsh, fish,
//! and powershell. Output is printed to stdout so the user can pipe it to
//! the appropriate location.
//!
//! # Usage
//!
//! ```sh
//! # Zsh (one-time setup)
//! truffle completion zsh > ~/.zfunc/_truffle
//! echo 'fpath=(~/.zfunc $fpath); compinit' >> ~/.zshrc
//!
//! # Zsh (session-only)
//! source <(truffle completion zsh)
//!
//! # Bash
//! truffle completion bash > /etc/bash_completion.d/truffle
//!
//! # Fish
//! truffle completion fish > ~/.config/fish/completions/truffle.fish
//! ```

use clap::CommandFactory;
use clap_complete::Shell;

/// Generate shell completions and print to stdout.
pub fn run(shell: Shell) {
    let mut cmd = crate::Cli::command();
    clap_complete::generate(shell, &mut cmd, "truffle", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completion_generation_zsh() {
        // Verify that generating zsh completions does not panic
        // and produces non-empty output.
        let mut cmd = crate::Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Zsh, &mut cmd, "truffle", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("truffle"));
    }

    #[test]
    fn test_completion_generation_bash() {
        let mut cmd = crate::Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Bash, &mut cmd, "truffle", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("truffle"));
    }

    #[test]
    fn test_completion_generation_fish() {
        let mut cmd = crate::Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Fish, &mut cmd, "truffle", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("truffle"));
    }

    #[test]
    fn test_completion_generation_powershell() {
        let mut cmd = crate::Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::PowerShell, &mut cmd, "truffle", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
    }
}
