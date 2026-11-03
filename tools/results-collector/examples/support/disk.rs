use async_trait::async_trait;
use civpulse_results_collector::{
    Error, Result,
    storage::{Conditions, Store, Stored, validate_key},
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};

pub struct DiskStore {
    root: PathBuf,
    lock: Mutex<()>,
}
impl DiskStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            lock: Mutex::new(()),
        }
    }
    fn path(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }
}
pub async fn digest(path: &Path) -> Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Error::Storage("Benchmark file open"))?;
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| Error::Storage("Benchmark file read"))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        count += n as u64;
    }
    Ok((format!("{:x}", hash.finalize()), count))
}
async fn precondition(path: &Path, c: &Conditions) -> Result<bool> {
    let exists = tokio::fs::try_exists(path)
        .await
        .map_err(|_| Error::Storage("Benchmark stat"))?;
    if c.if_none_match && exists {
        return Ok(false);
    }
    if let Some(etag) = &c.if_match
        && (!exists || &format!("\"{}\"", digest(path).await?.0) != etag)
    {
        return Ok(false);
    }
    Ok(true)
}
#[async_trait]
impl Store for DiskStore {
    fn url(&self, key: &str) -> Result<String> {
        validate_key(key)?;
        Ok(format!("https://files.civpulse.org/{key}"))
    }
    async fn get(&self, key: &str, limit: usize) -> Result<Stored> {
        let _guard = self.lock.lock().await;
        let path = self.path(key)?;
        if !tokio::fs::try_exists(&path)
            .await
            .map_err(|_| Error::Storage("Benchmark stat"))?
        {
            return Ok(Stored {
                status: 404,
                etag: None,
                body: vec![],
            });
        }
        let size = tokio::fs::metadata(&path)
            .await
            .map_err(|_| Error::Storage("Benchmark stat"))?
            .len();
        if size > limit as u64 {
            return Err(Error::Storage("Benchmark bounded read exceeded"));
        }
        let body = tokio::fs::read(path)
            .await
            .map_err(|_| Error::Storage("Benchmark bounded read"))?;
        Ok(Stored {
            status: 200,
            etag: Some(format!(
                "\"{}\"",
                civpulse_results_collector::storage::sha256(&body)
            )),
            body,
        })
    }
    async fn put(&self, key: &str, bytes: &[u8], c: Conditions) -> Result<u16> {
        let _guard = self.lock.lock().await;
        let path = self.path(key)?;
        if !precondition(&path, &c).await? {
            return Ok(412);
        }
        tokio::fs::create_dir_all(path.parent().ok_or(Error::Storage("Benchmark parent"))?)
            .await
            .map_err(|_| Error::Storage("Benchmark mkdir"))?;
        let temp = path.with_extension("upload");
        let mut file = tokio::fs::File::create(&temp)
            .await
            .map_err(|_| Error::Storage("Benchmark create"))?;
        file.write_all(bytes)
            .await
            .map_err(|_| Error::Storage("Benchmark write"))?;
        file.flush()
            .await
            .map_err(|_| Error::Storage("Benchmark flush"))?;
        tokio::fs::rename(temp, path)
            .await
            .map_err(|_| Error::Storage("Benchmark rename"))?;
        Ok(200)
    }
    async fn put_file(
        &self,
        key: &str,
        source: &Path,
        hash: &str,
        size: u64,
        c: Conditions,
    ) -> Result<u16> {
        let _guard = self.lock.lock().await;
        let path = self.path(key)?;
        if !precondition(&path, &c).await? {
            return Ok(412);
        }
        tokio::fs::create_dir_all(path.parent().ok_or(Error::Storage("Benchmark parent"))?)
            .await
            .map_err(|_| Error::Storage("Benchmark mkdir"))?;
        let temp = path.with_extension("upload");
        tokio::fs::copy(source, &temp)
            .await
            .map_err(|_| Error::Storage("Benchmark copy"))?;
        if digest(&temp).await? != (hash.to_owned(), size) {
            return Err(Error::Storage("Benchmark copy checksum"));
        }
        tokio::fs::rename(temp, path)
            .await
            .map_err(|_| Error::Storage("Benchmark rename"))?;
        Ok(200)
    }
    async fn download(&self, key: &str, path: &Path, hash: &str, size: u64) -> Result<()> {
        self.verify(key, hash, size).await?;
        tokio::fs::copy(self.path(key)?, path)
            .await
            .map_err(|_| Error::Storage("Benchmark streamed download"))?;
        if digest(path).await? != (hash.into(), size) {
            return Err(Error::Storage("Benchmark downloaded checksum"));
        }
        Ok(())
    }
    async fn verify(&self, key: &str, hash: &str, size: u64) -> Result<()> {
        if digest(&self.path(key)?).await? != (hash.into(), size) {
            return Err(Error::Storage("Benchmark immutable checksum"));
        }
        Ok(())
    }
}
