//! `truffle completion` -- generate shell completions.

use clap::CommandFactory;
use clap_complete::Shell;

/// Generate shell completions and print to stdout.
pub fn run(shell: Shell) {
    let mut cmd = crate::Cli::command();
    clap_complete::generate(shell, &mut cmd, "truffle", &mut std::io::stdout());
}
