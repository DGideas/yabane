use std::{collections::HashSet, io, path::Path};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

pub const CONFIG_TRANSACTION_FILE: &str = "data/config-transaction.json";
const TRANSACTION_VERSION: u32 = 1;
static PERSISTENCE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct AtomicWrite {
    path: String,
    contents: Vec<u8>,
}

impl AtomicWrite {
    pub fn bytes(path: impl AsRef<Path>, contents: Vec<u8>) -> io::Result<Self> {
        let path = path
            .as_ref()
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path must be UTF-8"))?
            .to_owned();
        Ok(Self { path, contents })
    }

    pub fn json(path: impl AsRef<Path>, value: &impl Serialize) -> io::Result<Self> {
        let contents = serde_json::to_vec_pretty(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Self::bytes(path, contents)
    }
}

#[derive(Deserialize, Serialize)]
struct RollbackJournal {
    version: u32,
    entries: Vec<RollbackEntry>,
}

#[derive(Deserialize, Serialize)]
struct RollbackEntry {
    path: String,
    previous: Option<Vec<u8>>,
}

pub async fn write_json_atomic(path: impl AsRef<Path>, value: &impl Serialize) -> io::Result<()> {
    let contents = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(path, &contents).await
}

pub async fn write_atomic(path: impl AsRef<Path>, contents: &[u8]) -> io::Result<()> {
    let _persistence = PERSISTENCE_LOCK.lock().await;
    write_atomic_unlocked(path.as_ref(), contents).await
}

pub async fn write_transaction(
    journal_path: impl AsRef<Path>,
    writes: &[AtomicWrite],
) -> io::Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    let _persistence = PERSISTENCE_LOCK.lock().await;
    let journal_path = journal_path.as_ref();
    let journal_name = journal_path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal path must be UTF-8"))?;
    match tokio::fs::metadata(journal_path).await {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an interrupted configuration transaction must be recovered first",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut paths = HashSet::new();
    if writes
        .iter()
        .any(|write| write.path == journal_name || !paths.insert(write.path.as_str()))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transaction target paths must be unique and exclude the journal",
        ));
    }

    let mut entries = Vec::with_capacity(writes.len());
    for write in writes {
        let previous = match tokio::fs::read(&write.path).await {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        entries.push(RollbackEntry {
            path: write.path.clone(),
            previous,
        });
    }
    let journal = RollbackJournal {
        version: TRANSACTION_VERSION,
        entries,
    };
    let journal_contents = serde_json::to_vec(&journal)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic_unlocked(journal_path, &journal_contents).await?;

    for write in writes {
        if let Err(error) = write_atomic_unlocked(Path::new(&write.path), &write.contents).await {
            return rollback_after_error(journal_path, &journal, error).await;
        }
    }
    if let Err(error) = remove_file_durable(journal_path).await {
        return rollback_after_error(journal_path, &journal, error).await;
    }
    Ok(())
}

pub async fn recover_transaction(
    journal_path: impl AsRef<Path>,
    allowed_paths: &[&str],
) -> Result<(), String> {
    let _persistence = PERSISTENCE_LOCK.lock().await;
    let journal_path = journal_path.as_ref();
    let contents = match tokio::fs::read(journal_path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read {}: {error}", journal_path.display())),
    };
    let journal: RollbackJournal = serde_json::from_slice(&contents)
        .map_err(|error| format!("parse {}: {error}", journal_path.display()))?;
    if journal.version != TRANSACTION_VERSION {
        return Err(format!(
            "{} has unsupported transaction version {}",
            journal_path.display(),
            journal.version
        ));
    }
    let allowed: HashSet<_> = allowed_paths.iter().copied().collect();
    let mut seen = HashSet::new();
    if journal.entries.is_empty()
        || journal.entries.iter().any(|entry| {
            !allowed.contains(entry.path.as_str()) || !seen.insert(entry.path.as_str())
        })
    {
        return Err(format!(
            "{} contains an invalid transaction target",
            journal_path.display()
        ));
    }
    rollback_unlocked(&journal).await.map_err(|error| {
        format!(
            "restore interrupted configuration transaction from {}: {error}",
            journal_path.display()
        )
    })?;
    remove_file_durable(journal_path)
        .await
        .map_err(|error| format!("remove {}: {error}", journal_path.display()))
}

async fn rollback_after_error(
    journal_path: &Path,
    journal: &RollbackJournal,
    original: io::Error,
) -> io::Result<()> {
    if let Err(rollback) = rollback_unlocked(journal).await {
        return Err(io::Error::new(
            rollback.kind(),
            format!("{original}; rollback also failed: {rollback}"),
        ));
    }
    if let Err(cleanup) = remove_file_durable(journal_path).await {
        return Err(io::Error::new(
            cleanup.kind(),
            format!("{original}; rollback succeeded but journal cleanup failed: {cleanup}"),
        ));
    }
    Err(original)
}

async fn rollback_unlocked(journal: &RollbackJournal) -> io::Result<()> {
    for entry in journal.entries.iter().rev() {
        match &entry.previous {
            Some(contents) => write_atomic_unlocked(Path::new(&entry.path), contents).await?,
            None => match tokio::fs::remove_file(&entry.path).await {
                Ok(()) => sync_parent(Path::new(&entry.path)).await?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            },
        }
    }
    Ok(())
}

async fn write_atomic_unlocked(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("y");
    let mut random = [0_u8; 8];
    rand::rng().fill_bytes(&mut random);
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temporary = parent.join(format!(".{file_name}.{suffix}.tmp"));

    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(contents).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        sync_parent(path).await
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

async fn remove_file_durable(path: &Path) -> io::Result<()> {
    tokio::fs::remove_file(path).await?;
    sync_parent(path).await
}

#[cfg(unix)]
async fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::File::open(parent).await?.sync_all().await
}

#[cfg(not(unix))]
async fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicWrite, RollbackEntry, RollbackJournal, TRANSACTION_VERSION, recover_transaction,
        write_json_atomic, write_transaction,
    };
    use serde::Serialize;

    #[derive(Serialize)]
    struct Value {
        name: &'static str,
    }

    fn directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "yabane-storage-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    #[tokio::test]
    async fn atomically_replaces_json_without_leaving_temporary_files() {
        let directory = directory("atomic");
        let path = directory.join("config.json");

        write_json_atomic(&path, &Value { name: "first" })
            .await
            .unwrap();
        write_json_atomic(&path, &Value { name: "second" })
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(contents, "{\n  \"name\": \"second\"\n}");
        let mut entries = tokio::fs::read_dir(&directory).await.unwrap();
        assert_eq!(entries.next_entry().await.unwrap().unwrap().path(), path);
        assert!(entries.next_entry().await.unwrap().is_none());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn transaction_replaces_every_file_and_removes_rollback_journal() {
        let directory = directory("transaction");
        let first = directory.join("first.json");
        let second = directory.join("second.json");
        let journal = directory.join("transaction.json");
        write_json_atomic(&first, &Value { name: "old-first" })
            .await
            .unwrap();
        write_json_atomic(&second, &Value { name: "old-second" })
            .await
            .unwrap();

        write_transaction(
            &journal,
            &[
                AtomicWrite::json(&first, &Value { name: "new-first" }).unwrap(),
                AtomicWrite::json(&second, &Value { name: "new-second" }).unwrap(),
            ],
        )
        .await
        .unwrap();

        assert!(
            tokio::fs::read_to_string(&first)
                .await
                .unwrap()
                .contains("new-first")
        );
        assert!(
            tokio::fs::read_to_string(&second)
                .await
                .unwrap()
                .contains("new-second")
        );
        assert!(!journal.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn transaction_rolls_back_prior_writes_when_a_later_write_fails() {
        let directory = directory("write-failure");
        let first = directory.join("first.json");
        let blocking_parent = directory.join("not-a-directory");
        let second = blocking_parent.join("second.json");
        let journal = directory.join("transaction.json");
        write_json_atomic(&first, &Value { name: "old-first" })
            .await
            .unwrap();
        tokio::fs::write(&blocking_parent, b"file").await.unwrap();

        let error = write_transaction(
            &journal,
            &[
                AtomicWrite::json(&first, &Value { name: "new-first" }).unwrap(),
                AtomicWrite::json(&second, &Value { name: "new-second" }).unwrap(),
            ],
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory
        ));
        assert!(
            tokio::fs::read_to_string(&first)
                .await
                .unwrap()
                .contains("old-first")
        );
        assert_eq!(tokio::fs::read(&blocking_parent).await.unwrap(), b"file");
        assert!(!second.exists());
        assert!(!journal.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn transaction_refuses_to_replace_an_existing_recovery_journal() {
        let directory = directory("existing-journal");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let target = directory.join("target.json");
        let journal = directory.join("transaction.json");
        tokio::fs::write(&target, b"old").await.unwrap();
        tokio::fs::write(&journal, b"pending recovery")
            .await
            .unwrap();

        let error = write_transaction(
            &journal,
            &[AtomicWrite::json(&target, &Value { name: "new" }).unwrap()],
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"old");
        assert_eq!(
            tokio::fs::read(&journal).await.unwrap(),
            b"pending recovery"
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn startup_recovery_restores_all_pre_transaction_files() {
        let directory = directory("recovery");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let first = directory.join("first.json");
        let second = directory.join("second.json");
        let created = directory.join("created.json");
        let journal = directory.join("transaction.json");
        tokio::fs::write(&first, b"new-first").await.unwrap();
        tokio::fs::write(&second, b"new-second").await.unwrap();
        tokio::fs::write(&created, b"new-created").await.unwrap();
        let rollback = RollbackJournal {
            version: TRANSACTION_VERSION,
            entries: vec![
                RollbackEntry {
                    path: first.to_str().unwrap().to_owned(),
                    previous: Some(b"old-first".to_vec()),
                },
                RollbackEntry {
                    path: second.to_str().unwrap().to_owned(),
                    previous: Some(b"old-second".to_vec()),
                },
                RollbackEntry {
                    path: created.to_str().unwrap().to_owned(),
                    previous: None,
                },
            ],
        };
        write_json_atomic(&journal, &rollback).await.unwrap();
        let allowed = [
            first.to_str().unwrap(),
            second.to_str().unwrap(),
            created.to_str().unwrap(),
        ];

        recover_transaction(&journal, &allowed).await.unwrap();

        assert_eq!(tokio::fs::read(&first).await.unwrap(), b"old-first");
        assert_eq!(tokio::fs::read(&second).await.unwrap(), b"old-second");
        assert!(!created.exists());
        assert!(!journal.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn recovery_rejects_unapproved_paths_before_restoring_anything() {
        let directory = directory("invalid-recovery");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let allowed = directory.join("allowed.json");
        let unapproved = directory.join("unapproved.json");
        let journal = directory.join("transaction.json");
        tokio::fs::write(&allowed, b"current").await.unwrap();
        let rollback = RollbackJournal {
            version: TRANSACTION_VERSION,
            entries: vec![
                RollbackEntry {
                    path: allowed.to_str().unwrap().to_owned(),
                    previous: Some(b"old".to_vec()),
                },
                RollbackEntry {
                    path: unapproved.to_str().unwrap().to_owned(),
                    previous: Some(b"unsafe".to_vec()),
                },
            ],
        };
        write_json_atomic(&journal, &rollback).await.unwrap();

        let error = recover_transaction(&journal, &[allowed.to_str().unwrap()])
            .await
            .unwrap_err();

        assert!(error.contains("invalid transaction target"));
        assert_eq!(tokio::fs::read(&allowed).await.unwrap(), b"current");
        assert!(!unapproved.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
