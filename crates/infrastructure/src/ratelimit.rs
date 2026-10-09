//! Fixed-window rate limiting on Redis (spec §16.3).
//!
//! Fail modes are per-caller: `Closed` denies the action when Redis is
//! unreachable (public booking/unsubscribe, login, sending) while `Open`
//! lets the request through (CRM reads). `Closed` maps to
//! `DependencyUnavailable` — the caller was not actually limited, the
//! limiter itself is down.

use std::time::Duration;

use contracts::{AppError, ErrorCode};
use redis::AsyncCommands;

use crate::error::map_redis;
use crate::redis::RedisClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailMode {
    Closed,
    Open,
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub allowed: bool,
    /// Seconds until the current window ends.
    pub retry_after: Option<u32>,
    pub remaining: u32,
}

#[derive(Clone)]
pub struct RateLimiter {
    redis: RedisClient,
}

impl RateLimiter {
    pub fn new(redis: RedisClient) -> Self {
        Self { redis }
    }

    /// Fixed-window check. `key` should include the dimension already, e.g.
    /// `login:{ip}` or `send:{workspace_id}:{mailbox_id}`.
    pub async fn check(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
        fail: FailMode,
    ) -> Result<Decision, AppError> {
        let rkey = format!("rl:{key}:{}", window.as_secs());
        let mut conn = self.redis.conn();
        let result: redis::RedisResult<(i64, i64)> = redis::pipe()
            .atomic()
            .incr(&rkey, 1)
            .expire(&rkey, window.as_secs() as i64)
            .query_async(&mut conn)
            .await;
        match result {
            Ok((count, _)) => {
                let ttl: i64 = conn.ttl(&rkey).await.unwrap_or(window.as_secs() as i64);
                let allowed = count <= i64::from(limit);
                Ok(Decision {
                    allowed,
                    retry_after: (!allowed).then(|| ttl.max(0) as u32),
                    remaining: (limit as i64 - count).max(0) as u32,
                })
            }
            Err(e) => match fail {
                FailMode::Open => Ok(Decision {
                    allowed: true,
                    retry_after: None,
                    remaining: limit,
                }),
                FailMode::Closed => Err(map_redis(e)),
            },
        }
    }

    /// Like `check` but produces the 429 AppError directly.
    pub async fn require(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
        fail: FailMode,
    ) -> Result<Decision, AppError> {
        let d = self.check(key, limit, window, fail).await?;
        if d.allowed {
            Ok(d)
        } else {
            Err(AppError::new(ErrorCode::RateLimited, "rate limit exceeded")
                .with_retry_after(d.retry_after.unwrap_or(1)))
        }
    }
}
