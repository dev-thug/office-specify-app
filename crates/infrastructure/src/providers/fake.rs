//! In-memory Fake providers for tests/dev (spec §4.9). Behavior is scripted:
//! callers push outcomes (`script_send`, …) or the `FAKE_PROVIDER_SCRIPT` env
//! JSON file primes the queues at construction. Sent mail is inspectable via
//! `sent()`; inbound mail is injected via `inject_inbound`.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

use super::*;

/// A scripted `send` outcome — `Result` so transport/config failures are
/// expressible (R27): the provider returning Err is distinct from a send
/// whose submission state is Unknown.
type SendResult = Result<SendOutcome, ProviderError>;

/// JSON file shape for `Config.fake_provider_script` / FAKE_PROVIDER_SCRIPT:
/// ```json
/// {
///   "send":           [{"kind":"accepted"},{"kind":"permanent_failed","code":"550"},
///                      {"kind":"temporary_failed","code":"421"},{"kind":"timeout"},
///                      {"kind":"unavailable","message":"..."}],
///   "delivery_send":  [ ...same kinds, drives FakeDeliveryProvider... ],
///   "free_busy":      [{"start":"...","end":"..."}],
///   "calendar_write": [{"kind":"ok"},{"kind":"conflict"},{"kind":"failed","code":"E"},
///                      {"kind":"unknown"}]
/// }
/// ```
/// `calendar_write` outcomes are consumed in order by
/// create_event/update_event/cancel_event.
#[derive(Debug, serde::Deserialize)]
struct ScriptFile {
    #[serde(default)]
    send: VecDeque<ScriptedSend>,
    #[serde(default)]
    delivery_send: VecDeque<ScriptedSend>,
    #[serde(default)]
    free_busy: Vec<BusyBlock>,
    #[serde(default)]
    calendar_write: VecDeque<ScriptedCalWrite>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScriptedSend {
    Accepted,
    PermanentFailed {
        code: Option<String>,
    },
    TemporaryFailed {
        code: Option<String>,
    },
    /// Submission state unknown — reconcile via lookup_sent.
    Timeout,
    /// Transport-level failure — `Err(ProviderError::Unavailable)`.
    Unavailable {
        message: Option<String>,
    },
    /// Legacy names still accepted.
    Rejected {
        code: String,
    },
    Unknown,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScriptedCalWrite {
    Ok,
    /// Calendar write conflict → SLOT_UNAVAILABLE-shaped failure.
    Conflict {
        code: Option<String>,
    },
    Failed {
        code: Option<String>,
    },
    Unknown,
}

/// Test-facing calendar-write script values (R27).
#[derive(Debug, Clone)]
pub enum ScriptedCalOutcome {
    Ok,
    /// Calendar-side write conflict (e.g. slot no longer free).
    Conflict,
    Failed(String),
    /// Response lost — caller must reconcile, not retry.
    Unknown,
}

impl ScriptedCalWrite {
    fn apply<T>(&self, ok: T) -> CalOutcome<T> {
        match self {
            Self::Ok => CalOutcome::Ok(ok),
            Self::Conflict { code } => CalOutcome::Failed {
                code: code.clone().unwrap_or_else(|| "SLOT_UNAVAILABLE".into()),
                message: "scripted calendar conflict".into(),
            },
            Self::Failed { code } => CalOutcome::Failed {
                code: code.clone().unwrap_or_else(|| "PROVIDER_FAILED".into()),
                message: "scripted calendar failure".into(),
            },
            Self::Unknown => CalOutcome::Unknown,
        }
    }
}

impl ScriptedSend {
    fn into_result(self) -> SendResult {
        match self {
            Self::Accepted => Ok(SendOutcome::Accepted {
                provider_message_id: String::new(),
            }),
            Self::PermanentFailed { code } => Ok(SendOutcome::Rejected {
                permanent: true,
                code: code.unwrap_or_else(|| "rejected".into()),
            }),
            Self::TemporaryFailed { code } => Ok(SendOutcome::Rejected {
                permanent: false,
                code: code.unwrap_or_else(|| "temporary".into()),
            }),
            Self::Timeout | Self::Unknown => Ok(SendOutcome::Unknown {
                reason: "scripted timeout".into(),
            }),
            Self::Unavailable { message } => Err(ProviderError::Unavailable(
                message.unwrap_or_else(|| "scripted unavailable".into()),
            )),
            Self::Rejected { code } => Ok(SendOutcome::Rejected {
                permanent: true,
                code,
            }),
        }
    }
}

#[derive(Default)]
struct FakeMailState {
    sent: Vec<(SendRequest, String)>,
    inbound: Vec<ProviderMessage>,
    script: VecDeque<SendResult>,
}

/// Fake mailbox provider — default outcome is `Accepted`.
pub struct FakeMailProvider {
    state: Mutex<FakeMailState>,
    free_busy: Mutex<VecDeque<CalOutcome<Vec<BusyBlock>>>>,
    events: Mutex<Vec<(String, CalEvent)>>,
    /// Consumed in order by create_event / update_event / cancel_event.
    cal_write: Mutex<VecDeque<ScriptedCalWrite>>,
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
            cal_write: Mutex::new(VecDeque::new()),
        }
    }

    /// Prime from a script file path (`Config.fake_provider_script`).
    pub fn from_script(path: &std::path::Path) -> Self {
        let p = Self::new();
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(sf) = serde_json::from_str::<ScriptFile>(&text)
        {
            for block in sf.free_busy {
                p.free_busy
                    .lock()
                    .unwrap()
                    .push_back(CalOutcome::Ok(vec![block]));
            }
            {
                let mut st = p.state.lock().unwrap();
                for s in sf.send {
                    st.script.push_back(s.into_result());
                }
            }
            *p.cal_write.lock().unwrap() = sf.calendar_write;
        }
        p
    }

    /// Prime from `FAKE_PROVIDER_SCRIPT` JSON; falls back to empty script.
    pub fn from_env() -> Self {
        match std::env::var("FAKE_PROVIDER_SCRIPT") {
            Ok(path) => Self::from_script(std::path::Path::new(&path)),
            Err(_) => Self::new(),
        }
    }

    /// Queue a scripted outcome consumed by the next `send`.
    pub fn script_send(&self, outcome: SendOutcome) {
        self.state.lock().unwrap().script.push_back(Ok(outcome));
    }

    /// Queue a scripted transport failure for the next `send`.
    pub fn script_send_error(&self, err: ProviderError) {
        self.state.lock().unwrap().script.push_back(Err(err));
    }

    /// Queue a scripted outcome for the next calendar write
    /// (create/update/cancel). `conflict` models a calendar-side write
    /// conflict; `unknown` models a dropped response.
    pub fn script_calendar_write(&self, outcome: ScriptedCalOutcome) {
        self.cal_write.lock().unwrap().push_back(match outcome {
            ScriptedCalOutcome::Ok => ScriptedCalWrite::Ok,
            ScriptedCalOutcome::Conflict => ScriptedCalWrite::Conflict { code: None },
            ScriptedCalOutcome::Failed(code) => ScriptedCalWrite::Failed { code: Some(code) },
            ScriptedCalOutcome::Unknown => ScriptedCalWrite::Unknown,
        });
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

    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError> {
        let outcome = self
            .state
            .lock()
            .unwrap()
            .script
            .pop_front()
            .unwrap_or_else(|| {
                Ok(SendOutcome::Accepted {
                    provider_message_id: String::new(),
                })
            })?;
        match outcome {
            SendOutcome::Accepted { .. } => {
                let id = format!("fake-{}", uuid::Uuid::new_v4());
                self.state.lock().unwrap().sent.push((req, id.clone()));
                Ok(SendOutcome::Accepted {
                    provider_message_id: id,
                })
            }
            other => Ok(other),
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
        if let Some(s) = self.cal_write.lock().unwrap().pop_front() {
            match s.apply(ev.clone()) {
                CalOutcome::Ok(e) => {
                    self.events
                        .lock()
                        .unwrap()
                        .push((host.to_string(), e.clone()));
                    return CalOutcome::Ok(e);
                }
                other => return other,
            }
        }
        self.events
            .lock()
            .unwrap()
            .push((host.to_string(), ev.clone()));
        CalOutcome::Ok(ev)
    }

    async fn update_event(
        &self,
        host: &str,
        provider_event_id: &str,
        _title: &str,
        start: chrono::DateTime<Utc>,
        end: chrono::DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        let ev = CalEvent {
            provider_event_id: provider_event_id.to_string(),
            revision: "2".into(),
            start,
            end,
        };
        if let Some(s) = self.cal_write.lock().unwrap().pop_front() {
            match s.apply(ev.clone()) {
                CalOutcome::Ok(e) => {
                    let mut events = self.events.lock().unwrap();
                    if let Some(slot) = events
                        .iter_mut()
                        .find(|(h, x)| h == host && x.provider_event_id == provider_event_id)
                    {
                        *slot = (host.to_string(), e.clone());
                    }
                    return CalOutcome::Ok(e);
                }
                other => return other,
            }
        }
        let mut events = self.events.lock().unwrap();
        match events
            .iter_mut()
            .find(|(h, x)| h == host && x.provider_event_id == provider_event_id)
        {
            Some(slot) => {
                *slot = (host.to_string(), ev.clone());
                CalOutcome::Ok(ev)
            }
            None => CalOutcome::Failed {
                code: "NOT_FOUND".into(),
                message: "event not found".into(),
            },
        }
    }

    async fn cancel_event(&self, host: &str, provider_event_id: &str) -> CalOutcome<()> {
        if let Some(s) = self.cal_write.lock().unwrap().pop_front() {
            match s.apply(()) {
                CalOutcome::Ok(()) => {}
                other => return other,
            }
        }
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

    /// Prime from a script file path (`Config.fake_provider_script`) — the
    /// `delivery_send` queue drives `send`.
    pub fn from_script(path: &std::path::Path) -> Self {
        let p = Self::new();
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(sf) = serde_json::from_str::<ScriptFile>(&text)
        {
            let mut st = p.state.lock().unwrap();
            for s in sf.delivery_send {
                st.script.push_back(s.into_result());
            }
        }
        p
    }

    pub fn from_env() -> Self {
        match std::env::var("FAKE_PROVIDER_SCRIPT") {
            Ok(path) => Self::from_script(std::path::Path::new(&path)),
            Err(_) => Self::new(),
        }
    }

    pub fn script_send(&self, outcome: SendOutcome) {
        self.state.lock().unwrap().script.push_back(Ok(outcome));
    }

    /// Queue a scripted transport failure for the next `send` (R27 —
    /// `unavailable` is an Err, not an Unknown outcome).
    pub fn script_send_error(&self, err: ProviderError) {
        self.state.lock().unwrap().script.push_back(Err(err));
    }

    pub fn sent(&self) -> Vec<(SendRequest, String)> {
        self.state.lock().unwrap().sent.clone()
    }
}

#[async_trait]
impl DeliveryProvider for FakeDeliveryProvider {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            supports_send: true,
            one_click_unsubscribe: true,
            custom_headers: true,
            idempotency: true,
        }
    }
    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError> {
        let outcome = self
            .state
            .lock()
            .unwrap()
            .script
            .pop_front()
            .unwrap_or_else(|| {
                Ok(SendOutcome::Accepted {
                    provider_message_id: String::new(),
                })
            })?;
        match outcome {
            SendOutcome::Accepted { .. } => {
                let id = format!("fake-dlv-{}", uuid::Uuid::new_v4());
                self.state.lock().unwrap().sent.push((req, id.clone()));
                Ok(SendOutcome::Accepted {
                    provider_message_id: id,
                })
            }
            other => Ok(other),
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
