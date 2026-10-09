use serde::{Deserialize, Serialize};

/// `{ "data": ..., "meta": { "request_id": ... } }` (B.1.2). List responses
/// additionally set `page`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
    pub meta: Meta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<Page>,
}

impl<T> Envelope<T> {
    pub fn new(data: T, request_id: impl Into<String>) -> Self {
        Self {
            data,
            meta: Meta {
                request_id: request_id.into(),
            },
            page: None,
        }
    }

    pub fn with_page(mut self, page: Page) -> Self {
        self.page = Some(page);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub request_id: String,
}

/// Cursor pagination block (B.1.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl Page {
    pub fn empty() -> Self {
        Self {
            next_cursor: None,
            has_more: false,
        }
    }
}
