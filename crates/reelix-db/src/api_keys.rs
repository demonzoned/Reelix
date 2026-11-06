use chrono::{DateTime, Utc};
use reelix_core::User;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, Result, from_ms, now_ms};

/// Stored API key metadata (the key itself is only kept as a hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

fn map_key(row: &SqliteRow) -> Result<ApiKey> {
    let last: Option<i64> = row.try_get("last_used_at")?;
    Ok(ApiKey {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        created_at: from_ms(row.try_get("created_at")?),
        last_used_at: last.map(from_ms),
    })
}

/// API keys. `key_hash` is the hex sha256 of the raw key.
/// How fresh `last_used_at` is kept: at most one write per key in this time.
pub const LAST_USED_GRANULARITY_MS: i64 = 5 * 60 * 1000;

pub struct ApiKeyRepo<'a> {
    pub(crate) db: &'a Db,
}

impl ApiKeyRepo<'_> {
    pub async fn create(&self, user_id: i64, name: &str, prefix: &str, key_hash: &str) -> Result<ApiKey> {
        let row = sqlx::query(
            "INSERT INTO api_keys (user_id, name, prefix, key_hash, created_at) VALUES (?, ?, ?, ?, ?)
             RETURNING id, user_id, name, prefix, created_at, last_used_at",
        )
        .bind(user_id)
        .bind(name)
        .bind(prefix)
        .bind(key_hash)
        .bind(now_ms())
        .fetch_one(self.db.pool())
        .await?;
        map_key(&row)
    }

    pub async fn list_for_user(&self, user_id: i64) -> Result<Vec<ApiKey>> {
        let rows = sqlx::query(
            "SELECT id, user_id, name, prefix, created_at, last_used_at FROM api_keys WHERE user_id = ? ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(map_key).collect()
    }

    /// Deletes one of the user's keys; `false` if it didn't exist or belongs to someone else.
    pub async fn delete(&self, user_id: i64, id: i64) -> Result<bool> {
        let r = sqlx::query("DELETE FROM api_keys WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// The key's owner; also stamps `last_used_at = now` when the last stamp is older than
    /// [`LAST_USED_GRANULARITY_MS`], so a key polled every few seconds (a dashboard, Torznab, an *arr
    /// client) is not a database write on every request.
    pub async fn find_user(&self, key_hash: &str) -> Result<Option<User>> {
        let row: Option<(i64, Option<i64>)> =
            sqlx::query_as("SELECT user_id, last_used_at FROM api_keys WHERE key_hash = ?")
                .bind(key_hash)
                .fetch_optional(self.db.pool())
                .await?;
        let now = now_ms();
        let user_id = row.map(|(id, last)| (id, last.is_none_or(|t| now - t >= LAST_USED_GRANULARITY_MS)));
        if let Some((_, true)) = user_id {
            sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE key_hash = ?")
                .bind(now)
                .bind(key_hash)
                .execute(self.db.pool())
                .await?;
        }
        match user_id.map(|(id, _)| id) {
            Some(id) => self.db.users().get(id).await,
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Db;
    use crate::users::tests::new_user;

    #[tokio::test]
    async fn api_key_lifecycle() {
        let db = Db::in_memory().await.unwrap();
        let alice = db.users().create(new_user("alice")).await.unwrap();
        let bob = db.users().create(new_user("bob")).await.unwrap();

        let key = db
            .api_keys()
            .create(alice.id, "ci", "abcd1234", "h1")
            .await
            .unwrap();
        assert_eq!(key.user_id, alice.id);
        assert_eq!(key.prefix, "abcd1234");
        assert!(key.last_used_at.is_none());

        let found = db.api_keys().find_user("h1").await.unwrap().unwrap();
        assert_eq!(found.id, alice.id);
        let listed = db.api_keys().list_for_user(alice.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].last_used_at.is_some());
        // A second use soon after does not write again; an old stamp is refreshed.
        let first = listed[0].last_used_at;
        db.api_keys().find_user("h1").await.unwrap().unwrap();
        assert_eq!(
            db.api_keys().list_for_user(alice.id).await.unwrap()[0].last_used_at,
            first
        );
        sqlx::query("UPDATE api_keys SET last_used_at = 1 WHERE key_hash = 'h1'")
            .execute(db.pool())
            .await
            .unwrap();
        db.api_keys().find_user("h1").await.unwrap().unwrap();
        let refreshed = db.api_keys().list_for_user(alice.id).await.unwrap()[0]
            .last_used_at
            .unwrap();
        assert!(refreshed.timestamp_millis() > 1);
        assert!(db.api_keys().list_for_user(bob.id).await.unwrap().is_empty());
        assert!(db.api_keys().find_user("nope").await.unwrap().is_none());

        assert!(!db.api_keys().delete(bob.id, key.id).await.unwrap());
        assert!(db.api_keys().delete(alice.id, key.id).await.unwrap());
        assert!(!db.api_keys().delete(alice.id, key.id).await.unwrap());
        assert!(db.api_keys().find_user("h1").await.unwrap().is_none());
    }
}
