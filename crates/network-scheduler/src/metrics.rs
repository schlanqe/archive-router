use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use router_controller::messages::{QueryExecuted, QueryFinished, QuerySubmitted};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    timestamp: u64, // Milliseconds since UNIX epoch
    peer_id: String,
    #[serde(flatten)]
    event: MetricsEvent,
}

impl Metrics {
    pub fn new(peer_id: impl ToString, event: impl Into<MetricsEvent>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("We're after 1970")
            .as_millis()
            .try_into()
            .expect("But before 2554");
        Self {
            timestamp,
            peer_id: peer_id.to_string(),
            event: event.into(),
        }
    }

    pub fn to_json_line(&self) -> anyhow::Result<Vec<u8>> {
        let json_str = serde_json::to_string(self)?;
        let vec = format!("metrics: {json_str}\n").into_bytes();
        Ok(vec)
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum MetricsEvent {
    QuerySubmitted(QuerySubmitted),
    QueryFinished(QueryFinished),
    QueryExecuted(QueryExecuted),
}

impl From<QuerySubmitted> for MetricsEvent {
    fn from(value: QuerySubmitted) -> Self {
        Self::QuerySubmitted(value)
    }
}

impl From<QueryFinished> for MetricsEvent {
    fn from(value: QueryFinished) -> Self {
        Self::QueryFinished(value)
    }
}

impl From<QueryExecuted> for MetricsEvent {
    fn from(value: QueryExecuted) -> Self {
        Self::QueryExecuted(value)
    }
}
