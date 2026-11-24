//! Test helpers: a tiny FIT encoder and per-test databases.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use running_viz::MIGRATOR;
use sqlx::PgPool;
use uuid::Uuid;

/// A synthetic workout for `fit_file`.
pub struct Workout {
    pub start: DateTime<Utc>,
    pub seconds: u32,
    pub distance_m: u32,
    /// FIT sport enum: 1 = running, 5 = swimming
    pub sport: u8,
    /// one GPS point (degrees) per record, or no GPS
    pub gps: Option<(f64, f64)>,
}

impl Workout {
    pub fn run(start: &str, seconds: u32, distance_m: u32) -> Self {
        Self {
            start: start.parse().unwrap(),
            seconds,
            distance_m,
            sport: 1,
            gps: Some((52.5, 13.4)),
        }
    }
}

const FIT_EPOCH: i64 = 631_065_600; // 1989-12-31T00:00:00Z

fn fit_time(t: DateTime<Utc>) -> u32 {
    (t.timestamp() - FIT_EPOCH) as u32
}

fn semicircles(deg: f64) -> i32 {
    (deg * (2f64.powi(31) / 180.0)) as i32
}

fn crc16(bytes: &[u8]) -> u16 {
    const T: [u16; 16] = [
        0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401, 0xA001, 0x6C00, 0x7800,
        0xB401, 0x5000, 0x9C01, 0x8801, 0x4400,
    ];
    bytes.iter().fold(0u16, |mut crc, &b| {
        let tmp = T[(crc & 0xF) as usize];
        crc = (crc >> 4) & 0x0FFF;
        crc ^= tmp ^ T[(b & 0xF) as usize];
        let tmp = T[(crc & 0xF) as usize];
        crc = (crc >> 4) & 0x0FFF;
        crc ^ tmp ^ T[(b >> 4) as usize]
    })
}

/// Encodes a minimal valid FIT file: GPS records every 10 s (if any) plus one session.
pub fn fit_file(w: &Workout) -> Vec<u8> {
    let mut data = Vec::new();
    // definition: local 0 = record (20): timestamp, position_lat, position_long
    data.extend([0x40, 0, 0, 20, 0, 3, 253, 4, 0x86, 0, 4, 0x85, 1, 4, 0x85]);
    if let Some((lat, lon)) = w.gps {
        for i in (0..=w.seconds).step_by(10) {
            data.push(0x00);
            data.extend(fit_time(w.start + chrono::Duration::seconds(i as i64)).to_le_bytes());
            data.extend(semicircles(lat + i as f64 * 1e-5).to_le_bytes());
            data.extend(semicircles(lon).to_le_bytes());
        }
    }
    // definition: local 1 = session (18)
    data.extend([
        0x41, 0, 0, 18, 0, 7, //
        253, 4, 0x86, // timestamp
        2, 4, 0x86, // start_time
        7, 4, 0x86, // total_elapsed_time (ms)
        8, 4, 0x86, // total_timer_time (ms)
        9, 4, 0x86, // total_distance (cm)
        5, 1, 0x00, // sport
        6, 1, 0x00, // sub_sport
    ]);
    data.push(0x01);
    let end = w.start + chrono::Duration::seconds(w.seconds as i64);
    data.extend(fit_time(end).to_le_bytes());
    data.extend(fit_time(w.start).to_le_bytes());
    data.extend((w.seconds * 1000).to_le_bytes());
    data.extend((w.seconds * 1000).to_le_bytes());
    data.extend((w.distance_m * 100).to_le_bytes());
    data.push(w.sport);
    data.push(0);

    let mut file = vec![14u8, 0x20];
    file.extend(2132u16.to_le_bytes());
    file.extend((data.len() as u32).to_le_bytes());
    file.extend(b".FIT");
    let header_crc = crc16(&file);
    file.extend(header_crc.to_le_bytes());
    file.extend(&data);
    let crc = crc16(&file);
    file.extend(crc.to_le_bytes());
    file
}

/// A fresh database per test (needs TEST_DATABASE_URL), so parallel tests never share rows.
pub async fn fresh_db() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set, skipping");
        return None;
    };
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!("t_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let (base, _) = url.rsplit_once('/').unwrap();
    let db = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
    MIGRATOR.run(&db).await.unwrap();
    Some(db)
}
