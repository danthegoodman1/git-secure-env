//! Dotenv parsing and stacks: `gse run` and `gse export`.

use crate::config::RepoConfig;
use crate::git::Repo;
use crate::sync::Ctx;
use crate::ui;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct Var {
    pub key: String,
    pub value: String,
    /// `KEY= #personal`: each developer supplies the value in a later layer.
    pub personal: bool,
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Byte offset of the closing quote, honoring backslash escapes in `"`.
fn find_close(s: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote == '"' {
            escaped = true;
        } else if c == quote {
            return Some(i);
        }
    }
    None
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// An unquoted value's inline comment starts at a `#` after whitespace.
fn comment_start(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    (1..bytes.len()).find(|&i| bytes[i] == b'#' && bytes[i - 1].is_ascii_whitespace())
}

pub fn parse(text: &str) -> Vec<Var> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_start();
        i += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !valid_key(key) {
            continue;
        }
        let trimmed = raw.trim_start();
        let quote = trimmed
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'' | '`'));
        let (value, comment) = match quote {
            Some(q) => {
                let mut body = String::new();
                let mut rest = &trimmed[1..];
                let mut tail = String::new();
                loop {
                    if let Some(end) = find_close(rest, q) {
                        body.push_str(&rest[..end]);
                        tail = rest[end + 1..].to_string();
                        break;
                    }
                    body.push_str(rest);
                    if i >= lines.len() {
                        break;
                    }
                    body.push('\n');
                    rest = lines[i];
                    i += 1;
                }
                let value = if q == '"' { unescape(&body) } else { body };
                (value, tail)
            }
            None => match comment_start(raw) {
                Some(k) => (raw[..k].trim().to_string(), raw[k..].to_string()),
                None => (raw.trim().to_string(), String::new()),
            },
        };
        let personal = comment.trim_start().starts_with("#personal");
        out.push(Var {
            key: key.to_string(),
            value,
            personal,
        });
    }
    out
}

pub struct Layered {
    pub vars: Vec<(String, String)>,
    /// `#personal` keys no layer gave a value.
    pub missing_personal: Vec<String>,
}

/// Layers files in order; later files override earlier ones.
pub fn layer(texts: &[String]) -> Layered {
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut needs: BTreeSet<String> = BTreeSet::new();
    for text in texts {
        for var in parse(text) {
            if var.personal && var.value.is_empty() {
                if !index.contains_key(&var.key) {
                    needs.insert(var.key);
                }
                continue;
            }
            needs.remove(&var.key);
            match index.get(&var.key) {
                Some(&i) => vars[i].1 = var.value,
                None => {
                    index.insert(var.key.clone(), vars.len());
                    vars.push((var.key, var.value));
                }
            }
        }
    }
    Layered {
        vars,
        missing_personal: needs.into_iter().collect(),
    }
}

fn rel_to_root(root: &Path, path: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok()?;
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    let rel = parent.strip_prefix(&root).ok()?.join(path.file_name()?);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Loads a stack: each file from the working tree, or decrypted from `.gse/`
/// when the plaintext is absent (as in CI).
pub fn resolve(stack: &str) -> Result<Layered> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover()?;
    let ctx = match &repo {
        Some(r) => Ctx::open(Repo::at(&r.root))?,
        None => None,
    };
    let names = match &ctx {
        Some(ctx) => ctx.cfg.stack(stack)?,
        None => RepoConfig::default().stack(stack)?,
    };
    let mut texts = Vec::new();
    for name in &names {
        let path = cwd.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            texts.push(String::from_utf8_lossy(&bytes).into_owned());
            continue;
        }
        let Some(ctx) = &ctx else { continue };
        let Some(rel) = rel_to_root(&ctx.repo.root, &path) else {
            continue;
        };
        let Some(entry) = ctx.entry_for_plain(&rel) else {
            continue;
        };
        let Ok(cipher) = std::fs::read(ctx.abs(&entry.cipher)) else {
            continue;
        };
        match ctx.decrypt(&cipher)? {
            Some(d) => texts.push(String::from_utf8_lossy(&d.content).into_owned()),
            None => ui::warn(format!("{name}: your keys cannot open {}", entry.cipher)),
        }
    }
    let layered = layer(&texts);
    if !layered.missing_personal.is_empty() {
        ui::warn(format!(
            "no layer sets these #personal keys: {}",
            layered.missing_personal.join(", ")
        ));
    }
    Ok(layered)
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ExportFormat {
    Dotenv,
    Shell,
    Json,
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn render(vars: &[(String, String)], format: ExportFormat) -> String {
    match format {
        ExportFormat::Dotenv => vars
            .iter()
            .map(|(k, v)| {
                let v = v
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                format!("{k}=\"{v}\"\n")
            })
            .collect(),
        ExportFormat::Shell => vars
            .iter()
            .map(|(k, v)| format!("export {k}='{}'\n", v.replace('\'', "'\\''")))
            .collect(),
        ExportFormat::Json => {
            let body: Vec<String> = vars
                .iter()
                .map(|(k, v)| format!("  {}: {}", json_escape(k), json_escape(v)))
                .collect();
            format!("{{\n{}\n}}\n", body.join(",\n"))
        }
    }
}

pub fn run(stack: &str, command: &[String]) -> Result<i32> {
    let Some((program, args)) = command.split_first() else {
        bail!("name a command to run, for example `gse run -- npm test`");
    };
    let layered = resolve(stack)?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).envs(layered.vars);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        Err(err).with_context(|| format!("cannot run {program}"))
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .with_context(|| format!("cannot run {program}"))?;
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(text: &str) -> Vec<(String, String, bool)> {
        parse(text)
            .into_iter()
            .map(|v| (v.key, v.value, v.personal))
            .collect()
    }

    #[test]
    fn parses_common_forms() {
        let text = "# comment\nA=1\nexport B=two words # note\nC=\"x\\ny\" #personal\nD='lit\\n'\nE=#notcomment\nF=\"multi\nline\"\nG= #personal\n bad key=1\n";
        assert_eq!(
            kv(text),
            vec![
                ("A".into(), "1".into(), false),
                ("B".into(), "two words".into(), false),
                ("C".into(), "x\ny".into(), true),
                ("D".into(), "lit\\n".into(), false),
                ("E".into(), "#notcomment".into(), false),
                ("F".into(), "multi\nline".into(), false),
                ("G".into(), "".into(), true),
            ]
        );
    }

    #[test]
    fn layers_override_and_track_personal() {
        let l = layer(&[
            "A=1\nB=1\nDB= #personal\nAWS= #personal\n".into(),
            "B=2\nDB=postgres://me\n".into(),
        ]);
        assert_eq!(
            l.vars,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "2".into()),
                ("DB".into(), "postgres://me".into()),
            ]
        );
        assert_eq!(l.missing_personal, vec!["AWS".to_string()]);
    }

    #[test]
    fn render_formats() {
        let vars = vec![("A".to_string(), "it's \"x\"\n".to_string())];
        assert_eq!(
            render(&vars, ExportFormat::Dotenv),
            "A=\"it's \\\"x\\\"\\n\"\n"
        );
        assert_eq!(
            render(&vars, ExportFormat::Shell),
            "export A='it'\\''s \"x\"\n'\n"
        );
        assert_eq!(
            render(&vars, ExportFormat::Json),
            "{\n  \"A\": \"it's \\\"x\\\"\\n\"\n}\n"
        );
    }
}
