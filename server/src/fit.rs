//! Reads the few facts we need from a FIT file.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use fitparser::{FitDataRecord, Value, profile::MesgNum};

/// Summary of a FIT activity (first session).
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// FIT sport, e.g. "running", "swimming"
    pub sport: String,
    /// FIT sub sport, e.g. "generic", "indoor_running"
    pub sub_sport: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub distance_m: i32,
    pub has_gps: bool,
    /// Moving time in seconds (pauses excluded), if the file has it.
    pub timer_s: Option<f64>,
}

pub fn summary(bytes: &[u8]) -> Result<Summary> {
    // fitparser can panic on malformed input (e.g. arithmetic overflow); treat that as unreadable
    let parsed = std::panic::catch_unwind(|| fitparser::from_bytes(bytes))
        .map_err(|_| anyhow::anyhow!("FIT parser crashed on this file"))?;
    let records = parsed.context("not a readable FIT file")?;
    let session = records
        .iter()
        .find(|r| r.kind() == MesgNum::Session)
        .context("FIT file has no session")?;
    let start = timestamp(session, "start_time").context("session without start_time")?;
    let elapsed = float(session, "total_elapsed_time").unwrap_or(0.0);
    let end = timestamp(session, "timestamp")
        .filter(|t| *t >= start)
        .unwrap_or(start + Duration::milliseconds((elapsed * 1000.0) as i64));
    if end < start {
        bail!("session ends before it starts");
    }
    let has_gps = records
        .iter()
        .filter(|r| r.kind() == MesgNum::Record)
        .any(|r| r.fields().iter().any(|f| f.name() == "position_lat"));
    Ok(Summary {
        sport: string(session, "sport").unwrap_or_else(|| "generic".into()),
        sub_sport: string(session, "sub_sport").unwrap_or_else(|| "generic".into()),
        start,
        end,
        distance_m: float(session, "total_distance").unwrap_or(0.0).round() as i32,
        has_gps,
        timer_s: float(session, "total_timer_time").filter(|t| *t > 0.0),
    })
}

fn field<'a>(r: &'a FitDataRecord, name: &str) -> Option<&'a Value> {
    r.fields()
        .iter()
        .find(|f| f.name() == name)
        .map(|f| f.value())
}

fn timestamp(r: &FitDataRecord, name: &str) -> Option<DateTime<Utc>> {
    match field(r, name)? {
        Value::Timestamp(t) => Some(t.with_timezone(&Utc)),
        _ => None,
    }
}

fn string(r: &FitDataRecord, name: &str) -> Option<String> {
    match field(r, name)? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn float(r: &FitDataRecord, name: &str) -> Option<f64> {
    match field(r, name)? {
        Value::Float64(v) => Some(*v),
        Value::Float32(v) => Some(f64::from(*v)),
        Value::UInt32(v) => Some(f64::from(*v)),
        Value::UInt16(v) => Some(f64::from(*v)),
        _ => None,
    }
}
