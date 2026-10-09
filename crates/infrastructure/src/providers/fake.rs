//! In-memory Fake providers for tests/dev (spec §4.9). Behavior is scripted:
//! callers push outcomes (`script_send`, …) or the `FAKE_PROVIDER_SCRIPT` env
//! JSON file primes the queues at construction. Sent mail is inspectable via
//! `sent()`; inbound mail is injected via `inject_inbound`.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

use super::*;

/// JSON file shape for FAKE_PROVIDER_SCRIPT.
#[derive(Debug, serde::Deserialize)]
struct ScriptFile {
    #[serde(default)]
    send: VecDeque<ScriptedSend>,
    #[serde(default)]
    free_busy: Vec<BusyBlock>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScriptedSend {
    Accepted,
    Rejected { code: String },
    Unknown,
}

#[derive(Default)]
struct FakeMailState {
    sent: Vec<(SendRequest, String)>,
    inbound: Vec<ProviderMessage>,
    script: VecDeque<SendOutcome>,
}

/// Fake mailbox provider — default outcome is `Accepted`.
pub struct FakeMailProvider {
    state: Mutex<FakeMailState>,
    free_busy: Mutex<VecDeque<CalOutcome<Vec<BusyBlock>>>>,
    events: Mutex<Vec<(String, CalEvent)>>,
}

impl Default for FakeMailProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeMailProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeMailState::default()),
            free_busy: Mutex::new(VecDeque::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    /// Prime from `FAKE_PROVIDER_SCRIPT` JSON; falls back to empty script.
    pub fn from_env() -> Self {
        let p = Self::new();
        if let Ok(path) = std::env::var("FAKE_PROVIDER_SCRIPT")
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(sf) = serde_json::from_str::<ScriptFile>(&text)
        {
            for block in sf.free_busy {
                p.free_busy
                    .lock()
                    .unwrap()
                    .push_back(CalOutcome::Ok(vec![block]));
            }
            let mut st = p.state.lock().unwrap();
            for s in sf.send {
                st.script.push_back(match s {
                    ScriptedSend::Accepted => SendOutcome::Accepted {
                        provider_message_id: String::new(),
                    },
                    ScriptedSend::Rejected { code } => SendOutcome::Rejected {
                        permanent: true,
                        code,
                    },
                    ScriptedSend::Unknown => SendOutcome::Unknown {
                        reason: "scripted".into(),
                    },
                });
            }
        }
        p
    }

    /// Queue a scripted outcome consumed by the next `send`.
    pub fn script_send(&self, outcome: SendOutcome) {
        self.state.lock().unwrap().script.push_back(outcome);
    }

    /// Inject a fake inbound message for sync tests.
    pub fn inject_inbound(&self, msg: ProviderMessage) {
        self.state.lock().unwrap().inbound.push(msg);
    }

    /// All sent requests with their assigned provider ids.
    pub fn sent(&self) -> Vec<(SendRequest, String)> {
        self.state.lock().unwrap().sent.clone()
    }

    /// Queue a scripted free-busy result.
    pub fn script_free_busy(&self, outcome: CalOutcome<Vec<BusyBlock>>) {
        self.free_busy.lock().unwrap().push_back(outcome);
    }

    pub fn calendar_events(&self) -> Vec<(String, CalEvent)> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl MailProvider for FakeMailProvider {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            supports_send: true,
            supports_delta_sync: true,
        }
    }

    async fn list_messages(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        let msgs = self.state.lock().unwrap().inbound.clone();
        Ok((msgs, None))
    }

    async fn get_message(
        &self,
        provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        self.state
            .lock()
            .unwrap()
            .inbound
            .iter()
            .find(|m| m.provider_message_id == provider_message_id)
            .cloned()
            .ok_or_else(|| ProviderError::unavailable("message not found"))
    }

    async fn send(&self, req: SendRequest) -> SendOutcome {
        let outcome = self
            .state
            .lock()
            .unwrap()
            .script
            .pop_front()
            .unwrap_or_else(|| SendOutcome::Accepted {
                provider_message_id: String::new(),
            });
        match outcome {
            SendOutcome::Accepted { .. } => {
                let id = format!("fake-{}", uuid::Uuid::new_v4());
                self.state.lock().unwrap().sent.push((req, id.clone()));
                SendOutcome::Accepted {
                    provider_message_id: id,
                }
            }
            other => other,
        }
    }

    async fn lookup_sent(&self, correlation_id: &str) -> Result<Option<SentLookup>, ProviderError> {
        let found = self
            .state
            .lock()
            .unwrap()
            .sent
            .iter()
            .find(|(r, _)| r.correlation_id == correlation_id)
            .map(|(_, id)| SentLookup {
                provider_message_id: id.clone(),
                sent_at: Utc::now(),
            });
        Ok(found)
    }
}

#[async_trait]
impl CalendarProvider for FakeMailProvider {
    async fn free_busy(
        &self,
        _host: &str,
        _range_start: chrono::DateTime<Utc>,
        _range_end: chrono::DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        self.free_busy
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(CalOutcome::Ok(vec![]))
    }

    async fn create_event(
        &self,
        host: &str,
        _title: &str,
        start: chrono::DateTime<Utc>,
        end: chrono::DateTime<Utc>,
        _attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        let ev = CalEvent {
            provider_event_id: format!("cal-{}", uuid::Uuid::new_v4()),
            revision: "1".into(),
            start,
            end,
        };
        self.events
            .lock()
            .unwrap()
            .push((host.to_string(), ev.clone()));
        CalOutcome::Ok(ev)
    }

    async fn cancel_event(&self, host: &str, provider_event_id: &str) -> CalOutcome<()> {
        self.events
            .lock()
            .unwrap()
            .retain(|(h, e)| !(h == host && e.provider_event_id == provider_event_id));
        CalOutcome::Ok(())
    }
}

/// Fake delivery provider — same script mechanics, delivery-tuned caps.
pub struct FakeDeliveryProvider {
    state: Mutex<FakeMailState>,
}

impl Default for FakeDeliveryProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDeliveryProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeMailState::default()),
        }
    }
    pub fn script_send(&self, outcome: SendOutcome) {
        self.state.lock().unwrap().script.push_back(outcome);
    }
    pub fn sent(&self) -> Vec<(SendRequest, String)> {
        self.state.lock().unwrap().sent.clone()
    }
}

#[async_trait]
impl DeliveryProvider for FakeDeliveryProvider {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            one_click_unsubscribe: true,
            custom_headers: true,
            idempotency: true,
        }
    }
    async fn send(&self, req: SendRequest) -> SendOutcome {
        let outcome = self
            .state
            .lock()
            .unwrap()
            .script
            .pop_front()
            .unwrap_or_else(|| SendOutcome::Accepted {
                provider_message_id: String::new(),
            });
        match outcome {
            SendOutcome::Accepted { .. } => {
                let id = format!("fake-dlv-{}", uuid::Uuid::new_v4());
                self.state.lock().unwrap().sent.push((req, id.clone()));
                SendOutcome::Accepted {
                    provider_message_id: id,
                }
            }
            other => other,
        }
    }
    async fn lookup_sent(&self, correlation_id: &str) -> Result<Option<SentLookup>, ProviderError> {
        let found = self
            .state
            .lock()
            .unwrap()
            .sent
            .iter()
            .find(|(r, _)| r.correlation_id == correlation_id)
            .map(|(_, id)| SentLookup {
                provider_message_id: id.clone(),
                sent_at: Utc::now(),
            });
        Ok(found)
    }
}
