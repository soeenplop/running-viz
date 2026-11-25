//! GPS track of a run: time, position and elevation only.
//! Heart rate, cadence, power, running dynamics and calories are never copied.

use std::fmt::Write;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use fitparser::{Value, profile::MesgNum};

const SEMICIRCLE: f64 = 180.0 / 2_147_483_648.0;

/// One point of a track.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackPoint {
    pub time: DateTime<Utc>,
    pub lat: f64,
    pub lon: f64,
    pub ele: Option<f64>,
}

/// The GPS track of a FIT file (empty if the run has no GPS).
pub fn track(bytes: &[u8]) -> Result<Vec<TrackPoint>> {
    let records = std::panic::catch_unwind(|| fitparser::from_bytes(bytes))
        .map_err(|_| anyhow::anyhow!("FIT parser crashed on this file"))?
        .context("not a readable FIT file")?;
    let mut points = Vec::new();
    for r in records.iter().filter(|r| r.kind() == MesgNum::Record) {
        let get = |n: &str| r.fields().iter().find(|f| f.name() == n).map(|f| f.value());
        let (Some(Value::SInt32(lat)), Some(Value::SInt32(lon)), Some(Value::Timestamp(t))) =
            (get("position_lat"), get("position_long"), get("timestamp"))
        else {
            continue;
        };
        let ele = match get("enhanced_altitude").or(get("altitude")) {
            Some(Value::Float64(e)) => Some(*e),
            _ => None,
        };
        points.push(TrackPoint {
            time: t.with_timezone(&Utc),
            lat: f64::from(*lat) * SEMICIRCLE,
            lon: f64::from(*lon) * SEMICIRCLE,
            ele,
        });
    }
    Ok(points)
}

/// Converts the GPS track of a FIT file into GPX 1.1.
pub fn from_fit(bytes: &[u8], name: &str) -> Result<String> {
    let points = track(bytes)?;
    if points.is_empty() {
        bail!("FIT file has no GPS track");
    }
    let mut out = String::new();
    for p in &points {
        write!(out, r#"<trkpt lat="{:.7}" lon="{:.7}">"#, p.lat, p.lon)?;
        if let Some(e) = p.ele {
            write!(out, "<ele>{e:.1}</ele>")?;
        }
        write!(
            out,
            "<time>{}</time></trkpt>",
            p.time.to_rfc3339_opts(SecondsFormat::Secs, true)
        )?;
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><gpx version="1.1" creator="running-viz" xmlns="http://www.topografix.com/GPX/1/1"><trk><name>{}</name><type>running</type><trkseg>{out}</trkseg></trk></gpx>"#,
        escape(name)
    ))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
