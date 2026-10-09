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
        // True fixed window (R6): INCR, then PEXPIRE NX so the TTL is set
        // only on the first hit — later hits never stretch the window.
        let result: redis::RedisResult<(i64, i64)> = redis::pipe()
            .atomic()
            .incr(&rkey, 1)
            .cmd("PEXPIRE")
            .arg(&rkey)
            .arg(window.as_millis() as i64)
            .arg("NX")
            .query_async(&mut conn)
            .await;
        match result {
            Ok((count, _)) => {
                let ttl: i64 = conn
                    .pttl(&rkey)
                    .await
                    .map(|ms: i64| (ms / 1000).max(0))
                    .unwrap_or(window.as_secs() as i64);
                let allowed = count <= i64::from(limit);
                Ok(Decision {
                    allowed,
                    retry_after: (!allowed).then(|| ttl.max(0) as u32),
                    remaining: (limit as i64 - count).max(0) as u32,
                })
            }
            Err(e) => redis_failure(e, fail, limit),
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

fn redis_failure(
    error: redis::RedisError,
    fail: FailMode,
    limit: u32,
) -> Result<Decision, AppError> {
    match fail {
        FailMode::Open => Ok(Decision {
            allowed: true,
            retry_after: None,
            remaining: limit,
        }),
        FailMode::Closed => Err(map_redis(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_rate_limit_fails_when_redis_is_unavailable() {
        let error = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "redis offline",
        ));
        let result = redis_failure(error, FailMode::Closed, 60);
        assert_eq!(result.unwrap_err().code, ErrorCode::DependencyUnavailable);

        let error = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "redis offline",
        ));
        let open = redis_failure(error, FailMode::Open, 60).unwrap();
        assert!(open.allowed);
        assert_eq!(open.remaining, 60);
    }

    #[tokio::test]
    async fn redis_outage_fails_closed_at_connection_or_limit_check() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("redis://{addr}/");

        let connected = tokio::time::timeout(
            Duration::from_secs(2),
            crate::redis::RedisClient::connect(&url),
        )
        .await
        .expect("closed Redis port should fail or expose failure on use");
        match connected {
            Err(error) => assert_eq!(error.code, ErrorCode::DependencyUnavailable),
            Ok(client) => {
                let limiter = RateLimiter::new(client);
                let result = tokio::time::timeout(
                    Duration::from_secs(2),
                    limiter.require(
                        "send:outage-test",
                        60,
                        Duration::from_secs(60),
                        FailMode::Closed,
                    ),
                )
                .await
                .expect("rate-limit check should fail promptly");
                assert_eq!(result.unwrap_err().code, ErrorCode::DependencyUnavailable);
            }
        }
    }
}
