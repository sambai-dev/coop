use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub type StoreResult<T> = Result<T, sqlx::Error>;

#[derive(Debug, Clone)]
pub struct JobRow {
    pub job_id: String,
    pub tenant: String,
    pub language: String,
    pub status: String,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub spec_json: String,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub seq: i64,
    pub ts_ms: i64,
    pub kind: String,
    pub data: Value,
}

pub struct Store {
    pool: SqlitePool,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl Store {
    pub async fn open(path: &Path) -> StoreResult<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> StoreResult<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS jobs (
                job_id TEXT PRIMARY KEY,
                tenant TEXT NOT NULL,
                language TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'queued',
                spec_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                exit_code INTEGER
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                ts_ms INTEGER NOT NULL,
                kind TEXT NOT NULL,
                data_json TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_job ON events(job_id, seq)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_job(
        &self,
        job_id: &str,
        tenant: &str,
        language: &str,
        spec_json: &str,
    ) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO jobs (job_id, tenant, language, status, spec_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5)",
        )
        .bind(job_id)
        .bind(tenant)
        .bind(language)
        .bind(spec_json)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_started(&self, job_id: &str) -> StoreResult<()> {
        sqlx::query("UPDATE jobs SET status = 'running', started_at_ms = ?2 WHERE job_id = ?1")
            .bind(job_id)
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish(
        &self,
        job_id: &str,
        status: &str,
        exit_code: Option<i32>,
    ) -> StoreResult<()> {
        sqlx::query(
            "UPDATE jobs SET status = ?2, exit_code = ?3, finished_at_ms = ?4 WHERE job_id = ?1",
        )
        .bind(job_id)
        .bind(status)
        .bind(exit_code)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn append_event(
        &self,
        job_id: &str,
        kind: &str,
        data: &Value,
    ) -> StoreResult<(i64, i64)> {
        let row = sqlx::query(
            "INSERT INTO events (job_id, ts_ms, kind, data_json) VALUES (?1, ?2, ?3, ?4) RETURNING seq, ts_ms",
        )
        .bind(job_id)
        .bind(now_ms())
        .bind(kind)
        .bind(data.to_string())
        .fetch_one(&self.pool)
        .await?;
        let seq: i64 = row.get("seq");
        let ts_ms: i64 = row.get("ts_ms");
        Ok((seq, ts_ms))
    }

    pub async fn last_seq(&self, job_id: &str) -> StoreResult<i64> {
        let row =
            sqlx::query("SELECT COALESCE(MAX(seq), 0) AS max_seq FROM events WHERE job_id = ?1")
                .bind(job_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.get("max_seq"))
    }

    pub async fn events_for(&self, job_id: &str) -> StoreResult<Vec<EventRow>> {
        let rows = sqlx::query(
            "SELECT seq, ts_ms, kind, data_json FROM events WHERE job_id = ?1 ORDER BY seq ASC",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| EventRow {
                seq: r.get("seq"),
                ts_ms: r.get("ts_ms"),
                kind: r.get("kind"),
                data: serde_json::from_str(&r.get::<String, _>("data_json")).unwrap_or(Value::Null),
            })
            .collect())
    }

    pub async fn get_job(&self, job_id: &str) -> StoreResult<Option<JobRow>> {
        let row = sqlx::query(
            "SELECT job_id, tenant, language, status, spec_json, created_at_ms, started_at_ms, finished_at_ms, exit_code
             FROM jobs WHERE job_id = ?1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Self::row_to_job))
    }

    pub async fn list_jobs(&self, tenant: Option<&str>, limit: i64) -> StoreResult<Vec<JobRow>> {
        let rows = sqlx::query(
            "SELECT job_id, tenant, language, status, spec_json, created_at_ms, started_at_ms, finished_at_ms, exit_code
             FROM jobs WHERE (?1 = '' OR tenant = ?1) ORDER BY created_at_ms DESC LIMIT ?2",
        )
        .bind(tenant.unwrap_or(""))
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Self::row_to_job).collect())
    }

    /// Count jobs grouped by status (all tenants). Used by the metrics
    /// endpoint; cheap on the small SQLite event store.
    pub async fn count_by_status(&self) -> StoreResult<Vec<(String, i64)>> {
        let rows =
            sqlx::query("SELECT status, COUNT(*) AS n FROM jobs GROUP BY status ORDER BY status")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>("status"), r.get::<i64, _>("n")))
            .collect())
    }

    /// Retention sweep: delete terminal jobs older than `max_age_ms` and
    /// their events, then reclaim space. Returns (jobs_deleted, events_deleted).
    pub async fn prune_older_than(&self, max_age_ms: i64) -> StoreResult<(u64, u64)> {
        let cutoff = now_ms() - max_age_ms;
        let mut tx = self.pool.begin().await?;
        // Event rows first (child rows by creation time; events inherit the
        // job's age via its row, so select the ids being deleted).
        let expired: Vec<String> = sqlx::query(
            "SELECT job_id FROM jobs WHERE status IN ('succeeded','failed','timed_out','oom_killed','cancelled','error')
             AND created_at_ms < ?1",
        )
        .bind(cutoff)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| r.get("job_id"))
        .collect();
        if expired.is_empty() {
            tx.rollback().await?;
            return Ok((0, 0));
        }
        let placeholders = expired.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let event_sql = format!("DELETE FROM events WHERE job_id IN ({placeholders})");
        let mut q = sqlx::query(&event_sql);
        for id in &expired {
            q = q.bind(id);
        }
        let events_deleted = q.execute(&mut *tx).await?.rows_affected();
        let job_sql = format!("DELETE FROM jobs WHERE job_id IN ({placeholders})");
        let mut q = sqlx::query(&job_sql);
        for id in &expired {
            q = q.bind(id);
        }
        let jobs_deleted = q.execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok((jobs_deleted, events_deleted))
    }

    /// Boot recovery: any job stuck in a non-terminal state from a previous
    /// process lifetime is marked `error`. A crashed server cannot finish or
    /// cancel them anymore, and leaving them running would poison tenant
    /// concurrency accounting on restart.
    pub async fn recover_stale_running(&self) -> StoreResult<u64> {
        let now = now_ms();
        let result = sqlx::query(
            "UPDATE jobs SET status = 'error', exit_code = NULL, finished_at_ms = ?2
             WHERE status IN ('queued','running')",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    fn row_to_job(r: sqlx::sqlite::SqliteRow) -> JobRow {
        JobRow {
            job_id: r.get("job_id"),
            tenant: r.get("tenant"),
            language: r.get("language"),
            status: r.get("status"),
            created_at_ms: r.get("created_at_ms"),
            started_at_ms: r.get("started_at_ms"),
            finished_at_ms: r.get("finished_at_ms"),
            exit_code: r.get("exit_code"),
            spec_json: r.get("spec_json"),
        }
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish()
    }
}
