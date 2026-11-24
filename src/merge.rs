//! Three-way merges, the git merge driver, and the diff text conversion.

use crate::crypto;
use crate::git::Repo;
use crate::identity::Identities;
use crate::recipients::RecipientList;
use crate::sync::{Ctx, Scope};
use crate::ui;
use crate::util::read_opt;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

/// Merges line by line. `Err` holds the result with conflict markers labeled
/// `ours_label` and `theirs_label`.
pub fn merge3(
    ancestor: &[u8],
    ours: &[u8],
    theirs: &[u8],
    ours_label: &str,
    theirs_label: &str,
) -> Result<Vec<u8>, Vec<u8>> {
    diffy::merge_bytes(ancestor, ours, theirs)
        .map_err(|conflicted| relabel(&conflicted, ours_label, theirs_label))
}

fn relabel(bytes: &[u8], ours: &str, theirs: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        if trimmed.starts_with(b"<<<<<<<") && trimmed.iter().skip(7).all(|&b| b != b'<') {
            out.extend_from_slice(format!("<<<<<<< {ours}").as_bytes());
        } else if trimmed.starts_with(b">>>>>>>") && trimmed.iter().skip(7).all(|&b| b != b'>') {
            out.extend_from_slice(format!(">>>>>>> {theirs}").as_bytes());
        } else {
            out.extend_from_slice(trimmed);
        }
        if line.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out
}

/// `gse merge-driver %O %A %B %P`: git calls it when both sides changed the
/// same `.age` file. Exit 0 leaves the merged ciphertext in `%A`; exit 1 marks
/// a conflict.
pub fn driver(base: &Path, ours: &Path, theirs: &Path, path: &str) -> Result<i32> {
    let repo = Repo::discover()?.context("not inside a git repository")?;
    let Some(ctx) = Ctx::open(repo)? else {
        ui::error(format!(
            "cannot merge {path}: this repository has no .gse/config.toml"
        ));
        return Ok(1);
    };
    let Some(entry) = Ctx::entry_for_cipher(path) else {
        ui::error(format!(
            "cannot merge {path}: it is outside .gse/users and .gse/shared"
        ));
        return Ok(1);
    };
    let o = std::fs::read(base).unwrap_or_default();
    let a = std::fs::read(ours).context("cannot read our side")?;
    let b = std::fs::read(theirs).context("cannot read their side")?;
    let (Some(da), Some(db)) = (ctx.decrypt(&a)?, ctx.decrypt(&b)?) else {
        let who = match &entry.scope {
            Scope::User(h) => format!("{h}, its owner,"),
            Scope::Shared(s) => format!("a member of {s}"),
        };
        ui::error(format!(
            "cannot merge {path}: your keys cannot open it. Ask {who} to merge it, \
             or keep one side with `git checkout --ours` or `--theirs` and `git add`."
        ));
        return Ok(1);
    };
    let (o_list, o_content) = if o.is_empty() {
        (RecipientList::default(), Vec::new())
    } else {
        match ctx.decrypt(&o)? {
            Some(d) => (d.list, d.content),
            None => (RecipientList::default(), Vec::new()),
        }
    };
    let list = match &entry.scope {
        Scope::User(_) => match &ctx.me {
            Some(me) => me.personal.clone(),
            None => da.list.clone(),
        },
        Scope::Shared(_) => RecipientList::merge3(&o_list, &da.list, &db.list),
    };
    match merge3(&o_content, &da.content, &db.content, "ours", "theirs") {
        Ok(merged) => {
            std::fs::write(ours, crypto::encrypt(&list, &merged)?)?;
            Ok(0)
        }
        Err(conflicted) => {
            let plain_path = ctx.abs(&entry.plain);
            if let Some(current) = read_opt(&plain_path)
                && current != da.content
                && current != db.content
            {
                let saved = ctx.state.backup(&entry.plain, &current)?;
                ui::warn(format!(
                    "saved your unsynced {} to {}",
                    entry.plain,
                    saved.display()
                ));
            }
            ctx.write_plain(&entry, &conflicted)?;
            // Git writes %A, unchanged, to the working tree; the next sync
            // treats the resolved plaintext as a local edit on top of it.
            ctx.state.set_base(&entry.cipher, &a)?;
            ctx.state.set_conflict(&entry.plain, true)?;
            ui::error(format!(
                "conflict in {}: resolve the markers, run `gse sync`, then `git commit` \
                 or `git rebase --continue`",
                entry.plain
            ));
            Ok(1)
        }
    }
}

/// `gse textconv <file>`: decrypted text for `git diff` and `git log -p`.
pub fn textconv(file: &Path) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    let ids = Identities::load()?;
    let mut out = std::io::stdout().lock();
    match crypto::decrypt(&bytes, &ids) {
        Ok(Some(d)) => {
            out.write_all(&crate::payload::wrap(&d.list, &d.content))?;
        }
        _ => writeln!(out, "<encrypted; not a recipient>")?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_merge_of_different_lines() {
        let base = b"A=1\nB=1\nC=1\n";
        let ours = b"A=2\nB=1\nC=1\n";
        let theirs = b"A=1\nB=1\nC=2\n";
        assert_eq!(
            merge3(base, ours, theirs, "local", "incoming").unwrap(),
            b"A=2\nB=1\nC=2\n"
        );
    }

    #[test]
    fn conflict_markers_carry_labels() {
        let out = merge3(b"A=1\n", b"A=2\n", b"A=3\n", "local", "incoming").unwrap_err();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("<<<<<<< local\n"), "{text}");
        assert!(text.contains(">>>>>>> incoming\n"), "{text}");
        assert!(crate::util::has_conflict_markers(text.as_bytes()));
    }
}
