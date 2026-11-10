#[cfg(test)]
use crate::capture_crypto::CaptureCipher;
use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
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

pub(crate) enum AdminCaptureError {
    NotFound,
    Unavailable,
}

impl From<CaptureArchiveError> for AdminCaptureError {
    fn from(_: CaptureArchiveError) -> Self {
        Self::Unavailable
    }
}

async fn persist_erasure(
    active: Arc<crate::capture_erasure::ActiveErasure>,
    tenant: Uuid,
    capture: Uuid,
) -> Result<(), CaptureArchiveError> {
    let permit = active
        .try_remote_permit()
        .map_err(|_| CaptureArchiveError)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        active.append(tenant, capture)
    })
    .await
    .map_err(|_| CaptureArchiveError)?
    .map_err(|_| CaptureArchiveError)
}

async fn lock_capture_erasure(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    tenant: Uuid,
    capture: Uuid,
) -> Result<(), CaptureArchiveError> {
    let mut hash = sha2::Sha256::new();
    hash.update(tenant.as_bytes());
    hash.update(capture.as_bytes());
    let digest = hash.finalize();
    let key = i32::from_be_bytes(digest[..4].try_into().map_err(|_| CaptureArchiveError)?);
    let locked =
        sqlx::query_file_scalar!("queries/capture_erasure_try_lock.sql", 0x41504552_i32, key)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| CaptureArchiveError)?;
    if !locked {
        return Err(CaptureArchiveError);
    }
    Ok(())
}

async fn mark_capture_erased(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    tenant: Uuid,
    capture: Uuid,
) -> Result<(), CaptureArchiveError> {
    sqlx::query_file!("queries/capture_erasure_mark.sql", tenant, capture)
        .execute(&mut **tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    Ok(())
}

pub(crate) async fn owned_capture_exists(
    pool: &PgPool,
    tenant: Uuid,
    user: Uuid,
    capture: Uuid,
) -> Result<bool, CaptureArchiveError> {
    sqlx::query_file_scalar!("queries/capture_owned_exists.sql", tenant, user, capture)
        .fetch_one(pool)
        .await
        .map_err(|_| CaptureArchiveError)
}

pub(crate) async fn admin_content(
    state: Arc<crate::state::AppState>,
    context: crate::admin::AdminContext,
    capture: Uuid,
    leg: CaptureLeg,
) -> Result<axum::body::Body, AdminCaptureError> {
    if context.role != crate::admin::AdminRole::Admin || leg.index() >= CaptureLeg::Manifest.index()
    {
        return Err(CaptureArchiveError.into());
    }
    let permit = state
        .capture_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| CaptureArchiveError)?;
    let row = sqlx::query_file!(
        "queries/capture_admin_details.sql",
        context.tenant_id,
        capture
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| CaptureArchiveError)?
    .ok_or(AdminCaptureError::NotFound)?;
    let fingerprint: [u8; 32] = row
        .key_fingerprint
        .try_into()
        .map_err(|_| CaptureArchiveError)?;
    let identity = CaptureIdentity {
        tenant: context.tenant_id,
        capture,
        user: row.user_id,
        created_ms: row.created_ms,
        retention_days: u16::try_from(row.days).map_err(|_| CaptureArchiveError)?,
    };
    let root = state.config.content_capture_key_dir.clone();
    let master = tokio::task::spawn_blocking(move || {
        crate::capture_master_key::load_versioned_key(&root, &fingerprint)
    })
    .await
    .map_err(|_| CaptureArchiveError)?
    .map_err(|_| CaptureArchiveError)?;
    let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let (done_sender, done_receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _permit = permit;
        let result = async {
            sqlx::query("INSERT INTO audit_logs (id, tenant_id, actor_admin_id, actor_role, action, target_type, target_id, after_state, occurred_at) VALUES ($1, $2, $3, 'admin', 'capture.content.read', 'capture', $4, jsonb_build_object('leg', $5::integer), now())")
                .bind(Uuid::new_v4()).bind(identity.tenant).bind(context.account_id).bind(identity.capture).bind(leg.index() as i32)
                .execute(&state.pool).await.map_err(|_| CaptureArchiveError)?;
            let (verified, mut snapshot) = open_admin_verified(&state.pool, identity, &context, &master, CaptureLimits::default().per_file).await?;
            ready_sender.send(()).map_err(|_| CaptureArchiveError)?;
            write_verified_leg(&mut snapshot, &verified, identity, leg, &mut writer).await?;
            snapshot.commit().await.map_err(|_| CaptureArchiveError)?;
            Ok::<(), CaptureArchiveError>(())
        }.await;
        drop(writer);
        if result.is_err() {
            tracing::warn!(error.type = "capture_content_read_failed");
        }
        let _ = done_sender.send(result);
    });
    ready_receiver.await.map_err(|_| CaptureArchiveError)?;
    let stream = async_stream::stream! {
        use tokio::io::AsyncReadExt as _;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(&buffer[..n])),
                Err(error) => { yield Err(error); return; }
            }
        }
        if !matches!(done_receiver.await, Ok(Ok(()))) {
            yield Err(std::io::Error::other("capture content read failed"));
        }
    };
    Ok(axum::body::Body::from_stream(stream))
}

pub(crate) async fn delete_coordinated(
    pool: &PgPool,
    spool: &Path,
    identity: CaptureIdentity,
    credential: Uuid,
    erasure: Arc<crate::capture_erasure::ActiveErasure>,
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
    lock_capture_erasure(&mut tx, identity.tenant, identity.capture).await?;
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM capture_records WHERE tenant_id = $1 AND user_id = $2 AND capture_id = $3 FOR UPDATE")
        .bind(identity.tenant).bind(identity.user).bind(identity.capture).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if state.is_none() {
        return Err(CaptureArchiveError);
    }
    persist_erasure(erasure, identity.tenant, identity.capture).await?;
    mark_capture_erased(&mut tx, identity.tenant, identity.capture).await?;
    let done = match state.as_deref() {
        Some("deleted") => true,
        _ => delete_locked_batch(&mut tx, identity.tenant, identity.capture).await?,
    };
    sqlx::query("INSERT INTO audit_logs (id, tenant_id, actor_credential_id, action, target_type, target_id, after_state, occurred_at) VALUES ($1, $2, $3, 'capture.delete', 'capture', $4, jsonb_build_object('complete', $5::boolean), now())")
        .bind(Uuid::new_v4()).bind(identity.tenant).bind(credential).bind(identity.capture).bind(done)
        .execute(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
pub(crate) async fn read_admin_keyring(
    pool: &PgPool,
    identity: CaptureIdentity,
    context: &crate::admin::AdminContext,
    key_root: &Path,
    max_bytes: u64,
) -> Result<VerifiedSpool, CaptureArchiveError> {
    if context.tenant_id != identity.tenant || context.role != crate::admin::AdminRole::Admin {
        return Err(CaptureArchiveError);
    }
    let fingerprint: Option<Vec<u8>> = sqlx::query_scalar("SELECT key_fingerprint FROM capture_records WHERE tenant_id = $1 AND capture_id = $2 AND user_id = $3 AND state IN ('complete', 'incomplete') AND expires_at > now()")
        .bind(identity.tenant).bind(identity.capture).bind(identity.user).fetch_optional(pool).await.map_err(|_| CaptureArchiveError)?;
    let fingerprint: [u8; 32] = fingerprint
        .ok_or(CaptureArchiveError)?
        .try_into()
        .map_err(|_| CaptureArchiveError)?;
    let key_root = key_root.to_owned();
    let master = tokio::task::spawn_blocking(move || {
        crate::capture_master_key::load_versioned_key(&key_root, &fingerprint)
    })
    .await
    .map_err(|_| CaptureArchiveError)?
    .map_err(|_| CaptureArchiveError)?;
    read_admin_verified(pool, identity, context, &master, max_bytes).await
}

async fn open_admin_verified<'a>(
    pool: &'a PgPool,
    identity: CaptureIdentity,
    context: &crate::admin::AdminContext,
    master: &[u8; 32],
    max_bytes: u64,
) -> Result<(VerifiedSpool, sqlx::Transaction<'a, Postgres>), CaptureArchiveError> {
    if identity.tenant != context.tenant_id || context.role != crate::admin::AdminRole::Admin {
        return Err(CaptureArchiveError);
    }
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    let record: Option<(Vec<u8>, String)> = sqlx::query_as("SELECT r.wrapped_key, r.state FROM capture_records r JOIN admin_sessions s ON s.tenant_id = r.tenant_id JOIN admin_accounts a ON (a.tenant_id, a.id) = (s.tenant_id, s.admin_account_id) AND a.version = s.account_version JOIN admin_login_credentials c ON (c.tenant_id, c.id, c.admin_account_id) = (s.tenant_id, s.login_credential_id, s.admin_account_id) AND c.version = s.login_credential_version WHERE r.tenant_id = $1 AND r.capture_id = $2 AND r.user_id = $3 AND s.id = $4 AND a.id = $5 AND a.role = 'admin' AND a.status = 'active' AND s.revoked_at IS NULL AND s.expires_at > now() AND c.status = 'active' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at > now()) AND r.state IN ('complete', 'incomplete') AND r.expires_at > now() AND r.created_at = to_timestamp($6::bigint::double precision / 1000) AND r.expires_at = r.created_at + make_interval(days => $7) AND r.key_fingerprint = $8 FOR SHARE OF r, s, a, c")
        .bind(identity.tenant).bind(identity.capture).bind(identity.user).bind(context.session_id).bind(context.account_id).bind(identity.created_ms).bind(i32::from(identity.retention_days)).bind(sha2::Sha256::digest(master).as_slice())
        .fetch_optional(&mut *tx).await.map_err(|error| {
            tracing::warn!(error.type = "capture_content_verification_failed", capture.phase = "authorization_snapshot", database.error_code = error.as_database_error().and_then(|database| database.code()).as_deref().unwrap_or("not_reported"));
            CaptureArchiveError
        })?;
    let (wrapped, state) = record.ok_or(CaptureArchiveError)?;
    let limit = i64::try_from(max_bytes).map_err(|_| CaptureArchiveError)?;
    let size: i64 = sqlx::query_scalar("SELECT 158 + COALESCE(sum(octet_length(sealed) + 21), 0) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2")
        .bind(identity.tenant).bind(identity.capture).fetch_one(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    if size > limit || size < 158 {
        return Err(CaptureArchiveError);
    }
    use futures_util::TryStreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    let (reader, sender) =
        std::os::unix::net::UnixStream::pair().map_err(|_| CaptureArchiveError)?;
    sender
        .set_nonblocking(true)
        .map_err(|_| CaptureArchiveError)?;
    let mut writer = tokio::net::UnixStream::from_std(sender).map_err(|_| CaptureArchiveError)?;
    let key = zeroize::Zeroizing::new(*master);
    let incomplete = state == "incomplete";
    let verifier = tokio::task::spawn_blocking(move || {
        CaptureSpool::verify_without_retaining(
            reader,
            size as u64,
            identity,
            &key,
            max_bytes,
            incomplete,
        )
    });
    let result = async {
        let mut header = Vec::with_capacity(158);
        header.extend_from_slice(b"APGCAP06");
        header.extend_from_slice(identity.tenant.as_bytes());
        header.extend_from_slice(identity.capture.as_bytes());
        header.extend_from_slice(identity.user.as_bytes());
        header.extend_from_slice(&identity.created_ms.to_be_bytes());
        header.extend_from_slice(&identity.retention_days.to_be_bytes());
        header.extend_from_slice(sha2::Sha256::digest(master).as_slice());
        header.extend_from_slice(&wrapped);
        writer.write_all(&header).await.map_err(|_| CaptureArchiveError)?;
        let mut rows = sqlx::query_as::<_, (i16, i64, i64, i64, Vec<u8>)>("SELECT leg, sequence, position, elapsed_ns, sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 ORDER BY position LIMIT 4000001")
            .bind(identity.tenant).bind(identity.capture).fetch(&mut *tx);
        let mut position = 0i64;
        let mut written = 158u64;
        while let Some((leg, sequence, row_position, elapsed, sealed)) = rows.try_next().await.map_err(|_| CaptureArchiveError)? {
            if row_position != position || leg < 0 || sequence < 0 || elapsed < 0 || sealed.len() > 1048604 {
                return Err(CaptureArchiveError);
            }
            written = written.checked_add(21 + sealed.len() as u64).filter(|value| *value <= size as u64).ok_or(CaptureArchiveError)?;
            let mut frame = [0u8; 21];
            frame[0] = u8::try_from(leg).map_err(|_| CaptureArchiveError)?;
            frame[1..9].copy_from_slice(&(sequence as u64).to_be_bytes());
            frame[9..13].copy_from_slice(&u32::try_from(sealed.len()).map_err(|_| CaptureArchiveError)?.to_be_bytes());
            frame[13..21].copy_from_slice(&(elapsed as u64).to_be_bytes());
            writer.write_all(&frame).await.map_err(|_| CaptureArchiveError)?;
            writer.write_all(&sealed).await.map_err(|_| CaptureArchiveError)?;
            position += 1;
        }
        if written != size as u64 { return Err(CaptureArchiveError); }
        Ok::<(), CaptureArchiveError>(())
    }.await;
    drop(writer);
    let verified = verifier
        .await
        .map_err(|_| CaptureArchiveError)?
        .map_err(|_| CaptureArchiveError)?;
    result?;
    if verified.complete != (state == "complete") {
        return Err(CaptureArchiveError);
    }
    Ok((verified, tx))
}

#[cfg(test)]
async fn write_admin_verified_leg(
    pool: &PgPool,
    identity: CaptureIdentity,
    context: &crate::admin::AdminContext,
    master: &[u8; 32],
    leg: CaptureLeg,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), CaptureArchiveError> {
    if leg.index() >= CaptureLeg::Manifest.index() {
        return Err(CaptureArchiveError);
    }
    let (verified, mut snapshot) = open_admin_verified(
        pool,
        identity,
        context,
        master,
        CaptureLimits::default().per_file,
    )
    .await?;
    sqlx::query("INSERT INTO audit_logs (id, tenant_id, actor_admin_id, actor_role, action, target_type, target_id, after_state, occurred_at) VALUES ($1, $2, $3, 'admin', 'capture.content.read', 'capture', $4, jsonb_build_object('leg', $5::integer), now())")
        .bind(Uuid::new_v4()).bind(identity.tenant).bind(context.account_id).bind(identity.capture).bind(leg.index() as i32)
        .execute(pool).await.map_err(|_| CaptureArchiveError)?;
    write_verified_leg(&mut snapshot, &verified, identity, leg, writer).await?;
    snapshot.commit().await.map_err(|_| CaptureArchiveError)?;
    Ok(())
}

async fn write_verified_leg(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    verified: &VerifiedSpool,
    identity: CaptureIdentity,
    leg: CaptureLeg,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), CaptureArchiveError> {
    use futures_util::TryStreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    if leg.index() >= CaptureLeg::Manifest.index() {
        return Err(CaptureArchiveError);
    }
    let mut rows = sqlx::query_as::<_, (i64, i64, i64, Vec<u8>)>("SELECT sequence, position, elapsed_ns, sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 AND leg = $3 ORDER BY sequence LIMIT 1000001")
        .bind(identity.tenant).bind(identity.capture).bind(leg.index() as i16).fetch(&mut **tx);
    let mut next = 0i64;
    while let Some((sequence, position, elapsed, sealed)) =
        rows.try_next().await.map_err(|_| CaptureArchiveError)?
    {
        if sequence != next || position < 0 || elapsed < 0 || sequence >= 1000000 {
            return Err(CaptureArchiveError);
        }
        let plaintext = Zeroizing::new(
            verified
                .cipher
                .unseal_timed(
                    leg,
                    sequence as u64,
                    position as u64,
                    elapsed as u64,
                    &sealed,
                )
                .map_err(|_| CaptureArchiveError)?,
        );
        writer
            .write_all(&plaintext)
            .await
            .map_err(|_| CaptureArchiveError)?;
        next += 1;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn read_admin_verified(
    pool: &PgPool,
    identity: CaptureIdentity,
    context: &crate::admin::AdminContext,
    master: &[u8; 32],
    max_bytes: u64,
) -> Result<VerifiedSpool, CaptureArchiveError> {
    let (verified, mut tx) =
        open_admin_verified(pool, identity, context, master, max_bytes).await?;
    sqlx::query("INSERT INTO audit_logs (id, tenant_id, actor_admin_id, actor_role, action, target_type, target_id, occurred_at) VALUES ($1, $2, $3, 'admin', 'capture.content.read', 'capture', $4, now())")
        .bind(Uuid::new_v4()).bind(identity.tenant).bind(context.account_id).bind(identity.capture)
        .execute(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    tx.commit().await.map_err(|_| CaptureArchiveError)?;
    Ok(verified)
}

#[cfg(test)]
pub(crate) async fn delete_batch(
    pool: &PgPool,
    tenant: Uuid,
    user: Uuid,
    capture: Uuid,
) -> Result<bool, CaptureArchiveError> {
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    lock_capture_erasure(&mut tx, tenant, capture).await?;
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM capture_records WHERE tenant_id = $1 AND user_id = $2 AND capture_id = $3 FOR UPDATE")
        .bind(tenant).bind(user).bind(capture).fetch_optional(&mut *tx).await.map_err(|_| CaptureArchiveError)?;
    match state.as_deref() {
        None => return Err(CaptureArchiveError),
        Some("deleted") => {
            mark_capture_erased(&mut tx, tenant, capture).await?;
            tx.commit().await.map_err(|_| CaptureArchiveError)?;
            return Ok(true);
        }
        _ => {}
    }
    mark_capture_erased(&mut tx, tenant, capture).await?;
    let done = delete_locked_batch(&mut tx, tenant, capture).await?;
    tx.commit().await.map_err(|_| CaptureArchiveError)?;
    Ok(done)
}

pub(crate) async fn expire_batch(
    pool: &PgPool,
    erasure: Arc<crate::capture_erasure::ActiveErasure>,
) -> Result<bool, CaptureArchiveError> {
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    let target = sqlx::query_file!("queries/capture_expire_candidate.sql")
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    if let Some(target) = target {
        let (tenant, capture) = (target.tenant_id, target.capture_id);
        lock_capture_erasure(&mut tx, tenant, capture).await?;
        let present =
            sqlx::query_file_scalar!("queries/capture_expire_recheck.sql", tenant, capture)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| CaptureArchiveError)?;
        if present.is_none() {
            tx.commit().await.map_err(|_| CaptureArchiveError)?;
            return Ok(false);
        }
        persist_erasure(erasure, tenant, capture).await?;
        mark_capture_erased(&mut tx, tenant, capture).await?;
        delete_locked_batch(&mut tx, tenant, capture).await?;
        tx.commit().await.map_err(|_| CaptureArchiveError)?;
        Ok(true)
    } else {
        tx.commit().await.map_err(|_| CaptureArchiveError)?;
        Ok(false)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReplayBatch {
    Missing,
    Pending,
    Complete,
}

#[cfg(test)]
fn replay_isolated_spool(
    journal: &crate::capture_erasure::ErasureJournal,
    tenant: Uuid,
    capture: Uuid,
    path: &Path,
    key_root: &Path,
    limits: CaptureLimits,
) -> Result<(), CaptureArchiveError> {
    if !journal
        .verified_records()
        .map_err(|_| CaptureArchiveError)?
        .contains(&(tenant, capture))
    {
        return Err(CaptureArchiveError);
    }
    let (verified, locked) =
        CaptureSpool::open_deleted_for_cleanup(path, key_root, limits.per_file)
            .map_err(|_| CaptureArchiveError)?;
    let identity = verified.cipher.identity();
    if (identity.tenant, identity.capture) != (tenant, capture) {
        return Err(CaptureArchiveError);
    }
    CaptureSpool::remove_locked(locked, limits.total).map_err(|_| CaptureArchiveError)
}

async fn replay_deletion_batch(
    pool: &PgPool,
    tenant: Uuid,
    capture: Uuid,
) -> Result<ReplayBatch, CaptureArchiveError> {
    let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
    lock_capture_erasure(&mut tx, tenant, capture).await?;
    mark_capture_erased(&mut tx, tenant, capture).await?;
    let state = sqlx::query_file_scalar!("queries/capture_replay_lock.sql", tenant, capture)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    let result = match state.as_deref() {
        None => ReplayBatch::Missing,
        Some("deleted") => ReplayBatch::Complete,
        Some(_) if delete_locked_batch(&mut tx, tenant, capture).await? => ReplayBatch::Complete,
        Some(_) => ReplayBatch::Pending,
    };
    tx.commit().await.map_err(|_| CaptureArchiveError)?;
    Ok(result)
}

pub(crate) async fn replay_erasure(
    pool: &PgPool,
    erasure: Arc<crate::capture_erasure::ActiveErasure>,
    spool: &Path,
    key_root: &Path,
    limits: CaptureLimits,
    start: usize,
) -> Result<usize, CaptureArchiveError> {
    let root = spool.to_owned();
    let backup_guard = Arc::new(
        tokio::task::spawn_blocking(move || crate::capture_backup_barrier::try_shared(&root))
            .await
            .map_err(|_| CaptureArchiveError)?
            .map_err(|_| CaptureArchiveError)?,
    );
    let active = erasure.clone();
    let records = tokio::task::spawn_blocking(move || active.verified_records())
        .await
        .map_err(|_| CaptureArchiveError)?
        .map_err(|_| CaptureArchiveError)?;
    let count = records.len();
    if start > count {
        return Err(CaptureArchiveError);
    }
    for (index, (tenant, capture)) in records.into_iter().enumerate().skip(start) {
        let mut complete = false;
        for _ in 0..78_126 {
            if replay_deletion_batch(pool, tenant, capture).await? != ReplayBatch::Pending {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err(CaptureArchiveError);
        }
        let path = spool.join(format!("{capture}.capture"));
        match tokio::fs::symlink_metadata(&path).await {
            Ok(_) => {
                let active = erasure.clone();
                let backup_guard = backup_guard.clone();
                let keys = key_root.to_owned();
                tokio::task::spawn_blocking(move || {
                    let _active = active;
                    let _backup_guard = backup_guard;
                    let (verified, locked) =
                        match CaptureSpool::open_deleted_for_cleanup(&path, &keys, limits.per_file)
                        {
                            Ok(result) => result,
                            Err(_)
                                if CaptureSpool::is_truncated_for_cleanup(&path)
                                    .map_err(|_| CaptureArchiveError)? =>
                            {
                                tracing::warn!(error.type = "capture_truncated_spool_retained");
                                return Ok(());
                            }
                            Err(_) => return Err(CaptureArchiveError),
                        };
                    let identity = verified.cipher.identity();
                    if (identity.tenant, identity.capture) == (tenant, capture) {
                        CaptureSpool::remove_locked(locked, limits.total)
                            .map_err(|_| CaptureArchiveError)?;
                    }
                    Ok::<_, CaptureArchiveError>(())
                })
                .await
                .map_err(|_| CaptureArchiveError)??;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CaptureArchiveError),
        }
        if index.is_multiple_of(256) {
            tracing::info!(
                erasure.replayed = index + 1,
                "capture erasure recovery progressing"
            );
        }
    }
    Ok(count)
}

async fn delete_locked_batch(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    tenant: Uuid,
    capture: Uuid,
) -> Result<bool, CaptureArchiveError> {
    sqlx::query_file!("queries/capture_mark_deleting.sql", tenant, capture)
        .execute(&mut **tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    sqlx::query_file!("queries/capture_delete_blocks_batch.sql", tenant, capture)
        .execute(&mut **tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    let remains: bool = sqlx::query_file_scalar!("queries/capture_has_blocks.sql", tenant, capture)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| CaptureArchiveError)?;
    if !remains {
        sqlx::query_file!("queries/capture_mark_deleted.sql", tenant, capture)
            .execute(&mut **tx)
            .await
            .map_err(|_| CaptureArchiveError)?;
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
    lock_capture_erasure(&mut tx, identity.tenant, identity.capture).await?;
    let erased = sqlx::query_file_scalar!(
        "queries/capture_erasure_exists.sql",
        identity.tenant,
        identity.capture
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    if erased {
        return Err(CaptureArchiveError);
    }
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
    sqlx::query_file!(
        "queries/capture_import_state.sql",
        identity.tenant,
        identity.capture,
        if verified.complete {
            "complete"
        } else {
            "incomplete"
        }
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| CaptureArchiveError)?;
    tx.commit().await.map_err(|_| CaptureArchiveError)
}

#[cfg(test)]
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
    async fn replay_does_not_change_database_while_backup_holds_lock(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
        let root = std::env::temp_dir().join(format!("capture-replay-backup-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let tenant = Uuid::new_v4();
        let capture = Uuid::new_v4();
        erasure.active.append(tenant, capture).unwrap();
        let backup = crate::capture_backup_barrier::try_exclusive(&root).unwrap();
        assert!(
            super::replay_erasure(
                &pool,
                erasure.active.clone(),
                &root,
                &root,
                crate::capture_spool::CaptureLimits::default(),
                0
            )
            .await
            .is_err()
        );
        assert!(
            !sqlx::query_file_scalar!("queries/capture_erasure_exists.sql", tenant, capture)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        drop(backup);
        assert_eq!(
            super::replay_erasure(
                &pool,
                erasure.active.clone(),
                &root,
                &root,
                crate::capture_spool::CaptureLimits::default(),
                0
            )
            .await
            .unwrap(),
            1
        );
        assert!(
            sqlx::query_file_scalar!("queries/capture_erasure_exists.sql", tenant, capture)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[sqlx::test]
    async fn replay_keeps_truncated_spool_and_marks_later_deletions(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
        let root = std::env::temp_dir().join(format!("capture-truncated-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let first_tenant = Uuid::new_v4();
        let first_capture = Uuid::new_v4();
        let second_tenant = Uuid::new_v4();
        let second_capture = Uuid::new_v4();
        erasure.active.append(first_tenant, first_capture).unwrap();
        erasure
            .active
            .append(second_tenant, second_capture)
            .unwrap();
        let path = root.join(format!("{first_capture}.capture"));
        std::fs::write(&path, b"partial").unwrap();
        let replayed = super::replay_erasure(
            &pool,
            erasure.active.clone(),
            &root,
            &root,
            crate::capture_spool::CaptureLimits::default(),
            0,
        )
        .await
        .unwrap();
        assert_eq!(replayed, 2);
        assert_eq!(std::fs::read(&path).unwrap(), b"partial");
        for (tenant, capture) in [
            (first_tenant, first_capture),
            (second_tenant, second_capture),
        ] {
            assert!(
                sqlx::query_file_scalar!("queries/capture_erasure_exists.sql", tenant, capture)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            );
        }
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        locked.try_lock().unwrap();
        assert!(
            super::replay_erasure(
                &pool,
                erasure.active.clone(),
                &root,
                &root,
                crate::capture_spool::CaptureLimits::default(),
                0
            )
            .await
            .is_err()
        );
        drop(locked);
        std::fs::write(&path, [0u8; 157]).unwrap();
        assert_eq!(
            super::replay_erasure(
                &pool,
                erasure.active.clone(),
                &root,
                &root,
                crate::capture_spool::CaptureLimits::default(),
                0
            )
            .await
            .unwrap(),
            2
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 157);
        std::fs::write(&path, [0u8; 158]).unwrap();
        assert!(
            super::replay_erasure(
                &pool,
                erasure.active.clone(),
                &root,
                &root,
                crate::capture_spool::CaptureLimits::default(),
                0
            )
            .await
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[sqlx::test]
    async fn replaying_a_record_preserves_other_tenants_and_missing_intents(pool: PgPool) {
        use std::os::unix::fs::PermissionsExt;
        let root = std::path::Path::new("/tmp").join(format!("capture-replay-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let journal = root.join("journal");
        let witness = root.join("witness");
        for path in [&journal, &witness] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = crate::capture_erasure::ErasureJournal::initialize(&journal, &witness).unwrap();
        let tenant = Uuid::new_v4();
        let other = Uuid::new_v4();
        let capture = Uuid::new_v4();
        let missing = Uuid::new_v4();
        for owner in [tenant, other] {
            sqlx::query_file!(
                "queries/test_replay_seed.sql",
                Uuid::new_v4(),
                owner,
                capture
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(
            sqlx::query_file_scalar!("queries/test_replay_ambiguous_id.sql", capture)
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        store.append(tenant, capture).unwrap();
        store.append(tenant, missing).unwrap();
        drop(store);
        let store = crate::capture_erasure::ErasureJournal::open(&journal, &witness).unwrap();
        assert_eq!(
            store.verified_records().unwrap(),
            vec![(tenant, capture), (tenant, missing)]
        );
        let before = sqlx::query_file!("queries/test_replay_result.sql", tenant, capture)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (before.state.as_str(), before.key_removed, before.blocks),
            ("complete", false, 70)
        );
        assert_eq!(
            super::replay_deletion_batch(&pool, tenant, capture)
                .await
                .unwrap(),
            super::ReplayBatch::Pending
        );
        let first = sqlx::query_file!("queries/test_replay_result.sql", tenant, capture)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (first.state.as_str(), first.key_removed, first.blocks),
            ("deleting", false, 6)
        );
        assert_eq!(
            super::replay_deletion_batch(&pool, tenant, capture)
                .await
                .unwrap(),
            super::ReplayBatch::Complete
        );
        assert_eq!(
            super::replay_deletion_batch(&pool, tenant, capture)
                .await
                .unwrap(),
            super::ReplayBatch::Complete
        );
        assert_eq!(
            super::replay_deletion_batch(&pool, tenant, missing)
                .await
                .unwrap(),
            super::ReplayBatch::Missing
        );
        let deleted = sqlx::query_file!("queries/test_replay_result.sql", tenant, capture)
            .fetch_one(&pool)
            .await
            .unwrap();
        let untouched = sqlx::query_file!("queries/test_replay_result.sql", other, capture)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (deleted.state.as_str(), deleted.key_removed, deleted.blocks),
            ("deleted", true, 0)
        );
        assert_eq!(
            (
                untouched.state.as_str(),
                untouched.key_removed,
                untouched.blocks
            ),
            ("complete", false, 70)
        );
        assert_eq!(store.verified_records().unwrap().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[sqlx::test]
    async fn missing_record_with_deleted_spool_must_not_be_imported(pool: PgPool) {
        use crate::capture_anchor::{create_versioned_key, reserve_anchored};
        use crate::capture_spool::CaptureLimits;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let root =
            std::path::Path::new("/tmp").join(format!("capture-missing-replay-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let spool_dir = root.join("spool");
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        let journal = root.join("journal");
        let witness = root.join("witness");
        for path in [&spool_dir, &keys, &anchors, &journal, &witness] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        sqlx::query_file!(
            "queries/test_replay_user.sql",
            identity.user,
            identity.tenant
        )
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
            .append(CaptureLeg::ClientRequest, 0, b"deleted content")
            .unwrap();
        let path = spool.path().to_owned();
        drop(spool);
        let store = crate::capture_erasure::ErasureJournal::initialize(&journal, &witness).unwrap();
        let other_tenant = Uuid::new_v4();
        store.append(other_tenant, identity.capture).unwrap();
        assert!(
            super::replay_isolated_spool(
                &store,
                other_tenant,
                identity.capture,
                &path,
                &keys,
                limits
            )
            .is_err()
        );
        assert!(path.exists());
        store.append(identity.tenant, identity.capture).unwrap();
        let mut concurrent = pool.begin().await.unwrap();
        super::lock_capture_erasure(&mut concurrent, identity.tenant, identity.capture)
            .await
            .unwrap();
        let capacity = Arc::new(Semaphore::new(1));
        assert!(
            recover_and_import_keyring(&pool, &path, &keys, limits.per_file, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        concurrent.rollback().await.unwrap();
        assert_eq!(
            super::replay_deletion_batch(&pool, identity.tenant, identity.capture)
                .await
                .unwrap(),
            super::ReplayBatch::Missing
        );
        assert!(path.exists());
        assert!(
            recover_and_import_keyring(&pool, &path, &keys, limits.per_file, capacity.clone())
                .await
                .is_err()
        );
        assert!(path.exists());
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        held.lock().unwrap();
        assert!(
            super::replay_isolated_spool(
                &store,
                identity.tenant,
                identity.capture,
                &path,
                &keys,
                limits
            )
            .is_err()
        );
        assert!(path.exists());
        held.unlock().unwrap();
        drop(held);
        super::replay_isolated_spool(
            &store,
            identity.tenant,
            identity.capture,
            &path,
            &keys,
            limits,
        )
        .unwrap();
        assert!(!path.exists());
        assert!(
            recover_and_import_keyring(&pool, &path, &keys, limits.per_file, capacity)
                .await
                .is_err()
        );
        assert_eq!(
            super::replay_deletion_batch(&pool, identity.tenant, identity.capture)
                .await
                .unwrap(),
            super::ReplayBatch::Missing
        );
        std::fs::remove_dir_all(root).unwrap();
    }

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
        let retained = base.join("retained");
        std::fs::create_dir(&retained).unwrap();
        let retained_copy = retained.join(path.file_name().unwrap());
        std::fs::copy(&path, &retained_copy).unwrap();
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
        lock.unlock().unwrap();
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
        sqlx::query_file!(
            "queries/test_capture_fixture_remove.sql",
            identity.tenant,
            identity.capture
        )
        .execute(&pool)
        .await
        .unwrap();
        std::fs::rename(&retained_copy, &path).unwrap();
        assert!(
            recover_and_import_keyring(
                &pool,
                &path,
                &keys,
                limits.per_file,
                Arc::new(Semaphore::new(1))
            )
            .await
            .is_err()
        );
        assert!(path.exists());
        assert!(
            sqlx::query_file_scalar!(
                "queries/capture_erasure_exists.sql",
                identity.tenant,
                identity.capture
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        );
        std::fs::remove_dir_all(base).unwrap();
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
        let mut snapshot = pool.begin().await.unwrap();
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *snapshot)
            .await
            .unwrap();
        let initial = sqlx::query_file_scalar!(
            "queries/test_capture_snapshot_state.sql",
            identity.tenant,
            identity.capture
        )
        .fetch_one(&mut *snapshot)
        .await
        .unwrap();
        import_spool(&pool, &partial, identity.user, 30)
            .await
            .unwrap();
        let locked = sqlx::query_file_scalar!(
            "queries/test_capture_snapshot_lock.sql",
            identity.tenant,
            identity.capture
        )
        .fetch_one(&mut *snapshot)
        .await;
        assert!(
            locked.is_ok(),
            "unchanged import invalidated capture read: {locked:?}"
        );
        assert_eq!(initial, locked.unwrap());
        snapshot.commit().await.unwrap();
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
    async fn background_finishes_requested_deletion_before_expiry(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let capture = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, created_at, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, now(), now() + interval '1 day', 'deleting', decode(repeat('01',32),'hex'))")
            .bind(tenant).bind(capture).bind(user).bind(vec![1u8; 60]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) SELECT $1, $2, 0, n, n, n, $3 FROM generate_series(0, 69) n")
            .bind(tenant).bind(capture).bind(vec![1u8; 28]).execute(&pool).await.unwrap();
        assert!(expire_batch(&pool, erasure.active.clone()).await.unwrap());
        assert!(expire_batch(&pool, erasure.active.clone()).await.unwrap());
        assert!(!expire_batch(&pool, erasure.active.clone()).await.unwrap());
        let result: (String, bool, i64) = sqlx::query_as("SELECT state, wrapped_key IS NULL, (SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2) FROM capture_records WHERE tenant_id = $1 AND capture_id = $2")
            .bind(tenant).bind(capture).fetch_one(&pool).await.unwrap();
        assert_eq!(result, ("deleted".to_owned(), true, 0));
    }

    #[sqlx::test]
    async fn expiration_removes_only_due_records_in_bounded_batches(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
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
        assert!(expire_batch(&pool, erasure.active.clone()).await.unwrap());
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(tenant)
        .bind(expired)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 6);
        assert!(expire_batch(&pool, erasure.active.clone()).await.unwrap());
        assert!(!expire_batch(&pool, erasure.active.clone()).await.unwrap());
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
    async fn expiration_skips_a_locked_record_and_processes_the_next(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
        let first_tenant = Uuid::new_v4();
        let first_capture = Uuid::new_v4();
        let second_tenant = Uuid::new_v4();
        let second_capture = Uuid::new_v4();
        sqlx::query_file!(
            "queries/test_capture_expired_insert.sql",
            Uuid::new_v4(),
            first_tenant,
            first_capture
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query_file!(
            "queries/test_capture_expired_insert.sql",
            Uuid::new_v4(),
            second_tenant,
            second_capture
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut held = pool.begin().await.unwrap();
        assert!(
            sqlx::query_file_scalar!(
                "queries/capture_replay_lock.sql",
                first_tenant,
                first_capture
            )
            .fetch_optional(&mut *held)
            .await
            .unwrap()
            .is_some()
        );
        assert!(expire_batch(&pool, erasure.active.clone()).await.unwrap());
        assert!(
            sqlx::query_file_scalar!(
                "queries/test_capture_deleted.sql",
                second_tenant,
                second_capture
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        );
        held.rollback().await.unwrap();
        assert!(expire_batch(&pool, erasure.active.clone()).await.unwrap());
        assert!(
            sqlx::query_file_scalar!(
                "queries/test_capture_deleted.sql",
                first_tenant,
                first_capture
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        );
    }

    #[sqlx::test]
    async fn complete_spool_import_is_atomic_and_repeatable(pool: PgPool) {
        use sha2::Digest as _;
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let root = std::env::temp_dir().join(format!("capture-import-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        for path in [&keys, &anchors] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let user = identity.user;
        let credential = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let other_credential = Uuid::new_v4();
        let master = [44u8; 32];
        let fingerprint = sha2::Sha256::digest(master);
        let key_name: String = fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(keys.join(format!("{key_name}.key")))
            .unwrap()
            .write_all(&master)
            .unwrap();
        crate::capture_keyring::rotate(&keys, &anchors, &root).unwrap();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO credentials (id, tenant_id, type, display_name, secret_hash, key_prefix, fingerprint, user_id, hash_key_id) VALUES ($1, $2, 'client_api_key', 'test', $3, 'test', $3, $4, 'v1')")
            .bind(credential).bind(identity.tenant).bind(vec![1u8; 32]).bind(user).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'other')")
            .bind(other_user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO credentials (id, tenant_id, type, display_name, secret_hash, key_prefix, fingerprint, user_id, hash_key_id) VALUES ($1, $2, 'client_api_key', 'other', $3, 'other', $3, $4, 'v1')")
            .bind(other_credential).bind(identity.tenant).bind(vec![2u8; 32]).bind(other_user).execute(&pool).await.unwrap();
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(1048576),
        )
        .await
        .unwrap();
        let prompt_block = vec![b'p'; 4096];
        for sequence in 0..70 {
            spool
                .append(CaptureLeg::ClientRequest, sequence, &prompt_block)
                .unwrap();
            if sequence == 34 {
                spool
                    .append(CaptureLeg::ClientResponse, 0, b"private response")
                    .unwrap();
            }
        }
        let path = spool.path().to_owned();
        assert!(CaptureSpool::read_verified(&path, identity, &master, 1048576).is_err());
        spool.finish().unwrap();
        let verified = CaptureSpool::recover(&path, &master, 1048576).unwrap();
        let source = std::fs::File::open(&path).unwrap();
        let length = source.metadata().unwrap().len();
        assert!(length > 256 * 1024);
        let bounded = CaptureSpool::verify_without_retaining(
            source, length, identity, &master, 1048576, false,
        )
        .unwrap();
        assert!(bounded.complete && bounded.blocks.is_empty() && bounded.elapsed_ns.is_empty());
        let mut damaged = std::fs::read(&path).unwrap();
        damaged[158 + 21 + 12] ^= 1;
        assert!(
            CaptureSpool::verify_without_retaining(
                damaged.as_slice(),
                damaged.len() as u64,
                identity,
                &master,
                1048576,
                false
            )
            .is_err()
        );
        assert_eq!(verified.cipher.identity().user, user);
        assert!(
            import_spool(&pool, &verified, Uuid::new_v4(), 30)
                .await
                .is_err()
        );
        assert!(import_spool(&pool, &verified, user, 366).await.is_err());
        import_spool(&pool, &verified, user, 30).await.unwrap();
        import_spool(&pool, &verified, user, 30).await.unwrap();
        let admin_id = Uuid::new_v4();
        let login_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, 'admin', 'admin')")
            .bind(admin_id).bind(identity.tenant).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO admin_login_credentials (id, tenant_id, admin_account_id, display_name, secret_hash, hash_key_id, key_prefix, fingerprint) VALUES ($1, $2, $3, 'fixture', $4, 'v1', 'fixture', $4)")
            .bind(login_id).bind(identity.tenant).bind(admin_id).bind(vec![8u8; 32]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO admin_sessions (id, tenant_id, admin_account_id, login_credential_id, token_hash, hash_key_id, fingerprint, expires_at, source_ip, account_version, login_credential_version) VALUES ($1, $2, $3, $4, $5, 'v1', $5, now() + interval '1 hour', '127.0.0.1', 1, 1)")
            .bind(session_id).bind(identity.tenant).bind(admin_id).bind(login_id).bind(vec![9u8; 32]).execute(&pool).await.unwrap();
        let admin = crate::admin::AdminContext {
            tenant_id: identity.tenant,
            account_id: admin_id,
            session_id,
            role: crate::admin::AdminRole::Admin,
        };
        let mut snapshot = pool.begin().await.unwrap();
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *snapshot)
            .await
            .unwrap();
        let first: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&mut *snapshot).await.unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        let observed: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&mut *snapshot).await.unwrap();
        assert_eq!(first, observed);
        let changed: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert_ne!(first, changed);
        snapshot.rollback().await.unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = $3 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).bind(first).execute(&pool).await.unwrap();
        let admin_read = super::read_admin_verified(&pool, identity, &admin, &master, 1048576)
            .await
            .unwrap();
        assert!(admin_read.complete);
        assert!(admin_read.blocks.is_empty() && admin_read.elapsed_ns.is_empty());
        let (snapshot_verified, mut snapshot_tx) =
            super::open_admin_verified(&pool, identity, &admin, &master, 1048576)
                .await
                .unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 35")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        let (mut sender, mut receiver) = tokio::io::duplex(4096);
        super::write_verified_leg(
            &mut snapshot_tx,
            &snapshot_verified,
            identity,
            CaptureLeg::ClientResponse,
            &mut sender,
        )
        .await
        .unwrap();
        drop(sender);
        let mut response = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut receiver, &mut response)
            .await
            .unwrap();
        assert_eq!(response, b"private response");
        assert!(
            super::write_verified_leg(
                &mut snapshot_tx,
                &snapshot_verified,
                identity,
                CaptureLeg::Manifest,
                &mut tokio::io::sink()
            )
            .await
            .is_err()
        );
        snapshot_tx.rollback().await.unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 35")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            super::read_admin_verified(&pool, identity, &admin, &master, 100)
                .await
                .is_err()
        );
        let historical = super::read_admin_keyring(&pool, identity, &admin, &keys, 1048576)
            .await
            .unwrap();
        assert!(historical.complete);
        assert_ne!(
            crate::capture_keyring::current(&keys).unwrap().as_ref(),
            &master
        );
        let old_key = keys.join(format!("{key_name}.key"));
        let held_key = keys.join(format!("{key_name}.held"));
        std::fs::rename(&old_key, &held_key).unwrap();
        assert!(
            super::read_admin_keyring(&pool, identity, &admin, &keys, 1048576)
                .await
                .is_err()
        );
        std::fs::rename(&held_key, &old_key).unwrap();
        let (mut output, mut input) = tokio::io::duplex(4096);
        super::write_admin_verified_leg(
            &pool,
            identity,
            &admin,
            &master,
            CaptureLeg::ClientResponse,
            &mut output,
        )
        .await
        .unwrap();
        drop(output);
        let mut streamed = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut input, &mut streamed)
            .await
            .unwrap();
        assert_eq!(streamed, b"private response");
        let (mut interrupted, abandoned) = tokio::io::duplex(4096);
        drop(abandoned);
        assert!(
            super::write_admin_verified_leg(
                &pool,
                identity,
                &admin,
                &master,
                CaptureLeg::ClientResponse,
                &mut interrupted
            )
            .await
            .is_err()
        );
        let mut denied_admin = admin.clone();
        denied_admin.role = crate::admin::AdminRole::Viewer;
        assert!(
            super::read_admin_verified(&pool, identity, &denied_admin, &master, 1048576)
                .await
                .is_err()
        );
        denied_admin = admin.clone();
        denied_admin.tenant_id = Uuid::new_v4();
        assert!(
            super::read_admin_verified(&pool, identity, &denied_admin, &master, 1048576)
                .await
                .is_err()
        );
        let admin_audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND actor_admin_id = $2 AND target_id = $3 AND action = 'capture.content.read'")
            .bind(identity.tenant).bind(admin_id).bind(identity.capture).fetch_one(&pool).await.unwrap();
        assert_eq!(admin_audits, 4);
        assert!(
            super::owned_capture_exists(&pool, identity.tenant, user, identity.capture)
                .await
                .unwrap()
        );
        assert!(
            !super::owned_capture_exists(&pool, identity.tenant, other_user, identity.capture)
                .await
                .unwrap()
        );
        assert!(
            !super::owned_capture_exists(&pool, Uuid::new_v4(), user, identity.capture)
                .await
                .unwrap()
        );
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
            read_verified(
                &pool,
                identity,
                Uuid::new_v4(),
                credential,
                &master,
                1048576
            )
            .await
            .is_err()
        );
        assert!(
            read_verified(
                &pool,
                identity,
                other_user,
                other_credential,
                &master,
                1048576
            )
            .await
            .is_err()
        );
        assert!(
            read_verified(&pool, identity, user, other_credential, &master, 1048576)
                .await
                .is_err()
        );
        let erasure = crate::capture_erasure::TestErasure::new();
        assert!(
            super::delete_coordinated(
                &pool,
                &root,
                identity,
                other_credential,
                erasure.active.clone(),
                std::sync::Arc::new(tokio::sync::Semaphore::new(1))
            )
            .await
            .is_err()
        );
        let preserved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved, 72);
        assert!(
            read_verified(&pool, identity, user, Uuid::new_v4(), &master, 1048576)
                .await
                .is_err()
        );
        assert!(
            read_verified(&pool, identity, user, credential, &master, 100)
                .await
                .is_err()
        );
        let retrieved = read_verified(&pool, identity, user, credential, &master, 1048576)
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
            read_verified(&pool, identity, user, credential, &master, 1048576)
                .await
                .is_err()
        );
        sqlx::query("UPDATE capture_blocks SET elapsed_ns = elapsed_ns - 1 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        sqlx::query("UPDATE capture_blocks SET position = 1000 WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            super::read_admin_verified(&pool, identity, &admin, &master, 1048576)
                .await
                .is_err()
        );
        assert!(
            read_verified(&pool, identity, user, credential, &master, 1048576)
                .await
                .is_err()
        );
        sqlx::query("UPDATE capture_blocks SET position = 0 WHERE tenant_id = $1 AND capture_id = $2 AND position = 1000")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(identity.tenant).bind(identity.capture).execute(&pool).await.unwrap();
        assert!(
            super::read_admin_verified(&pool, identity, &admin, &master, 1048576)
                .await
                .is_err()
        );
        assert!(
            read_verified(&pool, identity, user, credential, &master, 1048576)
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
            read_verified(&pool, identity, user, credential, &master, 1048576)
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
            super::read_admin_verified(&pool, identity, &admin, &master, 1048576)
                .await
                .is_err()
        );
        let (mut output, mut input) = tokio::io::duplex(4096);
        assert!(
            super::write_admin_verified_leg(
                &pool,
                identity,
                &admin,
                &master,
                CaptureLeg::ClientResponse,
                &mut output
            )
            .await
            .is_err()
        );
        drop(output);
        let mut absent = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut input, &mut absent)
            .await
            .unwrap();
        assert!(absent.is_empty());
        assert!(
            read_verified(&pool, identity, user, credential, &master, 1048576)
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
