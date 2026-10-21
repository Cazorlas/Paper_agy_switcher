use crate::color;

/// Prompt the user for y/N confirmation. Only an explicit "y" or "yes" accepts.
pub(crate) fn confirm_default_no(prompt: &str) -> bool {
    use std::io::{self, Write as _};

    eprint!("{}", color::dim(prompt));
    io::stderr().flush().ok();
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(0) => false,
        Ok(_) => matches!(input.trim().to_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}
