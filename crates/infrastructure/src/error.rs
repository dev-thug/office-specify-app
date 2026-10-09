//! Error translation into `AppError`. `From` impls for foreign error types
//! would violate orphan rules (both sides are foreign to this crate), so
//! mapping is explicit.

use contracts::{AppError, ErrorCode};

/// SQLx error → AppError. Constraint-specific handling (e.g. unique violation
/// → 409) is the caller's job when a narrower code is needed — inspect
/// `db_constraint(e)`.
pub fn map_sqlx(err: sqlx::Error) -> AppError {
    match &err {
        sqlx::Error::Database(db) => {
            let code = db.code().unwrap_or_default();
            match code.as_ref() {
                "23505" => AppError::conflict("a conflicting record already exists"),
                "23503" => AppError::conflict("referenced record does not exist"),
                "23514" | "23502" => {
                    // RAISE'd guard messages are authored domain text; a
                    // postgres-authored CHECK/NOT NULL message leaks table,
                    // column and constraint names — sanitize it (R23).
                    let authored = !db.message().contains("constraint")
                        && !db.message().contains("column")
                        && !db.message().contains("relation");
                    let detail = if authored {
                        db.message().to_string()
                    } else {
                        tracing::warn!(sqlstate = %code, "constraint violation");
                        "the request violates a data constraint".to_string()
                    };
                    AppError::new(ErrorCode::ValidationFailed, detail).with_status(422)
                }
                "40001" | "40P01" => AppError::new(
                    ErrorCode::Conflict,
                    "concurrent modification; retry the request",
                )
                .with_status(409),
                _ => AppError::internal(format!("database error {code}")),
            }
        }
        sqlx::Error::RowNotFound => AppError::not_found("record not found"),
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
            AppError::dependency_unavailable("database unavailable")
        }
        _ => AppError::internal("database error"),
    }
}

/// Constraint name of a database error, if present.
pub fn db_constraint(err: &sqlx::Error) -> Option<String> {
    match err {
        sqlx::Error::Database(db) => db.constraint().map(str::to_string),
        _ => None,
    }
}

/// true when `err` is a unique-constraint violation on `constraint`.
pub fn is_unique_violation_on(err: &sqlx::Error, constraint: &str) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505")
        && db.constraint() == Some(constraint))
}

pub fn map_redis(err: redis::RedisError) -> AppError {
    // The redis error text can contain server-provided detail; keep the
    // message generic — dependency name only.
    let _ = &err;
    AppError::dependency_unavailable("redis unavailable")
}
