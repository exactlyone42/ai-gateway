use std::net::IpAddr;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{AdminContext, AdminError, AdminService, AuditEntry, append_audit};
use crate::held_token::PersonalTokens;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupCredentialKind {
    Subscription,
    Api,
}

pub struct GroupCredentialTarget {
    pub group: Uuid,
    pub provider: String,
    pub kind: GroupCredentialKind,
}

impl GroupCredentialKind {
    fn name(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::Api => "api",
        }
    }
}

impl AdminService {
    async fn lock_group_credential_subject(
        &self,
        context: &AdminContext,
        target: &GroupCredentialTarget,
    ) -> Result<Transaction<'static, Postgres>, AdminError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query_file!("queries/credential_timeouts.sql")
            .fetch_one(&mut *transaction)
            .await?;
        let found = sqlx::query_file_scalar!(
            "queries/admin/group_credential_subject.sql",
            context.tenant_id,
            target.group,
            target.provider,
            target.kind.name()
        )
        .fetch_optional(&mut *transaction)
        .await?;
        if found.is_none() {
            return Err(AdminError::NotFound);
        }
        Ok(transaction)
    }

    async fn audit_group_credential(
        transaction: &mut Transaction<'_, Postgres>,
        context: &AdminContext,
        target: &GroupCredentialTarget,
        action: &'static str,
        addresses: (IpAddr, IpAddr),
        request: &str,
    ) -> Result<(), AdminError> {
        append_audit(
            transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action,
                target_type: "group",
                target_id: Some(target.group),
                before: None,
                after: Some(json!({"provider": target.provider, "kind": target.kind.name()})),
                source_ip: addresses.0,
                peer_ip: addresses.1,
                request_id: request,
            },
        )
        .await
    }

    async fn record_group_credential_failure(
        &self,
        context: &AdminContext,
        target: &GroupCredentialTarget,
        replacing: bool,
        addresses: (IpAddr, IpAddr),
        request: &str,
    ) -> Result<(), AdminError> {
        let mut failure = self.pool.begin().await.map_err(uncertain)?;
        sqlx::query_file!("queries/credential_timeouts.sql")
            .fetch_one(&mut *failure)
            .await
            .map_err(uncertain)?;
        Self::audit_group_credential(
            &mut failure,
            context,
            target,
            if replacing {
                "admin.group_credential.replace_failure"
            } else {
                "admin.group_credential.revoke_failure"
            },
            addresses,
            request,
        )
        .await
        .map_err(uncertain)?;
        failure.commit().await.map_err(uncertain)
    }

    pub async fn change_group_credential(
        &self,
        context: &AdminContext,
        target: GroupCredentialTarget,
        store: &PersonalTokens,
        token: Option<&str>,
        addresses: (IpAddr, IpAddr),
        request: &str,
    ) -> Result<(), AdminError> {
        let api = store.api_store(&target.provider);
        let selected = match target.kind {
            GroupCredentialKind::Subscription => store,
            GroupCredentialKind::Api => &api,
        };
        let digest = token
            .map(|raw| selected.digest(raw))
            .transpose()
            .map_err(|_| AdminError::InvalidRequest)?;
        let mut attempt = self.lock_group_credential_subject(context, &target).await?;
        Self::audit_group_credential(
            &mut attempt,
            context,
            &target,
            if token.is_some() {
                "admin.group_credential.replace_attempt"
            } else {
                "admin.group_credential.revoke_attempt"
            },
            addresses,
            request,
        )
        .await?;
        attempt.commit().await?;
        let mut transaction = match self.lock_group_credential_subject(context, &target).await {
            Ok(transaction) => transaction,
            Err(error) => {
                tracing::error!(error = %error, "group provider credential change did not start");
                self.record_group_credential_failure(
                    context,
                    &target,
                    token.is_some(),
                    addresses,
                    request,
                )
                .await?;
                return Err(error);
            }
        };
        let group_store = selected.group_store(&target.provider);
        let result = async {
            if let (Some(raw), Some(digest)) = (token, digest) {
                group_store.replace(context.tenant_id, target.group, raw).await.map_err(|error| {
                    tracing::error!(error = %error, "group provider credential replacement failed");
                    AdminError::OperationUncertain
                })?;
                sqlx::query_file!(
                    "queries/admin/group_credential_set.sql",
                    context.tenant_id,
                    target.group,
                    target.provider,
                    target.kind.name(),
                    digest
                )
                .execute(&mut *transaction)
                .await?;
            } else {
                group_store.revoke(context.tenant_id, target.group).await.map_err(|error| {
                    tracing::error!(error = %error, "group provider credential revocation failed");
                    AdminError::OperationUncertain
                })?;
                sqlx::query_file!(
                    "queries/admin/group_credential_revoke.sql",
                    context.tenant_id,
                    target.group,
                    target.provider,
                    target.kind.name()
                )
                .execute(&mut *transaction)
                .await?;
            }
            Self::audit_group_credential(
                &mut transaction,
                context,
                &target,
                if token.is_some() {
                    "admin.group_credential.replace_success"
                } else {
                    "admin.group_credential.revoke_success"
                },
                addresses,
                request,
            )
            .await?;
            Ok::<_, AdminError>(())
        }
        .await;
        if let Err(error) = result {
            tracing::error!(error = %error, "group provider credential change was not committed");
            transaction.rollback().await.map_err(uncertain)?;
            self.record_group_credential_failure(
                context,
                &target,
                token.is_some(),
                addresses,
                request,
            )
            .await?;
            return Err(AdminError::OperationUncertain);
        }
        transaction.commit().await.map_err(|error| {
            tracing::error!(error = %error, "group provider credential commit was not confirmed");
            AdminError::OperationUncertain
        })
    }

    pub async fn group_credential_status(
        &self,
        context: &AdminContext,
        target: GroupCredentialTarget,
        store: &PersonalTokens,
    ) -> Result<Value, AdminError> {
        let mut transaction = self.lock_group_credential_subject(context, &target).await?;
        let binding = sqlx::query_file!(
            "queries/admin/group_credential_status.sql",
            context.tenant_id,
            target.group,
            target.provider,
            target.kind.name()
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(binding) = binding else {
            return Ok(json!({"configured": false}));
        };
        let api = store.api_store(&target.provider);
        let selected = match target.kind {
            GroupCredentialKind::Subscription => store,
            GroupCredentialKind::Api => &api,
        };
        let (token, _lock) = selected
            .group_store(&target.provider)
            .read_locked(context.tenant_id, target.group)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "group provider credential status failed");
                AdminError::Unavailable
            })?;
        let consistent = token
            .as_ref()
            .map(|token| selected.digest(token))
            .transpose()
            .map_err(|_| AdminError::Unavailable)?
            .is_some_and(|digest| digest == binding.digest);
        Ok(
            json!({"configured": binding.enabled && consistent, "enabled": binding.enabled, "consistent": consistent}),
        )
    }
}

fn uncertain(error: impl std::fmt::Display) -> AdminError {
    tracing::error!(error = %error, "group provider credential result was not confirmed");
    AdminError::OperationUncertain
}
