use crate::capture_crypto::{CaptureCipher, CaptureIdentity, CaptureLeg};
use crate::capture_spool::{CaptureLimits, CaptureSpool, VerifiedSpool};
use sha2::Digest;
use sqlx::{PgPool, Postgres, QueryBuilder};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
#[error("capture archive unavailable")]
pub(crate) struct CaptureArchiveError;

pub(crate) async fn delete_coordinated(
    pool: &PgPool,
    spool: &Path,
    identity: CaptureIdentity,
    credential: Uuid,
    capacity: Arc<Semaphore>,
) -> Result<bool, CaptureArchiveError> {
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| CaptureArchiveError)?;
    let spool = spool.to_owned();
    let (guard, permit) = tokio::task::spawn_blocking(move || {
        let guard =
            crate::capture_backup_barrier::try_shared(&spool).map_err(|_| CaptureArchiveError)?;
        Ok::<_, CaptureArchiveError>((guard, permit))
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let allowed: Option<i32> = sqlx::query_scalar("SELECT 1 FROM credentials WHERE id = $1 AND tenant_id = $2 AND user_id = $3 AND status = 'active' AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now()) FOR SHARE")
        .bind(credential).bind(identity.tenant).bind(identity.user).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if allowed.is_none() {
        return Err(CaptureArchiveError);
    }
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM capture_records WHERE tenant_id = $1 AND user_id = $2 AND capture_id = $3 FOR UPDATE")
        .bind(identity.tenant).bind(identity.user).bind(identity.capture).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    let done = match state.as_deref() {
        None => return Err(CaptureArchiveError),
        Some("deleted") => true,
        _ => delete_locked_batch(&mut tx, identity.tenant, identity.capture).await?,
    };
    tx.commit().await.map_err(|_| CaptureArchiveError)?;
    drop(guard);
    drop(permit);
    Ok(done)
}

pub(crate) async fn remove_deleted_spool(
    pool: &PgPool,
    path: &Path,
    key_root: &Path,
    limits: CaptureLimits,
    capacity: Arc<Semaphore>,
) -> Result<(), CaptureArchiveError> {
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| CaptureArchiveError)?;
    let path = path.to_owned();
    let key_root = key_root.to_owned();
    let (verified, locked, permit) = tokio::task::spawn_blocking(move || {
        let (verified, locked) =
            CaptureSpool::open_deleted_for_cleanup(&path, &key_root, limits.per_file)
                .map_err(|_| CaptureArchiveError)?;
        Ok::<_, CaptureArchiveError>((verified, locked, permit))
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    let identity = verified.cipher.identity();
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let deleted: Option<i32> = sqlx::query_scalar("SELECT 1 FROM capture_records WHERE tenant_id = $1 AND capture_id = $2 AND user_id = $3 AND key_fingerprint = $4 AND state = 'deleted' AND wrapped_key IS NULL AND created_at = to_timestamp($5::bigint::double precision / 1000) AND expires_at = created_at + make_interval(days => $6) FOR UPDATE")
        .bind(identity.tenant).bind(identity.capture).bind(identity.user).bind(verified.cipher.fingerprint().as_slice())
        .bind(identity.created_ms).bind(i32::from(identity.retention_days)).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if deleted.is_none() {
        return Err(CaptureArchiveError);
    }
    let remains: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2)",
    )
    .bind(identity.tenant)
    .bind(identity.capture)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    if remains {
        return Err(CaptureArchiveError);
    }
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        CaptureSpool::remove_locked(locked, limits.total).map_err(|_| CaptureArchiveError)
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    tx.commit().await.map_err(|_| CaptureArchiveError)
}

pub(crate) async fn remove_archived_spool(
    pool: &PgPool,
    path: &Path,
    key_root: &Path,
    limits: CaptureLimits,
    capacity: Arc<Semaphore>,
) -> Result<(), CaptureArchiveError> {
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| CaptureArchiveError)?;
    let path = path.to_owned();
    let key_root = key_root.to_owned();
    let (verified, locked, permit) = tokio::task::spawn_blocking(move || {
        let (verified, locked) =
            CaptureSpool::open_complete_for_cleanup(&path, &key_root, limits.per_file)
                .map_err(|_| CaptureArchiveError)?;
        Ok::<_, CaptureArchiveError>((verified, locked, permit))
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    let identity = verified.cipher.identity();
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM capture_records WHERE tenant_id = $1 AND capture_id = $2 AND user_id = $3 AND wrapped_key = $4 AND key_fingerprint = $5 AND state = 'complete' AND created_at = to_timestamp($6::bigint::double precision / 1000) AND expires_at = created_at + make_interval(days => $7) FOR UPDATE")
        .bind(identity.tenant).bind(identity.capture).bind(identity.user).bind(verified.cipher.wrapped_key())
        .bind(verified.cipher.fingerprint().as_slice()).bind(identity.created_ms).bind(i32::from(identity.retention_days))
        .fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if exists.is_none() {
        return Err(CaptureArchiveError);
    }
    for (batch_index, batch) in verified.blocks.chunks(64).enumerate() {
        let start = batch_index * 64;
        let mut query =
            QueryBuilder::<Postgres>::new("SELECT count(*) FROM capture_blocks b JOIN (");
        query.push_values(
            batch.iter().enumerate(),
            |mut values, (offset, (leg, sequence, sealed))| {
                values
                    .push_bind(leg.index() as i16)
                    .push_bind(*sequence as i64)
                    .push_bind((start + offset) as i64)
                    .push_bind(verified.elapsed_ns[start + offset] as i64)
                    .push_bind(sealed);
            },
        );
        query.push(") AS v(leg, sequence, position, elapsed_ns, sealed) ON b.leg = v.leg::smallint AND b.sequence = v.sequence::bigint AND b.position = v.position::bigint AND b.elapsed_ns = v.elapsed_ns::bigint AND b.sealed = v.sealed::bytea WHERE b.tenant_id = ")
            .push_bind(identity.tenant).push(" AND b.capture_id = ").push_bind(identity.capture);
        let matches: i64 = query
            .build_query_scalar()
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| CaptureArchiveError)?;
        if usize::try_from(matches).ok() != Some(batch.len()) {
            return Err(CaptureArchiveError);
        }
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
    )
    .bind(identity.tenant)
    .bind(identity.capture)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    if usize::try_from(count).ok() != Some(verified.blocks.len()) {
        return Err(CaptureArchiveError);
    }
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        CaptureSpool::remove_locked(locked, limits.total).map_err(|_| CaptureArchiveError)
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    tx.commit().await.map_err(|_| CaptureArchiveError)
}

pub(crate) async fn recover_and_import(
    pool: &PgPool,
    path: &Path,
    master: &[u8; 32],
    max_bytes: u64,
    capacity: Arc<Semaphore>,
) -> Result<(), CaptureArchiveError> {
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| CaptureArchiveError)?;
    let path = path.to_owned();
    let master = Zeroizing::new(*master);
    let (verified, permit) = tokio::task::spawn_blocking(move || {
        let verified =
            CaptureSpool::recover(&path, &master, max_bytes).map_err(|_| CaptureArchiveError)?;
        Ok::<_, CaptureArchiveError>((verified, permit))
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    let result = import_spool(
        pool,
        &verified,
        verified.cipher.identity().user,
        i32::from(verified.cipher.identity().retention_days),
    )
    .await;
    drop(permit);
    result
}

pub(crate) async fn recover_and_import_keyring(
    pool: &PgPool,
    path: &Path,
    key_root: &Path,
    max_bytes: u64,
    capacity: Arc<Semaphore>,
) -> Result<bool, CaptureArchiveError> {
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| CaptureArchiveError)?;
    let path = path.to_owned();
    let key_root = key_root.to_owned();
    let (verified, permit) = tokio::task::spawn_blocking(move || {
        let verified = CaptureSpool::recover_with_keyring(&path, &key_root, max_bytes)
            .map_err(|_| CaptureArchiveError)?;
        Ok::<_, CaptureArchiveError>((verified, permit))
    })
    .await
    .map_err(|_| CaptureArchiveError)??;
    let complete = verified.complete;
    let result = import_spool(
        pool,
        &verified,
        verified.cipher.identity().user,
        i32::from(verified.cipher.identity().retention_days),
    )
    .await;
    drop(permit);
    result.map(|_| complete)
}

pub(crate) async fn read_verified(
    pool: &PgPool,
    identity: CaptureIdentity,
    user: Uuid,
    credential: Uuid,
    master: &[u8; 32],
    max_bytes: u64,
) -> Result<VerifiedSpool, CaptureArchiveError> {
    if identity.user != user {
        return Err(CaptureArchiveError);
    }
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let record: Option<(Vec<u8>, String)> = sqlx::query_as("SELECT r.wrapped_key, r.state FROM capture_records r JOIN credentials c ON c.tenant_id = r.tenant_id AND c.user_id = r.user_id WHERE r.tenant_id = $1 AND r.capture_id = $2 AND r.user_id = $3 AND c.id = $4 AND c.status = 'active' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at > now()) AND r.state IN ('complete', 'incomplete') AND r.expires_at > now() AND r.created_at = to_timestamp($5::bigint::double precision / 1000) AND r.expires_at = r.created_at + make_interval(days => $6) AND r.key_fingerprint = $7 FOR SHARE OF r, c")
        .bind(identity.tenant).bind(identity.capture).bind(user).bind(credential).bind(identity.created_ms).bind(i32::from(identity.retention_days)).bind(sha2::Sha256::digest(master).as_slice())
        .fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    let (wrapped, state) = record.ok_or(CaptureArchiveError)?;
    let limit = i64::try_from(max_bytes).map_err(|_| CaptureArchiveError)?;
    let size: i64 = sqlx::query_scalar("SELECT 158 + COALESCE(sum(octet_length(sealed) + 21), 0) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2")
        .bind(identity.tenant).bind(identity.capture).fetch_one(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if size > limit || size < 158 {
        return Err(CaptureArchiveError);
    }
    let rows: Vec<(i16, i64, i64, i64, Vec<u8>)> = sqlx::query_as("SELECT leg, sequence, position, elapsed_ns, sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 ORDER BY position")
        .bind(identity.tenant).bind(identity.capture).fetch_all(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    let mut data = Vec::with_capacity(size as usize);
    data.extend_from_slice(b"APGCAP06");
    data.extend_from_slice(identity.tenant.as_bytes());
    data.extend_from_slice(identity.capture.as_bytes());
    data.extend_from_slice(identity.user.as_bytes());
    data.extend_from_slice(&identity.created_ms.to_be_bytes());
    data.extend_from_slice(&identity.retention_days.to_be_bytes());
    data.extend_from_slice(sha2::Sha256::digest(master).as_slice());
    data.extend_from_slice(&wrapped);
    for (index, (leg, sequence, position, elapsed, sealed)) in rows.into_iter().enumerate() {
        if position != index as i64 || leg < 0 || sequence < 0 || elapsed < 0 {
            return Err(CaptureArchiveError);
        }
        data.push(u8::try_from(leg).map_err(|_| CaptureArchiveError)?);
        data.extend_from_slice(&(sequence as u64).to_be_bytes());
        data.extend_from_slice(
            &u32::try_from(sealed.len())
                .map_err(|_| CaptureArchiveError)?
                .to_be_bytes(),
        );
        data.extend_from_slice(&(elapsed as u64).to_be_bytes());
        data.extend_from_slice(&sealed);
    }
    let verified =
        CaptureSpool::verify_serialized(&data, identity, master, max_bytes, state == "incomplete")
            .map_err(|_| CaptureArchiveError)?;
    if verified.complete != (state == "complete") {
        return Err(CaptureArchiveError);
    }
    sqlx::query("INSERT INTO audit_logs (id, tenant_id, actor_credential_id, action, target_type, target_id, occurred_at) VALUES ($1, $2, $3, 'capture.read', 'capture', $4, now())")
        .bind(Uuid::new_v4()).bind(identity.tenant).bind(credential).bind(identity.capture)
        .execute(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    tx.commit().await.map_err(|_| CaptureArchiveError)?;
    Ok(verified)
}

pub(crate) async fn delete_batch(
    pool: &PgPool,
    tenant: Uuid,
    user: Uuid,
    capture: Uuid,
) -> Result<bool, CaptureArchiveError> {
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM capture_records WHERE tenant_id = $1 AND user_id = $2 AND capture_id = $3 FOR UPDATE")
        .bind(tenant).bind(user).bind(capture).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    match state.as_deref() {
        None => return Err(CaptureArchiveError),
        Some("deleted") => return Ok(true),
        _ => {}
    }
    let done = delete_locked_batch(&mut tx, tenant, capture).await?;
    tx.commit().await.map_err(|_| CaptureArchiveError)?;
    Ok(done)
}

pub(crate) async fn expire_batch(pool: &PgPool) -> Result<bool, CaptureArchiveError> {
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let target: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT tenant_id, capture_id FROM capture_records WHERE expires_at <= now() AND state <> 'deleted' ORDER BY expires_at, tenant_id, capture_id FOR UPDATE SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if let Some((tenant, capture)) = target {
        delete_locked_batch(&mut tx, tenant, capture).await?;
        tx.commit().await.map_err(|_| CaptureArchiveError)?;
        Ok(true)
    } else {
        tx.commit().await.map_err(|_| CaptureArchiveError)?;
        Ok(false)
    }
}

async fn delete_locked_batch(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    tenant: Uuid,
    capture: Uuid,
) -> Result<bool, CaptureArchiveError> {
    sqlx::query(
        "UPDATE capture_records SET state = 'deleting' WHERE tenant_id = $1 AND capture_id = $2",
    )
    .bind(tenant)
    .bind(capture)
    .execute(&mut **tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    sqlx::query("DELETE FROM capture_blocks WHERE (tenant_id, capture_id, leg, sequence) IN (SELECT tenant_id, capture_id, leg, sequence FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 ORDER BY leg, sequence LIMIT 64)")
        .bind(tenant).bind(capture).execute(&mut **tx).await.map_err(|_| CaptureArchiveError)?;
    let remains: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2)",
    )
    .bind(tenant)
    .bind(capture)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    if !remains {
        sqlx::query("UPDATE capture_records SET state = 'deleted', wrapped_key = NULL WHERE tenant_id = $1 AND capture_id = $2")
            .bind(tenant).bind(capture).execute(&mut **tx).await.map_err(|_| CaptureArchiveError)?;
    }
    Ok(!remains)
}

pub(crate) async fn import_spool(
    pool: &PgPool,
    verified: &VerifiedSpool,
    user: Uuid,
    retention_days: i32,
) -> Result<(), CaptureArchiveError> {
    if !(1..=365).contains(&retention_days)
        || i32::from(verified.cipher.identity().retention_days) != retention_days
        || verified.cipher.identity().user != user
        || verified.elapsed_ns.len() != verified.blocks.len()
        || verified
            .blocks
            .iter()
            .any(|(_, sequence, _)| *sequence >= 1_000_000)
    {
        return Err(CaptureArchiveError);
    }
    let identity = verified.cipher.identity();
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, created_at, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, to_timestamp($5::bigint::double precision / 1000), to_timestamp($5::bigint::double precision / 1000) + make_interval(days => $6), 'recording', $7) ON CONFLICT DO NOTHING")
        .bind(identity.tenant).bind(identity.capture).bind(user).bind(verified.cipher.wrapped_key()).bind(identity.created_ms).bind(retention_days).bind(verified.cipher.fingerprint().as_slice())
        .execute(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2 AND user_id = $3 AND wrapped_key = $4 AND created_at = to_timestamp($5::bigint::double precision / 1000) AND expires_at = created_at + make_interval(days => $6) AND key_fingerprint = $7 AND state IN ('recording', 'incomplete', 'complete') FOR UPDATE")
        .bind(identity.tenant).bind(identity.capture).bind(user).bind(verified.cipher.wrapped_key()).bind(identity.created_ms).bind(retention_days).bind(verified.cipher.fingerprint().as_slice())
        .fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    let state = state.ok_or(CaptureArchiveError)?;
    if state == "complete" && !verified.complete {
        return Err(CaptureArchiveError);
    }
    for (batch_index, batch) in verified.blocks.chunks(64).enumerate() {
        let start = batch_index * 64;
        if state == "complete" {
            let mut query =
                QueryBuilder::<Postgres>::new("SELECT count(*) FROM capture_blocks b JOIN (");
            query.push_values(
                batch.iter().enumerate(),
                |mut values, (offset, (leg, sequence, sealed))| {
                    values
                        .push_bind(leg.index() as i16)
                        .push_bind(*sequence as i64)
                        .push_bind((start + offset) as i64)
                        .push_bind(verified.elapsed_ns[start + offset] as i64)
                        .push_bind(sealed);
                },
            );
            query.push(") AS v(leg, sequence, position, elapsed_ns, sealed) ON b.leg = v.leg::smallint AND b.sequence = v.sequence::bigint AND b.position = v.position::bigint AND b.elapsed_ns = v.elapsed_ns::bigint AND b.sealed = v.sealed::bytea WHERE b.tenant_id = ")
                .push_bind(identity.tenant).push(" AND b.capture_id = ").push_bind(identity.capture);
            let matches: i64 = query
                .build_query_scalar()
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| CaptureArchiveError)?;
            if usize::try_from(matches).ok() != Some(batch.len()) {
                return Err(CaptureArchiveError);
            }
        } else {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) ",
            );
            query.push_values(
                batch.iter().enumerate(),
                |mut values, (offset, (leg, sequence, sealed))| {
                    values
                        .push_bind(identity.tenant)
                        .push_bind(identity.capture)
                        .push_bind(leg.index() as i16)
                        .push_bind(*sequence as i64)
                        .push_bind((start + offset) as i64)
                        .push_bind(verified.elapsed_ns[start + offset] as i64)
                        .push_bind(sealed);
                },
            );
            query.push(" ON CONFLICT (tenant_id, capture_id, leg, sequence) DO UPDATE SET sealed = capture_blocks.sealed WHERE capture_blocks.sealed = EXCLUDED.sealed AND capture_blocks.position = EXCLUDED.position AND capture_blocks.elapsed_ns = EXCLUDED.elapsed_ns");
            let saved = query
                .build()
                .execute(&mut *tx)
                .await
                .map_err(|_| CaptureArchiveError)?
                .rows_affected();
            if usize::try_from(saved).ok() != Some(batch.len()) {
                return Err(CaptureArchiveError);
            }
        }
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
    )
    .bind(identity.tenant)
    .bind(identity.capture)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    if usize::try_from(count).ok() != Some(verified.blocks.len()) {
        return Err(CaptureArchiveError);
    }
    sqlx::query("UPDATE capture_records SET state = $3 WHERE tenant_id = $1 AND capture_id = $2")
        .bind(identity.tenant)
        .bind(identity.capture)
        .bind(if verified.complete {
            "complete"
        } else {
            "incomplete"
        })
        .execute(&mut *tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    tx.commit().await.map_err(|_| CaptureArchiveError)
}

pub(crate) async fn save_block(
    pool: &PgPool,
    cipher: &CaptureCipher,
    leg: CaptureLeg,
    sequence: u64,
    position: i64,
    elapsed_ns: i64,
    sealed: &[u8],
) -> Result<(), CaptureArchiveError> {
    let _plaintext = Zeroizing::new(
        cipher
            .unseal_timed(
                leg,
                sequence,
                u64::try_from(position).map_err(|_| CaptureArchiveError)?,
                u64::try_from(elapsed_ns).map_err(|_| CaptureArchiveError)?,
                sealed,
            )
            .map_err(|_| CaptureArchiveError)?,
    );
    let identity = cipher.identity();
    let sequence = i64::try_from(sequence).map_err(|_| CaptureArchiveError)?;
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let locked: Option<i32> = sqlx::query_scalar("SELECT 1 FROM capture_records WHERE tenant_id = $1 AND capture_id = $2 AND state = 'recording' AND wrapped_key = $3 AND key_fingerprint = $4 FOR UPDATE")
        .bind(identity.tenant).bind(identity.capture).bind(cipher.wrapped_key()).bind(cipher.fingerprint().as_slice()).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if locked.is_none() {
        return Err(CaptureArchiveError);
    }
    let inserted = sqlx::query_scalar::<_, i32>(
        "INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) \
         SELECT tenant_id, capture_id, $3, $4, $5, $6, $7 FROM capture_records \
         WHERE tenant_id = $1 AND capture_id = $2 AND state = 'recording' AND wrapped_key = $8 \
         ON CONFLICT (tenant_id, capture_id, leg, sequence) DO UPDATE SET sealed = capture_blocks.sealed \
         WHERE capture_blocks.sealed = EXCLUDED.sealed AND capture_blocks.position = EXCLUDED.position AND capture_blocks.elapsed_ns = EXCLUDED.elapsed_ns RETURNING 1",
    )
    .bind(identity.tenant)
    .bind(identity.capture)
    .bind(leg.index() as i16)
    .bind(sequence)
    .bind(position)
    .bind(elapsed_ns)
    .bind(sealed)
    .bind(cipher.wrapped_key())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    inserted.ok_or(CaptureArchiveError)?;
    tx.commit().await.map_err(|_| CaptureArchiveError)
}

#[cfg(test)]
mod tests {
    use super::{
        delete_batch, expire_batch, import_spool, read_verified, recover_and_import,
        recover_and_import_keyring, remove_archived_spool, remove_deleted_spool, save_block,
    };
    use crate::capture_crypto::{CaptureCipher, CaptureIdentity, CaptureLeg};
    use crate::capture_key_usage::reserve_master_wrap;
    use crate::capture_spool::CaptureSpool;
    use sqlx::PgPool;
    use uuid::Uuid;

    #[sqlx::test]
    async fn deleted_incomplete_spool_is_removed_only_after_tombstone_and_unlock(pool: PgPool) {
        use crate::capture_anchor::{create_versioned_key, reserve_anchored};
        use crate::capture_spool::CaptureLimits;
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let base = std::env::temp_dir().join(format!("capture-delete-spool-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let spool_dir = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        for dir in [&spool_dir, &keys, &anchors] {
            std::fs::create_dir(dir).unwrap();
        }
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let master = create_versioned_key(&keys, &anchors).unwrap();
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let limits = CaptureLimits::for_test(4096);
        let mut spool =
            CaptureSpool::create_with_permit(&spool_dir, identity, &master, &[], permit, limits)
                .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"incomplete content")
            .unwrap();
        let path = spool.path().to_owned();
        drop(spool);
        let capacity = Arc::new(Semaphore::new(1));
        assert!(
            !recover_and_import_keyring(&pool, &path, &keys, limits.per_file, capacity.clone())
                .await
                .unwrap()
        );
        assert!(
            remove_deleted_spool(&pool, &path, &keys, limits, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        assert!(
            delete_batch(&pool, identity.tenant, identity.user, identity.capture)
                .await
                .unwrap()
        );
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        lock.lock().unwrap();
        assert!(
            remove_deleted_spool(&pool, &path, &keys, limits, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        drop(lock);
        remove_deleted_spool(&pool, &path, &keys, limits, capacity)
            .await
            .unwrap();
        assert!(!path.exists());
        let state: String = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "deleted");
    }

    #[sqlx::test]
    async fn spool_is_removed_only_after_complete_matching_archive(pool: PgPool) {
        use crate::capture_anchor::{create_versioned_key, reserve_anchored};
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let base = std::env::temp_dir().join(format!("capture-cleanup-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let spool_dir = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        std::fs::create_dir(&spool_dir).unwrap();
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        let master = create_versioned_key(&keys, &anchors).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let limits = crate::capture_spool::CaptureLimits::for_test(4096);
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let mut spool =
            CaptureSpool::create_with_permit(&spool_dir, identity, &master, &[], permit, limits)
                .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"keep me")
            .unwrap();
        let path = spool.path().to_owned();
        let capacity = Arc::new(Semaphore::new(1));
        assert!(
            remove_archived_spool(&pool, &path, &keys, limits, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        spool.finish().unwrap();
        assert!(
            remove_archived_spool(&pool, &path, &keys, limits, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        recover_and_import_keyring(&pool, &path, &keys, limits.per_file, capacity.clone())
            .await
            .unwrap();
        let verified = CaptureSpool::recover_with_keyring(&path, &keys, limits.per_file).unwrap();
        sqlx::query("UPDATE capture_blocks SET elapsed_ns = elapsed_ns + 1 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            remove_archived_spool(&pool, &path, &keys, limits, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        sqlx::query("UPDATE capture_blocks SET elapsed_ns = elapsed_ns - 1 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        remove_archived_spool(&pool, &path, &keys, limits, capacity)
            .await
            .unwrap();
        assert!(!path.exists());
        let (elapsed, sealed): (i64, Vec<u8>) = sqlx::query_as("SELECT elapsed_ns, sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert_eq!(
            verified
                .cipher
                .unseal_timed(CaptureLeg::ClientRequest, 0, 0, elapsed as u64, &sealed)
                .unwrap(),
            b"keep me"
        );
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 2);
    }

    #[sqlx::test]
    async fn interrupted_record_is_verified_and_marked_incomplete(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("capture-incomplete-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [57u8; 32];
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let credential = Uuid::new_v4();
        sqlx::query("INSERT INTO credentials (id, tenant_id, type, display_name, secret_hash, key_prefix, fingerprint, user_id, hash_key_id) VALUES ($1, $2, 'client_api_key', 'test', $3, 'test', $3, $4, 'v1')")
            .bind(credential).bind(identity.tenant).bind(vec![3u8; 32]).bind(identity.user).execute(&pool).await.unwrap();
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"first")
            .unwrap();
        spool
            .append(CaptureLeg::ProviderResponse, 0, b"second")
            .unwrap();
        let path = spool.path().to_owned();
        assert!(CaptureSpool::recover(&path, &master, 4096).is_err());
        assert!(CaptureSpool::recover_incomplete(&path, &master, 4096).is_err());
        drop(spool);
        let partial = CaptureSpool::recover_incomplete(&path, &master, 4096).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.blocks.len(), 2);
        let bytes = std::fs::read(&path).unwrap();
        let mut damaged = bytes.clone();
        damaged[178] ^= 1;
        std::fs::write(&path, &damaged).unwrap();
        assert!(CaptureSpool::recover_incomplete(&path, &master, 4096).is_err());
        std::fs::write(&path, &bytes).unwrap();
        import_spool(&pool, &partial, identity.user, 30)
            .await
            .unwrap();
        let state: String = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "incomplete");
        assert!(
            read_verified(&pool, identity, Uuid::new_v4(), credential, &master, 4096)
                .await
                .is_err()
        );
        let retrieved = read_verified(&pool, identity, identity.user, credential, &master, 4096)
            .await
            .unwrap();
        assert!(!retrieved.complete);
        assert_eq!(
            retrieved
                .cipher
                .unseal_timed(
                    retrieved.blocks[1].0,
                    retrieved.blocks[1].1,
                    1,
                    retrieved.elapsed_ns[1],
                    &retrieved.blocks[1].2
                )
                .unwrap(),
            b"second"
        );
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND target_id = $2 AND action = 'capture.read'")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert_eq!(audited, 1);
        sqlx::query("UPDATE capture_records SET state = 'complete' WHERE tenant_id = $1 AND capture_id = $2")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            read_verified(&pool, identity, identity.user, credential, &master, 4096)
                .await
                .is_err()
        );
        sqlx::query("UPDATE capture_records SET state = 'incomplete' WHERE tenant_id = $1 AND capture_id = $2")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        import_spool(&pool, &partial, identity.user, 30)
            .await
            .unwrap();
        assert!(CaptureSpool::recover(&path, &master, 4096).is_err());
    }

    #[sqlx::test]
    async fn recovered_file_retries_database_failure_without_duplicates(pool: PgPool) {
        use std::sync::Arc;
        use tokio::sync::Semaphore;

        let root = std::env::temp_dir().join(format!("capture-recover-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis() - 86_400_000,
            retention_days: 30,
        };
        let master = [55u8; 32];
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"recover me")
            .unwrap();
        let path = spool.path().to_owned();
        spool.finish().unwrap();
        sqlx::query("CREATE FUNCTION reject_capture_test_import() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test failure'; END; $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_capture_test_import BEFORE INSERT ON capture_records FOR EACH ROW EXECUTE FUNCTION reject_capture_test_import()")
            .execute(&pool).await.unwrap();
        let capacity = Arc::new(Semaphore::new(1));
        assert!(
            recover_and_import(&pool, &path, &master, 4096, capacity.clone())
                .await
                .is_err()
        );
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
        assert!(path.exists());
        sqlx::query("DROP TRIGGER reject_capture_test_import ON capture_records")
            .execute(&pool)
            .await
            .unwrap();
        recover_and_import(&pool, &path, &master, 4096, capacity.clone())
            .await
            .unwrap();
        let verified = CaptureSpool::recover(&path, &master, 4096).unwrap();
        assert!(
            import_spool(&pool, &verified, identity.user, 31)
                .await
                .is_err()
        );
        let expires_ms: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM expires_at) * 1000)::bigint FROM capture_records WHERE tenant_id = $1 AND capture_id = $2")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert_eq!(expires_ms, identity.created_ms + 30 * 86_400_000);
        recover_and_import(&pool, &path, &master, 4096, capacity.clone())
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 2);
        assert!(path.exists());
    }

    #[sqlx::test]
    async fn concurrent_delete_and_revocation_wait_for_verified_read(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("capture-concurrent-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let user = identity.user;
        let credential = Uuid::new_v4();
        let master = [47u8; 32];
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO credentials (id, tenant_id, type, display_name, secret_hash, key_prefix, fingerprint, user_id, hash_key_id) VALUES ($1, $2, 'client_api_key', 'test', $3, 'test', $3, $4, 'v1')")
            .bind(credential).bind(identity.tenant).bind(vec![2u8; 32]).bind(user).execute(&pool).await.unwrap();
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"prompt")
            .unwrap();
        let path = spool.path().to_owned();
        spool.finish().unwrap();
        let verified = CaptureSpool::read_verified(&path, identity, &master, 4096).unwrap();
        import_spool(&pool, &verified, user, 30).await.unwrap();
        sqlx::query("CREATE FUNCTION hold_capture_test_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(724601); RETURN NEW; END; $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER hold_capture_test_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION hold_capture_test_audit()")
            .execute(&pool).await.unwrap();
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(724601)")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let reader_pool = pool.clone();
        let reader = tokio::spawn(async move {
            read_verified(&reader_pool, identity, user, credential, &master, 4096)
                .await
                .is_ok()
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND objid = 724601 AND NOT granted)")
                    .fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let delete_pool = pool.clone();
        let mut deletion = tokio::spawn(async move {
            delete_batch(&delete_pool, identity.tenant, user, identity.capture).await
        });
        let revoke_pool = pool.clone();
        let mut revocation = tokio::spawn(async move {
            sqlx::query("UPDATE credentials SET status = 'disabled' WHERE id = $1")
                .bind(credential)
                .execute(&revoke_pool)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND (query LIKE 'SELECT state FROM capture_records%FOR UPDATE' OR query LIKE 'UPDATE credentials SET status = %')")
                    .fetch_one(&pool).await.unwrap();
                if waiting == 2 { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut deletion)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut revocation)
                .await
                .is_err()
        );
        blocker.commit().await.unwrap();
        assert!(reader.await.unwrap());
        assert!(deletion.await.unwrap().unwrap());
        assert!(revocation.await.unwrap().is_ok());
        assert!(
            read_verified(&pool, identity, user, credential, &master, 4096)
                .await
                .is_err()
        );
    }

    #[sqlx::test]
    async fn expiration_removes_only_due_records_in_bounded_batches(pool: PgPool) {
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let expired = Uuid::new_v4();
        let fresh = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, created_at, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, now() - interval '2 days', now() - interval '1 day', 'complete', decode(repeat('01',32),'hex')), ($1, $5, $3, $4, now(), now() + interval '1 day', 'complete', decode(repeat('01',32),'hex'))")
            .bind(tenant).bind(expired).bind(user).bind(vec![1u8; 60]).bind(fresh).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) SELECT $1, $2, 0, n, n, n, $3 FROM generate_series(0, 69) n")
            .bind(tenant).bind(expired).bind(vec![1u8; 28]).execute(&pool).await.unwrap();
        assert!(expire_batch(&pool).await.unwrap());
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(tenant)
        .bind(expired)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 6);
        assert!(expire_batch(&pool).await.unwrap());
        assert!(!expire_batch(&pool).await.unwrap());
        let states: Vec<String> = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 ORDER BY state",
        )
        .bind(tenant)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(states, vec!["complete", "deleted"]);
    }

    #[sqlx::test]
    async fn complete_spool_import_is_atomic_and_repeatable(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("capture-import-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let user = identity.user;
        let credential = Uuid::new_v4();
        let master = [44u8; 32];
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO credentials (id, tenant_id, type, display_name, secret_hash, key_prefix, fingerprint, user_id, hash_key_id) VALUES ($1, $2, 'client_api_key', 'test', $3, 'test', $3, $4, 'v1')")
            .bind(credential).bind(identity.tenant).bind(vec![1u8; 32]).bind(user).execute(&pool).await.unwrap();
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(16384),
        )
        .await
        .unwrap();
        for sequence in 0..70 {
            spool
                .append(CaptureLeg::ClientRequest, sequence, b"private prompt")
                .unwrap();
            if sequence == 34 {
                spool
                    .append(CaptureLeg::ClientResponse, 0, b"private response")
                    .unwrap();
            }
        }
        let path = spool.path().to_owned();
        assert!(CaptureSpool::read_verified(&path, identity, &master, 16384).is_err());
        spool.finish().unwrap();
        let verified = CaptureSpool::recover(&path, &master, 16384).unwrap();
        assert_eq!(verified.cipher.identity().user, user);
        assert!(
            import_spool(&pool, &verified, Uuid::new_v4(), 30)
                .await
                .is_err()
        );
        assert!(import_spool(&pool, &verified, user, 366).await.is_err());
        import_spool(&pool, &verified, user, 30).await.unwrap();
        import_spool(&pool, &verified, user, 30).await.unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 72);
        let order: Vec<(i16, i64, i64, i64)> = sqlx::query_as("SELECT leg, sequence, position, elapsed_ns FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 ORDER BY position")
            .bind(identity.tenant).bind(identity.capture).fetch_all(&pool).await.unwrap();
        assert_eq!((order[0].0, order[0].1, order[0].2), (0, 0, 0));
        assert_eq!((order[35].0, order[35].1, order[35].2), (3, 0, 35));
        assert_eq!((order[70].0, order[70].1, order[70].2), (0, 69, 70));
        assert_eq!((order[71].0, order[71].2), (4, 71));
        assert!(order[..71].windows(2).all(|pair| pair[0].3 <= pair[1].3));
        assert!(
            read_verified(&pool, identity, Uuid::new_v4(), credential, &master, 16384)
                .await
                .is_err()
        );
        assert!(
            read_verified(&pool, identity, user, Uuid::new_v4(), &master, 16384)
                .await
                .is_err()
        );
        assert!(
            read_verified(&pool, identity, user, credential, &master, 100)
                .await
                .is_err()
        );
        let retrieved = read_verified(&pool, identity, user, credential, &master, 16384)
            .await
            .unwrap();
        assert_eq!(
            retrieved
                .cipher
                .unseal_timed(
                    retrieved.blocks[35].0,
                    retrieved.blocks[35].1,
                    35,
                    retrieved.elapsed_ns[35],
                    &retrieved.blocks[35].2
                )
                .unwrap(),
            b"private response"
        );
        sqlx::query("UPDATE capture_blocks SET elapsed_ns = elapsed_ns + 1 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            read_verified(&pool, identity, user, credential, &master, 16384)
                .await
                .is_err()
        );
        sqlx::query("UPDATE capture_blocks SET elapsed_ns = elapsed_ns - 1 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        sqlx::query("UPDATE capture_blocks SET position = 1000 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            read_verified(&pool, identity, user, credential, &master, 16384)
                .await
                .is_err()
        );
        sqlx::query("UPDATE capture_blocks SET position = 0 WHERE tenant_id = $1 AND capture_id = $2 AND position = 1000")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            read_verified(&pool, identity, user, credential, &master, 16384)
                .await
                .is_err()
        );
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        sqlx::query("UPDATE credentials SET status = 'disabled' WHERE id = $1")
            .bind(credential)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            read_verified(&pool, identity, user, credential, &master, 16384)
                .await
                .is_err()
        );
        sqlx::query("UPDATE credentials SET status = 'active' WHERE id = $1")
            .bind(credential)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE FUNCTION reject_capture_test_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test failure'; END; $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_capture_test_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_capture_test_audit()")
            .execute(&pool).await.unwrap();
        assert!(
            read_verified(&pool, identity, user, credential, &master, 16384)
                .await
                .is_err()
        );
        sqlx::query("DROP TRIGGER reject_capture_test_audit ON audit_logs")
            .execute(&pool)
            .await
            .unwrap();
        let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND action = 'capture.read' AND target_id = $2")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert_eq!(audits, 1);
        let state: String = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "complete");
        let blocks: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(blocks.iter().all(|block| {
            !block
                .windows(b"private".len())
                .any(|part| part == b"private")
        }));
        assert!(
            delete_batch(&pool, identity.tenant, Uuid::new_v4(), identity.capture)
                .await
                .is_err()
        );
        assert!(
            delete_batch(&pool, Uuid::new_v4(), user, identity.capture)
                .await
                .is_err()
        );
        sqlx::query("CREATE FUNCTION reject_capture_test_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test failure'; END; $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_capture_test_delete BEFORE DELETE ON capture_blocks FOR EACH ROW EXECUTE FUNCTION reject_capture_test_delete()")
            .execute(&pool).await.unwrap();
        assert!(
            delete_batch(&pool, identity.tenant, user, identity.capture)
                .await
                .is_err()
        );
        let unchanged: bool = sqlx::query_scalar("SELECT state = 'complete' FROM capture_records WHERE tenant_id = $1 AND capture_id = $2")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert!(unchanged);
        sqlx::query("DROP TRIGGER reject_capture_test_delete ON capture_blocks")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !delete_batch(&pool, identity.tenant, user, identity.capture)
                .await
                .unwrap()
        );
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 8);
        assert!(import_spool(&pool, &verified, user, 30).await.is_err());
        assert!(
            delete_batch(&pool, identity.tenant, user, identity.capture)
                .await
                .unwrap()
        );
        assert!(
            delete_batch(&pool, identity.tenant, user, identity.capture)
                .await
                .unwrap()
        );
        let key_removed: bool = sqlx::query_scalar("SELECT state = 'deleted' AND wrapped_key IS NULL FROM capture_records WHERE tenant_id = $1 AND capture_id = $2")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert!(key_removed);
    }

    #[sqlx::test]
    async fn conflicting_block_rolls_back_earlier_blocks(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("capture-rollback-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let user = identity.user;
        let master = [46u8; 32];
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(16384),
        )
        .await
        .unwrap();
        for sequence in 0..70 {
            spool
                .append(CaptureLeg::ClientRequest, sequence, b"first")
                .unwrap();
        }
        spool
            .append(CaptureLeg::ClientResponse, 0, b"second")
            .unwrap();
        let path = spool.path().to_owned();
        spool.finish().unwrap();
        let verified = CaptureSpool::read_verified(&path, identity, &master, 16384).unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, created_at, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, to_timestamp($5::bigint::double precision / 1000), to_timestamp($5::bigint::double precision / 1000) + interval '30 days', 'recording', $6)")
            .bind(identity.tenant).bind(identity.capture).bind(user).bind(verified.cipher.wrapped_key()).bind(identity.created_ms).bind(verified.cipher.fingerprint().as_slice()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) VALUES ($1, $2, 3, 0, 70, 0, $3)")
            .bind(identity.tenant).bind(identity.capture).bind(vec![42u8; 28]).execute(&pool).await.unwrap();
        assert!(import_spool(&pool, &verified, user, 30).await.is_err());
        let legs: Vec<i16> = sqlx::query_scalar(
            "SELECT leg FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(legs, vec![3]);
    }

    #[sqlx::test]
    async fn replay_accepts_only_the_same_authenticated_block(pool: PgPool) {
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let user = identity.user;
        let master = [45u8; 32];
        let permit = reserve_master_wrap(&pool, &master).await.unwrap();
        let cipher = CaptureCipher::create(&master, identity, permit).unwrap();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, now() + interval '30 days', 'recording', $5)")
            .bind(identity.tenant).bind(identity.capture).bind(user).bind(cipher.wrapped_key()).bind(cipher.fingerprint().as_slice()).execute(&pool).await.unwrap();
        let sealed = cipher
            .seal_timed(CaptureLeg::ClientRequest, 0, 0, 0, b"private prompt")
            .unwrap();
        save_block(&pool, &cipher, CaptureLeg::ClientRequest, 0, 0, 0, &sealed)
            .await
            .unwrap();
        save_block(&pool, &cipher, CaptureLeg::ClientRequest, 0, 0, 0, &sealed)
            .await
            .unwrap();
        let replacement = cipher
            .seal_timed(CaptureLeg::ClientRequest, 0, 0, 0, b"different")
            .unwrap();
        assert!(
            save_block(
                &pool,
                &cipher,
                CaptureLeg::ClientRequest,
                0,
                0,
                0,
                &replacement
            )
            .await
            .is_err()
        );
        assert!(
            save_block(&pool, &cipher, CaptureLeg::ClientRequest, 1, 1, 0, &sealed)
                .await
                .is_err()
        );
        let stored: Vec<u8> = sqlx::query_scalar(
            "SELECT sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, sealed);
        assert!(
            !stored
                .windows(b"private prompt".len())
                .any(|part| part == b"private prompt")
        );
    }
}
