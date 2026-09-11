use std::process::{Command, Output};

use crate::error::{ClixError, Result};
use crate::types::Grant;

/// Run the granted binary. `argv[0]` must be `grant.tool`. Not a shell.
pub fn run_granted(grant: &Grant, argv: &[String]) -> Result<Output> {
    let argv0 = argv.first().map(String::as_str).unwrap_or("");
    if argv0 != grant.tool {
        return Err(ClixError::Usage(format!("{argv0} is not granted")));
    }
    Ok(Command::new(&grant.binary).args(&argv[1..]).output()?)
}
