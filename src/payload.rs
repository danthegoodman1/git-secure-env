//! The encrypted payload: a comment header naming the recipient list, then the
//! file's content. The header is dotenv comments, so `age -d` output still
//! loads as an env file.

use crate::recipients::{Owner, RecipientList};
use anyhow::{Result, anyhow};

pub const BEGIN: &str = "# gse-recipients v1";
pub const END: &str = "# gse-end";

pub fn wrap(list: &RecipientList, content: &[u8]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(BEGIN);
    out.push('\n');
    for owner in &list.owners {
        for key in &owner.keys {
            out.push_str(&format!("# {key} {}\n", owner.label()));
        }
    }
    out.push_str(END);
    out.push('\n');
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(content);
    bytes
}

/// Splits a decrypted payload into its recipient list and content. Data
/// without a header is all content with an empty list.
pub fn unwrap(data: &[u8]) -> Result<(RecipientList, Vec<u8>)> {
    if !data.starts_with(BEGIN.as_bytes()) {
        return Ok((RecipientList::default(), data.to_vec()));
    }
    let mut list = RecipientList::default();
    let mut offset = 0;
    for line in data.split_inclusive(|&b| b == b'\n') {
        offset += line.len();
        let text = std::str::from_utf8(line)
            .map_err(|_| anyhow!("invalid recipient header"))?
            .trim_end();
        if text == BEGIN {
            continue;
        }
        if text == END {
            return Ok((list, data[offset..].to_vec()));
        }
        let entry = text
            .strip_prefix("# ")
            .ok_or_else(|| anyhow!("invalid recipient header line {text:?}"))?;
        let (key, label) = entry
            .rsplit_once(" @")
            .ok_or_else(|| anyhow!("invalid recipient header line {text:?}"))?;
        let parsed = RecipientList::parse(&format!("# @{label}\n{key}\n"))?;
        let incoming = parsed
            .owners
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("invalid recipient header line {text:?}"))?;
        match list
            .owners
            .iter_mut()
            .find(|o: &&mut Owner| o.label() == incoming.label())
        {
            Some(owner) => owner.keys.extend(incoming.keys),
            None => list.owners.push(incoming),
        }
    }
    Err(anyhow!("recipient header has no end line"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipients::OwnerKind;

    #[test]
    fn wrap_unwrap() {
        let mut list = RecipientList::default();
        let mut o = Owner::new(OwnerKind::Key, "dan");
        o.keys
            .push(age::x25519::Identity::generate().to_public().to_string());
        list.upsert(o);
        let data = wrap(&list, b"A=1\n# comment\n");
        let (back, content) = unwrap(&data).unwrap();
        assert_eq!(back, list);
        assert_eq!(content, b"A=1\n# comment\n");
    }

    #[test]
    fn headerless_data_is_content() {
        let (list, content) = unwrap(b"A=1\n").unwrap();
        assert!(list.is_empty());
        assert_eq!(content, b"A=1\n");
    }
}
