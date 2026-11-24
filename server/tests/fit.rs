//! FIT summaries of synthetic files (CI) and, if available, real HealthFit exports.

mod common;

use common::{Workout, fit_file};
use running_viz::fit::summary;

#[test]
fn synthetic_run_with_gps() {
    let w = Workout::run("2026-12-01T07:00:00Z", 1800, 6000);
    let s = summary(&fit_file(&w)).unwrap();
    assert_eq!(s.sport, "running");
    assert_eq!(s.start, w.start);
    assert_eq!(s.end, w.start + chrono::Duration::seconds(1800));
    assert_eq!(s.distance_m, 6000);
    assert!(s.has_gps);
}

#[test]
fn synthetic_swim_without_gps() {
    let w = Workout {
        sport: 5,
        gps: None,
        ..Workout::run("2026-12-01T07:00:00Z", 600, 1000)
    };
    let s = summary(&fit_file(&w)).unwrap();
    assert_eq!((s.sport.as_str(), s.has_gps), ("swimming", false));
}

#[test]
fn garbage_is_rejected() {
    assert!(summary(b"not a fit file").is_err());
}

/// fitparser panics on a trailing byte; that must be an error, not a crash.
#[test]
fn trailing_byte_is_an_error_not_a_panic() {
    let mut bytes = fit_file(&Workout::run("2026-12-01T07:00:00Z", 600, 2000));
    bytes.push(0);
    assert!(summary(&bytes).is_err());
}

/// Real exports contain GPS tracks and are not committed: point
/// RUNNING_VIZ_FIT_FIXTURES at a folder with `run.fit` and `treadmill.fit`.
#[test]
fn real_healthfit_exports() {
    let Ok(dir) = std::env::var("RUNNING_VIZ_FIT_FIXTURES") else {
        eprintln!("RUNNING_VIZ_FIT_FIXTURES not set, skipping");
        return;
    };
    let run = summary(&std::fs::read(format!("{dir}/run.fit")).unwrap()).unwrap();
    assert_eq!((run.sport.as_str(), run.has_gps), ("running", true));
    let treadmill = summary(&std::fs::read(format!("{dir}/treadmill.fit")).unwrap()).unwrap();
    assert_eq!(
        (treadmill.sub_sport.as_str(), treadmill.has_gps),
        ("indoor_running", false)
    );
}
