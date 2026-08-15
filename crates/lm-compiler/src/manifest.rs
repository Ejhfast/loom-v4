//! The `lm.package` manifest: a strict hand-written TOML subset.
//!
//! The accepted grammar, in full:
//!
//! ```text
//! manifest  = line*
//! line      = blank | comment | table | pair
//! blank     = spaces
//! comment   = spaces "#" any*
//! table     = spaces "[" name "]" spaces comment?
//! pair      = spaces key spaces "=" spaces value spaces comment?
//! value     = string | inline
//! string    = '"' char* '"'          # no escape sequence
//! inline    = "{" spaces "path" spaces "=" spaces string spaces "}"
//! ```
//!
//! Two tables exist: `[package]` and `[dependencies]`. `[package]`
//! comes first and declares `name` and `version`. `[dependencies]`
//! declares one path dependency per line. Anything else rejects with
//! the line number and the exact fix. There is no array, no nested
//! table, no multi-line value, no escape sequence, and no number or
//! boolean literal.
//!
//! Version 0.2 supports path dependencies only. The dependency key is
//! the local name of that package inside this one, so a name clash is
//! fixed by renaming the key.

use std::collections::BTreeMap;

/// One parsed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// Local dependency name to relative path, in declaration order.
    pub dependencies: Vec<(String, String)>,
}

/// A manifest defect with the line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lm.package:{}: {}", self.line, self.message)
    }
}

fn err(line: usize, message: impl Into<String>) -> ManifestError {
    ManifestError {
        line,
        message: message.into(),
    }
}

/// True when the text is a valid lowercase package or module name:
/// a letter, then letters, digits, and underscores.
pub fn valid_name(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// True when the text is `MAJOR.MINOR.PATCH` with decimal digits.
fn valid_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Strip a trailing comment that starts outside a string.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_string = !in_string,
            b'#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Read one basic string. No escape sequence is accepted, so a
/// backslash or a control character rejects.
fn parse_string(line: usize, text: &str) -> Result<String, ManifestError> {
    let text = text.trim();
    if text.len() < 2 || !text.starts_with('"') || !text.ends_with('"') {
        return Err(err(
            line,
            format!("`{text}` is not a quoted string; write a value like \"0.1.0\""),
        ));
    }
    let inner = &text[1..text.len() - 1];
    if inner.contains('"') {
        return Err(err(line, "a string must not contain a quote"));
    }
    if inner.contains('\\') {
        return Err(err(
            line,
            "the manifest accepts no escape sequence; remove the backslash",
        ));
    }
    if inner.chars().any(|c| c.is_control()) {
        return Err(err(line, "a string must not contain a control character"));
    }
    Ok(inner.to_string())
}

/// Read one inline dependency table `{ path = "..." }`.
fn parse_dependency(line: usize, text: &str) -> Result<String, ManifestError> {
    let text = text.trim();
    if !text.starts_with('{') || !text.ends_with('}') {
        return Err(err(
            line,
            "a dependency value is `{ path = \"../name\" }`; version 0.2 \
             supports path dependencies only",
        ));
    }
    let inner = text[1..text.len() - 1].trim();
    let Some((key, value)) = inner.split_once('=') else {
        return Err(err(line, "a dependency value needs `path = \"...\"`"));
    };
    if key.trim() != "path" {
        return Err(err(
            line,
            format!(
                "`{}` is not a dependency key; version 0.2 accepts `path` only",
                key.trim()
            ),
        ));
    }
    parse_string(line, value)
}

/// Parse one manifest. Every rejection names the line and the fix.
pub fn parse_manifest(text: &str) -> Result<Manifest, ManifestError> {
    let mut section: Option<String> = None;
    let mut seen_sections: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut dependencies: Vec<(String, String)> = Vec::new();
    let mut keys: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = idx + 1;
        let content = strip_comment(raw).trim();
        if content.is_empty() {
            continue;
        }
        if let Some(rest) = content.strip_prefix('[') {
            let Some(table) = rest.strip_suffix(']') else {
                return Err(err(line, "a table header ends with `]`"));
            };
            let table = table.trim();
            if table != "package" && table != "dependencies" {
                return Err(err(
                    line,
                    format!(
                        "`[{table}]` is not a manifest table; the manifest has \
                         `[package]` and `[dependencies]`"
                    ),
                ));
            }
            if seen_sections.iter().any(|s| s == table) {
                return Err(err(line, format!("the table `[{table}]` appears twice")));
            }
            if table == "dependencies" && !seen_sections.iter().any(|s| s == "package") {
                return Err(err(line, "`[package]` must come before `[dependencies]`"));
            }
            seen_sections.push(table.to_string());
            section = Some(table.to_string());
            continue;
        }
        let Some(section_name) = section.as_deref() else {
            return Err(err(
                line,
                "a key comes after a table header; start the manifest with `[package]`",
            ));
        };
        let Some((key, value)) = content.split_once('=') else {
            return Err(err(line, "a manifest line is `key = value`"));
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(err(line, "a manifest line needs a key"));
        }
        let full = format!("{section_name}.{key}");
        if let Some(first) = keys.insert(full, line) {
            return Err(err(
                line,
                format!(
                    "the key `{key}` appears twice; line {first} \
                                          declares it already"
                ),
            ));
        }
        match section_name {
            "package" => match key.as_str() {
                "name" => {
                    let value = parse_string(line, value)?;
                    if !valid_name(&value) {
                        return Err(err(
                            line,
                            format!(
                                "`{value}` is not a package name; use a lowercase \
                                 letter, then letters, digits, or underscores"
                            ),
                        ));
                    }
                    name = Some(value);
                }
                "version" => {
                    let value = parse_string(line, value)?;
                    if !valid_version(&value) {
                        return Err(err(
                            line,
                            format!("`{value}` is not a version; write `MAJOR.MINOR.PATCH`"),
                        ));
                    }
                    version = Some(value);
                }
                other => {
                    return Err(err(
                        line,
                        format!(
                            "`{other}` is not a `[package]` key; the table has \
                             `name` and `version`"
                        ),
                    ));
                }
            },
            "dependencies" => {
                if !valid_name(&key) {
                    return Err(err(
                        line,
                        format!(
                            "`{key}` is not a dependency name; use a lowercase \
                             letter, then letters, digits, or underscores"
                        ),
                    ));
                }
                let path = parse_dependency(line, value)?;
                if path.is_empty() {
                    return Err(err(line, "a dependency path must not be empty"));
                }
                dependencies.push((key, path));
            }
            _ => unreachable!("only two tables pass the header check"),
        }
    }
    let Some(name) = name else {
        return Err(err(0, "the manifest needs `[package]` with `name`"));
    };
    let Some(version) = version else {
        return Err(err(0, "the manifest needs `[package]` with `version`"));
    };
    for (dep, _) in &dependencies {
        if *dep == name {
            return Err(err(
                0,
                format!(
                    "the dependency `{dep}` has the name of this package; \
                     rename the dependency key in the manifest"
                ),
            ));
        }
        if dep == "std" || dep == "sys" {
            return Err(err(
                0,
                format!(
                    "`{dep}` is a reserved root name; rename the dependency \
                     key in the manifest"
                ),
            ));
        }
    }
    Ok(Manifest {
        name,
        version,
        dependencies,
    })
}

/// Render one manifest in the canonical scaffold form.
pub fn render_manifest(manifest: &Manifest) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "[package]");
    let _ = writeln!(out, "name = \"{}\"", manifest.name);
    let _ = writeln!(out, "version = \"{}\"", manifest.version);
    if !manifest.dependencies.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "[dependencies]");
        for (name, path) in &manifest.dependencies {
            let _ = writeln!(out, "{name} = {{ path = \"{path}\" }}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_scaffold_form() {
        let text = "[package]\nname = \"hello\"\nversion = \"0.1.0\"\n";
        let manifest = parse_manifest(text).expect("parses");
        assert_eq!(manifest.name, "hello");
        assert_eq!(manifest.version, "0.1.0");
        assert!(manifest.dependencies.is_empty());
        assert_eq!(render_manifest(&manifest), text);
    }

    #[test]
    fn parses_a_path_dependency() {
        let text = "# a comment\n[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                    [dependencies]\nmathlib = { path = \"../mathlib\" }  # here\n";
        let manifest = parse_manifest(text).expect("parses");
        assert_eq!(
            manifest.dependencies,
            vec![("mathlib".to_string(), "../mathlib".to_string())]
        );
    }

    #[test]
    fn rejects_every_defect_with_a_line() {
        let cases = [
            ("name = \"a\"\n", "table header"),
            ("[package]\nname = \"a\"\n", "version"),
            ("[package]\nversion = \"0.1.0\"\n", "name"),
            ("[package]\nname = \"A\"\nversion = \"0.1.0\"\n", "not a package name"),
            ("[package]\nname = \"a\"\nversion = \"1\"\n", "not a version"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"x\"\n", "not a `[package]` key"),
            ("[build]\nname = \"a\"\n", "not a manifest table"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\n[package]\n", "appears twice"),
            ("[dependencies]\na = { path = \"x\" }\n", "before `[dependencies]`"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = \"1.0\"\n", "path dependencies only"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = { git = \"x\" }\n", "accepts `path` only"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\na = { path = \"x\" }\n", "rename the dependency key"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nstd = { path = \"x\" }\n", "reserved root name"),
            ("[package]\nname = \"a\"\nversion = \"0.1.0\"\nname = \"b\"\n", "appears twice"),
            ("[package]\nname = a\nversion = \"0.1.0\"\n", "not a quoted string"),
            ("[package]\nname = \"a\\\\b\"\nversion = \"0.1.0\"\n", "escape sequence"),
        ];
        for (text, needle) in cases {
            let error = parse_manifest(text).expect_err(&format!("accepted: {text:?}"));
            assert!(
                error.message.contains(needle),
                "case {text:?}: expected `{needle}`, found `{}`",
                error.message
            );
        }
    }
}
