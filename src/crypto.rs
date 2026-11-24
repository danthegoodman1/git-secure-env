//! age encryption with ASCII armor around the gse payload.

use crate::identity::Identities;
use crate::payload;
use crate::recipients::RecipientList;
use age::armor::{ArmoredReader, ArmoredWriter, Format};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use std::io::{BufRead, Read, Write};

pub struct Decrypted {
    pub list: RecipientList,
    pub content: Vec<u8>,
}

pub fn encrypt(list: &RecipientList, content: &[u8]) -> Result<Vec<u8>> {
    let recipients = list.recipients()?;
    if recipients.is_empty() {
        bail!("no recipients to encrypt to");
    }
    let encryptor = age::Encryptor::with_recipients(
        recipients.iter().map(|r| r.as_ref() as &dyn age::Recipient),
    )
    .context("cannot encrypt to these recipients")?;
    let mut out = Vec::new();
    let armor = ArmoredWriter::wrap_output(&mut out, Format::AsciiArmor)?;
    let mut writer = encryptor.wrap_output(armor)?;
    writer.write_all(&payload::wrap(list, content))?;
    writer.finish()?.finish()?;
    Ok(out)
}

/// Decrypts with any identity that matches. `Ok(None)` means no identity on
/// this machine is a recipient.
///
/// Keys that need no passphrase go first; `~/.ssh` keys load only when they
/// are needed and their tag matches one of the file's stanzas.
pub fn decrypt(cipher: &[u8], ids: &Identities) -> Result<Option<Decrypted>> {
    let mut first: Vec<&dyn age::Identity> = Vec::new();
    for id in &ids.native {
        first.push(id);
    }
    for id in &ids.ssh_inline {
        first.push(id);
    }
    if let Some(d) = try_decrypt(cipher, first)? {
        return Ok(Some(d));
    }
    let tags = ssh_stanza_tags(cipher);
    let mut ssh: Vec<&dyn age::Identity> = Vec::new();
    for key in &ids.ssh {
        if tags
            .iter()
            .any(|(kind, tag)| *kind == key.kind && *tag == key.tag)
            && let Some(id) = key.get()
        {
            ssh.push(id);
        }
    }
    try_decrypt(cipher, ssh)
}

fn try_decrypt(cipher: &[u8], identities: Vec<&dyn age::Identity>) -> Result<Option<Decrypted>> {
    if identities.is_empty() {
        return Ok(None);
    }
    let decryptor =
        age::Decryptor::new_buffered(ArmoredReader::new(cipher)).context("not an age file")?;
    match decryptor.decrypt(identities.into_iter()) {
        Ok(mut reader) => {
            let mut buf = Vec::new();
            reader
                .read_to_end(&mut buf)
                .context("ciphertext is corrupt")?;
            let (list, content) = payload::unwrap(&buf)?;
            Ok(Some(Decrypted { list, content }))
        }
        Err(age::DecryptError::NoMatchingKeys) => Ok(None),
        Err(e) => Err(e).context("cannot decrypt"),
    }
}

/// The `(kind, tag)` of each SSH stanza in an age header, read without
/// decrypting, so gse loads only SSH keys that can open the file.
pub fn ssh_stanza_tags(cipher: &[u8]) -> Vec<(String, [u8; 4])> {
    let mut reader = std::io::BufReader::new(ArmoredReader::new(cipher));
    let mut out = Vec::new();
    let mut line = Vec::new();
    for _ in 0..10_000 {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        if text.starts_with("---") {
            break;
        }
        if let Some(rest) = text.strip_prefix("-> ") {
            let mut fields = rest.split_whitespace();
            if let (Some(kind), Some(tag)) = (fields.next(), fields.next())
                && (kind == "ssh-ed25519" || kind == "ssh-rsa")
                && let Ok(bytes) = STANDARD_NO_PAD.decode(tag)
                && let Ok(tag) = <[u8; 4]>::try_from(bytes.as_slice())
            {
                out.push((kind.to_string(), tag));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipients::{Owner, OwnerKind};

    #[test]
    fn round_trip_with_payload() {
        let id = age::x25519::Identity::generate();
        let mut list = RecipientList::default();
        let mut owner = Owner::new(OwnerKind::Key, "dan");
        owner.keys.push(id.to_public().to_string());
        list.upsert(owner);
        let cipher = encrypt(&list, b"SECRET=1\n").unwrap();
        assert!(cipher.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----"));
        let ids = Identities {
            native: vec![id],
            ..Default::default()
        };
        let d = decrypt(&cipher, &ids).unwrap().unwrap();
        assert_eq!(d.content, b"SECRET=1\n");
        assert_eq!(d.list, list);

        let other = Identities {
            native: vec![age::x25519::Identity::generate()],
            ..Default::default()
        };
        assert!(decrypt(&cipher, &other).unwrap().is_none());
    }
}
