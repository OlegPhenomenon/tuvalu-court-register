//! CLI-only encrypted, authenticated snapshots. Restore publishes only after every check passes.
use crate::{
    audit,
    auth::random_bytes,
    db::Db,
    error::{AppError, AppResult},
};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};
const MAGIC: &[u8; 5] = b"TCRB1";
const CHUNK: usize = 1024 * 1024;
fn invalid(message: &str) -> AppError {
    AppError::validation(message)
}
fn zip_err(_: zip::result::ZipError) -> AppError {
    invalid("Invalid backup archive.")
}
#[derive(Serialize, Deserialize)]
struct Blob {
    storage_key: String,
    sha256: String,
    size: u64,
}
#[derive(Serialize, Deserialize)]
struct AuditHead {
    events: i64,
    head_hash: String,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    created_at: String,
    schema: i64,
    tables: BTreeMap<String, i64>,
    audit: AuditHead,
    files: Vec<Blob>,
}
struct Scratch(PathBuf);
impl Scratch {
    fn new(parent: &Path) -> AppResult<Self> {
        Self::at(parent.join(format!(".tcr-backup-{}", hex::encode(random_bytes::<16>()))))
    }
    fn at(path: PathBuf) -> AppResult<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut b = fs::DirBuilder::new();
            b.mode(0o700).create(&path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn private_file(path: &Path) -> AppResult<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    Ok(o.open(path)?)
}
pub fn gen_key(keyfile: &Path) -> AppResult<()> {
    let mut f = private_file(keyfile)?;
    f.write_all(hex::encode(random_bytes::<32>()).as_bytes())?;
    f.sync_all()?;
    Ok(())
}
fn cipher(keyfile: &Path) -> AppResult<XChaCha20Poly1305> {
    let mut bytes = Vec::new();
    File::open(keyfile)?.take(1024).read_to_end(&mut bytes)?;
    let key = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| hex::decode(s.trim()).ok())
        .filter(|k| k.len() == 32)
        .ok_or_else(|| invalid("The key file must contain 32 bytes encoded as hex."))?;
    XChaCha20Poly1305::new_from_slice(&key).map_err(|_| invalid("Invalid backup key."))
}
fn valid_key(k: &str) -> bool {
    k.len() == 100
        && k.as_bytes()[2] == b'/'
        && k.as_bytes()[67] == b'-'
        && k.bytes()
            .enumerate()
            .all(|(i, b)| i == 2 || i == 67 || b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && k[..2] == k[3..5]
}
fn tables(c: &Connection) -> AppResult<BTreeMap<String, i64>> {
    let mut stmt = c.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    names
        .into_iter()
        .map(|n| {
            let sql = format!("SELECT COUNT(*) FROM \"{}\"", n.replace('"', "\"\""));
            let count = c.query_row(&sql, [], |r| r.get(0))?;
            Ok((n, count))
        })
        .collect()
}
fn head(c: &Connection) -> AppResult<AuditHead> {
    let (events, broken) = audit::verify_chain(c)?;
    if broken.is_some() {
        return Err(invalid("The audit chain is broken."));
    }
    let head_hash = c
        .query_row(
            "SELECT hash FROM audit_events ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "GENESIS".into());
    Ok(AuditHead { events, head_hash })
}
fn keys(c: &Connection) -> AppResult<BTreeSet<String>> {
    let mut stmt = c.prepare(
        "SELECT storage_key FROM document_versions UNION SELECT storage_key FROM import_batches",
    )?;
    Ok(stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}
fn file_hash(path: &Path) -> AppResult<(String, u64)> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut f = File::open(path)?;
    let mut buf = [0u8; 65536];
    let mut size = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex::encode(h.finalize()), size))
}
fn verify_blob_rows(c: &Connection, blobs: &[Blob]) -> AppResult<()> {
    let by_key: BTreeMap<_, _> = blobs.iter().map(|b| (&b.storage_key, b)).collect();
    let expected = keys(c)?;
    if expected.len() != blobs.len() || expected.iter().any(|k| !by_key.contains_key(k)) {
        return Err(invalid("Backup file list does not match the database."));
    }
    let mut stmt = c.prepare("SELECT storage_key,sha256,size_bytes FROM document_versions")?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let k: String = r.get(0)?;
        let sha: String = r.get(1)?;
        let size: i64 = r.get(2)?;
        let b = by_key
            .get(&k)
            .ok_or_else(|| invalid("Missing backup file."))?;
        if b.sha256 != sha || size < 0 || b.size != size as u64 {
            return Err(invalid("Backup file metadata does not match the database."));
        }
    }
    let mut stmt = c.prepare("SELECT storage_key,source_sha256 FROM import_batches")?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let k: String = r.get(0)?;
        let sha: String = r.get(1)?;
        if by_key.get(&k).is_none_or(|b| b.sha256 != sha) {
            return Err(invalid("Import source checksum mismatch."));
        }
    }
    Ok(())
}
fn aad(index: u64, last: bool) -> Vec<u8> {
    let mut v = MAGIC.to_vec();
    v.extend_from_slice(&index.to_be_bytes());
    v.push(u8::from(last));
    v
}
fn nonce(prefix: &[u8; 16], index: u64) -> [u8; 24] {
    let mut n = [0u8; 24];
    n[..16].copy_from_slice(prefix);
    n[16..].copy_from_slice(&index.to_be_bytes());
    n
}
fn read_chunk(f: &mut File) -> AppResult<Vec<u8>> {
    let mut b = vec![0u8; CHUNK];
    let mut n = 0;
    while n < CHUNK {
        let read = f.read(&mut b[n..])?;
        if read == 0 {
            break;
        }
        n += read;
    }
    b.truncate(n);
    Ok(b)
}
fn encrypt(input: &Path, out: &Path, cipher: &XChaCha20Poly1305) -> AppResult<()> {
    let mut source = File::open(input)?;
    let mut target = private_file(out)?;
    let prefix = random_bytes::<16>();
    target.write_all(MAGIC)?;
    target.write_all(&prefix)?;
    let mut chunk = read_chunk(&mut source)?;
    let mut index = 0u64;
    loop {
        let next = read_chunk(&mut source)?;
        let last = next.is_empty();
        let n = nonce(&prefix, index);
        let a = aad(index, last);
        let encrypted = cipher
            .encrypt(
                &XNonce::from(n),
                Payload {
                    msg: &chunk,
                    aad: &a,
                },
            )
            .map_err(|_| invalid("Backup encryption failed."))?;
        target.write_all(&(encrypted.len() as u32).to_be_bytes())?;
        target.write_all(&encrypted)?;
        if last {
            break;
        }
        chunk = next;
        index = index
            .checked_add(1)
            .ok_or_else(|| invalid("Too many backup chunks."))?;
    }
    target.sync_all()?;
    Ok(())
}
fn record(f: &mut File) -> AppResult<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    let n = f.read(&mut length[..1])?;
    if n == 0 {
        return Ok(None);
    }
    f.read_exact(&mut length[1..])
        .map_err(|_| invalid("Truncated backup chunk."))?;
    let len = u32::from_be_bytes(length) as usize;
    if !(16..=CHUNK + 16).contains(&len) {
        return Err(invalid("Invalid backup chunk size."));
    }
    let mut b = vec![0; len];
    f.read_exact(&mut b)
        .map_err(|_| invalid("Truncated backup chunk."))?;
    Ok(Some(b))
}
fn decrypt(input: &Path, out: &Path, cipher: &XChaCha20Poly1305) -> AppResult<()> {
    let mut f = File::open(input)?;
    let mut magic = [0u8; 5];
    let mut prefix = [0u8; 16];
    f.read_exact(&mut magic)
        .map_err(|_| invalid("Truncated backup header."))?;
    f.read_exact(&mut prefix)
        .map_err(|_| invalid("Truncated backup header."))?;
    if &magic != MAGIC {
        return Err(invalid("Invalid backup format."));
    }
    let mut current = record(&mut f)?.ok_or_else(|| invalid("Missing final backup chunk."))?;
    let mut index = 0u64;
    let mut target = private_file(out)?;
    loop {
        let next = record(&mut f)?;
        let last = next.is_none();
        let n = nonce(&prefix, index);
        let a = aad(index, last);
        let bytes = cipher
            .decrypt(
                &XNonce::from(n),
                Payload {
                    msg: &current,
                    aad: &a,
                },
            )
            .map_err(|_| {
                invalid("Backup authentication failed (wrong key, damage, or truncation).")
            })?;
        if !last && bytes.len() != CHUNK {
            return Err(invalid("Invalid backup chunk sequence."));
        }
        target.write_all(&bytes)?;
        match next {
            None => break,
            Some(b) => current = b,
        };
        index = index
            .checked_add(1)
            .ok_or_else(|| invalid("Too many backup chunks."))?;
    }
    target.sync_all()?;
    Ok(())
}
pub fn backup(db: &Db, out: &Path, keyfile: &Path) -> AppResult<String> {
    let cipher = cipher(keyfile)?;
    if out.exists() {
        return Err(invalid("The backup output already exists."));
    }
    let scratch = Scratch::new(db.path().parent().unwrap_or(Path::new(".")))?;
    let snapshot = scratch.0.join("db.sqlite");
    db.open()?
        .execute("VACUUM INTO ?1", [snapshot.to_string_lossy().as_ref()])?;
    let c = Connection::open(&snapshot)?;
    let mut blobs = vec![];
    for key in keys(&c)? {
        if !valid_key(&key) {
            return Err(invalid("Invalid stored file key."));
        }
        let (sha, size) = file_hash(&db.files_dir().join(&key))?;
        if sha != key[3..67] {
            return Err(invalid("Stored file checksum mismatch."));
        }
        blobs.push(Blob {
            storage_key: key,
            sha256: sha,
            size,
        });
    }
    verify_blob_rows(&c, &blobs)?;
    let manifest = Manifest {
        format: "tcr-backup/1".into(),
        created_at: crate::time::now_utc(),
        schema: c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
        tables: tables(&c)?,
        audit: head(&c)?,
        files: blobs,
    };
    drop(c);
    let packed = scratch.0.join("backup.zip");
    let mut zip = ZipWriter::new(private_file(&packed)?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("manifest.json", options).map_err(zip_err)?;
    zip.write_all(&serde_json::to_vec(&manifest)?)?;
    zip.start_file("db.sqlite", options).map_err(zip_err)?;
    std::io::copy(&mut File::open(&snapshot)?, &mut zip)?;
    for b in &manifest.files {
        zip.start_file(format!("files/{}", b.storage_key), options)
            .map_err(zip_err)?;
        std::io::copy(
            &mut File::open(db.files_dir().join(&b.storage_key))?,
            &mut zip,
        )?;
    }
    zip.finish().map_err(zip_err)?.sync_all()?;
    let mut tmp = out.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if tmp.exists() {
        return Err(invalid("The temporary backup output already exists."));
    }
    let result: AppResult<()> = (|| {
        encrypt(&packed, &tmp, &cipher)?;
        fs::rename(&tmp, out)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(format!(
        "Backup created: {} tables, {} files, {} audit events, {} bytes.",
        manifest.tables.len(),
        manifest.files.len(),
        manifest.audit.events,
        fs::metadata(out)?.len()
    ))
}
// ZipArchive coalesces equal names. Check the original directory so duplicate entries
// cannot be hidden by its name index during restore verification.
fn verify_entry_count(path: &Path, start: u64, expected: usize) -> AppResult<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut count = 0usize;
    loop {
        let mut signature = [0; 4];
        file.read_exact(&mut signature)
            .map_err(|_| invalid("Truncated backup directory."))?;
        if signature != *b"PK\x01\x02" {
            break;
        }
        let mut h = [0; 42];
        file.read_exact(&mut h)?;
        count += 1;
        if count > expected {
            return Err(invalid("Duplicate or unexpected backup entries."));
        }
        let extra = [24, 26, 28]
            .iter()
            .map(|&i| u16::from_le_bytes([h[i], h[i + 1]]) as i64)
            .sum();
        file.seek(SeekFrom::Current(extra))?;
    }
    if count != expected {
        return Err(invalid("Backup archive entry count mismatch."));
    }
    Ok(())
}
/// Resolve installation identity from an authenticated backup before CLI confirmation.
pub fn installation_id(input: &Path, keyfile: &Path, target_dir: &Path) -> AppResult<String> {
    struct EmptyTarget<'a>(&'a Path, bool);
    impl Drop for EmptyTarget<'_> {
        fn drop(&mut self) {
            if self.1 {
                let _ = fs::remove_dir(self.0);
            }
        }
    }
    let _target = EmptyTarget(target_dir, !target_dir.exists());
    prepare_restore_target(target_dir)?;
    let scratch = Scratch::at(target_dir.join(".restore-tmp"))?;
    let packed = scratch.0.join("backup.zip");
    decrypt(input, &packed, &cipher(keyfile)?)?;
    let mut zip = ZipArchive::new(File::open(&packed)?).map_err(zip_err)?;
    let mut source = zip.by_name("db.sqlite").map_err(zip_err)?;
    let path = scratch.0.join("db.sqlite");
    std::io::copy(&mut source, &mut private_file(&path)?)?;
    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let id = crate::db::setting(&conn, "installation_id", "")?;
    if version == 0 {
        return Err(invalid("Backup does not contain an initialised database."));
    }
    // Older archives predate persistent ids: confirm their authenticated DB fingerprint.
    // Startup migration creates the persistent random installation id after restore.
    if id.is_empty() {
        return Ok(format!("legacy-{}", &file_hash(&path)?.0[..32]));
    }
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("Invalid backup installation id."));
    }
    Ok(id)
}

fn prepare_restore_target(target_dir: &Path) -> AppResult<()> {
    if target_dir.exists() && (!target_dir.is_dir() || fs::read_dir(target_dir)?.next().is_some()) {
        return Err(invalid("Restore requires a new or empty target directory."));
    }
    if fs::symlink_metadata(target_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("Restore target must not be a symlink."));
    }
    fs::create_dir_all(target_dir)?;
    Ok(())
}

pub fn restore(input: &Path, keyfile: &Path, target_dir: &Path) -> AppResult<String> {
    prepare_restore_target(target_dir)?;
    let cipher = cipher(keyfile)?;
    let scratch = Scratch::at(target_dir.join(".restore-tmp"))?;
    let packed = scratch.0.join("backup.zip");
    decrypt(input, &packed, &cipher)?;
    let mut zip = ZipArchive::new(File::open(&packed)?).map_err(zip_err)?;
    verify_entry_count(&packed, zip.central_directory_start(), zip.len())?;
    let mut mf = zip.by_name("manifest.json").map_err(zip_err)?;
    if mf.size() > 16 * 1024 * 1024 {
        return Err(invalid("Backup manifest is too large."));
    }
    let mut bytes = Vec::new();
    (&mut mf)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    drop(mf);
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.format != "tcr-backup/1" {
        return Err(invalid("Unsupported backup format."));
    }
    let mut expected = BTreeMap::new();
    expected.insert("manifest.json".to_string(), None);
    expected.insert("db.sqlite".to_string(), None);
    for b in &manifest.files {
        if !valid_key(&b.storage_key)
            || b.sha256 != b.storage_key[3..67]
            || expected
                .insert(format!("files/{}", b.storage_key), Some(b))
                .is_some()
        {
            return Err(invalid("Invalid backup file manifest."));
        }
    }
    if zip.len() != expected.len() {
        return Err(invalid("Unexpected backup archive entries."));
    }
    let layout = scratch.0.join("verified");
    fs::create_dir(&layout)?;
    fs::create_dir(layout.join("files"))?;
    let mut seen = BTreeSet::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(zip_err)?;
        let name = entry.name().to_string();
        let Some(blob) = expected.get(&name) else {
            return Err(invalid("Unexpected backup archive path."));
        };
        if !seen.insert(name.clone())
            || entry.is_dir()
            || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err(invalid("Unsafe backup archive entry."));
        }
        if name == "manifest.json" {
            continue;
        }
        let path = if name == "db.sqlite" {
            layout.join("court.sqlite")
        } else {
            layout.join(&name)
        };
        if let Some(b) = blob {
            if entry.size() != b.size {
                return Err(invalid("Backup file size mismatch."));
            }
        }
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let mut dest = private_file(&path)?;
        let copied = std::io::copy(&mut entry, &mut dest)?;
        dest.sync_all()?;
        if copied != entry.size() {
            return Err(invalid("Truncated archive entry."));
        }
        if let Some(b) = blob {
            let (sha, size) = file_hash(&path)?;
            if sha != b.sha256 || size != b.size {
                return Err(invalid("Backup file integrity check failed."));
            }
        }
    }
    let c = Connection::open(layout.join("court.sqlite"))?;
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(invalid("Database integrity check failed."));
    }
    let foreign_key_failures = c
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    if foreign_key_failures {
        return Err(invalid("Database foreign key check failed."));
    }
    let schema: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if schema != manifest.schema || tables(&c)? != manifest.tables {
        return Err(invalid(
            "Database schema or row counts do not match the manifest.",
        ));
    }
    let checked = head(&c)?;
    if checked.events != manifest.audit.events || checked.head_hash != manifest.audit.head_hash {
        return Err(invalid("Audit head does not match the manifest."));
    }
    verify_blob_rows(&c, &manifest.files)?;
    drop(c);
    drop(zip);
    // Keep the target itself in place: it may be a mounted volume.
    for entry in fs::read_dir(target_dir)? {
        if entry?.path() != scratch.0 {
            return Err(invalid("Restore target changed during verification."));
        }
    }
    let mut published = Vec::new();
    let publish = (|| -> AppResult<()> {
        for entry in fs::read_dir(&layout)? {
            let entry = entry?;
            let dest = target_dir.join(entry.file_name());
            fs::rename(entry.path(), &dest)?;
            published.push(dest);
        }
        Ok(())
    })();
    if let Err(error) = publish {
        for path in published {
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        return Err(error);
    }
    Ok(format!(
        "Backup restored: {} tables, {} files, {} audit events.",
        manifest.tables.len(),
        manifest.files.len(),
        manifest.audit.events
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_keys_are_strict() {
        let k = format!("ab/{}-{}", "ab".repeat(32), "01".repeat(16));
        assert!(valid_key(&k));
        assert!(!valid_key("../evil"));
        assert!(!valid_key(&k.replace("ab/", "cd/")));
    }
}
