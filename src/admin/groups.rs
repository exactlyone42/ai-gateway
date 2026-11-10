use std::net::IpAddr;

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    AdminContext, AdminError, AdminService, AuditEntry, ListQuery, ResourcePage, append_audit,
    map_conflict, validate_name,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupCreate {
    pub display_name: String,
}

impl AdminService {
    pub async fn list_groups(
        &self,
        context: &AdminContext,
        query: ListQuery,
    ) -> Result<ResourcePage, AdminError> {
        let limit = query.limit.unwrap_or(50);
        if limit == 0 || limit > 100 || query.email.is_some() {
            return Err(AdminError::InvalidRequest);
        }
        let mut rows: Vec<Value> = sqlx::query_file_scalar!(
            "queries/admin/groups_list.sql",
            context.tenant_id,
            query.after.unwrap_or(Uuid::nil()),
            i64::from(limit) + 1
        )
        .fetch_all(&self.pool)
        .await?;
        let more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        let next_after = if more {
            rows.last()
                .and_then(|row| row["id"].as_str())
                .and_then(|id| Uuid::parse_str(id).ok())
        } else {
            None
        };
        Ok(ResourcePage {
            items: rows,
            next_after,
        })
    }

    pub async fn list_group_members(
        &self,
        context: &AdminContext,
        group: Uuid,
        query: ListQuery,
    ) -> Result<ResourcePage, AdminError> {
        let limit = query.limit.unwrap_or(50);
        if limit == 0 || limit > 100 || query.email.is_some() {
            return Err(AdminError::InvalidRequest);
        }
        let mut rows: Vec<Value> = sqlx::query_file_scalar!(
            "queries/admin/group_members_list.sql",
            context.tenant_id,
            group,
            query.after.unwrap_or(Uuid::nil()),
            i64::from(limit) + 1
        )
        .fetch_all(&self.pool)
        .await?;
        let more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        let next_after = if more {
            rows.last()
                .and_then(|row| row["user_id"].as_str())
                .and_then(|id| Uuid::parse_str(id).ok())
        } else {
            None
        };
        Ok(ResourcePage {
            items: rows,
            next_after,
        })
    }

    pub async fn create_group(
        &self,
        context: &AdminContext,
        input: GroupCreate,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<Value, AdminError> {
        validate_name(&input.display_name)?;
        let mut transaction = self.pool.begin().await?;
        let id = Uuid::new_v4();
        let group: Value = sqlx::query_file_scalar!(
            "queries/admin/group_create.sql",
            id,
            context.tenant_id,
            input.display_name
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_conflict)?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.group.create",
                target_type: "group",
                target_id: Some(id),
                before: None,
                after: Some(group.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(group)
    }

    pub async fn change_group_member(
        &self,
        context: &AdminContext,
        (group, user): (Uuid, Uuid),
        add: bool,
        store: Option<&crate::held_token::PersonalTokens>,
        (source_ip, peer_ip): (IpAddr, IpAddr),
        request_id: &str,
    ) -> Result<(), AdminError> {
        let _member_lock = if !add {
            match store {
                Some(store) => Some(store.lock_membership(context.tenant_id, user, false).await.map_err(|error| {
                    tracing::error!(error = %error, "group member removal is blocked by an active credential request");
                    AdminError::Unavailable
                })?),
                None => None,
            }
        } else {
            None
        };
        let mut transaction = self.pool.begin().await?;
        sqlx::query_file!("queries/credential_timeouts.sql")
            .fetch_one(&mut *transaction)
            .await?;
        if !add && store.is_none() {
            let group_exists = sqlx::query_file_scalar!(
                "queries/admin/group_member_subject_lock.sql",
                context.tenant_id,
                group
            )
            .fetch_optional(&mut *transaction)
            .await?;
            if group_exists.is_none() {
                return Err(AdminError::NotFound);
            }
            let bound = sqlx::query_file_scalar!(
                "queries/admin/group_member_credential_guard.sql",
                context.tenant_id,
                group
            )
            .fetch_optional(&mut *transaction)
            .await?;
            if bound == Some(true) {
                return Err(AdminError::Unavailable);
            }
        }
        let changed: Option<Uuid> = if add {
            sqlx::query_file_scalar!(
                "queries/admin/group_member_add.sql",
                context.tenant_id,
                group,
                user
            )
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_conflict)?
        } else {
            sqlx::query_file_scalar!(
                "queries/admin/group_member_remove.sql",
                context.tenant_id,
                group,
                user
            )
            .fetch_optional(&mut *transaction)
            .await?
        };
        if changed.is_none() {
            return Err(AdminError::NotFound);
        }
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: if add {
                    "admin.group.member.add"
                } else {
                    "admin.group.member.remove"
                },
                target_type: "group",
                target_id: Some(group),
                before: None,
                after: Some(json!({"user_id":user})),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        if let Some(policies) = &self.policies {
            policies.clear_tenant_cache(context.tenant_id);
        }
        Ok(())
    }
}
