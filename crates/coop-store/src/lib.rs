use futures_util::TryStreamExt;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type StoreResult<T> = Result<T, sqlx::Error>;

const CURRENT_SCHEMA_VERSION: i64 = 2;
const ROW_VALIDATION_REVISION: i64 = 1;
// Transaction-local durable sentinel used to distinguish Coop-owned writes
// from offline/raw SQL changes in the validation-dirty triggers. SQLite's
// immediate writer lock prevents another connection from observing or using
// the sentinel until the transaction commits (which Coop never does).
const OWNED_ROW_WRITE_REVISION: i64 = ROW_VALIDATION_REVISION + 1;
const STORAGE_GUARD_REVISION_MARKER: &str = "coop-storage-guard-r1";
const STORAGE_GUARD_NAMES: [&str; 13] = [
    "coop_schema_migrations_storage_guard_insert",
    "coop_schema_migrations_storage_guard_update",
    "coop_jobs_storage_guard_insert",
    "coop_jobs_storage_guard_update",
    "coop_events_storage_guard_insert",
    "coop_events_storage_guard_update",
    "coop_events_sequence_guard_insert",
    "coop_jobs_validation_dirty_insert",
    "coop_jobs_validation_dirty_update",
    "coop_jobs_validation_dirty_delete",
    "coop_events_validation_dirty_insert",
    "coop_events_validation_dirty_update",
    "coop_events_validation_dirty_delete",
];
const DEFAULT_RETENTION_BATCH: i64 = 32;
const MAX_RETENTION_BATCH: i64 = 64;
/// Hard event-row budget for one retention transaction. Oversized legacy jobs
/// are drained newest-first over multiple sweeps before their row is deleted.
pub const MAX_RETENTION_EVENTS_PER_BATCH: u64 = 4_096;
const MAX_EVENT_PAGE: i64 = 5_000;
const MAX_JOB_PAGE: i64 = 500;
const MAX_JOB_LOOKAHEAD_PAGE: i64 = MAX_JOB_PAGE + 1;
const MAX_RECOVERY_PAGE: i64 = 1_024;
const MAX_RUNNING_RECOVERY_ID_PAGE: i64 = 32;
/// Upper bound for one atomic event append. This keeps a single writer from
/// holding SQLite's write lock indefinitely while still amortizing the FULL
/// synchronous commit cost over output bursts.
pub const MAX_EVENT_BATCH_SIZE: usize = 256;
const TERMINAL_STATUSES_SQL: &str =
    "'succeeded','failed','timed_out','oom_killed','cancelled','error'";
const JOB_ROW_PROJECTION: &str =
    "job_id, tenant, language, status, spec_json, effective_spec_json, receipt_json, \
     created_at_ms, started_at_ms, finished_at_ms, exit_code";
const JOB_SUMMARY_PROJECTION: &str =
    "job_id, tenant, language, status, created_at_ms, started_at_ms, finished_at_ms, exit_code";

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
    pub effective_spec_json: Option<String>,
    pub receipt_json: Option<String>,
}

/// Lightweight projection for job-list surfaces. Deliberately excludes the
/// potentially multi-megabyte requested/effective specs and receipt payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
    pub job_id: String,
    pub tenant: String,
    pub language: String,
    pub status: String,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub seq: i64,
    pub ts_ms: i64,
    pub kind: String,
    pub data: Value,
    /// SHA-256 of the preceding verified event in this job's chain. Empty for
    /// the first verified event and for migrated legacy events.
    pub prev_hash: String,
    /// SHA-256 of this event's versioned canonical payload. Empty only for
    /// migrated legacy events.
    pub event_hash: String,
    /// Zero denotes a legacy/unverified event; one is Coop's canonical v1
    /// event-chain format.
    pub hash_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCursor {
    pub created_at_ms: i64,
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedJobRow {
    pub job_id: String,
    pub tenant: String,
    pub created_at_ms: i64,
}

impl From<&QueuedJobRow> for JobCursor {
    fn from(row: &QueuedJobRow) -> Self {
        Self {
            created_at_ms: row.created_at_ms,
            job_id: row.job_id.clone(),
        }
    }
}

impl From<&JobSummary> for JobCursor {
    fn from(row: &JobSummary) -> Self {
        Self {
            created_at_ms: row.created_at_ms,
            job_id: row.job_id.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListJobsQuery {
    /// `None` is an explicitly privileged all-tenant query. `Some("")` is
    /// never treated as a wildcard.
    pub tenant: Option<String>,
    pub status: Option<String>,
    pub language: Option<String>,
    pub before: Option<JobCursor>,
    pub limit: i64,
}

impl Default for ListJobsQuery {
    fn default() -> Self {
        Self {
            tenant: None,
            status: None,
            language: None,
            before: None,
            limit: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChainHead {
    pub event_count: i64,
    pub verified_event_count: i64,
    pub legacy_event_count: i64,
    pub head_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChainVerification {
    pub head: EventChainHead,
    pub valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionReport {
    pub jobs_deleted: u64,
    pub events_deleted: u64,
    pub more_remaining: bool,
}

pub struct Store {
    pool: SqlitePool,
    db_path: PathBuf,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn build_job_list_query<'args>(
    prefix: &'static str,
    projection: &'static str,
    query: &'args ListJobsQuery,
) -> QueryBuilder<'args, Sqlite> {
    let mut statement = QueryBuilder::<Sqlite>::new(prefix);
    statement.push("SELECT ").push(projection).push(
        " FROM jobs
              WHERE NOT EXISTS (
                  SELECT 1 FROM retention_tombstones
                  WHERE retention_tombstones.job_id = jobs.job_id
              )",
    );
    if let Some(tenant) = query.tenant.as_deref() {
        statement.push(" AND jobs.tenant = ").push_bind(tenant);
    }
    if let Some(status) = query.status.as_deref() {
        statement.push(" AND jobs.status = ").push_bind(status);
    }
    if let Some(language) = query.language.as_deref() {
        statement.push(" AND jobs.language = ").push_bind(language);
    }
    if let Some(before) = query.before.as_ref() {
        statement
            .push(" AND (jobs.created_at_ms, jobs.job_id) < (")
            .push_bind(before.created_at_ms)
            .push(", ")
            .push_bind(before.job_id.as_str())
            .push(")");
    }
    statement
        .push(" ORDER BY jobs.created_at_ms DESC, jobs.job_id DESC LIMIT ")
        .push_bind(query.limit.clamp(1, MAX_JOB_LOOKAHEAD_PAGE));
    statement
}

fn build_queued_jobs_query<'args>(
    prefix: &'static str,
    after: Option<&'args JobCursor>,
    limit: i64,
) -> QueryBuilder<'args, Sqlite> {
    let mut statement = QueryBuilder::<Sqlite>::new(prefix);
    statement.push(
        "SELECT job_id, tenant, created_at_ms
         FROM jobs
         WHERE status = 'queued'
           AND NOT EXISTS (
                SELECT 1 FROM retention_tombstones
                WHERE retention_tombstones.job_id = jobs.job_id
           )",
    );
    if let Some(after) = after {
        statement
            .push(" AND (created_at_ms, job_id) > (")
            .push_bind(after.created_at_ms)
            .push(", ")
            .push_bind(after.job_id.as_str())
            .push(")");
    }
    statement
        .push(" ORDER BY created_at_ms ASC, job_id ASC LIMIT ")
        .push_bind(limit.clamp(1, MAX_RECOVERY_PAGE));
    statement
}

impl Store {
    pub async fn open(path: &Path) -> StoreResult<Self> {
        prepare_storage_path(path)?;

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // UTF-8 physical validation streams potentially large JSON rows.
            // Apply backpressure after one pending row so a valid database
            // cannot queue dozens of maximum-sized specs in process memory.
            .row_buffer_size(1)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .pragma("secure_delete", "FAST")
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        let store = Self {
            pool,
            db_path: path.to_path_buf(),
        };

        if let Err(error) = store.migrate().await {
            store.pool.close().await;
            return Err(error);
        }

        // SQLite derives sidecar permissions from the database mode. Enforce
        // them explicitly too, both as defense in depth and for pre-existing
        // WAL/SHM files created by an older Coop version.
        harden_storage_files(path)?;
        Ok(store)
    }

    async fn migrate(&self) -> StoreResult<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::migrate_locked(&mut tx).await?;
        tx.commit().await
    }

    async fn migrate_locked(conn: &mut SqliteConnection) -> StoreResult<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY CHECK (typeof(version) = 'integer' AND version > 0),
                applied_at_ms INTEGER NOT NULL
                    CHECK (typeof(applied_at_ms) = 'integer' AND applied_at_ms >= 0)
            )",
        )
        .execute(&mut *conn)
        .await?;

        validate_required_columns(
            conn,
            "schema_migrations",
            &[
                RequiredColumn::primary_key("version", "INTEGER"),
                RequiredColumn::not_null("applied_at_ms", "INTEGER"),
            ],
        )
        .await?;
        validate_migration_history_rows(conn).await?;

        let row = sqlx::query("SELECT COALESCE(MAX(version), 0) AS version FROM schema_migrations")
            .fetch_one(&mut *conn)
            .await?;
        let history_version: i64 = row.get("version");
        let user_version: i64 = sqlx::query("PRAGMA user_version")
            .fetch_one(&mut *conn)
            .await?
            .get("user_version");
        if user_version < 0 {
            return Err(sqlx::Error::Protocol(format!(
                "database has an invalid negative user_version ({user_version})"
            )));
        }
        if history_version > CURRENT_SCHEMA_VERSION || user_version > CURRENT_SCHEMA_VERSION {
            return Err(sqlx::Error::Protocol(format!(
                "database schema is newer than supported version {CURRENT_SCHEMA_VERSION} (history={history_version}, user_version={user_version})"
            )));
        }
        if history_version != 0 && user_version != 0 && history_version != user_version {
            return Err(sqlx::Error::Protocol(format!(
                "database schema version markers disagree (history={history_version}, user_version={user_version})"
            )));
        }
        let version = history_version.max(user_version);
        let schema_markers_current =
            history_version == CURRENT_SCHEMA_VERSION && user_version == CURRENT_SCHEMA_VERSION;

        let has_jobs = table_exists(conn, "jobs").await?;
        match (version, has_jobs) {
            (0, false) => {
                Self::create_current_schema(conn).await?;
                record_migration(conn, 1).await?;
                record_migration(conn, CURRENT_SCHEMA_VERSION).await?;
            }
            (0, true) | (1, true) => {
                if schema_has_v2_extensions(conn).await? {
                    // Lost or stale markers must not project a current table
                    // through the v1 migration, which would discard receipt
                    // and event-hash evidence. Validate and reconcile it in
                    // place; a partial v2 shape fails closed below.
                    Self::validate_current_schema(conn).await?;
                } else {
                    Self::migrate_legacy_schema(conn).await?;
                    if history_version == 0 {
                        record_migration(conn, 1).await?;
                    }
                    record_migration(conn, CURRENT_SCHEMA_VERSION).await?;
                }
            }
            (1, false) => {
                return Err(sqlx::Error::Protocol(
                    "schema migration history exists but the jobs table is missing".to_string(),
                ));
            }
            (CURRENT_SCHEMA_VERSION, true) => {}
            (CURRENT_SCHEMA_VERSION, false) => {
                return Err(sqlx::Error::Protocol(
                    "current schema migration is recorded but the jobs table is missing"
                        .to_string(),
                ));
            }
            _ => {
                return Err(sqlx::Error::Protocol(format!(
                    "unsupported schema migration state: version={version}, jobs_table={has_jobs}"
                )));
            }
        }

        if !table_exists(conn, "events").await? {
            return Err(sqlx::Error::Protocol(
                "events table is missing after schema migration".to_string(),
            ));
        }

        // A marker alone is not evidence that the physical schema is usable.
        // Validate the columns and cascade that all current read/write paths
        // rely on before reconciling either version marker.
        Self::validate_current_schema(conn).await?;
        // sqlite_sequence is writable outside Coop and cannot be guarded by a
        // table trigger. This indexed high-watermark check is cheap enough for
        // every open and prevents cursor reuse/exhaustion on the fast path.
        validated_event_sequence_counter(conn, "events").await?;
        Self::ensure_integrity_state(conn).await?;
        let requires_full_validation = !schema_markers_current
            || !Self::row_validation_current(conn).await?
            || !storage_guards_current(conn).await?;
        if requires_full_validation {
            Self::validate_current_rows(conn).await?;
            validate_foreign_keys(conn).await?;
        }
        // Existing v2 tables predate the storage-class CHECKs. Idempotent
        // guards preserve compatibility while enforcing the same invariants
        // for all future inserts and updates.
        Self::create_indexes(conn).await?;
        if requires_full_validation {
            Self::record_row_validation(conn).await?;
        }
        record_migration(conn, 1).await?;
        record_migration(conn, CURRENT_SCHEMA_VERSION).await?;

        sqlx::query(&format!("PRAGMA user_version = {CURRENT_SCHEMA_VERSION}"))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    async fn create_current_schema(conn: &mut SqliteConnection) -> StoreResult<()> {
        create_jobs_table(conn, "jobs").await?;
        create_events_table(conn, "events", "jobs").await?;
        Self::create_indexes(conn).await
    }

    async fn ensure_integrity_state(conn: &mut SqliteConnection) -> StoreResult<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS store_integrity (
                 singleton INTEGER PRIMARY KEY
                     CHECK (typeof(singleton) = 'integer' AND singleton = 1),
                 row_validation_revision INTEGER NOT NULL
                     CHECK (typeof(row_validation_revision) = 'integer'
                            AND row_validation_revision >= 0),
                 validated_at_ms INTEGER NOT NULL
                     CHECK (typeof(validated_at_ms) = 'integer' AND validated_at_ms >= 0),
                 full_scan_count INTEGER NOT NULL
                     CHECK (typeof(full_scan_count) = 'integer' AND full_scan_count > 0)
             )",
        )
        .execute(&mut *conn)
        .await?;
        validate_required_columns(
            conn,
            "store_integrity",
            &[
                RequiredColumn::primary_key("singleton", "INTEGER"),
                RequiredColumn::not_null("row_validation_revision", "INTEGER"),
                RequiredColumn::not_null("validated_at_ms", "INTEGER"),
                RequiredColumn::not_null("full_scan_count", "INTEGER"),
            ],
        )
        .await?;
        let invalid: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM store_integrity
                 WHERE typeof(singleton) != 'integer' OR singleton != 1
                    OR typeof(row_validation_revision) != 'integer'
                    OR row_validation_revision < 0
                    OR typeof(validated_at_ms) != 'integer' OR validated_at_ms < 0
                    OR typeof(full_scan_count) != 'integer' OR full_scan_count <= 0
             ) AS invalid",
        )
        .fetch_one(&mut *conn)
        .await?
        .try_get("invalid")?;
        if invalid != 0 {
            return Err(sqlx::Error::Protocol(
                "store_integrity contains invalid values".to_string(),
            ));
        }
        Ok(())
    }

    async fn row_validation_current(conn: &mut SqliteConnection) -> StoreResult<bool> {
        let revision =
            sqlx::query("SELECT row_validation_revision FROM store_integrity WHERE singleton = 1")
                .fetch_optional(&mut *conn)
                .await?
                .map(|row| row.try_get::<i64, _>("row_validation_revision"))
                .transpose()?;
        if revision.is_some_and(|value| value > ROW_VALIDATION_REVISION) {
            return Err(sqlx::Error::Protocol(format!(
                "database row-validation revision is newer than supported revision {ROW_VALIDATION_REVISION}"
            )));
        }
        Ok(revision == Some(ROW_VALIDATION_REVISION))
    }

    async fn record_row_validation(conn: &mut SqliteConnection) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO store_integrity(
                 singleton, row_validation_revision, validated_at_ms, full_scan_count
             ) VALUES (1, ?1, ?2, 1)
             ON CONFLICT(singleton) DO UPDATE SET
                 row_validation_revision = excluded.row_validation_revision,
                 validated_at_ms = excluded.validated_at_ms,
                 full_scan_count = store_integrity.full_scan_count + 1",
        )
        .bind(ROW_VALIDATION_REVISION)
        .bind(now_ms())
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    async fn begin_row_writes(conn: &mut SqliteConnection) -> StoreResult<()> {
        let updated = sqlx::query(
            "UPDATE store_integrity
             SET row_validation_revision = ?2
             WHERE singleton = 1 AND row_validation_revision = ?1",
        )
        .bind(ROW_VALIDATION_REVISION)
        .bind(OWNED_ROW_WRITE_REVISION)
        .execute(&mut *conn)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(sqlx::Error::Protocol(
                "row validation is stale during a Coop write; run validate_integrity before retrying"
                    .to_string(),
            ));
        }
        Ok(())
    }

    async fn mark_row_writes_validated(conn: &mut SqliteConnection) -> StoreResult<()> {
        let updated = sqlx::query(
            "UPDATE store_integrity
             SET row_validation_revision = ?1, validated_at_ms = ?2
             WHERE singleton = 1 AND row_validation_revision = ?3",
        )
        .bind(ROW_VALIDATION_REVISION)
        .bind(now_ms())
        .bind(OWNED_ROW_WRITE_REVISION)
        .execute(&mut *conn)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(sqlx::Error::Protocol(
                "row-validation ownership was lost during a Coop write".to_string(),
            ));
        }
        Ok(())
    }

    async fn create_indexes(conn: &mut SqliteConnection) -> StoreResult<()> {
        Self::ensure_integrity_state(conn).await?;
        Self::ensure_retention_tombstones(conn).await?;
        // These non-covering v2-development indexes can force SQLite to walk
        // large table-record overflow chains for lifecycle columns stored
        // after the JSON payloads. Replace them transactionally with indexes
        // that contain only the lightweight summary/recovery projection.
        for stale_index in [
            "idx_jobs_tenant_created",
            "idx_jobs_tenant_status_created",
            "idx_jobs_tenant_language_created",
            "idx_jobs_status",
        ] {
            sqlx::query(&format!("DROP INDEX IF EXISTS {stale_index}"))
                .execute(&mut *conn)
                .await?;
        }
        for statement in [
            "CREATE INDEX IF NOT EXISTS idx_events_job_seq ON events(job_id, seq)",
            "CREATE INDEX IF NOT EXISTS idx_jobs_tenant_created_summary ON jobs(
                 tenant, created_at_ms DESC, job_id DESC, language, status,
                 started_at_ms, finished_at_ms, exit_code
             )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_tenant_status_created_summary ON jobs(
                 tenant, status, created_at_ms DESC, job_id DESC, language,
                 started_at_ms, finished_at_ms, exit_code
             )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_tenant_language_created_summary ON jobs(
                 tenant, language, created_at_ms DESC, job_id DESC, status,
                 started_at_ms, finished_at_ms, exit_code
             )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_tenant_status_language_created_summary ON jobs(
                 tenant, status, language, created_at_ms DESC, job_id DESC,
                 started_at_ms, finished_at_ms, exit_code
             )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_id_summary ON jobs(
                 job_id, tenant, language, status, created_at_ms,
                 started_at_ms, finished_at_ms, exit_code
             )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_status_created_recovery ON jobs(
                 status, created_at_ms ASC, job_id ASC, tenant
             )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_retention ON jobs(finished_at_ms, job_id) WHERE finished_at_ms IS NOT NULL",
        ] {
            sqlx::query(statement).execute(&mut *conn).await?;
        }
        create_storage_guard_triggers(conn).await?;
        Ok(())
    }

    async fn ensure_retention_tombstones(conn: &mut SqliteConnection) -> StoreResult<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS retention_tombstones (
                 job_id TEXT PRIMARY KEY NOT NULL
                     REFERENCES jobs(job_id) ON DELETE CASCADE
                     CHECK (typeof(job_id) = 'text' AND length(trim(job_id)) > 0),
                 marked_at_ms INTEGER NOT NULL
                     CHECK (typeof(marked_at_ms) = 'integer' AND marked_at_ms >= 0)
             )",
        )
        .execute(&mut *conn)
        .await?;
        validate_required_columns(
            conn,
            "retention_tombstones",
            &[
                RequiredColumn::primary_key("job_id", "TEXT"),
                RequiredColumn::not_null("marked_at_ms", "INTEGER"),
            ],
        )
        .await?;
        let invalid_rows: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM retention_tombstones
                 WHERE typeof(job_id) != 'text' OR length(trim(job_id)) = 0
                    OR typeof(marked_at_ms) != 'integer' OR marked_at_ms < 0
             ) AS invalid",
        )
        .fetch_one(&mut *conn)
        .await?
        .try_get("invalid")?;
        if invalid_rows != 0 {
            return Err(sqlx::Error::Protocol(
                "retention_tombstones contains invalid values".to_string(),
            ));
        }
        let tombstones = sqlx::query(
            "SELECT rowid AS storage_rowid, CAST(job_id AS BLOB) AS job_id_bytes
             FROM retention_tombstones",
        )
        .fetch_all(&mut *conn)
        .await?;
        for row in tombstones {
            validate_utf8_bytes(
                "retention_tombstones",
                row.try_get("storage_rowid")?,
                "job_id",
                &row.try_get::<Vec<u8>, _>("job_id_bytes")?,
            )?;
        }
        let foreign_keys = sqlx::query("PRAGMA foreign_key_list(retention_tombstones)")
            .fetch_all(&mut *conn)
            .await?;
        let has_job_cascade = foreign_keys.iter().any(|row| {
            row.get::<String, _>("table") == "jobs"
                && row.get::<String, _>("from") == "job_id"
                && row.get::<String, _>("to") == "job_id"
                && row
                    .get::<String, _>("on_delete")
                    .eq_ignore_ascii_case("CASCADE")
        });
        if !has_job_cascade {
            return Err(sqlx::Error::Protocol(
                "retention_tombstones.job_id is missing its cascading jobs foreign key".to_string(),
            ));
        }
        Ok(())
    }

    async fn validate_current_schema(conn: &mut SqliteConnection) -> StoreResult<()> {
        validate_required_columns(
            conn,
            "jobs",
            &[
                RequiredColumn::primary_key("job_id", "TEXT"),
                RequiredColumn::not_null("tenant", "TEXT"),
                RequiredColumn::not_null("language", "TEXT"),
                RequiredColumn::not_null("status", "TEXT"),
                RequiredColumn::not_null("spec_json", "TEXT"),
                RequiredColumn::nullable("effective_spec_json", "TEXT"),
                RequiredColumn::nullable("receipt_json", "TEXT"),
                RequiredColumn::not_null("created_at_ms", "INTEGER"),
                RequiredColumn::nullable("started_at_ms", "INTEGER"),
                RequiredColumn::nullable("finished_at_ms", "INTEGER"),
                RequiredColumn::nullable("exit_code", "INTEGER"),
            ],
        )
        .await?;
        validate_required_columns(
            conn,
            "events",
            &[
                RequiredColumn::primary_key("seq", "INTEGER"),
                RequiredColumn::not_null("job_id", "TEXT"),
                RequiredColumn::not_null("ts_ms", "INTEGER"),
                RequiredColumn::not_null("kind", "TEXT"),
                RequiredColumn::not_null("data_json", "TEXT"),
                RequiredColumn::not_null("prev_hash", "TEXT"),
                RequiredColumn::not_null("event_hash", "TEXT"),
                RequiredColumn::not_null("hash_version", "INTEGER"),
            ],
        )
        .await?;

        let foreign_keys = sqlx::query("PRAGMA foreign_key_list(events)")
            .fetch_all(&mut *conn)
            .await?;
        let has_job_cascade = foreign_keys.iter().any(|row| {
            row.get::<String, _>("table") == "jobs"
                && row.get::<String, _>("from") == "job_id"
                && row.get::<String, _>("to") == "job_id"
                && row
                    .get::<String, _>("on_delete")
                    .eq_ignore_ascii_case("CASCADE")
        });
        if !has_job_cascade {
            return Err(sqlx::Error::Protocol(
                "events.job_id is missing its jobs(job_id) ON DELETE CASCADE foreign key"
                    .to_string(),
            ));
        }

        let events_sql: String =
            sqlx::query("SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'events'")
                .fetch_one(&mut *conn)
                .await?
                .get("sql");
        if !events_sql.to_ascii_uppercase().contains("AUTOINCREMENT") {
            return Err(sqlx::Error::Protocol(
                "events.seq must use AUTOINCREMENT so replay cursors are never reused".to_string(),
            ));
        }
        Ok(())
    }

    async fn validate_current_rows(conn: &mut SqliteConnection) -> StoreResult<()> {
        let invalid_jobs: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM jobs
                 WHERE typeof(job_id) != 'text' OR length(trim(job_id)) = 0
                    OR typeof(tenant) != 'text' OR length(trim(tenant)) = 0
                    OR typeof(language) != 'text' OR length(trim(language)) = 0
                    OR typeof(status) != 'text'
                    OR status NOT IN ('queued','running','succeeded','failed','timed_out','oom_killed','cancelled','error')
                    OR typeof(spec_json) != 'text' OR NOT json_valid(spec_json)
                    OR (effective_spec_json IS NOT NULL AND
                        (typeof(effective_spec_json) != 'text' OR NOT json_valid(effective_spec_json)))
                    OR (receipt_json IS NOT NULL AND
                        (typeof(receipt_json) != 'text' OR NOT json_valid(receipt_json)))
                    OR typeof(created_at_ms) != 'integer' OR created_at_ms < 0
                    OR (started_at_ms IS NOT NULL AND
                        (typeof(started_at_ms) != 'integer' OR started_at_ms < 0))
                    OR (finished_at_ms IS NOT NULL AND
                        (typeof(finished_at_ms) != 'integer' OR finished_at_ms < 0))
                    OR (exit_code IS NOT NULL AND
                        (typeof(exit_code) != 'integer' OR
                         exit_code < -2147483648 OR exit_code > 2147483647))
                    OR (status = 'queued' AND
                        (started_at_ms IS NOT NULL OR finished_at_ms IS NOT NULL))
                    OR (status = 'running' AND
                        (started_at_ms IS NULL OR finished_at_ms IS NOT NULL))
                    OR (status NOT IN ('queued','running') AND finished_at_ms IS NULL)
             ) AS invalid",
        )
        .fetch_one(&mut *conn)
        .await?
        .try_get("invalid")?;
        if invalid_jobs != 0 {
            return Err(sqlx::Error::Protocol(
                "jobs contains values incompatible with the current schema".to_string(),
            ));
        }

        let invalid_events: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM events
                 WHERE typeof(seq) != 'integer'
                    OR seq <= 0 OR seq >= 9223372036854775806
                    OR typeof(job_id) != 'text'
                    OR typeof(ts_ms) != 'integer' OR ts_ms < 0
                    OR typeof(kind) != 'text' OR length(trim(kind)) = 0
                    OR typeof(data_json) != 'text' OR NOT json_valid(data_json)
                    OR typeof(prev_hash) != 'text'
                    OR typeof(event_hash) != 'text'
                    OR typeof(hash_version) != 'integer' OR hash_version NOT IN (0, 1)
                    OR (hash_version = 1 AND length(event_hash) != 64)
             ) AS invalid",
        )
        .fetch_one(&mut *conn)
        .await?
        .try_get("invalid")?;
        if invalid_events != 0 {
            return Err(sqlx::Error::Protocol(
                "events contains values incompatible with the current schema".to_string(),
            ));
        }

        Self::validate_current_utf8(conn).await?;
        Ok(())
    }

    async fn validate_current_utf8(conn: &mut SqliteConnection) -> StoreResult<()> {
        // `json_valid` verifies JSON syntax but SQLite can still accept invalid
        // UTF-8 bytes inside a JSON string. Read the raw bytes through a
        // backpressured stream: physical validation remains fail-closed while
        // retaining at most the current and one pending maximum-sized row.
        {
            let mut rows = sqlx::query(
                "SELECT rowid AS storage_rowid,
                        CAST(job_id AS BLOB) AS job_id_bytes,
                        CAST(tenant AS BLOB) AS tenant_bytes,
                        CAST(language AS BLOB) AS language_bytes,
                        CAST(status AS BLOB) AS status_bytes,
                        CAST(spec_json AS BLOB) AS spec_json_bytes,
                        CAST(effective_spec_json AS BLOB) AS effective_spec_json_bytes,
                        CAST(receipt_json AS BLOB) AS receipt_json_bytes
                 FROM jobs ORDER BY rowid ASC",
            )
            .fetch(&mut *conn);
            while let Some(row) = rows.try_next().await? {
                let rowid: i64 = row.try_get("storage_rowid")?;
                for (column, bytes) in [
                    ("job_id", row.try_get::<Vec<u8>, _>("job_id_bytes")?),
                    ("tenant", row.try_get::<Vec<u8>, _>("tenant_bytes")?),
                    ("language", row.try_get::<Vec<u8>, _>("language_bytes")?),
                    ("status", row.try_get::<Vec<u8>, _>("status_bytes")?),
                    ("spec_json", row.try_get::<Vec<u8>, _>("spec_json_bytes")?),
                ] {
                    validate_utf8_bytes("jobs", rowid, column, &bytes)?;
                }
                for (column, bytes) in [
                    (
                        "effective_spec_json",
                        row.try_get::<Option<Vec<u8>>, _>("effective_spec_json_bytes")?,
                    ),
                    (
                        "receipt_json",
                        row.try_get::<Option<Vec<u8>>, _>("receipt_json_bytes")?,
                    ),
                ] {
                    if let Some(bytes) = bytes {
                        validate_utf8_bytes("jobs", rowid, column, &bytes)?;
                    }
                }
            }
        }

        {
            let mut rows = sqlx::query(
                "SELECT seq,
                        CAST(job_id AS BLOB) AS job_id_bytes,
                        CAST(kind AS BLOB) AS kind_bytes,
                        CAST(data_json AS BLOB) AS data_json_bytes,
                        CAST(prev_hash AS BLOB) AS prev_hash_bytes,
                        CAST(event_hash AS BLOB) AS event_hash_bytes
                 FROM events ORDER BY seq ASC",
            )
            .fetch(&mut *conn);
            while let Some(row) = rows.try_next().await? {
                let seq: i64 = row.try_get("seq")?;
                for (column, bytes) in [
                    ("job_id", row.try_get::<Vec<u8>, _>("job_id_bytes")?),
                    ("kind", row.try_get::<Vec<u8>, _>("kind_bytes")?),
                    ("data_json", row.try_get::<Vec<u8>, _>("data_json_bytes")?),
                    ("prev_hash", row.try_get::<Vec<u8>, _>("prev_hash_bytes")?),
                    ("event_hash", row.try_get::<Vec<u8>, _>("event_hash_bytes")?),
                ] {
                    validate_utf8_bytes("events", seq, column, &bytes)?;
                }
            }
        }
        Ok(())
    }

    async fn migrate_legacy_schema(conn: &mut SqliteConnection) -> StoreResult<()> {
        let has_events = table_exists(conn, "events").await?;
        let legacy_event_sequence = if has_events {
            validated_event_sequence_counter(conn, "events").await?
        } else {
            None
        };
        sqlx::query("ALTER TABLE jobs RENAME TO jobs_legacy_v1")
            .execute(&mut *conn)
            .await?;
        if has_events {
            sqlx::query("ALTER TABLE events RENAME TO events_legacy_v1")
                .execute(&mut *conn)
                .await?;
        }

        create_jobs_table(conn, "jobs").await?;
        create_events_table(conn, "events", "jobs").await?;

        let migration_time = now_ms();
        // Existing rows are preserved. Values which older schemas accepted
        // but the hardened schema cannot safely expose are quarantined or
        // normalized instead of being silently discarded.
        sqlx::query(
            "INSERT INTO jobs (
                job_id, tenant, language, status, spec_json,
                effective_spec_json, receipt_json, created_at_ms,
                started_at_ms, finished_at_ms, exit_code
             )
             SELECT
                job_id,
                CASE WHEN typeof(tenant) != 'text' OR trim(tenant) = ''
                     THEN '__legacy_invalid__:' || job_id ELSE tenant END,
                CASE WHEN typeof(language) != 'text' OR trim(language) = ''
                     THEN 'unknown' ELSE language END,
                CASE
                    WHEN status IN ('queued','running','succeeded','failed','timed_out','oom_killed','cancelled','error') THEN status
                    ELSE 'error'
                END,
                CASE WHEN typeof(spec_json) = 'text' AND json_valid(spec_json) THEN spec_json
                     ELSE json_object(
                         'legacy_invalid_spec_json',
                         CASE WHEN typeof(spec_json) = 'blob'
                              THEN 'blob:hex:' || hex(spec_json)
                              ELSE CAST(spec_json AS TEXT) END
                     ) END,
                NULL,
                NULL,
                CASE WHEN typeof(created_at_ms) = 'integer' AND created_at_ms >= 0
                     THEN created_at_ms ELSE 0 END,
                CASE
                    WHEN status = 'queued' THEN NULL
                    WHEN status = 'running' THEN
                        CASE
                            WHEN typeof(started_at_ms) = 'integer' AND started_at_ms >= 0
                                THEN started_at_ms
                            WHEN typeof(created_at_ms) = 'integer' AND created_at_ms >= 0
                                THEN created_at_ms
                            ELSE 0
                        END
                    WHEN started_at_ms IS NULL THEN NULL
                    WHEN typeof(started_at_ms) = 'integer' AND started_at_ms >= 0
                        THEN started_at_ms
                    ELSE NULL
                END,
                CASE
                    WHEN status IN ('queued','running') THEN NULL
                    WHEN typeof(finished_at_ms) = 'integer' AND finished_at_ms >= 0
                        THEN finished_at_ms
                    WHEN typeof(started_at_ms) = 'integer' AND started_at_ms >= 0
                        THEN started_at_ms
                    WHEN typeof(created_at_ms) = 'integer' AND created_at_ms >= 0
                        THEN created_at_ms
                    ELSE ?1
                END,
                CASE WHEN exit_code IS NULL OR
                               (typeof(exit_code) = 'integer' AND
                                exit_code BETWEEN -2147483648 AND 2147483647)
                     THEN exit_code ELSE NULL END
             FROM jobs_legacy_v1",
        )
        .bind(migration_time)
        .execute(&mut *conn)
        .await?;

        if has_events {
            let invalid_sequence_count: i64 = sqlx::query(
                "SELECT COUNT(*) AS n FROM events_legacy_v1
                 WHERE typeof(seq) != 'integer'
                    OR seq <= 0 OR seq >= 9223372036854775806",
            )
            .fetch_one(&mut *conn)
            .await?
            .try_get("n")?;
            if invalid_sequence_count != 0 {
                return Err(sqlx::Error::Protocol(format!(
                    "legacy database contains {invalid_sequence_count} non-positive event sequence values"
                )));
            }
            let orphan_count: i64 = sqlx::query(
                "SELECT COUNT(*) AS n
                 FROM events_legacy_v1 AS event
                 LEFT JOIN jobs AS job ON job.job_id = event.job_id
                 WHERE job.job_id IS NULL",
            )
            .fetch_one(&mut *conn)
            .await?
            .get("n");
            if orphan_count > 0 {
                return Err(sqlx::Error::Protocol(format!(
                    "legacy database contains {orphan_count} orphan event rows"
                )));
            }
            sqlx::query(
                "INSERT INTO events (
                    seq, job_id, ts_ms, kind, data_json,
                    prev_hash, event_hash, hash_version
                 )
                 SELECT
                    event.seq,
                    event.job_id,
                    CASE WHEN typeof(event.ts_ms) = 'integer' AND event.ts_ms >= 0
                         THEN event.ts_ms ELSE 0 END,
                    CASE WHEN typeof(event.kind) != 'text' OR trim(event.kind) = ''
                         THEN 'legacy_unknown' ELSE event.kind END,
                    CASE WHEN typeof(event.data_json) = 'text' AND json_valid(event.data_json)
                         THEN event.data_json
                         ELSE json_object(
                             'legacy_invalid_data_json',
                             CASE WHEN typeof(event.data_json) = 'blob'
                                  THEN 'blob:hex:' || hex(event.data_json)
                                  ELSE CAST(event.data_json AS TEXT) END
                         ) END,
                    '', '', 0
                 FROM events_legacy_v1 AS event
                 INNER JOIN jobs AS job ON job.job_id = event.job_id
                 ORDER BY event.seq ASC",
            )
            .execute(&mut *conn)
            .await?;
            if let Some(high_watermark) = legacy_event_sequence {
                raise_event_sequence_counter(conn, "events", high_watermark).await?;
            }
            sqlx::query("DROP TABLE events_legacy_v1")
                .execute(&mut *conn)
                .await?;
        }
        sqlx::query("DROP TABLE jobs_legacy_v1")
            .execute(&mut *conn)
            .await?;
        Self::create_indexes(conn).await
    }

    pub async fn schema_version(&self) -> StoreResult<i64> {
        let row = sqlx::query("SELECT COALESCE(MAX(version), 0) AS version FROM schema_migrations")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("version"))
    }

    /// Explicit, O(database-bytes) physical/value integrity check. Ordinary
    /// opens repeat this scan only when the durable validation revision or
    /// owned storage guards are absent/stale; callers can invoke this method
    /// during a maintenance window to detect offline byte-level corruption.
    pub async fn validate_integrity(&self) -> StoreResult<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::validate_current_schema(&mut tx).await?;
        Self::ensure_integrity_state(&mut tx).await?;
        let _ = Self::row_validation_current(&mut tx).await?;
        Self::validate_current_rows(&mut tx).await?;
        validate_foreign_keys(&mut tx).await?;
        Self::create_indexes(&mut tx).await?;
        Self::record_row_validation(&mut tx).await?;
        tx.commit().await
    }

    pub async fn create_job(
        &self,
        job_id: &str,
        tenant: &str,
        language: &str,
        spec_json: &str,
    ) -> StoreResult<()> {
        self.create_job_with_event(job_id, tenant, language, spec_json)
            .await
            .map(|_| ())
    }

    /// Atomically creates the queued row and the first, hashed `accepted`
    /// event. Callers that need to broadcast the event can use the returned
    /// row; the compatibility `create_job` wrapper deliberately discards it.
    pub async fn create_job_with_event(
        &self,
        job_id: &str,
        tenant: &str,
        language: &str,
        spec_json: &str,
    ) -> StoreResult<EventRow> {
        if job_id.trim().is_empty() || tenant.trim().is_empty() || language.trim().is_empty() {
            return Err(sqlx::Error::InvalidArgument(
                "job_id, tenant, and language must be non-empty".to_string(),
            ));
        }
        let requested_spec: Value = serde_json::from_str(spec_json)
            .map_err(|error| sqlx::Error::InvalidArgument(error.to_string()))?;
        let requested_spec_sha256 = sha256_hex(canonical_json(&requested_spec).as_bytes());

        let created_at = now_ms();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;
        sqlx::query(
            "INSERT INTO jobs (
                job_id, tenant, language, status, spec_json, created_at_ms
             ) VALUES (?1, ?2, ?3, 'queued', ?4, ?5)",
        )
        .bind(job_id)
        .bind(tenant)
        .bind(language)
        .bind(spec_json)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        let event = append_event_tx(
            &mut tx,
            job_id,
            "accepted",
            &json!({
                "status": "queued",
                "tenant": tenant,
                "language": language,
                "requested_spec_sha256": requested_spec_sha256,
            }),
            created_at,
        )
        .await?;
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(event)
    }

    /// Compatibility transition which does not append a `started` event.
    /// New scheduler code should prefer `start_with_event_if_queued` so the
    /// transition, effective spec, and evidence event commit atomically.
    pub async fn set_started_if_queued(&self, job_id: &str) -> StoreResult<bool> {
        self.set_started_inner(job_id, None).await
    }

    /// Persist the effective (post-policy/clamping) spec with the guarded
    /// queued-to-running transition. This compatibility form omits the event;
    /// `start_with_event_if_queued` is preferred for new code.
    pub async fn set_started_with_effective_if_queued(
        &self,
        job_id: &str,
        effective_spec: &Value,
    ) -> StoreResult<bool> {
        self.set_started_inner(job_id, Some(effective_spec)).await
    }

    async fn set_started_inner(
        &self,
        job_id: &str,
        effective_spec: Option<&Value>,
    ) -> StoreResult<bool> {
        let effective_json = effective_spec.map(canonical_json);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'running', started_at_ms = ?2,
                 effective_spec_json = COALESCE(?3, effective_spec_json)
             WHERE job_id = ?1 AND status = 'queued'",
        )
        .bind(job_id)
        .bind(now_ms())
        .bind(effective_json)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Preferred guarded start: commits row state, effective spec, and the
    /// hashed `started` event as one transaction.
    pub async fn start_with_event_if_queued(
        &self,
        job_id: &str,
        effective_spec: &Value,
    ) -> StoreResult<Option<EventRow>> {
        let started_at = now_ms();
        let effective_json = canonical_json(effective_spec);
        let initial_effective_spec_sha256 = sha256_hex(effective_json.as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'running', started_at_ms = ?2, effective_spec_json = ?3
             WHERE job_id = ?1 AND status = 'queued'",
        )
        .bind(job_id)
        .bind(started_at)
        .bind(&effective_json)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(None);
        }

        let event = append_event_tx(
            &mut tx,
            job_id,
            "started",
            &json!({
                "status": "running",
                "initial_effective_spec_sha256": initial_effective_spec_sha256,
            }),
            started_at,
        )
        .await?;
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(Some(event))
    }

    /// Compatibility terminal write. New code should pass a receipt and
    /// broadcast the event returned by `finalize_with_event`.
    pub async fn finish(
        &self,
        job_id: &str,
        status: &str,
        exit_code: Option<i32>,
    ) -> StoreResult<()> {
        self.finalize_with_event(job_id, status, exit_code, 0, None)
            .await
            .map(|_| ())
    }

    /// Finalize a queued or running job exactly once. The terminal row state,
    /// final hashed event, receipt JSON, and the receipt's chain head/count
    /// are committed atomically. `None` means the row was absent or already
    /// terminal.
    pub async fn finalize_with_event(
        &self,
        job_id: &str,
        status: &str,
        exit_code: Option<i32>,
        duration_ms: i64,
        receipt: Option<&Value>,
    ) -> StoreResult<Option<EventRow>> {
        self.finalize_with_event_and_effective_spec(
            job_id,
            status,
            exit_code,
            duration_ms,
            None,
            receipt,
        )
        .await
    }

    /// Finalize a job and, when supplied, atomically replace the initial
    /// effective-spec snapshot with controls observed by the executor. The
    /// terminal event binds that final canonical spec by SHA-256.
    pub async fn finalize_with_event_and_effective_spec(
        &self,
        job_id: &str,
        status: &str,
        exit_code: Option<i32>,
        duration_ms: i64,
        effective_spec: Option<&Value>,
        receipt: Option<&Value>,
    ) -> StoreResult<Option<EventRow>> {
        if !is_terminal_status(status) {
            return Err(sqlx::Error::InvalidArgument(format!(
                "cannot finalize a job with non-terminal status {status:?}"
            )));
        }
        self.finalize_inner(
            job_id,
            None,
            None,
            status,
            exit_code,
            duration_ms,
            effective_spec,
            receipt,
        )
        .await
    }

    /// Atomically cancel a job only if it is still queued and belongs to the
    /// supplied tenant. Blank tenants are rejected, never interpreted as a
    /// cross-tenant wildcard.
    pub async fn cancel_queued_with_event(
        &self,
        job_id: &str,
        tenant: &str,
        receipt: Option<&Value>,
    ) -> StoreResult<Option<EventRow>> {
        if tenant.trim().is_empty() {
            return Err(sqlx::Error::InvalidArgument(
                "tenant must be non-empty".to_string(),
            ));
        }
        self.finalize_inner(
            job_id,
            Some(tenant),
            Some("queued"),
            "cancelled",
            None,
            0,
            None,
            receipt,
        )
        .await
    }

    /// Compatibility wrapper for callers without a tenant in hand. It still
    /// emits the terminal event atomically, but API paths should use the
    /// tenant-scoped method above.
    pub async fn cancel_if_queued(&self, job_id: &str) -> StoreResult<bool> {
        self.finalize_inner(
            job_id,
            None,
            Some("queued"),
            "cancelled",
            None,
            0,
            None,
            None,
        )
        .await
        .map(|event| event.is_some())
    }

    #[allow(clippy::too_many_arguments)]
    async fn finalize_inner(
        &self,
        job_id: &str,
        tenant: Option<&str>,
        required_status: Option<&str>,
        status: &str,
        exit_code: Option<i32>,
        duration_ms: i64,
        effective_spec: Option<&Value>,
        receipt: Option<&Value>,
    ) -> StoreResult<Option<EventRow>> {
        let effective_json = effective_spec.map(canonical_json);
        let effective_spec_sha256 = effective_json
            .as_deref()
            .map(|value| sha256_hex(value.as_bytes()));
        let finished_at = now_ms();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;

        let result = sqlx::query(
            "UPDATE jobs
             SET status = ?2, exit_code = ?3, finished_at_ms = ?4,
                 effective_spec_json = COALESCE(?7, effective_spec_json)
             WHERE job_id = ?1
               AND status IN ('queued','running')
               AND (?5 IS NULL OR tenant = ?5)
               AND (?6 IS NULL OR status = ?6)",
        )
        .bind(job_id)
        .bind(status)
        .bind(exit_code)
        .bind(finished_at)
        .bind(tenant)
        .bind(required_status)
        .bind(effective_json.as_deref())
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(None);
        }

        let lifecycle =
            sqlx::query("SELECT created_at_ms, started_at_ms FROM jobs WHERE job_id = ?1")
                .bind(job_id)
                .fetch_one(&mut *tx)
                .await?;
        let created_at_ms: i64 = lifecycle.get("created_at_ms");
        let started_at_ms: Option<i64> = lifecycle.get("started_at_ms");

        let mut finished_data = json!({
            "status": status,
            "exit_code": exit_code,
            "duration_ms": duration_ms.max(0),
        });
        if let Some(digest) = effective_spec_sha256 {
            finished_data
                .as_object_mut()
                .expect("finished event data is an object")
                .insert("effective_spec_sha256".to_string(), json!(digest));
        }
        let event =
            append_event_tx(&mut tx, job_id, "finished", &finished_data, finished_at).await?;
        let chain = event_chain_head_tx(&mut tx, job_id).await?;
        let receipt_json = receipt_with_chain(
            receipt,
            &chain,
            ReceiptCore {
                job_id,
                status,
                exit_code,
                created_at_ms,
                started_at_ms,
                finished_at_ms: finished_at,
                duration_ms: duration_ms.max(0),
            },
        );
        sqlx::query("UPDATE jobs SET receipt_json = ?2 WHERE job_id = ?1")
            .bind(job_id)
            .bind(receipt_json)
            .execute(&mut *tx)
            .await?;
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(Some(event))
    }

    pub async fn append_event(
        &self,
        job_id: &str,
        kind: &str,
        data: &Value,
    ) -> StoreResult<(i64, i64)> {
        self.append_event_row(job_id, kind, data)
            .await
            .map(|event| (event.seq, event.ts_ms))
    }

    pub async fn append_event_row(
        &self,
        job_id: &str,
        kind: &str,
        data: &Value,
    ) -> StoreResult<EventRow> {
        let pending = [(kind.to_string(), data.clone())];
        let mut appended = self.append_events_batch(job_id, &pending).await?;
        appended.pop().ok_or_else(|| {
            sqlx::Error::Protocol("single-event append returned no event".to_string())
        })
    }

    /// Append a bounded ordered batch using one SQLite transaction and one
    /// durable commit. Every returned row corresponds to the same-position
    /// input, and each hash links to the event immediately before it (whether
    /// that event predated this batch or was inserted earlier in the batch).
    ///
    /// An empty slice is a no-op. Oversized batches and invalid kinds fail
    /// before any transaction is opened. A database error at any position
    /// rolls back the complete batch.
    pub async fn append_events_batch(
        &self,
        job_id: &str,
        events: &[(String, Value)],
    ) -> StoreResult<Vec<EventRow>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        if events.len() > MAX_EVENT_BATCH_SIZE {
            return Err(sqlx::Error::InvalidArgument(format!(
                "event batch contains {} entries; maximum is {MAX_EVENT_BATCH_SIZE}",
                events.len()
            )));
        }
        if events.iter().any(|(kind, _)| kind.trim().is_empty()) {
            return Err(sqlx::Error::InvalidArgument(
                "event kind must be non-empty".to_string(),
            ));
        }

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;
        let job = sqlx::query(
            "SELECT status FROM jobs
             WHERE job_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = jobs.job_id
               )",
        )
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?;
        match job
            .map(|row| row.try_get::<String, _>("status"))
            .transpose()?
        {
            Some(status) if matches!(status.as_str(), "queued" | "running") => {}
            Some(_) => {
                tx.rollback().await?;
                return Err(sqlx::Error::InvalidArgument(
                    "cannot append an event after terminal finalization".to_string(),
                ));
            }
            None => {
                tx.rollback().await?;
                return Err(sqlx::Error::RowNotFound);
            }
        }

        let mut prev_hash = previous_event_hash_tx(&mut tx, job_id).await?;
        let batch_ts = now_ms();
        let mut appended = Vec::with_capacity(events.len());
        for (kind, data) in events {
            let event =
                match insert_hashed_event_tx(&mut tx, job_id, kind, data, batch_ts, &prev_hash)
                    .await
                {
                    Ok(event) => event,
                    Err(error) => {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                };
            prev_hash.clone_from(&event.event_hash);
            appended.push(event);
        }
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(appended)
    }

    pub async fn last_seq(&self, job_id: &str) -> StoreResult<i64> {
        let row = sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) AS max_seq FROM events
                 WHERE job_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM retention_tombstones
                       WHERE retention_tombstones.job_id = events.job_id
                   )",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("max_seq"))
    }

    pub async fn events_for(&self, job_id: &str) -> StoreResult<Vec<EventRow>> {
        let rows = sqlx::query(
            "SELECT seq, ts_ms, kind, data_json, prev_hash, event_hash, hash_version
             FROM events
             WHERE job_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = events.job_id
               )
             ORDER BY seq ASC",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_event).collect()
    }

    /// Cursor page for replay and streaming catch-up. The cursor is exclusive
    /// and global event sequence numbers remain stable across retention.
    pub async fn events_after(
        &self,
        job_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> StoreResult<Vec<EventRow>> {
        let rows = sqlx::query(
            "SELECT seq, ts_ms, kind, data_json, prev_hash, event_hash, hash_version
             FROM events
             WHERE job_id = ?1 AND seq > ?2
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = events.job_id
               )
             ORDER BY seq ASC
             LIMIT ?3",
        )
        .bind(job_id)
        .bind(after_seq.max(0))
        .bind(limit.clamp(1, MAX_EVENT_PAGE))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_event).collect()
    }

    pub async fn event_chain_head(&self, job_id: &str) -> StoreResult<EventChainHead> {
        let mut tx = self.pool.begin().await?;
        let head = event_chain_head_tx(&mut tx, job_id).await?;
        tx.commit().await?;
        Ok(head)
    }

    /// Recomputes every v1 digest and link. Legacy rows are reported, not
    /// treated as verified; a verified suffix after legacy history is valid
    /// only when it starts a new chain with an empty `prev_hash`.
    pub async fn verify_event_chain(&self, job_id: &str) -> StoreResult<EventChainVerification> {
        let events = self.events_for(job_id).await?;
        let mut valid = true;
        let mut expected_prev = String::new();
        let mut previous_was_legacy = false;
        let mut verified = 0_i64;
        let mut legacy = 0_i64;

        for event in &events {
            match event.hash_version {
                0 => {
                    legacy += 1;
                    expected_prev.clear();
                    previous_was_legacy = true;
                }
                1 => {
                    verified += 1;
                    if (previous_was_legacy && !event.prev_hash.is_empty())
                        || (!previous_was_legacy && event.prev_hash != expected_prev)
                        || event.event_hash
                            != compute_event_hash(
                                job_id,
                                &event.prev_hash,
                                event.seq,
                                event.ts_ms,
                                &event.kind,
                                &event.data,
                            )
                    {
                        valid = false;
                    }
                    expected_prev.clone_from(&event.event_hash);
                    previous_was_legacy = false;
                }
                _ => valid = false,
            }
        }

        let head = EventChainHead {
            event_count: events.len() as i64,
            verified_event_count: verified,
            legacy_event_count: legacy,
            head_hash: events
                .last()
                .filter(|event| event.hash_version == 1)
                .map(|event| event.event_hash.clone()),
        };
        Ok(EventChainVerification { head, valid })
    }

    pub async fn get_job(&self, job_id: &str) -> StoreResult<Option<JobRow>> {
        let row = sqlx::query(
            "SELECT job_id, tenant, language, status, spec_json,
                    effective_spec_json, receipt_json, created_at_ms,
                    started_at_ms, finished_at_ms, exit_code
             FROM jobs
             WHERE job_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = jobs.job_id
               )",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(Self::row_to_job).transpose()
    }

    /// Point lookup for status/ownership and other lifecycle-only paths. This
    /// intentionally avoids decoding the potentially multi-megabyte spec and
    /// receipt columns selected by `get_job`.
    pub async fn get_job_summary(&self, job_id: &str) -> StoreResult<Option<JobSummary>> {
        let row = sqlx::query(
            "SELECT job_id, tenant, language, status, created_at_ms,
                    started_at_ms, finished_at_ms, exit_code
             FROM jobs INDEXED BY idx_jobs_id_summary
             WHERE job_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = jobs.job_id
               )",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(Self::row_to_job_summary).transpose()
    }

    pub async fn list_jobs(&self, tenant: Option<&str>, limit: i64) -> StoreResult<Vec<JobRow>> {
        self.list_jobs_page(ListJobsQuery {
            tenant: tenant.map(ToOwned::to_owned),
            limit: limit.clamp(1, MAX_JOB_PAGE),
            ..ListJobsQuery::default()
        })
        .await
    }

    /// Stable keyset pagination over `(created_at_ms, job_id)` with optional
    /// strict filters. This deliberately avoids offset pagination, whose
    /// pages shift as new jobs arrive.
    pub async fn list_jobs_page(&self, query: ListJobsQuery) -> StoreResult<Vec<JobRow>> {
        if query.tenant.as_deref().is_some_and(str::is_empty) {
            return Ok(Vec::new());
        }
        // Only active predicates are emitted. Optional `? IS NULL OR ...`
        // clauses prevent SQLite from using the tenant/keyset indexes and
        // turn every authenticated page into a full-table scan and temp sort.
        let mut statement = build_job_list_query("", JOB_ROW_PROJECTION, &query);
        let rows = statement.build().fetch_all(&self.pool).await?;
        rows.into_iter().map(Self::row_to_job).collect()
    }

    /// Stable keyset pagination for list surfaces without loading the large
    /// spec and receipt columns. Like `list_jobs_page`, this accepts one row
    /// beyond the public page maximum so callers can determine `has_more`.
    pub async fn list_job_summaries_page(
        &self,
        query: ListJobsQuery,
    ) -> StoreResult<Vec<JobSummary>> {
        if query.tenant.as_deref().is_some_and(str::is_empty) {
            return Ok(Vec::new());
        }
        let mut statement = build_job_list_query("", JOB_SUMMARY_PROJECTION, &query);
        let rows = statement.build().fetch_all(&self.pool).await?;
        rows.into_iter().map(Self::row_to_job_summary).collect()
    }

    pub async fn count_by_status(&self) -> StoreResult<Vec<(String, i64)>> {
        let rows = sqlx::query(
            "SELECT status, COUNT(*) AS n FROM jobs
             WHERE NOT EXISTS (
                 SELECT 1 FROM retention_tombstones
                 WHERE retention_tombstones.job_id = jobs.job_id
             )
             GROUP BY status ORDER BY status",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("status")?, row.try_get("n")?)))
            .collect()
    }

    pub async fn count_by_status_for_tenant(
        &self,
        tenant: &str,
    ) -> StoreResult<Vec<(String, i64)>> {
        if tenant.trim().is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT status, COUNT(*) AS n
             FROM jobs WHERE tenant = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = jobs.job_id
               )
             GROUP BY status ORDER BY status",
        )
        .bind(tenant)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("status")?, row.try_get("n")?)))
            .collect()
    }

    /// Return persisted queued jobs oldest first so startup recovery can
    /// re-enqueue accepted work rather than incorrectly failing it.
    pub async fn queued_job_ids(&self, limit: i64) -> StoreResult<Vec<String>> {
        let rows = sqlx::query(
            "SELECT job_id FROM jobs
             WHERE status = 'queued'
               AND NOT EXISTS (
                   SELECT 1 FROM retention_tombstones
                   WHERE retention_tombstones.job_id = jobs.job_id
               )
             ORDER BY created_at_ms ASC, job_id ASC
             LIMIT ?1",
        )
        .bind(limit.clamp(1, 100_000))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|row| row.try_get("job_id")).collect()
    }

    /// Stable ascending page for boot reconciliation. Advancing with a cursor
    /// derived from the final row does not skip later jobs when earlier rows
    /// concurrently transition out of `queued`.
    pub async fn queued_jobs_page(
        &self,
        after: Option<&JobCursor>,
        limit: i64,
    ) -> StoreResult<Vec<QueuedJobRow>> {
        let mut statement = build_queued_jobs_query("", after, limit);
        let rows = statement.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(QueuedJobRow {
                    job_id: row.try_get("job_id")?,
                    tenant: row.try_get("tenant")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                })
            })
            .collect()
    }

    /// Delete one bounded batch of terminal jobs according to their actual
    /// finish time. Both the candidate-job count and cumulative event count
    /// are hard-bounded so cascade deletion cannot monopolize SQLite's sole
    /// writer. An individually oversized legacy history is drained newest
    /// first over multiple sweeps; its stale receipt is cleared as soon as
    /// partial pruning begins. This is a logical retention operation; use
    /// `compact` during a maintenance window for physical reclaim.
    pub async fn prune_older_than_batch(
        &self,
        max_age_ms: i64,
        batch_size: i64,
    ) -> StoreResult<RetentionReport> {
        if max_age_ms < 0 {
            return Err(sqlx::Error::InvalidArgument(
                "max_age_ms must be non-negative".to_string(),
            ));
        }
        let cutoff = now_ms().saturating_sub(max_age_ms);
        let batch_size = batch_size.clamp(1, MAX_RETENTION_BATCH);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;
        let candidate_sql = format!(
            "SELECT job_id FROM jobs
             WHERE status IN ({TERMINAL_STATUSES_SQL})
               AND finished_at_ms < ?1
             ORDER BY finished_at_ms ASC, job_id ASC
             LIMIT ?2"
        );
        let candidates = sqlx::query(&candidate_sql)
            .bind(cutoff)
            .bind(batch_size)
            .fetch_all(&mut *tx)
            .await?;

        let mut jobs_deleted = 0_u64;
        let mut events_deleted = 0_u64;
        for candidate in candidates {
            let job_id: String = candidate.try_get("job_id")?;
            let event_count = sqlx::query("SELECT COUNT(*) AS n FROM events WHERE job_id = ?1")
                .bind(&job_id)
                .fetch_one(&mut *tx)
                .await?
                .try_get::<i64, _>("n")?
                .max(0) as u64;
            let event_budget = MAX_RETENTION_EVENTS_PER_BATCH.saturating_sub(events_deleted);

            if event_count <= event_budget {
                let deleted = sqlx::query("DELETE FROM jobs WHERE job_id = ?1")
                    .bind(&job_id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                jobs_deleted = jobs_deleted.saturating_add(deleted);
                if deleted != 0 {
                    events_deleted = events_deleted.saturating_add(event_count);
                }
                if events_deleted >= MAX_RETENTION_EVENTS_PER_BATCH {
                    break;
                }
                continue;
            }

            // Do not begin partially draining another job after this sweep
            // has already made progress. It will be the oldest candidate on
            // the next sweep and receive the complete event budget then.
            if jobs_deleted != 0 || events_deleted != 0 {
                break;
            }

            // The tombstone and first chunk commit atomically. Every public
            // reader excludes tombstoned jobs/events, so no transaction can
            // observe a receipt beside a partially retained hash chain.
            sqlx::query(
                "INSERT OR IGNORE INTO retention_tombstones(job_id, marked_at_ms)
                 VALUES (?1, ?2)",
            )
            .bind(&job_id)
            .bind(now_ms())
            .execute(&mut *tx)
            .await?;
            let deleted = sqlx::query(
                "DELETE FROM events
                 WHERE seq IN (
                     SELECT seq FROM events
                     WHERE job_id = ?1
                     ORDER BY seq DESC
                     LIMIT ?2
                 )",
            )
            .bind(&job_id)
            .bind(MAX_RETENTION_EVENTS_PER_BATCH as i64)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            events_deleted = events_deleted.saturating_add(deleted);
            sqlx::query("UPDATE jobs SET receipt_json = NULL WHERE job_id = ?1")
                .bind(&job_id)
                .execute(&mut *tx)
                .await?;
            break;
        }

        let remaining_sql = format!(
            "SELECT EXISTS(
                SELECT 1 FROM jobs
                WHERE status IN ({TERMINAL_STATUSES_SQL})
                  AND finished_at_ms < ?1
             ) AS more_remaining"
        );
        let more_remaining = sqlx::query(&remaining_sql)
            .bind(cutoff)
            .fetch_one(&mut *tx)
            .await?
            .get::<i64, _>("more_remaining")
            != 0;
        let report = RetentionReport {
            jobs_deleted,
            events_deleted,
            more_remaining,
        };
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(report)
    }

    /// Compatibility wrapper: one bounded retention batch, never a
    /// variable-length `IN (?, ... ?)` query.
    pub async fn prune_older_than(&self, max_age_ms: i64) -> StoreResult<(u64, u64)> {
        let report = self
            .prune_older_than_batch(max_age_ms, DEFAULT_RETENTION_BATCH)
            .await?;
        Ok((report.jobs_deleted, report.events_deleted))
    }

    /// Run SQLite's physical space reclamation. This can take an exclusive
    /// lock and is intentionally separate from routine retention sweeps.
    pub async fn compact(&self) -> StoreResult<()> {
        checkpoint_wal(&self.pool).await?;
        sqlx::query("VACUUM").execute(&self.pool).await?;
        checkpoint_wal(&self.pool).await?;
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;
        harden_storage_files(&self.db_path)?;
        Ok(())
    }

    /// Boot recovery finalizes only work which had actually started. Accepted
    /// queued jobs remain queued and can be obtained with `queued_job_ids`.
    /// Every recovered running job receives a terminal hashed event in the
    /// same transaction as its row update. Only a small page of identifiers
    /// and one job's potentially large specs are resident at a time. Each job
    /// commits independently, so a later corrupt row or I/O failure leaves
    /// earlier recoveries durable and the failing row wholly running.
    pub async fn recover_stale_running(&self) -> StoreResult<u64> {
        let recovered_at = now_ms();
        let mut recovered = 0_u64;
        loop {
            let rows = sqlx::query(
                "SELECT job_id FROM jobs
                 WHERE status = 'running'
                 ORDER BY created_at_ms ASC, job_id ASC
                 LIMIT ?1",
            )
            .bind(MAX_RUNNING_RECOVERY_ID_PAGE)
            .fetch_all(&self.pool)
            .await?;
            if rows.is_empty() {
                return Ok(recovered);
            }
            let job_ids = rows
                .into_iter()
                .map(|row| row.try_get::<String, _>("job_id"))
                .collect::<StoreResult<Vec<_>>>()?;

            for job_id in job_ids {
                if self
                    .recover_one_stale_running(&job_id, recovered_at)
                    .await?
                {
                    recovered = recovered.saturating_add(1);
                }
            }
        }
    }

    async fn recover_one_stale_running(
        &self,
        job_id: &str,
        recovered_at: i64,
    ) -> StoreResult<bool> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::begin_row_writes(&mut tx).await?;
        let row = sqlx::query(
            "SELECT created_at_ms, started_at_ms, spec_json, effective_spec_json
             FROM jobs
             WHERE job_id = ?1 AND status = 'running'",
        )
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(false);
        };
        let created_at_ms: i64 = row.try_get("created_at_ms")?;
        let started_at: Option<i64> = row.try_get("started_at_ms")?;
        let requested_spec: String = row.try_get("spec_json")?;
        let effective_spec: Option<String> = row.try_get("effective_spec_json")?;
        drop(row);

        let duration_ms = started_at
            .map(|started| recovered_at.saturating_sub(started).max(0))
            .unwrap_or(0);
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'error', exit_code = NULL, finished_at_ms = ?2
             WHERE job_id = ?1 AND status = 'running'",
        )
        .bind(job_id)
        .bind(recovered_at)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }

        append_event_tx(
            &mut tx,
            job_id,
            "finished",
            &json!({
                "status": "error",
                "exit_code": Value::Null,
                "duration_ms": duration_ms,
                "reason": "server_restarted",
            }),
            recovered_at,
        )
        .await?;
        let chain = event_chain_head_tx(&mut tx, job_id).await?;
        let recovery_receipt = recovery_receipt_details(
            &requested_spec,
            effective_spec.as_deref(),
            created_at_ms,
            started_at,
        );
        let receipt = receipt_with_chain(
            Some(&recovery_receipt),
            &chain,
            ReceiptCore {
                job_id,
                status: "error",
                exit_code: None,
                created_at_ms,
                started_at_ms: started_at,
                finished_at_ms: recovered_at,
                duration_ms,
            },
        );
        sqlx::query("UPDATE jobs SET receipt_json = ?2 WHERE job_id = ?1")
            .bind(job_id)
            .bind(receipt)
            .execute(&mut *tx)
            .await?;
        Self::mark_row_writes_validated(&mut tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    fn row_to_job(row: sqlx::sqlite::SqliteRow) -> StoreResult<JobRow> {
        let exit_code = row
            .try_get::<Option<i64>, _>("exit_code")?
            .map(i32::try_from)
            .transpose()
            .map_err(|_| sqlx::Error::Protocol("exit_code is outside the i32 range".to_string()))?;
        Ok(JobRow {
            job_id: row.try_get("job_id")?,
            tenant: row.try_get("tenant")?,
            language: row.try_get("language")?,
            status: row.try_get("status")?,
            created_at_ms: row.try_get("created_at_ms")?,
            started_at_ms: row.try_get("started_at_ms")?,
            finished_at_ms: row.try_get("finished_at_ms")?,
            exit_code,
            spec_json: row.try_get("spec_json")?,
            effective_spec_json: row.try_get("effective_spec_json")?,
            receipt_json: row.try_get("receipt_json")?,
        })
    }

    fn row_to_job_summary(row: sqlx::sqlite::SqliteRow) -> StoreResult<JobSummary> {
        let exit_code = row
            .try_get::<Option<i64>, _>("exit_code")?
            .map(i32::try_from)
            .transpose()
            .map_err(|_| sqlx::Error::Protocol("exit_code is outside the i32 range".to_string()))?;
        Ok(JobSummary {
            job_id: row.try_get("job_id")?,
            tenant: row.try_get("tenant")?,
            language: row.try_get("language")?,
            status: row.try_get("status")?,
            created_at_ms: row.try_get("created_at_ms")?,
            started_at_ms: row.try_get("started_at_ms")?,
            finished_at_ms: row.try_get("finished_at_ms")?,
            exit_code,
        })
    }
}

#[derive(Clone, Copy)]
struct RequiredColumn {
    name: &'static str,
    declared_type: &'static str,
    require_not_null: bool,
    require_primary_key: bool,
}

impl RequiredColumn {
    const fn primary_key(name: &'static str, declared_type: &'static str) -> Self {
        Self {
            name,
            declared_type,
            require_not_null: false,
            require_primary_key: true,
        }
    }

    const fn not_null(name: &'static str, declared_type: &'static str) -> Self {
        Self {
            name,
            declared_type,
            require_not_null: true,
            require_primary_key: false,
        }
    }

    const fn nullable(name: &'static str, declared_type: &'static str) -> Self {
        Self {
            name,
            declared_type,
            require_not_null: false,
            require_primary_key: false,
        }
    }
}

fn validate_utf8_bytes(table: &str, row_key: i64, column: &str, bytes: &[u8]) -> StoreResult<()> {
    std::str::from_utf8(bytes).map_err(|_| {
        sqlx::Error::Protocol(format!(
            "{table}.{column} contains invalid UTF-8 at row {row_key}"
        ))
    })?;
    Ok(())
}

async fn validate_required_columns(
    conn: &mut SqliteConnection,
    table: &str,
    required: &[RequiredColumn],
) -> StoreResult<()> {
    let statement = format!("PRAGMA table_info({table})");
    let rows = sqlx::query(&statement).fetch_all(&mut *conn).await?;
    for expected in required {
        let Some(row) = rows
            .iter()
            .find(|row| row.get::<String, _>("name") == expected.name)
        else {
            return Err(sqlx::Error::Protocol(format!(
                "{table} table is missing required column {}",
                expected.name
            )));
        };
        let declared_type: String = row.get("type");
        if !declared_type
            .trim()
            .eq_ignore_ascii_case(expected.declared_type)
        {
            return Err(sqlx::Error::Protocol(format!(
                "{table}.{} has type {declared_type:?}; expected {}",
                expected.name, expected.declared_type
            )));
        }
        if expected.require_not_null && row.get::<i64, _>("notnull") == 0 {
            return Err(sqlx::Error::Protocol(format!(
                "{table}.{} must be NOT NULL",
                expected.name
            )));
        }
        if expected.require_primary_key && row.get::<i64, _>("pk") == 0 {
            return Err(sqlx::Error::Protocol(format!(
                "{table}.{} must be part of the primary key",
                expected.name
            )));
        }
    }
    Ok(())
}

async fn table_exists(conn: &mut SqliteConnection, name: &str) -> StoreResult<bool> {
    let row = sqlx::query(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
         ) AS present",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.get::<i64, _>("present") != 0)
}

async fn column_exists(
    conn: &mut SqliteConnection,
    table: &str,
    column: &str,
) -> StoreResult<bool> {
    let statement = format!("PRAGMA table_info({table})");
    let rows = sqlx::query(&statement).fetch_all(&mut *conn).await?;
    Ok(rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column))
}

async fn schema_has_v2_extensions(conn: &mut SqliteConnection) -> StoreResult<bool> {
    for (table, column) in [
        ("jobs", "effective_spec_json"),
        ("jobs", "receipt_json"),
        ("events", "prev_hash"),
        ("events", "event_hash"),
        ("events", "hash_version"),
    ] {
        if column_exists(conn, table, column).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn validated_event_sequence_counter(
    conn: &mut SqliteConnection,
    events_table: &str,
) -> StoreResult<Option<i64>> {
    if !matches!(events_table, "events" | "events_legacy_v1") {
        return Err(sqlx::Error::InvalidArgument(
            "unexpected events table for sequence validation".to_string(),
        ));
    }
    let max_sequence: i64 = sqlx::query(&format!(
        "SELECT COALESCE(MAX(seq), 0) AS max_sequence FROM {events_table}"
    ))
    .fetch_one(&mut *conn)
    .await?
    .try_get("max_sequence")?;
    let rows = sqlx::query(
        "SELECT seq, typeof(seq) AS storage_type
         FROM sqlite_sequence WHERE name = ?1",
    )
    .bind(events_table)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() > 1 {
        return Err(sqlx::Error::Protocol(format!(
            "{events_table} has duplicate sqlite_sequence rows"
        )));
    }
    let Some(row) = rows.first() else {
        if max_sequence != 0 {
            return Err(sqlx::Error::Protocol(format!(
                "{events_table} has events but no AUTOINCREMENT counter"
            )));
        }
        return Ok(None);
    };
    if row.try_get::<String, _>("storage_type")? != "integer" {
        return Err(sqlx::Error::Protocol(format!(
            "{events_table} AUTOINCREMENT counter is not an integer"
        )));
    }
    let counter: i64 = row.try_get("seq")?;
    if counter < max_sequence || !(0..9223372036854775806).contains(&counter) {
        return Err(sqlx::Error::Protocol(format!(
            "{events_table} AUTOINCREMENT counter is below retained events, invalid, or exhausted"
        )));
    }
    Ok(Some(counter))
}

async fn raise_event_sequence_counter(
    conn: &mut SqliteConnection,
    events_table: &str,
    floor: i64,
) -> StoreResult<()> {
    if floor <= 0 {
        return Ok(());
    }
    match validated_event_sequence_counter(conn, events_table).await? {
        Some(current) if current >= floor => {}
        Some(_) => {
            let updated = sqlx::query("UPDATE sqlite_sequence SET seq = ?2 WHERE name = ?1")
                .bind(events_table)
                .bind(floor)
                .execute(&mut *conn)
                .await?;
            if updated.rows_affected() != 1 {
                return Err(sqlx::Error::Protocol(format!(
                    "failed to advance {events_table} AUTOINCREMENT counter"
                )));
            }
        }
        None => {
            sqlx::query("INSERT INTO sqlite_sequence(name, seq) VALUES (?1, ?2)")
                .bind(events_table)
                .bind(floor)
                .execute(&mut *conn)
                .await?;
        }
    }
    validated_event_sequence_counter(conn, events_table).await?;
    Ok(())
}

async fn validate_migration_history_rows(conn: &mut SqliteConnection) -> StoreResult<()> {
    let invalid: i64 = sqlx::query(
        "SELECT EXISTS(
             SELECT 1 FROM schema_migrations
             WHERE typeof(version) != 'integer' OR version <= 0
                OR typeof(applied_at_ms) != 'integer' OR applied_at_ms < 0
         ) AS invalid",
    )
    .fetch_one(&mut *conn)
    .await?
    .try_get("invalid")?;
    if invalid != 0 {
        return Err(sqlx::Error::Protocol(
            "schema_migrations contains invalid storage classes".to_string(),
        ));
    }
    Ok(())
}

async fn storage_guards_current(conn: &mut SqliteConnection) -> StoreResult<bool> {
    let rows = sqlx::query(
        "SELECT name, sql FROM sqlite_schema
         WHERE type = 'trigger' AND name IN (
             'coop_schema_migrations_storage_guard_insert',
             'coop_schema_migrations_storage_guard_update',
             'coop_jobs_storage_guard_insert',
             'coop_jobs_storage_guard_update',
             'coop_events_storage_guard_insert',
             'coop_events_storage_guard_update',
             'coop_events_sequence_guard_insert',
             'coop_jobs_validation_dirty_insert',
             'coop_jobs_validation_dirty_update',
             'coop_jobs_validation_dirty_delete',
             'coop_events_validation_dirty_insert',
             'coop_events_validation_dirty_update',
             'coop_events_validation_dirty_delete'
         )",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.len() == STORAGE_GUARD_NAMES.len()
        && rows.iter().all(|row| {
            row.try_get::<String, _>("sql")
                .is_ok_and(|sql| sql.contains(STORAGE_GUARD_REVISION_MARKER))
        }))
}

async fn validate_foreign_keys(conn: &mut SqliteConnection) -> StoreResult<()> {
    if sqlx::query("PRAGMA foreign_key_check")
        .fetch_optional(&mut *conn)
        .await?
        .is_some()
    {
        return Err(sqlx::Error::Protocol(
            "foreign-key validation failed".to_string(),
        ));
    }
    Ok(())
}

async fn create_storage_guard_triggers(conn: &mut SqliteConnection) -> StoreResult<()> {
    // These names are owned by Coop. Recreate them transactionally on every
    // open so a stale same-version definition cannot silently weaken newly
    // added invariants.
    for trigger in STORAGE_GUARD_NAMES {
        sqlx::query(&format!("DROP TRIGGER IF EXISTS {trigger}"))
            .execute(&mut *conn)
            .await?;
    }
    for statement in [
        "CREATE TRIGGER IF NOT EXISTS coop_schema_migrations_storage_guard_insert
         BEFORE INSERT ON schema_migrations
         WHEN typeof(NEW.version) != 'integer' OR NEW.version <= 0
           OR typeof(NEW.applied_at_ms) != 'integer' OR NEW.applied_at_ms < 0
         BEGIN
             SELECT RAISE(ABORT, 'invalid schema_migrations storage class [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_schema_migrations_storage_guard_update
         BEFORE UPDATE ON schema_migrations
         WHEN typeof(NEW.version) != 'integer' OR NEW.version <= 0
           OR typeof(NEW.applied_at_ms) != 'integer' OR NEW.applied_at_ms < 0
         BEGIN
             SELECT RAISE(ABORT, 'invalid schema_migrations storage class [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_jobs_storage_guard_insert
         BEFORE INSERT ON jobs
         WHEN typeof(NEW.job_id) != 'text' OR length(trim(NEW.job_id)) = 0
           OR typeof(NEW.tenant) != 'text' OR length(trim(NEW.tenant)) = 0
           OR typeof(NEW.language) != 'text' OR length(trim(NEW.language)) = 0
           OR typeof(NEW.status) != 'text'
           OR NEW.status NOT IN ('queued','running','succeeded','failed','timed_out','oom_killed','cancelled','error')
           OR typeof(NEW.spec_json) != 'text' OR NOT json_valid(NEW.spec_json)
           OR (NEW.effective_spec_json IS NOT NULL AND
               (typeof(NEW.effective_spec_json) != 'text' OR NOT json_valid(NEW.effective_spec_json)))
           OR (NEW.receipt_json IS NOT NULL AND
               (typeof(NEW.receipt_json) != 'text' OR NOT json_valid(NEW.receipt_json)))
           OR typeof(NEW.created_at_ms) != 'integer' OR NEW.created_at_ms < 0
           OR (NEW.started_at_ms IS NOT NULL AND
               (typeof(NEW.started_at_ms) != 'integer' OR NEW.started_at_ms < 0))
           OR (NEW.finished_at_ms IS NOT NULL AND
               (typeof(NEW.finished_at_ms) != 'integer' OR NEW.finished_at_ms < 0))
           OR (NEW.exit_code IS NOT NULL AND
               (typeof(NEW.exit_code) != 'integer' OR
                NEW.exit_code < -2147483648 OR NEW.exit_code > 2147483647))
           OR (NEW.status = 'queued' AND
               (NEW.started_at_ms IS NOT NULL OR NEW.finished_at_ms IS NOT NULL))
           OR (NEW.status = 'running' AND
               (NEW.started_at_ms IS NULL OR NEW.finished_at_ms IS NOT NULL))
           OR (NEW.status NOT IN ('queued','running') AND NEW.finished_at_ms IS NULL)
         BEGIN
             SELECT RAISE(ABORT, 'invalid jobs storage class [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_jobs_storage_guard_update
         BEFORE UPDATE ON jobs
         WHEN typeof(NEW.job_id) != 'text' OR length(trim(NEW.job_id)) = 0
           OR typeof(NEW.tenant) != 'text' OR length(trim(NEW.tenant)) = 0
           OR typeof(NEW.language) != 'text' OR length(trim(NEW.language)) = 0
           OR typeof(NEW.status) != 'text'
           OR NEW.status NOT IN ('queued','running','succeeded','failed','timed_out','oom_killed','cancelled','error')
           OR typeof(NEW.spec_json) != 'text' OR NOT json_valid(NEW.spec_json)
           OR (NEW.effective_spec_json IS NOT NULL AND
               (typeof(NEW.effective_spec_json) != 'text' OR NOT json_valid(NEW.effective_spec_json)))
           OR (NEW.receipt_json IS NOT NULL AND
               (typeof(NEW.receipt_json) != 'text' OR NOT json_valid(NEW.receipt_json)))
           OR typeof(NEW.created_at_ms) != 'integer' OR NEW.created_at_ms < 0
           OR (NEW.started_at_ms IS NOT NULL AND
               (typeof(NEW.started_at_ms) != 'integer' OR NEW.started_at_ms < 0))
           OR (NEW.finished_at_ms IS NOT NULL AND
               (typeof(NEW.finished_at_ms) != 'integer' OR NEW.finished_at_ms < 0))
           OR (NEW.exit_code IS NOT NULL AND
               (typeof(NEW.exit_code) != 'integer' OR
                NEW.exit_code < -2147483648 OR NEW.exit_code > 2147483647))
           OR (NEW.status = 'queued' AND
               (NEW.started_at_ms IS NOT NULL OR NEW.finished_at_ms IS NOT NULL))
           OR (NEW.status = 'running' AND
               (NEW.started_at_ms IS NULL OR NEW.finished_at_ms IS NOT NULL))
           OR (NEW.status NOT IN ('queued','running') AND NEW.finished_at_ms IS NULL)
         BEGIN
             SELECT RAISE(ABORT, 'invalid jobs storage class [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_events_storage_guard_insert
         BEFORE INSERT ON events
         WHEN typeof(NEW.job_id) != 'text'
           OR typeof(NEW.ts_ms) != 'integer' OR NEW.ts_ms < 0
           OR typeof(NEW.kind) != 'text' OR length(trim(NEW.kind)) = 0
           OR typeof(NEW.data_json) != 'text' OR NOT json_valid(NEW.data_json)
           OR typeof(NEW.prev_hash) != 'text'
           OR typeof(NEW.event_hash) != 'text'
           OR typeof(NEW.hash_version) != 'integer' OR NEW.hash_version NOT IN (0, 1)
           OR (NEW.hash_version = 1 AND length(NEW.event_hash) != 64)
         BEGIN
             SELECT RAISE(ABORT, 'invalid events storage class [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_events_storage_guard_update
         BEFORE UPDATE ON events
         WHEN typeof(NEW.seq) != 'integer'
           OR NEW.seq <= 0 OR NEW.seq >= 9223372036854775807
           OR typeof(NEW.job_id) != 'text'
           OR typeof(NEW.ts_ms) != 'integer' OR NEW.ts_ms < 0
           OR typeof(NEW.kind) != 'text' OR length(trim(NEW.kind)) = 0
           OR typeof(NEW.data_json) != 'text' OR NOT json_valid(NEW.data_json)
           OR typeof(NEW.prev_hash) != 'text'
           OR typeof(NEW.event_hash) != 'text'
           OR typeof(NEW.hash_version) != 'integer' OR NEW.hash_version NOT IN (0, 1)
           OR (NEW.hash_version = 1 AND length(NEW.event_hash) != 64)
         BEGIN
             SELECT RAISE(ABORT, 'invalid events storage class [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_events_sequence_guard_insert
         AFTER INSERT ON events
         WHEN typeof(NEW.seq) != 'integer'
           OR NEW.seq <= 0 OR NEW.seq >= 9223372036854775807
         BEGIN
             SELECT RAISE(ABORT, 'invalid event sequence [coop-storage-guard-r1]');
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_jobs_validation_dirty_insert
         AFTER INSERT ON jobs BEGIN
             UPDATE store_integrity SET row_validation_revision = 0
             WHERE singleton = 1
               AND row_validation_revision != 2
               AND 'coop-storage-guard-r1' = 'coop-storage-guard-r1';
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_jobs_validation_dirty_update
         AFTER UPDATE ON jobs BEGIN
             UPDATE store_integrity SET row_validation_revision = 0
             WHERE singleton = 1
               AND row_validation_revision != 2
               AND 'coop-storage-guard-r1' = 'coop-storage-guard-r1';
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_jobs_validation_dirty_delete
         AFTER DELETE ON jobs BEGIN
             UPDATE store_integrity SET row_validation_revision = 0
             WHERE singleton = 1
               AND row_validation_revision != 2
               AND 'coop-storage-guard-r1' = 'coop-storage-guard-r1';
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_events_validation_dirty_insert
         AFTER INSERT ON events BEGIN
             UPDATE store_integrity SET row_validation_revision = 0
             WHERE singleton = 1
               AND row_validation_revision != 2
               AND 'coop-storage-guard-r1' = 'coop-storage-guard-r1';
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_events_validation_dirty_update
         AFTER UPDATE ON events BEGIN
             UPDATE store_integrity SET row_validation_revision = 0
             WHERE singleton = 1
               AND row_validation_revision != 2
               AND 'coop-storage-guard-r1' = 'coop-storage-guard-r1';
         END",
        "CREATE TRIGGER IF NOT EXISTS coop_events_validation_dirty_delete
         AFTER DELETE ON events BEGIN
             UPDATE store_integrity SET row_validation_revision = 0
             WHERE singleton = 1
               AND row_validation_revision != 2
               AND 'coop-storage-guard-r1' = 'coop-storage-guard-r1';
         END",
    ] {
        sqlx::query(statement).execute(&mut *conn).await?;
    }
    Ok(())
}

async fn record_migration(conn: &mut SqliteConnection, version: i64) -> StoreResult<()> {
    sqlx::query("INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (?1, ?2)")
        .bind(version)
        .bind(now_ms())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn checkpoint_wal(pool: &SqlitePool) -> StoreResult<()> {
    let row = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await?;
    let busy: i64 = row.try_get(0)?;
    if busy != 0 {
        return Err(sqlx::Error::Protocol(
            "WAL checkpoint could not complete because the database is busy".to_string(),
        ));
    }
    Ok(())
}

async fn create_jobs_table(conn: &mut SqliteConnection, table: &str) -> StoreResult<()> {
    let statement = format!(
        "CREATE TABLE {table} (
            job_id TEXT PRIMARY KEY NOT NULL
                CHECK (typeof(job_id) = 'text' AND length(trim(job_id)) > 0),
            tenant TEXT NOT NULL
                CHECK (typeof(tenant) = 'text' AND length(trim(tenant)) > 0),
            language TEXT NOT NULL
                CHECK (typeof(language) = 'text' AND length(trim(language)) > 0),
            status TEXT NOT NULL DEFAULT 'queued'
                CHECK (typeof(status) = 'text' AND status IN ('queued','running','succeeded','failed','timed_out','oom_killed','cancelled','error')),
            spec_json TEXT NOT NULL
                CHECK (typeof(spec_json) = 'text' AND json_valid(spec_json)),
            effective_spec_json TEXT
                CHECK (effective_spec_json IS NULL OR (typeof(effective_spec_json) = 'text' AND json_valid(effective_spec_json))),
            receipt_json TEXT
                CHECK (receipt_json IS NULL OR (typeof(receipt_json) = 'text' AND json_valid(receipt_json))),
            created_at_ms INTEGER NOT NULL
                CHECK (typeof(created_at_ms) = 'integer' AND created_at_ms >= 0),
            started_at_ms INTEGER
                CHECK (started_at_ms IS NULL OR (typeof(started_at_ms) = 'integer' AND started_at_ms >= 0)),
            finished_at_ms INTEGER
                CHECK (finished_at_ms IS NULL OR (typeof(finished_at_ms) = 'integer' AND finished_at_ms >= 0)),
            exit_code INTEGER CHECK (
                exit_code IS NULL OR
                (typeof(exit_code) = 'integer' AND
                 exit_code BETWEEN -2147483648 AND 2147483647)
            ),
            CHECK (status != 'queued' OR (started_at_ms IS NULL AND finished_at_ms IS NULL)),
            CHECK (status != 'running' OR (started_at_ms IS NOT NULL AND finished_at_ms IS NULL)),
            CHECK (status IN ('queued','running') OR finished_at_ms IS NOT NULL)
        )"
    );
    sqlx::query(&statement).execute(&mut *conn).await?;
    Ok(())
}

async fn create_events_table(
    conn: &mut SqliteConnection,
    table: &str,
    jobs_table: &str,
) -> StoreResult<()> {
    let statement = format!(
        "CREATE TABLE {table} (
            seq INTEGER PRIMARY KEY AUTOINCREMENT
                CHECK (
                    typeof(seq) = 'integer' AND
                    seq > 0 AND seq < 9223372036854775807
                ),
            job_id TEXT NOT NULL REFERENCES {jobs_table}(job_id) ON DELETE CASCADE
                CHECK (typeof(job_id) = 'text'),
            ts_ms INTEGER NOT NULL
                CHECK (typeof(ts_ms) = 'integer' AND ts_ms >= 0),
            kind TEXT NOT NULL
                CHECK (typeof(kind) = 'text' AND length(trim(kind)) > 0),
            data_json TEXT NOT NULL
                CHECK (typeof(data_json) = 'text' AND json_valid(data_json)),
            prev_hash TEXT NOT NULL DEFAULT '' CHECK (typeof(prev_hash) = 'text'),
            event_hash TEXT NOT NULL DEFAULT '' CHECK (typeof(event_hash) = 'text'),
            hash_version INTEGER NOT NULL DEFAULT 0
                CHECK (typeof(hash_version) = 'integer' AND hash_version IN (0, 1)),
            CHECK (hash_version = 0 OR length(event_hash) = 64)
        )"
    );
    sqlx::query(&statement).execute(&mut *conn).await?;
    Ok(())
}

async fn append_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
    kind: &str,
    data: &Value,
    ts_ms: i64,
) -> StoreResult<EventRow> {
    let prev_hash = previous_event_hash_tx(tx, job_id).await?;
    insert_hashed_event_tx(tx, job_id, kind, data, ts_ms, &prev_hash).await
}

async fn previous_event_hash_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
) -> StoreResult<String> {
    let prior = sqlx::query(
        "SELECT event_hash, hash_version FROM events
         WHERE job_id = ?1 ORDER BY seq DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match prior {
        Some(row) if row.get::<i64, _>("hash_version") == 1 => row.get("event_hash"),
        _ => String::new(),
    })
}

async fn insert_hashed_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
    kind: &str,
    data: &Value,
    ts_ms: i64,
    prev_hash: &str,
) -> StoreResult<EventRow> {
    let data_json = canonical_json(data);
    // Insert as version zero so the schema accepts the temporary empty hash;
    // the row cannot escape the transaction before it is upgraded to v1.
    let inserted = sqlx::query(
        "INSERT INTO events (
            job_id, ts_ms, kind, data_json, prev_hash, event_hash, hash_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, '', 0)
         RETURNING seq",
    )
    .bind(job_id)
    .bind(ts_ms)
    .bind(kind)
    .bind(&data_json)
    .bind(prev_hash)
    .fetch_one(&mut **tx)
    .await?;
    let seq: i64 = inserted.get("seq");
    let event_hash = compute_event_hash(job_id, prev_hash, seq, ts_ms, kind, data);
    sqlx::query("UPDATE events SET event_hash = ?2, hash_version = 1 WHERE seq = ?1")
        .bind(seq)
        .bind(&event_hash)
        .execute(&mut **tx)
        .await?;

    Ok(EventRow {
        seq,
        ts_ms,
        kind: kind.to_string(),
        data: data.clone(),
        prev_hash: prev_hash.to_string(),
        event_hash,
        hash_version: 1,
    })
}

async fn event_chain_head_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
) -> StoreResult<EventChainHead> {
    let row = sqlx::query(
        "SELECT
            COUNT(*) AS event_count,
            COALESCE(SUM(CASE WHEN hash_version = 1 THEN 1 ELSE 0 END), 0) AS verified_count,
            COALESCE(SUM(CASE WHEN hash_version = 0 THEN 1 ELSE 0 END), 0) AS legacy_count,
            (
                SELECT CASE WHEN hash_version = 1 THEN event_hash ELSE NULL END
                FROM events AS latest
                WHERE latest.job_id = ?1
                  AND NOT EXISTS (
                      SELECT 1 FROM retention_tombstones
                      WHERE retention_tombstones.job_id = latest.job_id
                  )
                ORDER BY seq DESC LIMIT 1
            ) AS head_hash
         FROM events
         WHERE job_id = ?1
           AND NOT EXISTS (
               SELECT 1 FROM retention_tombstones
               WHERE retention_tombstones.job_id = events.job_id
           )",
    )
    .bind(job_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(EventChainHead {
        event_count: row.get("event_count"),
        verified_event_count: row.get("verified_count"),
        legacy_event_count: row.get("legacy_count"),
        head_hash: row.get("head_hash"),
    })
}

fn recovery_receipt_details(
    requested_spec_json: &str,
    _effective_spec_json: Option<&str>,
    created_at_ms: i64,
    started_at_ms: Option<i64>,
) -> Value {
    let requested = serde_json::from_str::<Value>(requested_spec_json).unwrap_or(Value::Null);
    let mut details = Map::from_iter([
        ("terminal_reason".to_string(), json!("server_restarted")),
        ("killed_by".to_string(), json!("server_restarted")),
        ("created_at_ms".to_string(), json!(created_at_ms)),
        ("started_at_ms".to_string(), json!(started_at_ms)),
        ("evidence_complete".to_string(), json!(false)),
    ]);

    // Static request evidence remains knowable after a restart. Only include
    // members whose stored value has the SDK's documented non-null type;
    // execution/output observations that were lost with the process are
    // omitted rather than fabricated as nulls or zeroes.
    if let Some(limits @ Value::Object(_)) = requested.get("limits") {
        details.insert("requested_limits".to_string(), limits.clone());
    }
    // The configured execution plan survived, but whether its controls ever
    // became active did not. Recovery therefore preserves requested policy
    // only and deliberately omits effective limits/enforcement provenance.
    if let Some(code) = requested.get("code").and_then(Value::as_str) {
        details.insert(
            "code_sha256".to_string(),
            json!(sha256_hex(code.as_bytes())),
        );
    }
    if let Some(object) = requested.as_object() {
        let stdin = object.get("stdin").and_then(Value::as_str).unwrap_or("");
        details.insert(
            "stdin_sha256".to_string(),
            json!(sha256_hex(stdin.as_bytes())),
        );
    }

    Value::Object(details)
}

struct ReceiptCore<'a> {
    job_id: &'a str,
    status: &'a str,
    exit_code: Option<i32>,
    created_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: i64,
    duration_ms: i64,
}

fn receipt_with_chain(
    receipt: Option<&Value>,
    chain: &EventChainHead,
    core: ReceiptCore<'_>,
) -> String {
    let mut object = match receipt {
        Some(Value::Object(object)) => object.clone(),
        Some(other) => {
            let mut object = Map::new();
            object.insert("details".to_string(), other.clone());
            object
        }
        None => Map::new(),
    };
    // Never incorporate a caller-supplied digest into the digest input.
    object.remove("receipt_sha256");
    // These values come from the same transaction as the job row and final
    // event. Overwrite caller estimates so an exported receipt always agrees
    // exactly with the durable evidence.
    object.insert("version".to_string(), json!(1));
    object.insert("job_id".to_string(), json!(core.job_id));
    object.insert("outcome".to_string(), json!(core.status));
    object.insert("exit_code".to_string(), json!(core.exit_code));
    object.insert("created_at_ms".to_string(), json!(core.created_at_ms));
    object.insert("started_at_ms".to_string(), json!(core.started_at_ms));
    object.insert("finished_at_ms".to_string(), json!(core.finished_at_ms));
    object.insert("duration_ms".to_string(), json!(core.duration_ms.max(0)));
    object.insert(
        "event_chain".to_string(),
        json!({
            "version": 1,
            "head": chain.head_hash,
            "events": chain.event_count,
            "event_count": chain.event_count,
            "verified_events": chain.verified_event_count,
            "legacy_events": chain.legacy_event_count,
            "complete": chain.legacy_event_count == 0
                && chain.verified_event_count == chain.event_count,
        }),
    );
    let digest = compute_receipt_sha256(&Value::Object(object.clone()));
    object.insert("receipt_sha256".to_string(), Value::String(digest));
    canonical_json(&Value::Object(object))
}

fn row_to_event(row: sqlx::sqlite::SqliteRow) -> StoreResult<EventRow> {
    let data_json: String = row.try_get("data_json")?;
    let data =
        serde_json::from_str(&data_json).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    Ok(EventRow {
        seq: row.try_get("seq")?,
        ts_ms: row.try_get("ts_ms")?,
        kind: row.try_get("kind")?,
        data,
        prev_hash: row.try_get("prev_hash")?,
        event_hash: row.try_get("event_hash")?,
        hash_version: row.try_get("hash_version")?,
    })
}

fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "succeeded" | "failed" | "timed_out" | "oom_killed" | "cancelled" | "error"
    )
}

/// Canonical JSON used for receipt persistence and event hashing. Object keys
/// are sorted lexicographically regardless of serde_json's map backend.
pub fn canonical_json(value: &Value) -> String {
    let mut output = String::new();
    write_canonical_json(value, &mut output);
    output
}

/// Compute the receipt's portable integrity digest. The `receipt_sha256`
/// member itself is excluded, allowing callers to verify a stored receipt by
/// parsing it and comparing this result with the member. This is an integrity
/// checksum, not a signature or proof against a database administrator.
pub fn compute_receipt_sha256(receipt: &Value) -> String {
    let unsigned = match receipt {
        Value::Object(object) => {
            let mut object = object.clone();
            object.remove("receipt_sha256");
            Value::Object(object)
        }
        other => other.clone(),
    };
    let canonical = canonical_json(&unsigned);
    sha256_hex(canonical.as_bytes())
}

fn write_canonical_json(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&value.to_string()),
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).expect("JSON strings always serialize"));
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical_json(value, output);
            }
            output.push(']');
        }
        Value::Object(object) => {
            output.push('{');
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(
                    &serde_json::to_string(key).expect("JSON object keys always serialize"),
                );
                output.push(':');
                write_canonical_json(&object[key], output);
            }
            output.push('}');
        }
    }
}

/// v1 digest input is length-delimited and architecture-independent:
/// domain separator, job id, previous hash, signed big-endian seq/time, event
/// kind, and canonical JSON data.
fn compute_event_hash(
    job_id: &str,
    prev_hash: &str,
    seq: i64,
    ts_ms: i64,
    kind: &str,
    data: &Value,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"coop:event-chain:v1\0");
    hash_field(&mut hasher, job_id.as_bytes());
    hash_field(&mut hasher, prev_hash.as_bytes());
    hasher.update(seq.to_be_bytes());
    hasher.update(ts_ms.to_be_bytes());
    hash_field(&mut hasher, kind.as_bytes());
    let data = canonical_json(data);
    hash_field(&mut hasher, data.as_bytes());
    digest_hex(hasher.finalize())
}

fn sha256_hex(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    digest_hex(hasher.finalize())
}

fn digest_hex(digest: impl IntoIterator<Item = u8>) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn hash_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update((field.len() as u64).to_be_bytes());
    hasher.update(field);
}

#[cfg(unix)]
fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn prepare_storage_path(path: &Path) -> StoreResult<()> {
    if path.as_os_str().is_empty() {
        return Err(sqlx::Error::InvalidArgument(
            "database path must not be empty".to_string(),
        ));
    }
    reject_symlink_components(path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_file() {
            return Err(sqlx::Error::InvalidArgument(
                "database path must be a regular file".to_string(),
            ));
        }
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
        reject_symlink_components(parent)?;
        harden_storage_directory(parent)?;
    }

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
    }
    // Tighten pre-existing database and sidecar files before SQLite reads or
    // migrates any tenant data, not only after opening the pool.
    harden_storage_files(path)?;
    Ok(())
}

fn reject_symlink_components(path: &Path) -> StoreResult<()> {
    for component in path.ancestors() {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                #[cfg(target_os = "macos")]
                if is_trusted_macos_system_alias(component) {
                    continue;
                }
                return Err(sqlx::Error::InvalidArgument(format!(
                    "database path contains a symbolic link: {}",
                    component.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_trusted_macos_system_alias(path: &Path) -> bool {
    let expected = match path.to_str() {
        Some("/var") => "/private/var",
        Some("/tmp") => "/private/tmp",
        Some("/etc") => "/private/etc",
        _ => return false,
    };
    path.canonicalize()
        .is_ok_and(|target| target == Path::new(expected))
}

#[cfg(unix)]
fn harden_storage_directory(path: &Path) -> StoreResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let canonical = std::fs::canonicalize(path)?;
    let canonical_temp_dir = std::fs::canonicalize(std::env::temp_dir()).ok();
    match storage_parent_policy(&canonical, canonical_temp_dir.as_deref()) {
        StorageParentPolicy::PreserveSharedTemp => return Ok(()),
        StorageParentPolicy::RejectBroad => {
            return Err(sqlx::Error::InvalidArgument(format!(
                "database parent must be a dedicated directory, not {}",
                canonical.display()
            )))
        }
        StorageParentPolicy::HardenDedicated => {}
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageParentPolicy {
    PreserveSharedTemp,
    RejectBroad,
    HardenDedicated,
}

#[cfg(unix)]
fn storage_parent_policy(path: &Path, canonical_temp_dir: Option<&Path>) -> StorageParentPolicy {
    // Test harnesses and callers may intentionally put the 0600 database file
    // directly in the platform temp directory. Preserve its directory mode;
    // dedicated children are still hardened below.
    if canonical_temp_dir == Some(path) {
        return StorageParentPolicy::PreserveSharedTemp;
    }
    // Refuse other broad paths rather than changing their permissions. Include
    // macOS canonical targets as well as public aliases: trusting `/var` during
    // component traversal must never make `/private/var` eligible for chmod.
    if [
        "/",
        "/Applications",
        "/Library",
        "/Network",
        "/System",
        "/Users",
        "/Volumes",
        "/app",
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/home",
        "/lib",
        "/lib64",
        "/media",
        "/mnt",
        "/opt",
        "/proc",
        "/private",
        "/private/etc",
        "/private/tmp",
        "/private/var",
        "/private/var/lib",
        "/private/var/tmp",
        "/root",
        "/run",
        "/sbin",
        "/srv",
        "/sys",
        "/tmp",
        "/usr",
        "/var",
        "/var/lib",
        "/var/tmp",
        "/workspace",
    ]
    .iter()
    .any(|broad| path == Path::new(broad))
    {
        StorageParentPolicy::RejectBroad
    } else {
        StorageParentPolicy::HardenDedicated
    }
}

#[cfg(not(unix))]
fn harden_storage_directory(_path: &Path) -> StoreResult<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_storage_files(path: &Path) -> StoreResult<()> {
    use std::os::unix::fs::PermissionsExt;

    for candidate in [
        path.to_path_buf(),
        sidecar_path(path, "-wal"),
        sidecar_path(path, "-shm"),
    ] {
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::set_permissions(candidate, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(_) => {
                return Err(sqlx::Error::InvalidArgument(format!(
                    "database or sidecar path is not a regular file: {}",
                    candidate.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_storage_files(_path: &Path) -> StoreResult<()> {
    Ok(())
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod query_plan_tests {
    use super::*;

    #[test]
    fn tenant_list_variants_use_composite_keyset_indexes_without_temp_sort() {
        sqlx::test_block_on(async {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let db = std::env::temp_dir()
                .join(format!(
                    "coop-store-list-plan-{}-{nonce}",
                    std::process::id()
                ))
                .join("coop.db");
            let store = Store::open(&db).await.unwrap();
            sqlx::query(
                "WITH RECURSIVE sequence(n) AS (
                     VALUES(1) UNION ALL SELECT n + 1 FROM sequence WHERE n < 8192
                 )
                 INSERT INTO jobs(
                     job_id, tenant, language, status, spec_json,
                     created_at_ms, finished_at_ms
                 )
                 SELECT printf('plan-%05d', n),
                        CASE n % 2 WHEN 0 THEN 'tenant-a' ELSE 'tenant-b' END,
                        CASE (n / 2) % 2 WHEN 0 THEN 'python' ELSE 'node' END,
                        CASE (n / 4) % 2 WHEN 0 THEN 'queued' ELSE 'succeeded' END,
                        '{}', n,
                        CASE (n / 4) % 2 WHEN 0 THEN NULL ELSE n END
                 FROM sequence",
            )
            .execute(&store.pool)
            .await
            .unwrap();
            sqlx::query("ANALYZE").execute(&store.pool).await.unwrap();

            let cases = [
                (
                    ListJobsQuery {
                        tenant: Some("tenant-a".to_string()),
                        before: Some(JobCursor {
                            created_at_ms: 7000,
                            job_id: "plan-99999".to_string(),
                        }),
                        ..ListJobsQuery::default()
                    },
                    "idx_jobs_tenant_created_summary",
                ),
                (
                    ListJobsQuery {
                        tenant: Some("tenant-a".to_string()),
                        status: Some("queued".to_string()),
                        before: Some(JobCursor {
                            created_at_ms: 7000,
                            job_id: "plan-99999".to_string(),
                        }),
                        ..ListJobsQuery::default()
                    },
                    "idx_jobs_tenant_status_created_summary",
                ),
                (
                    ListJobsQuery {
                        tenant: Some("tenant-a".to_string()),
                        language: Some("python".to_string()),
                        before: Some(JobCursor {
                            created_at_ms: 7000,
                            job_id: "plan-99999".to_string(),
                        }),
                        ..ListJobsQuery::default()
                    },
                    "idx_jobs_tenant_language_created_summary",
                ),
                (
                    ListJobsQuery {
                        tenant: Some("tenant-a".to_string()),
                        status: Some("queued".to_string()),
                        language: Some("python".to_string()),
                        before: Some(JobCursor {
                            created_at_ms: 7000,
                            job_id: "plan-99999".to_string(),
                        }),
                        ..ListJobsQuery::default()
                    },
                    "idx_jobs_tenant_status_language_created_summary",
                ),
            ];

            for (query, expected_index) in cases {
                let mut statement =
                    build_job_list_query("EXPLAIN QUERY PLAN ", JOB_SUMMARY_PROJECTION, &query);
                let plan = statement
                    .build()
                    .fetch_all(&store.pool)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.get::<String, _>("detail"))
                    .collect::<Vec<_>>();
                assert!(
                    plan.iter()
                        .any(|detail| detail
                            .contains(&format!("USING COVERING INDEX {expected_index}"))),
                    "expected covering {expected_index} in plan: {plan:?}"
                );
                assert!(
                    !plan.iter().any(|detail| detail.contains("SCAN jobs")),
                    "unexpected jobs scan: {plan:?}"
                );
                assert!(
                    !plan.iter().any(|detail| detail.contains("USE TEMP B-TREE")),
                    "unexpected temp sort: {plan:?}"
                );
            }

            let recovery_cursor = JobCursor {
                created_at_ms: 2000,
                job_id: "plan-02000".to_string(),
            };
            let mut statement = build_queued_jobs_query(
                "EXPLAIN QUERY PLAN ",
                Some(&recovery_cursor),
                MAX_RECOVERY_PAGE,
            );
            let plan = statement
                .build()
                .fetch_all(&store.pool)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.get::<String, _>("detail"))
                .collect::<Vec<_>>();
            assert!(
                plan.iter().any(|detail| detail
                    .contains("USING COVERING INDEX idx_jobs_status_created_recovery")),
                "expected covering recovery index in plan: {plan:?}"
            );
            assert!(
                !plan.iter().any(
                    |detail| detail.contains("SCAN jobs") || detail.contains("USE TEMP B-TREE")
                ),
                "unexpected recovery scan/sort: {plan:?}"
            );

            let point_plan = sqlx::query(
                "EXPLAIN QUERY PLAN
                 SELECT job_id, tenant, language, status, created_at_ms,
                        started_at_ms, finished_at_ms, exit_code
                 FROM jobs INDEXED BY idx_jobs_id_summary
                 WHERE job_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM retention_tombstones
                       WHERE retention_tombstones.job_id = jobs.job_id
                   )",
            )
            .bind("plan-00001")
            .fetch_all(&store.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>();
            assert!(
                point_plan
                    .iter()
                    .any(|detail| detail.contains("USING COVERING INDEX idx_jobs_id_summary")),
                "expected covering point-lookup index in plan: {point_plan:?}"
            );
            assert!(
                !point_plan.iter().any(
                    |detail| detail.contains("SCAN jobs") || detail.contains("USE TEMP B-TREE")
                ),
                "unexpected point-lookup scan/sort: {point_plan:?}"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn storage_parent_policy_never_hardens_shared_or_system_roots() {
        use std::os::unix::fs::PermissionsExt;

        let canonical_temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(
            storage_parent_policy(&canonical_temp, Some(&canonical_temp)),
            StorageParentPolicy::PreserveSharedTemp
        );
        let before = std::fs::metadata(&canonical_temp)
            .unwrap()
            .permissions()
            .mode();
        harden_storage_directory(&canonical_temp).unwrap();
        let after = std::fs::metadata(&canonical_temp)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(after, before, "shared temp directory mode changed");

        for forbidden in ["/var", "/var/lib", "/var/tmp", "/private"] {
            let forbidden = Path::new(forbidden);
            let Ok(canonical) = std::fs::canonicalize(forbidden) else {
                continue;
            };
            if canonical == canonical_temp {
                continue;
            }
            let before = std::fs::metadata(forbidden).unwrap().permissions().mode();
            assert!(harden_storage_directory(forbidden).is_err());
            let after = std::fs::metadata(forbidden).unwrap().permissions().mode();
            assert_eq!(
                after, before,
                "forbidden parent mode changed: {canonical:?}"
            );
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dedicated = canonical_temp.join(format!(
            "coop-store-parent-policy-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dedicated).unwrap();
        std::fs::set_permissions(&dedicated, std::fs::Permissions::from_mode(0o755)).unwrap();
        harden_storage_directory(&dedicated).unwrap();
        assert_eq!(
            std::fs::metadata(&dedicated).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir(&dedicated).unwrap();

        for broad in [
            "/var",
            "/var/lib",
            "/var/tmp",
            "/private",
            "/private/var",
            "/private/var/lib",
            "/private/var/tmp",
            "/private/tmp",
            "/Applications",
            "/Library",
            "/Network",
            "/System",
            "/Users",
            "/Volumes",
        ] {
            let path = Path::new(broad);
            assert_eq!(
                storage_parent_policy(path, None),
                StorageParentPolicy::RejectBroad,
                "broad parent was not rejected: {broad}"
            );
        }

        let dedicated = canonical_temp.join("coop-store-dedicated-child");
        assert_eq!(
            storage_parent_policy(&dedicated, Some(&canonical_temp)),
            StorageParentPolicy::HardenDedicated
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_alias_allowlist_is_exact_and_target_verified() {
        for alias in ["/var", "/tmp", "/etc"] {
            assert!(is_trusted_macos_system_alias(Path::new(alias)), "{alias}");
        }
        for untrusted in ["/private", "/var/lib", "/Users", "/usr"] {
            assert!(
                !is_trusted_macos_system_alias(Path::new(untrusted)),
                "{untrusted}"
            );
        }
    }
}
