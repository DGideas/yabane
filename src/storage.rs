use std::{io, path::Path};

use rand::RngCore;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

pub async fn write_json_atomic(path: impl AsRef<Path>, value: &impl Serialize) -> io::Result<()> {
    let contents = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(path, &contents).await
}

pub async fn write_atomic(path: impl AsRef<Path>, contents: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
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
        tokio::fs::rename(&temporary, path).await
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Value {
        name: &'static str,
    }

    #[tokio::test]
    async fn atomically_replaces_json_without_leaving_temporary_files() {
        let directory = std::env::temp_dir().join(format!("yabane-storage-{}", std::process::id()));
        let path = directory.join("config.json");
        let _ = tokio::fs::remove_dir_all(&directory).await;

        super::write_json_atomic(&path, &Value { name: "first" })
            .await
            .unwrap();
        super::write_json_atomic(&path, &Value { name: "second" })
            .await
            .unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(contents, "{\n  \"name\": \"second\"\n}");
        let mut entries = tokio::fs::read_dir(&directory).await.unwrap();
        assert_eq!(entries.next_entry().await.unwrap().unwrap().path(), path);
        assert!(entries.next_entry().await.unwrap().is_none());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
