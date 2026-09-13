use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Claude,
    Codex,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Official,
    LocalEstimate,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Quota,
    Spend,
    Credits,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Amount {
    pub used: Option<String>,
    pub limit: Option<String>,
    pub balance: Option<String>,
    pub currency: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Limit {
    pub id: String,
    pub label: String,
    pub kind: LimitKind,
    pub used_fraction: Option<f64>,
    pub resets_at: Option<i64>,
    pub window_seconds: Option<u64>,
    pub provenance: Provenance,
    pub enabled: bool,
    pub amount: Option<Amount>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderSnapshot {
    pub provider: ProviderId,
    pub limits: Vec<Limit>,
    pub plan: Option<String>,
    pub fetched_at: i64,
    pub warnings: Vec<String>,
}
