//! Subaccount group resolution for WebSocket channel participation.
//!
//! A player alias's *group* is the server-resolved `parent_account_id` from the
//! `players` table: a subaccount participates as its (immediate) parent, a root
//! account as itself. Channel membership is tracked per group (see
//! [`crate::sockets::channels`]), so a subaccount and its parent share a
//! channel and receive each other's chat messages while still appearing as
//! distinct players in presence broadcasts.

use sqlx::MySqlPool;

/// Minimal row projection for the alias → parent lookup.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PlayerParentRow {
    /// `players.parent_account_id`; `None` for a root account.
    pub parent_account_id: Option<u64>,
}

/// Resolve the `parent_account_id` of a live (non-deleted) player alias.
///
/// Returns `Ok(None)` when the alias does not exist (or is deleted);
/// `Ok(Some(None))` when the alias is a root account with no parent; and
/// `Ok(Some(Some(parent)))` when the alias is a subaccount of `parent`. The
/// column is BIGINT UNSIGNED, so the parent id is widened to `i64` — the id
/// space socket tickets already use — before it is surfaced.
pub async fn resolve_parent_account_id(
    pool: &MySqlPool,
    alias_id: i64,
) -> sqlx::Result<Option<Option<i64>>> {
    let row = sqlx::query_as::<_, PlayerParentRow>(
        "SELECT parent_account_id FROM players WHERE id = ? AND status <> 'deleted'",
    )
    .bind(alias_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.parent_account_id.map(|p| p as i64)))
}

/// Compute the channel-participation group key for an alias.
///
/// A subaccount participates in channels under its (immediate) parent's id; a
/// root account — or any alias without a resolvable parent — participates as
/// itself. The `parent_account_id` argument is the value produced by
/// [`resolve_parent_account_id`].
pub fn group_key(alias_id: i64, parent_account_id: Option<i64>) -> i64 {
    parent_account_id.unwrap_or(alias_id)
}

/// Resolve the channel-participation group key for an alias from the database.
///
/// Returns `Ok(Some(group))` when the alias exists, `Ok(None)` when it does
/// not (or is deleted). A missing alias is never a connect failure — callers
/// degrade to per-alias identity.
pub async fn resolve_group(pool: &MySqlPool, alias_id: i64) -> sqlx::Result<Option<i64>> {
    Ok(resolve_parent_account_id(pool, alias_id)
        .await?
        .map(|parent| group_key(alias_id, parent)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Row projection for reading back freshly inserted player ids (BIGINT
    /// UNSIGNED — decode as `u64`, matching the `entities::Player` projection).
    #[derive(sqlx::FromRow)]
    struct PlayerIdRow {
        id: u64,
    }

    #[test]
    fn group_key_root_groups_as_self() {
        assert_eq!(group_key(42, None), 42);
    }

    #[test]
    fn group_key_subaccount_groups_as_parent() {
        assert_eq!(group_key(42, Some(7)), 7);
    }

    #[test]
    fn group_key_missing_parent_maps_to_self() {
        // The degraded-mode pattern: a parent that never resolved
        // (None) keeps the alias as its own group.
        let parent: Option<i64> = None;
        assert_eq!(group_key(42, parent), 42);
    }

    /// DB-gated smoke test mirroring the `db.rs` R-7 pattern: runs against a
    /// live MySQL (the Docker dev stack) when `MYSQL_URL` is set.
    #[tokio::test]
    #[ignore = "requires a live MySQL; set MYSQL_URL to enable"]
    async fn resolve_group_against_live_db() {
        let url = std::env::var("MYSQL_URL").expect("MYSQL_URL must be set for this test");
        let pool = crate::db::build_pool(&url).await.expect("pool builds");

        // Insert a root account, then a subaccount under it. Unique usernames
        // and public ids avoid collisions across runs on the shared dev DB.
        let root_public_id = format!("root-{}-group", Uuid::new_v4());
        let root_username = format!("root-{}-group", Uuid::new_v4());
        sqlx::query(
            "INSERT INTO players (public_id, username, password_hash, status) \
             VALUES (?, ?, ?, 'active')",
        )
        .bind(&root_public_id)
        .bind(&root_username)
        .bind("unit-test-group-hash")
        .execute(&pool)
        .await
        .expect("insert root player");

        let root: PlayerIdRow = sqlx::query_as("SELECT id FROM players WHERE public_id = ?")
            .bind(&root_public_id)
            .fetch_one(&pool)
            .await
            .expect("read root player id");
        let root_id = root.id as i64;

        let sub_public_id = format!("sub-{}-group", Uuid::new_v4());
        let sub_username = format!("sub-{}-group", Uuid::new_v4());
        sqlx::query(
            "INSERT INTO players (public_id, username, password_hash, parent_account_id, status) \
             VALUES (?, ?, ?, ?, 'active')",
        )
        .bind(&sub_public_id)
        .bind(&sub_username)
        .bind("unit-test-group-hash")
        .bind(root.id)
        .execute(&pool)
        .await
        .expect("insert subaccount player");

        let sub: PlayerIdRow = sqlx::query_as("SELECT id FROM players WHERE public_id = ?")
            .bind(&sub_public_id)
            .fetch_one(&pool)
            .await
            .expect("read subaccount player id");
        let sub_id = sub.id as i64;

        assert_eq!(
            resolve_parent_account_id(&pool, root_id)
                .await
                .expect("root resolves"),
            Some(None),
            "root accounts have no parent"
        );
        assert_eq!(
            resolve_group(&pool, root_id).await.expect("root resolves"),
            Some(root_id),
            "root accounts group as themselves"
        );
        assert_eq!(
            resolve_parent_account_id(&pool, sub_id)
                .await
                .expect("subaccount resolves"),
            Some(Some(root_id)),
            "subaccounts resolve their immediate parent"
        );
        assert_eq!(
            resolve_group(&pool, sub_id)
                .await
                .expect("subaccount resolves"),
            Some(root_id),
            "subaccounts group as their immediate parent"
        );
        assert_eq!(
            resolve_group(&pool, 999_999_999)
                .await
                .expect("unknown id resolves"),
            None,
            "unknown aliases resolve to no group (caller degrades per-alias)"
        );
    }
}
