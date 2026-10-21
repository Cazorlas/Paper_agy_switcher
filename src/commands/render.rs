use crate::color;

/// Pad plain text by terminal display width before applying ANSI styling.
pub(crate) fn padded_cell(text: &str, width: usize, style: impl FnOnce(&str) -> String) -> String {
    let padding = width.saturating_sub(ratatui::text::Line::raw(text).width());
    style(&format!("{text}{}", " ".repeat(padding)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn padded_colour_cell_measures_plain_display_width() {
        let cell = super::padded_cell("界e\u{301}", 6, |text| format!("\x1b[32m{text}\x1b[0m"));
        assert_eq!(cell, "\x1b[32m界e\u{301}   \x1b[0m");
    }
}

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
