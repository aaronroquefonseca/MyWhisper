//! Disposable before/after schema compatibility probe; no real keyring or data.
use mywhisper::store::Store;
use sha2::Digest;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::var("MYWHISPER_RESET_COMPAT_ROOT")?);
    assert!(root.file_name().unwrap().to_string_lossy().starts_with("mywhisper-reset-compat-"));
    assert!(root.join(".disposable").is_file());
    let db = root.join("state.db");
    match std::env::args().nth(1).as_deref() {
        Some("create") => {
            let store = Store::open_with_key(&db, &[0;32])?;
            store.upsert_word("synthetic preserved word", "synthetic")?;
            store.add_history("synthetic raw", "synthetic final", 2, 3, "synthetic")?;
            store.checkpoint()?;
            drop(store);
            let conn = rusqlite::Connection::open(&db)?;
            conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"; PRAGMA user_version=5", "00".repeat(32)))?;
            drop(conn);
            let store = Store::open_with_key(&db, &[0;32])?;
            assert_eq!(store.load_words()?, ["synthetic preserved word"]);
            assert_eq!(store.export_history()?[0].text, "synthetic final");
            store.checkpoint()?;
            let conn = rusqlite::Connection::open(&db)?;
            conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", "00".repeat(32)))?;
            assert_eq!(conn.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0))?,6);
            println!("Current build upgraded encrypted schema 5 to 6 and preserved synthetic data");
        }
        Some("refuse") => {
            let before = sha2::Sha256::digest(std::fs::read(&db)?);
            let error = Store::open_with_key(&db, &[0;32]).err().expect("older build must refuse schema 6");
            assert!(error.to_string().contains("newer than"));
            assert_eq!(before, sha2::Sha256::digest(std::fs::read(&db)?));
            println!("Schema-5 build refused schema 6 without changing the encrypted database");
        }
        Some("verify") => {
            let store = Store::open_with_key(&db, &[0;32])?;
            assert_eq!(store.load_words()?, ["synthetic preserved word"]);
            assert_eq!(store.export_history()?[0].text, "synthetic final");
            let conn = rusqlite::Connection::open(&db)?;
            conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", "00".repeat(32)))?;
            assert_eq!(conn.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0))?,6);
            println!("Current build reopened preserved schema-6 data after downgrade refusal");
        }
        _ => return Err("create/refuse/verify required".into()),
    }
    Ok(())
}
