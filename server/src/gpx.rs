//! GPX copy of a run for the dogs: time, position and elevation only.
//! Heart rate, cadence, power, running dynamics and calories are never copied.

use std::fmt::Write;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use fitparser::{Value, profile::MesgNum};

const SEMICIRCLE: f64 = 180.0 / 2_147_483_648.0;

/// Converts the GPS track of a FIT file into GPX 1.1.
pub fn from_fit(bytes: &[u8], name: &str) -> Result<String> {
    let records = std::panic::catch_unwind(|| fitparser::from_bytes(bytes))
        .map_err(|_| anyhow::anyhow!("FIT parser crashed on this file"))?
        .context("not a readable FIT file")?;
    let mut points = String::new();
    let mut count = 0;
    for r in records.iter().filter(|r| r.kind() == MesgNum::Record) {
        let get = |n: &str| r.fields().iter().find(|f| f.name() == n).map(|f| f.value());
        let (Some(Value::SInt32(lat)), Some(Value::SInt32(lon)), Some(Value::Timestamp(t))) =
            (get("position_lat"), get("position_long"), get("timestamp"))
        else {
            continue;
        };
        let time: DateTime<Utc> = t.with_timezone(&Utc);
        write!(
            points,
            r#"<trkpt lat="{:.7}" lon="{:.7}">"#,
            f64::from(*lat) * SEMICIRCLE,
            f64::from(*lon) * SEMICIRCLE
        )?;
        let ele = get("enhanced_altitude").or(get("altitude"));
        if let Some(Value::Float64(e)) = ele {
            write!(points, "<ele>{e:.1}</ele>")?;
        }
        write!(
            points,
            "<time>{}</time></trkpt>",
            time.to_rfc3339_opts(SecondsFormat::Secs, true)
        )?;
        count += 1;
    }
    if count == 0 {
        bail!("FIT file has no GPS track");
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><gpx version="1.1" creator="running-viz" xmlns="http://www.topografix.com/GPX/1/1"><trk><name>{}</name><type>running</type><trkseg>{points}</trkseg></trk></gpx>"#,
        escape(name)
    ))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
