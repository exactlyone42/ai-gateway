use crate::capture_key_usage::{WrapPermit, reserve_master_wrap};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::io::Read;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"APGANC01";

#[derive(Debug, thiserror::Error)]
#[error("capture key counter unavailable")]
pub(crate) struct CaptureAnchorError;

fn fingerprint(master: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(master).into()
}

fn anchor_path(root: &Path, fingerprint: &[u8; 32]) -> PathBuf {
    let name: String = fingerprint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    root.join(format!("{name}.anchor"))
}

fn encode(fingerprint: &[u8; 32], count: i64, poisoned: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(81);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(fingerprint);
    bytes.extend_from_slice(&count.to_be_bytes());
    bytes.push(u8::from(poisoned));
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}

pub(crate) fn initial_anchor(fingerprint: &[u8; 32]) -> Vec<u8> {
    encode(fingerprint, 0, false)
}

fn write_anchor(
    file: &mut std::fs::File,
    fingerprint: &[u8; 32],
    count: i64,
    poisoned: bool,
) -> Result<(), CaptureAnchorError> {
    let bytes = encode(fingerprint, count, poisoned);
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(&bytes))
        .and_then(|_| file.sync_all())
        .map_err(|_| CaptureAnchorError)
}

#[cfg(test)]
pub(crate) fn create_key(
    key_path: &Path,
    anchor_root: &Path,
) -> Result<Zeroizing<[u8; 32]>, CaptureAnchorError> {
    let mut master = Zeroizing::new([0u8; 32]);
    getrandom::fill(master.as_mut()).map_err(|_| CaptureAnchorError)?;
    persist_key(key_path, anchor_root, &master)?;
    Ok(master)
}

#[cfg(test)]
pub(crate) fn create_versioned_key(
    key_root: &Path,
    anchor_root: &Path,
) -> Result<Zeroizing<[u8; 32]>, CaptureAnchorError> {
    let mut master = Zeroizing::new([0u8; 32]);
    getrandom::fill(master.as_mut()).map_err(|_| CaptureAnchorError)?;
    let fp = fingerprint(&master);
    let name: String = fp.iter().map(|byte| format!("{byte:02x}")).collect();
    persist_key(&key_root.join(format!("{name}.key")), anchor_root, &master)?;
    Ok(master)
}

#[cfg(test)]
fn persist_key(
    key_path: &Path,
    anchor_root: &Path,
    master: &[u8; 32],
) -> Result<(), CaptureAnchorError> {
    let key_parent = key_path.parent().ok_or(CaptureAnchorError)?;
    if std::fs::canonicalize(key_parent).map_err(|_| CaptureAnchorError)?
        == std::fs::canonicalize(anchor_root).map_err(|_| CaptureAnchorError)?
    {
        return Err(CaptureAnchorError);
    }
    let mut key_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(key_path)
        .map_err(|_| CaptureAnchorError)?;
    key_file
        .write_all(master)
        .and_then(|_| key_file.sync_all())
        .and_then(|_| std::fs::File::open(key_parent)?.sync_all())
        .map_err(|_| CaptureAnchorError)?;
    let fp = fingerprint(master);
    let mut anchor = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(anchor_path(anchor_root, &fp))
        .map_err(|_| CaptureAnchorError)?;
    write_anchor(&mut anchor, &fp, 0, false)?;
    std::fs::File::open(anchor_root)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| CaptureAnchorError)?;
    Ok(())
}

fn open_anchor(
    root: &Path,
    fp: &[u8; 32],
    wait: bool,
) -> Result<(std::fs::File, i64), CaptureAnchorError> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(anchor_path(root, fp))
        .map_err(|_| CaptureAnchorError)?;
    if wait {
        file.lock().map_err(|_| CaptureAnchorError)?;
    } else {
        file.try_lock().map_err(|_| CaptureAnchorError)?;
    }
    let metadata = file.metadata().map_err(|_| CaptureAnchorError)?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.len() != 81 {
        return Err(CaptureAnchorError);
    }
    let mut bytes = [0u8; 81];
    file.read_exact(&mut bytes)
        .map_err(|_| CaptureAnchorError)?;
    if &bytes[..8] != MAGIC
        || &bytes[8..40] != fp
        || bytes[48] != 0
        || Sha256::digest(&bytes[..49]).as_slice() != &bytes[49..]
    {
        return Err(CaptureAnchorError);
    }
    let count = i64::from_be_bytes(bytes[40..48].try_into().map_err(|_| CaptureAnchorError)?);
    if !(0..=1_048_576).contains(&count) {
        return Err(CaptureAnchorError);
    }
    Ok((file, count))
}

pub(crate) fn check_current_anchor(
    root: &Path,
    master: &[u8; 32],
) -> Result<(), CaptureAnchorError> {
    let _ = open_anchor(root, &fingerprint(master), false)?;
    Ok(())
}

pub(crate) async fn reserve_anchored(
    pool: &PgPool,
    root: &Path,
    master: &[u8; 32],
) -> Result<WrapPermit, CaptureAnchorError> {
    let fp = fingerprint(master);
    let root = root.to_owned();
    let (mut file, last) = tokio::task::spawn_blocking(move || open_anchor(&root, &fp, true))
        .await
        .map_err(|_| CaptureAnchorError)??;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT wrap_count FROM capture_master_key_usage WHERE key_fingerprint = $1",
    )
    .bind(fp.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(|_| CaptureAnchorError)?;
    if current.unwrap_or(0) < last {
        tokio::task::spawn_blocking(move || write_anchor(&mut file, &fp, last, true))
            .await
            .map_err(|_| CaptureAnchorError)??;
        return Err(CaptureAnchorError);
    }
    let permit = reserve_master_wrap(pool, master)
        .await
        .map_err(|_| CaptureAnchorError)?;
    let count = permit.count();
    tokio::task::spawn_blocking(move || write_anchor(&mut file, &fp, count, false))
        .await
        .map_err(|_| CaptureAnchorError)??;
    Ok(permit)
}

#[cfg(test)]
mod tests {
    use super::{create_key, create_versioned_key, fingerprint, reserve_anchored};
    use crate::capture_master_key::load_master_key;
    use sqlx::PgPool;
    use uuid::Uuid;

    #[sqlx::test]
    async fn old_capture_selects_its_original_key_after_rotation(pool: PgPool) {
        use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
        use crate::capture_spool::{CaptureLimits, CaptureSpool};
        let root = std::env::temp_dir().join(format!("capture-keyring-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        let spool = root.join("spool");
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        std::fs::create_dir(&spool).unwrap();
        let old_key = create_versioned_key(&keys, &anchors).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let permit = reserve_anchored(&pool, &anchors, &old_key).await.unwrap();
        let mut capture = CaptureSpool::create_with_permit(
            &spool,
            identity,
            &old_key,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        capture
            .append(CaptureLeg::ClientRequest, 0, b"old record")
            .unwrap();
        let path = capture.path().to_owned();
        capture.finish().unwrap();
        let new_key = create_versioned_key(&keys, &anchors).unwrap();
        assert_ne!(*old_key, *new_key);
        assert!(CaptureSpool::recover(&path, &new_key, 4096).is_err());
        let recovered = CaptureSpool::recover_with_keyring(&path, &keys, 4096).unwrap();
        assert_eq!(
            recovered
                .cipher
                .unseal_timed(
                    recovered.blocks[0].0,
                    recovered.blocks[0].1,
                    0,
                    recovered.elapsed_ns[0],
                    &recovered.blocks[0].2
                )
                .unwrap(),
            b"old record"
        );
        let fp = fingerprint(&old_key);
        let name: String = fp.iter().map(|byte| format!("{byte:02x}")).collect();
        std::fs::write(keys.join(format!("{name}.key")), new_key.as_ref()).unwrap();
        assert!(CaptureSpool::recover_with_keyring(&path, &keys, 4096).is_err());
    }

    #[sqlx::test]
    async fn rollback_blocks_old_key_even_after_database_catches_up(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("capture-anchor-{}", Uuid::new_v4()));
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        let path = keys.join("master");
        let master = create_key(&path, &anchors).unwrap();
        assert_eq!(*load_master_key(&path).unwrap(), *master);
        assert!(create_key(&path, &anchors).is_err());
        assert_eq!(*load_master_key(&path).unwrap(), *master);
        let (first, second) = tokio::join!(
            reserve_anchored(&pool, &anchors, &master),
            reserve_anchored(&pool, &anchors, &master)
        );
        let mut values = [first.unwrap().count(), second.unwrap().count()];
        values.sort();
        assert_eq!(values, [1, 2]);
        let fp = fingerprint(&master);
        sqlx::query(
            "ALTER TABLE capture_master_key_usage DISABLE TRIGGER capture_master_key_usage_update",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE capture_master_key_usage SET wrap_count = 1 WHERE key_fingerprint = $1",
        )
        .bind(fp.as_slice())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "ALTER TABLE capture_master_key_usage ENABLE TRIGGER capture_master_key_usage_update",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(reserve_anchored(&pool, &anchors, &master).await.is_err());
        let count: i64 = sqlx::query_scalar(
            "SELECT wrap_count FROM capture_master_key_usage WHERE key_fingerprint = $1",
        )
        .bind(fp.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        sqlx::query(
            "UPDATE capture_master_key_usage SET wrap_count = 3 WHERE key_fingerprint = $1",
        )
        .bind(fp.as_slice())
        .execute(&pool)
        .await
        .unwrap();
        assert!(reserve_anchored(&pool, &anchors, &master).await.is_err());
        assert!(reserve_anchored(&pool, &keys, &master).await.is_err());
        let spool = root.join("spool");
        std::fs::create_dir(&spool).unwrap();
        let identity = crate::capture_crypto::CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let result = crate::capture_session::AsyncCaptureSession::create(
            &pool,
            crate::capture_session::CapturePaths {
                spool: &spool,
                anchors: &anchors,
            },
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await;
        assert!(result.is_err());
        assert!(!spool.join(format!("{}.capture", identity.capture)).exists());
    }
}
