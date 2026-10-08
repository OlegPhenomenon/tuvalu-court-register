mod common;
use common::*;
use rusqlite::params;
use std::{collections::BTreeMap, path::Path};
use tuvalu_court::{audit, auth, backup, db::Db};
fn counts(db: &Db) -> BTreeMap<String, i64> {
    let c = db.open().unwrap();
    let names = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let count = c
                .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| {
                    r.get(0)
                })
                .unwrap();
            (name, count)
        })
        .collect()
}
fn empty(path: &Path) -> bool {
    !path.exists() || std::fs::read_dir(path).unwrap().next().is_none()
}
#[test]
fn encrypted_backup_restore_checks_all_data_and_failure_paths() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let uid = tuvalu_court::seed::create_user(
        db,
        "registrar",
        "Registrar",
        "long-test-password",
        false,
        &[],
    )
    .unwrap();
    let c = db.open().unwrap();
    let now = tuvalu_court::time::now_utc();
    c.execute(
        "INSERT INTO court_units(code,name) VALUES('TEST','Test court')",
        [],
    )
    .unwrap();
    let unit = c.last_insert_rowid();
    c.execute(
        "INSERT INTO registries(court_unit_id,series,name) VALUES(?1,'TEST','Test registry')",
        [unit],
    )
    .unwrap();
    let reg = c.last_insert_rowid();
    c.execute("INSERT INTO cases(registry_id,year,seq,number,title,category,status,registered_date,registered_at,registered_by,updated_at) VALUES(?1,2000,1,'TEST-2000-0001','Historical case','civil_contract','registered','2000-01-01',?2,?3,?2)",params![reg,now,uid]).unwrap();
    let cid = c.last_insert_rowid();
    let (doc, _) = insert_document(
        db,
        cid,
        "Backup material",
        "evidence",
        "party_material",
        uid,
    );
    // More than a MiB forces multiple authenticated chunks. Versions and import sources both
    // belong in the manifest, including sources which do not have document rows.
    let mut bytes = pdf("Large material");
    bytes.extend(vec![b'x'; 2 * 1024 * 1024]);
    let (key, sha) = tuvalu_court::storage::write_blob(db, &bytes).unwrap();
    c.execute("INSERT INTO document_versions(document_id,version_no,filename,content_type,size_bytes,sha256,storage_key,scan_status,uploaded_by,uploaded_at) VALUES(?1,2,'large.pdf','application/pdf',?2,?3,?4,'clean',?5,?6)",params![doc,bytes.len() as i64,sha,key,uid,now]).unwrap();
    let source = b"original,csv\nunchanged,bytes\n";
    let (source_key, source_sha) = tuvalu_court::storage::write_blob(db, source).unwrap();
    c.execute("INSERT INTO import_batches(kind,filename,source_sha256,storage_key,status,preview_json,created_by,created_at) VALUES('cases_csv','original.csv',?1,?2,'previewed','{}',?3,?4)",params![source_sha,source_key,uid,now]).unwrap();
    let actor = auth::load_actor(&c, uid, None).unwrap().unwrap();
    db.write_blocking(|tx| {
        audit::record(
            tx,
            Some(&actor),
            audit::Event::new("case.registered", "case", cid, "Production case").case(Some(cid)),
        )?;
        Ok(())
    })
    .unwrap();
    let keyfile = app.dir.join("backup.key");
    backup::gen_key(&keyfile).unwrap();
    assert_eq!(std::fs::read_to_string(&keyfile).unwrap().len(), 64);
    assert!(backup::gen_key(&keyfile).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&keyfile).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let out = app.dir.join("encrypted.tcrb");
    let summary = backup::backup(db, &out, &keyfile).unwrap();
    assert!(summary.contains("3 files"));
    let encrypted = std::fs::read(&out).unwrap();
    assert!(encrypted.starts_with(b"TCRB1"));
    assert!(!encrypted.windows(15).any(|w| w == b"Historical case"));
    assert!(backup::backup(db, &out, &keyfile).is_err());
    assert!(!app.dir.join("encrypted.tcrb.tmp").exists());
    let target = app.dir.join("restored");
    std::fs::create_dir(&target).unwrap();
    #[cfg(unix)]
    let target_inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&target).unwrap().ino()
    };
    let summary = backup::restore(&out, &keyfile, &target).unwrap();
    assert!(!target.join(".restore-tmp").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&target).unwrap().ino(), target_inode);
    }
    assert!(summary.contains("3 files"));
    let restored = Db::new(target.join("court.sqlite"), target.join("files"), None);
    assert_eq!(counts(db), counts(&restored));
    assert_eq!(
        audit::verify_chain(&restored.open().unwrap()).unwrap(),
        audit::verify_chain(&c).unwrap()
    );
    let head = |db: &Db| -> String {
        db.open()
            .unwrap()
            .query_row(
                "SELECT hash FROM audit_events ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(head(db), head(&restored));
    let keys=c.prepare("SELECT storage_key FROM document_versions UNION SELECT storage_key FROM import_batches").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    for key in keys {
        let original = std::fs::read(db.files_dir().join(&key)).unwrap();
        let restored = std::fs::read(restored.files_dir().join(&key)).unwrap();
        assert_eq!(auth::sha256_hex(&original), auth::sha256_hex(&restored));
    }
    assert!(backup::restore(&out, &keyfile, &target).is_err());
    assert_eq!(counts(db), counts(&restored));
    let wrong = app.dir.join("wrong.key");
    backup::gen_key(&wrong).unwrap();
    let target = app.dir.join("wrong-restore");
    assert!(backup::restore(&out, &wrong, &target).is_err());
    assert!(empty(&target));
    let damaged = app.dir.join("damaged.tcrb");
    let mut flipped = encrypted.clone();
    flipped[100] ^= 1;
    std::fs::write(&damaged, &flipped).unwrap();
    let target = app.dir.join("damaged-restore");
    assert!(backup::restore(&damaged, &keyfile, &target).is_err());
    assert!(empty(&target));
    for (i, length) in [
        10,
        encrypted.len() - 1,
        encrypted.len() / 2,
        21 + 4 + 1024 * 1024 + 16,
    ]
    .iter()
    .enumerate()
    {
        let truncated = app.dir.join(format!("truncated-{i}.tcrb"));
        std::fs::write(&truncated, &encrypted[..*length]).unwrap();
        let target = app.dir.join(format!("truncated-{i}-restore"));
        std::fs::create_dir(&target).unwrap();
        assert!(backup::restore(&truncated, &keyfile, &target).is_err());
        assert!(empty(&target));
    }
    let mut trailing = encrypted.clone();
    trailing.extend_from_slice(&[0, 0, 0, 16]);
    trailing.extend_from_slice(&[0; 16]);
    std::fs::write(&damaged, trailing).unwrap();
    let target = app.dir.join("appended-restore");
    assert!(backup::restore(&damaged, &keyfile, &target).is_err());
    assert!(empty(&target));
    assert!(std::fs::read_dir(&app.dir).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tcr-backup-")
    }));
}
