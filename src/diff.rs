use std::fmt::Write;

use similar::TextDiff;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

/// Generate a unified diff between `old_bytes` and `new_bytes`.
/// Returns an empty string if contents are identical, or a concise binary notice if non-UTF-8.
pub fn unified_diff(
    old_bytes: &[u8],
    new_bytes: &[u8],
    old_label: &str,
    new_label: &str,
    color: bool,
) -> String {
    if old_bytes == new_bytes {
        return String::new();
    }

    let (Ok(old_text), Ok(new_text)) = (
        std::str::from_utf8(old_bytes),
        std::str::from_utf8(new_bytes),
    ) else {
        return format_binary_notice(old_label, new_label, color);
    };

    if old_text.contains('\0') || new_text.contains('\0') {
        return format_binary_notice(old_label, new_label, color);
    }

    let diff = TextDiff::from_lines(old_text, new_text);
    let plain = diff
        .unified_diff()
        .context_radius(3)
        .header(old_label, new_label)
        .to_string();

    if !color {
        return plain;
    }

    let mut colored = String::with_capacity(plain.len() + 128);
    for line in plain.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            let _ = writeln!(colored, "{BOLD}{line}{RESET}");
        } else if line.starts_with("@@ ") {
            let _ = writeln!(colored, "{CYAN}{line}{RESET}");
        } else if line.starts_with('+') {
            let _ = writeln!(colored, "{GREEN}{line}{RESET}");
        } else if line.starts_with('-') {
            let _ = writeln!(colored, "{RED}{line}{RESET}");
        } else {
            let _ = writeln!(colored, "{line}");
        }
    }
    colored
}

fn format_binary_notice(old_label: &str, new_label: &str, color: bool) -> String {
    if color {
        format!("{YELLOW}Binary files {old_label} and {new_label} differ{RESET}\n")
    } else {
        format!("Binary files {old_label} and {new_label} differ\n")
    }
}
