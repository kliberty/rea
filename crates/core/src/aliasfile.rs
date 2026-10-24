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

/// Split a line into name and template.
///
/// Returns `None` for a blank or comment line, which holds no definition.
fn split_line(raw: &str) -> Option<Result<(&str, &str), &'static str>> {
    let line = raw.trim();
    // Blank lines and comments carry no definition.
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Split at the first `=` only; later ones belong to the template.
    Some(
        line.split_once('=')
            .map(|(name, template)| (name.trim(), template.trim()))
            .ok_or("expected `name = template`"),
    )
}

/// Check that a name is usable as a file name on `PATH`.
///
/// Returns an error message, or `None` if the name is fine. Used for names from
/// the alias file and from `rea new` alike, so both accept exactly the same
/// names.
pub fn name_problem(name: &str) -> Option<String> {
    // The character set is an arbitrary conservative choice: safe in file names
    // and in any shell without quoting. Widen it if you need to.
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._+-".contains(c);
    if name.is_empty() {
        Some("the alias name is empty".into())
    } else if let Some(bad) = name.chars().find(|&c| !allowed(c)) {
        Some(format!(
            "alias name `{name}` contains `{bad}`; use letters, digits, `.`, `_`, `+` or `-`"
        ))
    } else if name.starts_with(['-', '.']) {
        // A leading `-` reads as an option; a leading `.` makes a hidden file.
        Some(format!(
            "alias name `{name}` must not start with `-` or `.`"
        ))
    } else {
        None
    }
}

/// Parse and validate the whole file (names only; templates are checked by
/// the caller, which has the template parser).
///
/// # Errors
///
/// Every problem found, not just the first.
pub fn parse(src: &str) -> Result<Vec<Entry<'_>>, Vec<FileError>> {
    let (entries, errors) = parse_lenient(src);
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// Parse the file, keeping the valid entries even when other lines have
/// problems, so a caller can go on to check those entries' templates and
/// report everything wrong with the file in one go.
///
/// Returns the valid entries, and a problem per invalid line.
pub fn parse_lenient(src: &str) -> (Vec<Entry<'_>>, Vec<FileError>) {
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
                errors.push(FileError {
                    line,
                    message: message.into(),
                });
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
            errors.push(FileError {
                line,
                message: format!("alias `{name}` is already defined on line {first}"),
            });
            continue;
        }
        // An empty template has nothing to run.
        if template.is_empty() {
            errors.push(FileError {
                line,
                message: format!("alias `{name}` has an empty template"),
            });
            continue;
        }
        entries.push(Entry {
            name,
            template,
            line,
        });
    }
    (entries, errors)
}

/// The comment [`merge`] puts above the aliases it appends.
pub const ADDED_MARKER: &str = "# added by rea save";

/// Append definition lines to an alias file's text, under [`ADDED_MARKER`].
///
/// - A file with nothing but blank lines gets the lines alone, as `rea list`
///   would print them.
/// - If the file already ends with a marker block (its last comment is the
///   marker, followed only by definitions and blank lines), the lines join
///   that block, after its last definition, instead of starting another.
/// - Otherwise a new block starts at the end: a blank line, unless the file
///   already ends with one, then the marker, then the lines.
///
/// Lines end with the file's own line ending, `\r\n` if it uses any.
fn append(text: &mut String, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block: String = lines
        .iter()
        .map(|line| format!("{line}{newline}"))
        .collect();
    if text.trim().is_empty() {
        *text = block;
        return;
    }
    // Split the file into its content, the line ending after the last line
    // with content (none if the file doesn't end with one), and the blank
    // lines after that.
    let tail = text.split_off(text.trim_end().len());
    let (ending, blanks) = ["\r\n", "\n"]
        .iter()
        .find_map(|end| tail.strip_prefix(end).map(|rest| (*end, rest)))
        .unwrap_or((newline, tail.as_str()));
    // The last comment decides: the marker means the file ends with a block
    // of added aliases already, since only definitions can follow it.
    let reuse = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('#'))
        .is_some_and(|comment| comment == ADDED_MARKER);
    text.push_str(ending);
    if reuse {
        // Join the block, keeping the blank lines after it at the end.
        text.push_str(&block);
        text.push_str(blanks);
    } else {
        // The file's own blank lines separate the new block, or one is added.
        if blanks.is_empty() {
            text.push_str(newline);
        } else {
            text.push_str(blanks);
            if !blanks.ends_with('\n') {
                text.push_str(newline);
            }
        }
        text.push_str(ADDED_MARKER);
        text.push_str(newline);
        text.push_str(&block);
    }
}

/// The result of [`merge`]: the new file text and what changed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// The whole file, updated.
    pub text: String,
    /// Aliases whose template was replaced, with their 1-based line, in file
    /// order.
    pub updated: Vec<(String, usize)>,
    /// Aliases appended at the end, in the order given.
    pub added: Vec<String>,
    /// Aliases the file already had exactly.
    pub unchanged: usize,
}

/// Bring an alias file's text up to date with a set of aliases, changing as
/// little of it as possible.
///
/// An alias the file already defines keeps its line. If its template
/// differs, only the template is replaced: everything up to the `=` and the
/// blanks after it stays, so hand alignment survives. Aliases the file
/// doesn't define are appended, one `name = template` line each, in the
/// order given, under an [`ADDED_MARKER`] comment that sets them apart from
/// whatever section the file ends with. Every other byte, comments, blank
/// lines, definitions of aliases not given and line endings included, is
/// copied unchanged.
///
/// Templates are compared and written trimmed, as the file stores them.
/// `aliases` must have unique, valid names and single-line, non-blank
/// templates; the caller checks this.
///
/// # Errors
///
/// Every problem [`parse`] finds in `src`. The file must parse cleanly, so
/// that the result defines exactly what the original did apart from the
/// merged aliases.
pub fn merge(src: &str, aliases: &[(&str, &str)]) -> Result<Merged, Vec<FileError>> {
    let entries = parse(src)?;
    // 1-based line -> the new template for it, for lines that change.
    let mut replacements: HashMap<usize, &str> = HashMap::new();
    let mut updated = Vec::new();
    let mut added = Vec::new();
    let mut unchanged = 0;
    let by_name: HashMap<&str, &Entry> = entries.iter().map(|e| (e.name, e)).collect();
    for &(name, template) in aliases {
        let template = template.trim();
        match by_name.get(name) {
            Some(entry) if entry.template == template => unchanged += 1,
            Some(entry) => {
                replacements.insert(entry.line, template);
                updated.push((name.to_owned(), entry.line));
            }
            None => added.push(name.to_owned()),
        }
    }
    updated.sort_by_key(|(_, line)| *line);

    // Rebuild line by line. `split_inclusive` keeps each line's ending, and
    // numbers lines the same way `str::lines` does in `parse`.
    let mut text = String::with_capacity(src.len());
    for (index, raw) in src.split_inclusive('\n').enumerate() {
        let Some(template) = replacements.get(&(index + 1)) else {
            text.push_str(raw);
            continue;
        };
        // Keep the line ending, `\r\n` or `\n` (or none, on a last line).
        let body = raw.trim_end_matches(['\n', '\r']);
        let ending = &raw[body.len()..];
        // A replaced line is a definition, so it has an `=`. Keep it and the
        // blanks after it, then the new template.
        let eq = body.find('=').expect("a definition line has `=`");
        let after = &body[eq + 1..];
        let blanks = after.len() - after.trim_start().len();
        text.push_str(&body[..eq + 1 + blanks]);
        text.push_str(template);
        text.push_str(ending);
    }
    let new_lines: Vec<String> = aliases
        .iter()
        .filter(|(name, _)| !by_name.contains_key(name))
        .map(|(name, template)| format!("{name} = {}", template.trim()))
        .collect();
    append(&mut text, &new_lines);
    Ok(Merged {
        text,
        updated,
        added,
        unchanged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_replaces_only_the_template() {
        let src = "# mine\nll    = ls -la\n\nenc=ffmpeg {1}\r\nold = keep me\n";
        let merged = merge(src, &[("enc", " ffmpeg -crf=20 {1} "), ("ll", "ls -lah")]).unwrap();
        // Alignment, the missing blank after `=`, CRLF, comments, blank lines
        // and an entry not given are all kept.
        assert_eq!(
            merged.text,
            "# mine\nll    = ls -lah\n\nenc=ffmpeg -crf=20 {1}\r\nold = keep me\n"
        );
        assert_eq!(
            merged.updated,
            vec![("ll".to_owned(), 2), ("enc".to_owned(), 4)]
        );
        assert!(merged.added.is_empty());
    }

    #[test]
    fn merge_appends_new_aliases_and_keeps_identical_ones() {
        // No final newline: one is added before the appended lines.
        let src = "ll = ls -la";
        let merged = merge(
            src,
            &[("ll", "  ls -la "), ("b", "echo b"), ("a", "echo a")],
        )
        .unwrap();
        assert_eq!(
            merged.text,
            "ll = ls -la\n\n# added by rea save\nb = echo b\na = echo a\n"
        );
        assert_eq!(merged.added, vec!["b".to_owned(), "a".to_owned()]);
        assert_eq!(merged.unchanged, 1);
        // An empty file just gets the lines; CRLF files get CRLF lines.
        assert_eq!(merge("", &[("a", "x")]).unwrap().text, "a = x\n");
        assert_eq!(
            merge("# c\r\n", &[("a", "x")]).unwrap().text,
            "# c\r\n\r\n# added by rea save\r\na = x\r\n"
        );
        // Nothing to do leaves the text identical.
        assert_eq!(
            merge("ll = ls -la\n", &[("ll", "ls -la")]).unwrap().text,
            "ll = ls -la\n"
        );
    }

    #[test]
    fn merge_puts_added_aliases_under_one_marker() {
        let add = |src: &str| merge(src, &[("n", "new")]).unwrap().text;
        // A file ending in a blank line uses it as the separator.
        assert_eq!(add("a = x\n\n"), "a = x\n\n# added by rea save\nn = new\n");
        // An existing block at the end is joined, before its trailing blanks.
        assert_eq!(
            add("a = x\n\n# added by rea save\nb = y\n\n"),
            "a = x\n\n# added by rea save\nb = y\nn = new\n\n"
        );
        // ...even when what's left of it is just the marker.
        assert_eq!(
            add("a = x\n\n# added by rea save\n"),
            "a = x\n\n# added by rea save\nn = new\n"
        );
        // A marker followed by another comment no longer ends the file, so a
        // new block starts.
        assert_eq!(
            add("# added by rea save\nb = y\n# video\nc = z\n"),
            "# added by rea save\nb = y\n# video\nc = z\n\n# added by rea save\nn = new\n"
        );
        // A file of blank lines only is treated as empty: no marker.
        assert_eq!(add("\n\n"), "n = new\n");
    }

    #[test]
    fn merge_refuses_files_that_do_not_parse() {
        let src = "a = x\na = y\n";
        let errors = merge(src, &[("a", "z")]).unwrap_err();
        assert_eq!(errors[0].line, 2);
    }

    #[test]
    fn parses_entries_and_skips_comments() {
        let src = "# comment\n\nll = ls -la\n  enc = ffmpeg -crf=20 {1}  \n";
        let entries = parse(src).unwrap();
        // Templates keep inner `=` and lose surrounding whitespace.
        assert_eq!(
            entries[0],
            Entry {
                name: "ll",
                template: "ls -la",
                line: 3
            }
        );
        assert_eq!(
            entries[1],
            Entry {
                name: "enc",
                template: "ffmpeg -crf=20 {1}",
                line: 4
            }
        );
    }

    #[test]
    fn reports_every_problem() {
        let src = "no equals here\nbad/name = x\n-x = y\nok = a\nok = b\nempty =\n";
        let lines: Vec<usize> = parse(src).unwrap_err().iter().map(|e| e.line).collect();
        // Line 4 is the only valid one; every other line is reported.
        assert_eq!(lines, vec![1, 2, 3, 5, 6]);
    }
}
