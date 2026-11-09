//! Consistent SQLite snapshots, including committed WAL contents. Existing
//! snapshots are never overwritten; restore requires the runtime to be stopped.
use crate::{Error, Result, config::safe_path, store::Store};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
// v10 persists video artifacts with the turn that produced them.
pub const SCHEMA_VERSION: i64 = 10;
pub fn snapshot(conn: &Connection, directory: &Path, label: &str) -> Result<Value> {
    let parent = directory
        .parent()
        .ok_or_else(|| Error::invalid("Invalid backup directory."))?;
    safe_path(parent, directory)?;
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let file = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    // VACUUM INTO takes a SQLite read snapshot rather than copying the main
    // file, which would lose transactions still resident in the WAL.
    conn.execute("VACUUM INTO ?", [file.path().to_string_lossy().as_ref()])?;
    file.as_file().sync_all()?;
    let check = Connection::open_with_flags(file.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = check.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(Error::conflict("Backup integrity check failed."));
    }
    let version: i64 = check.pragma_query_value(None, "user_version", |r| r.get(0))?;
    drop(check);
    let name = format!("{label}-{}.sqlite3", uuid::Uuid::new_v4());
    let destination = directory.join(&name);
    file.persist_noclobber(&destination)
        .map_err(|_| Error::conflict("Could not publish database backup."))?;
    use std::io::Read;
    let mut input = std::fs::File::open(&destination)?;
    let size = input.metadata()?.len();
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let digest = format!("{:x}", hasher.finalize());
    Ok({
        let manifest = json!({"file":name,"schema_version":version,"sha256":digest,"bytes":size,"integrity":"ok"});
        crate::config::write_json(&destination.with_extension("json"), &manifest)?;
        manifest
    })
}
impl Store {
    pub fn backup(&self) -> Result<Value> {
        snapshot(
            &self.connect()?,
            &self
                .path
                .parent()
                .ok_or_else(|| Error::invalid("Invalid database path."))?
                .join("backups"),
            "manual",
        )
    }
}
