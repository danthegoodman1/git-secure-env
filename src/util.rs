//! File helpers.

use anyhow::{Context, Result};
use std::path::Path;

pub fn read_opt(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Writes a file, creating its parent directories.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("cannot write {}", path.display()))
}

/// Writes plaintext secrets: readable only by the owner on Unix, replaced
/// through a rename from `tmp_dir` so readers never see a partial file.
pub fn write_private(path: &Path, bytes: &[u8], tmp_dir: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::create_dir_all(tmp_dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = tmp_dir.join(format!("write-{}-{nanos}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    set_private(&tmp);
    if std::fs::rename(&tmp, path).is_err() {
        // A temporary directory on another filesystem cannot rename into place.
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
        set_private(path);
    }
    Ok(())
}

#[cfg(unix)]
pub fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
pub fn set_private(_path: &Path) {}

#[cfg(unix)]
pub fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
pub fn set_executable(_path: &Path) {}

/// Today's date as `YYYY-MM-DD` in UTC.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// True when a file holds git-style conflict markers at line starts.
pub fn has_conflict_markers(bytes: &[u8]) -> bool {
    bytes.split(|&b| b == b'\n').any(|line| {
        line.starts_with(b"<<<<<<< ")
            || line.starts_with(b">>>>>>> ")
            || line == b"<<<<<<<"
            || line == b">>>>>>>"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_416), (2025, 11, 24));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn conflict_markers() {
        assert!(has_conflict_markers(
            b"A=1\n<<<<<<< ours\nB=1\n=======\nB=2\n>>>>>>> theirs\n"
        ));
        assert!(!has_conflict_markers(b"A=<<<<<<< not a marker\n"));
    }
}
