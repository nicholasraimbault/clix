use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

/// Display name, unique in the pair (`laptop`, `server`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BodyId(pub String);

impl std::fmt::Display for BodyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Owner public identity. Pairing fills this in later.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OwnerId(pub Vec<u8>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub tool: String,
    pub binary: PathBuf,
    pub allow_from: Option<Vec<BodyId>>,
    pub once: bool,
    #[serde(default, with = "system_time_opt")]
    pub until: Option<SystemTime>,
    #[serde(default)]
    pub schedule: Option<Schedule>,
    /// A successful run consumes this grant. Interrupted runs retain the reservation.
    #[serde(default)]
    pub reservation: Option<String>,
}

/// Repeating window on this box's local clock.
///
/// Calendar: empty `days` and `dates` means every day. If both are set, either
/// match is enough. Then `from`/`to` (if set) must contain local time.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Schedule {
    #[serde(default)]
    pub days: Vec<Weekday>,
    #[serde(default)]
    pub dates: Vec<u8>,
    pub from: Option<NaiveTime>,
    pub to: Option<NaiveTime>,
}

impl Schedule {
    pub fn allows_at(&self, now: DateTime<Local>) -> bool {
        self.calendar_ok(now.date_naive()) && self.time_ok(now.time())
    }

    fn calendar_ok(&self, date: chrono::NaiveDate) -> bool {
        let days_set = !self.days.is_empty();
        let dates_set = !self.dates.is_empty();
        if !days_set && !dates_set {
            return true;
        }
        let day_match = days_set && self.days.contains(&date.weekday());
        let date_match = dates_set && self.dates.contains(&(date.day() as u8));
        day_match || date_match
    }

    fn time_ok(&self, t: NaiveTime) -> bool {
        match (self.from, self.to) {
            (None, None) => true,
            (Some(from), None) => t >= from,
            (None, Some(to)) => t <= to,
            (Some(from), Some(to)) if from <= to => t >= from && t <= to,
            (Some(from), Some(to)) => t >= from || t <= to,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub body: BodyId,
    pub argv: Vec<String>,
    pub from: BodyId,
    pub status: JobStatus,
    #[serde(default)]
    pub stdout: Vec<u8>,
    #[serde(default)]
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    WaitingBody,
    Running,
    Done { exit: i32 },
    Denied { reason: String },
    Failed { reason: String },
    Uncertain { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub name: BodyId,
    pub owner_pk: Vec<u8>,
    pub addr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub from: BodyId,
    pub tool: String,
    #[serde(default = "true_default")]
    pub once_suggested: bool,
}

/// Durable delivery of a permission request, without executing a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundRequest {
    pub id: String,
    pub body: BodyId,
    pub tool: String,
    #[serde(default)]
    pub waiting: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Retained after owner approval/denial so a lost acknowledgement cannot
/// recreate a prompt for the same delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestReceipt {
    pub delivery_id: String,
    pub request: Request,
}

fn true_default() -> bool {
    true
}

mod system_time_opt {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let secs = t.map(|st| st.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
        secs.serialize(s)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<SystemTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs: Option<u64> = Option::deserialize(d)?;
        Ok(secs.map(|n| UNIX_EPOCH + Duration::from_secs(n)))
    }
}
