//! Parser for the alias definition file.
//!
//! Format: one alias per line, `name = template`. Blank lines and lines whose
//! first non-blank character is `#` are ignored. Everything after the first `=`
//! is the template, so templates may contain `=` freely (`--crf=20`).

use std::collections::HashMap;

/// One alias definition, borrowing from the file source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The alias name, i.e. the symlink name it will be invoked as.
    pub name: &'a str,
    /// The template source, trimmed.
    pub template: &'a str,
    /// 1-based line number, for error messages.
    pub line: usize,
}

/// A problem with one line of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    /// 1-based line number.
    pub line: usize,
    /// Explanation for the user.
    pub message: String,
}

/// Split a line into name and template, or `None` if it holds no definition.
///
/// :param raw: one line of the file
/// :return: `None` for blank/comment lines, otherwise the halves or an error
fn split_line(raw: &str) -> Option<Result<(&str, &str), &'static str>> {
    let line = raw.trim();
    // Blank lines and comments carry no definition.
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Split at the first `=` only; later ones belong to the template.
    Some(line.split_once('=').map(|(name, template)| (name.trim(), template.trim())).ok_or("expected `name = template`"))
}

/// Check that a name is usable as a file name on `PATH`.
///
/// :param name: the proposed alias name
/// :return: an error message, or `None` if the name is fine
fn name_problem(name: &str) -> Option<String> {
    // The character set is an arbitrary conservative choice: safe in file names
    // and in any shell without quoting. Widen it if you need to.
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._+-".contains(c);
    if name.is_empty() {
        Some("missing alias name before `=`".into())
    } else if let Some(bad) = name.chars().find(|&c| !allowed(c)) {
        Some(format!("alias name `{name}` contains `{bad}`; use letters, digits, `.`, `_`, `+` or `-`"))
    } else if name.starts_with(['-', '.']) {
        // A leading `-` reads as an option; a leading `.` makes a hidden file.
        Some(format!("alias name `{name}` must not start with `-` or `.`"))
    } else {
        None
    }
}

/// Parse and validate the whole file (names only; templates are checked by
/// the caller, which has the template parser).
///
/// :param src: file contents
/// :return: the entries, or every problem found (not just the first)
pub fn parse(src: &str) -> Result<Vec<Entry<'_>>, Vec<FileError>> {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    // Name -> first line it was defined on, to report duplicates usefully.
    let mut seen: HashMap<&str, usize> = HashMap::new();

    for (index, raw) in src.lines().enumerate() {
        let line = index + 1;
        // Skip non-definition lines; record malformed ones and move on.
        let (name, template) = match split_line(raw) {
            None => continue,
            Some(Ok(halves)) => halves,
            Some(Err(message)) => {
                errors.push(FileError { line, message: message.into() });
                continue;
            }
        };
        // Validate the name before considering it for duplicates.
        if let Some(message) = name_problem(name) {
            errors.push(FileError { line, message });
            continue;
        }
        // A duplicate would make lookup order-dependent, which is a trap.
        if let Some(first) = seen.insert(name, line) {
            errors.push(FileError { line, message: format!("alias `{name}` is already defined on line {first}") });
            continue;
        }
        // An empty template has nothing to run.
        if template.is_empty() {
            errors.push(FileError { line, message: format!("alias `{name}` has an empty template") });
            continue;
        }
        entries.push(Entry { name, template, line });
    }
    // Report all problems together so a build fails once, not once per typo.
    if errors.is_empty() { Ok(entries) } else { Err(errors) }
}

/// Find one alias's template without validating the rest of the file.
///
/// Assumption: the file was already validated (build.rs does this for the
/// embedded copy), so a cheap scan is enough and avoids allocating.
///
/// :param src: file contents
/// :param name: alias to look up
/// :return: the template source, if defined
pub fn find<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    src.lines().filter_map(split_line).filter_map(Result::ok).find(|(n, _)| *n == name).map(|(_, template)| template)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries_and_skips_comments() {
        let src = "# comment\n\nll = ls -la\n  enc = ffmpeg -crf=20 {1}  \n";
        let entries = parse(src).unwrap();
        // Templates keep inner `=` and lose surrounding whitespace.
        assert_eq!(entries[0], Entry { name: "ll", template: "ls -la", line: 3 });
        assert_eq!(entries[1], Entry { name: "enc", template: "ffmpeg -crf=20 {1}", line: 4 });
        // Lookup agrees with the full parse.
        assert_eq!(find(src, "enc"), Some("ffmpeg -crf=20 {1}"));
        assert_eq!(find(src, "nope"), None);
    }

    #[test]
    fn reports_every_problem() {
        let src = "no equals here\nbad/name = x\n-x = y\nok = a\nok = b\nempty =\n";
        let lines: Vec<usize> = parse(src).unwrap_err().iter().map(|e| e.line).collect();
        // Line 4 is the only valid one; every other line is reported.
        assert_eq!(lines, vec![1, 2, 3, 5, 6]);
    }
}
