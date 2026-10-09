//! Redis connection wrapper. Optional dependency — callers decide fail-open
//! vs fail-closed (spec §16.3).

use contracts::AppError;
use redis::aio::MultiplexedConnection;

use crate::error::map_redis;

#[derive(Clone)]
pub struct RedisClient {
    conn: MultiplexedConnection,
}

impl RedisClient {
    pub async fn connect(url: &str) -> Result<Self, AppError> {
        let client = redis::Client::open(url).map_err(map_redis)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(map_redis)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> MultiplexedConnection {
        self.conn.clone()
    }
}
