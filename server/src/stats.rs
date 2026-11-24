//! Turns Endurain activities into the numbers on the display.

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};
use chrono_tz::Europe::Berlin;

use crate::{
    endurain::{Account, Activity},
    render::{Runner, Stats},
};

/// Activity types that count for me.
pub const MY_RUN_TYPES: &[&str] = &[
    "run",
    "trail_run",
    "track_run",
    "treadmill_run",
    "virtual_run",
];
/// Activity types that count for the dogs (they only get GPS copies of outdoor runs).
pub const DOG_RUN_TYPES: &[&str] = &["run", "trail_run", "track_run"];

/// A runner's month, computed from their activities.
#[derive(Debug, Clone, PartialEq)]
pub struct Month {
    pub km: f64,
    pub pace_s_per_km: Option<u32>,
    /// km per day of the month, index 0 = day 1 (length = days in month)
    pub daily_km: Vec<f64>,
}

/// Sums the counted activities that started (in Europe/Berlin) in `today`'s month.
pub fn month(activities: &[Activity], types: &[&str], today: NaiveDate) -> Month {
    let days = days_in_month(today);
    let mut daily_km = vec![0.0; days as usize];
    let mut metres = 0i64;
    // pace only from runs that have both distance and moving time
    let (mut timed_metres, mut seconds) = (0i64, 0.0);
    for a in activities {
        if !types.contains(&a.activity_type.as_str()) {
            continue;
        }
        let Some(start) = a.start_time else { continue };
        let local = start.with_timezone(&Berlin).date_naive();
        if local.year() != today.year() || local.month() != today.month() {
            continue;
        }
        daily_km[local.day0() as usize] += a.distance as f64 / 1000.0;
        metres += a.distance;
        if let Some(t) = a.total_timer_time.filter(|t| *t > 0.0 && a.distance > 0) {
            timed_metres += a.distance;
            seconds += t;
        }
    }
    let pace_s_per_km =
        (timed_metres > 0).then(|| (seconds / (timed_metres as f64 / 1000.0)).round() as u32);
    Month {
        km: metres as f64 / 1000.0,
        pace_s_per_km,
        daily_km,
    }
}

/// Fetches name, goal and this month's runs for one account.
pub async fn runner(
    account: &Account,
    types: &[&str],
    default_goal_km: f64,
    today: NaiveDate,
) -> Result<(Runner, Month)> {
    let first = today.with_day(1).expect("day 1");
    // one day of margin on both sides: Endurain filters by its own date, we by Europe/Berlin
    let from = first - Duration::days(1);
    let to = first + Duration::days(days_in_month(today) as i64);
    let activities = account.activities(from, to).await?;
    let month = month(&activities, types, today);
    let runner = Runner {
        name: account.name().await?,
        km: month.km,
        goal_km: account
            .monthly_run_goal_km()
            .await?
            .unwrap_or(default_goal_km),
        pace_s_per_km: month.pace_s_per_km,
    };
    Ok((runner, month))
}

/// Fetches everything for the display: me plus the dogs.
pub async fn fetch(me: &Account, dogs: &[Account], today: NaiveDate) -> Result<Stats> {
    let (me, my_month) = runner(me, MY_RUN_TYPES, 200.0, today).await?;
    let mut dog_runners = Vec::new();
    for dog in dogs {
        dog_runners.push(runner(dog, DOG_RUN_TYPES, 100.0, today).await?.0);
    }
    Ok(Stats {
        today,
        me,
        dogs: dog_runners,
        my_daily_km: my_month.daily_km,
    })
}

/// The display before the first successful fetch: zeros, no names guessed.
pub fn empty(names: &[String], today: NaiveDate) -> Stats {
    let runner = |name: &str, goal_km| Runner {
        name: name.to_owned(),
        km: 0.0,
        goal_km,
        pace_s_per_km: None,
    };
    Stats {
        today,
        me: runner(names.first().map(String::as_str).unwrap_or(""), 200.0),
        dogs: names.iter().skip(1).map(|n| runner(n, 100.0)).collect(),
        my_daily_km: vec![0.0; days_in_month(today) as usize],
    }
}

fn days_in_month(d: NaiveDate) -> u32 {
    let first_next = if d.month() == 12 {
        NaiveDate::from_ymd_opt(d.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1)
    };
    first_next.unwrap().pred_opt().unwrap().day()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(t: &str, start: &str, km: f64, secs: f64) -> Activity {
        Activity {
            id: "x".into(),
            activity_type: t.into(),
            distance: (km * 1000.0) as i64,
            total_timer_time: Some(secs),
            start_time: Some(start.parse().unwrap()),
            end_time: None,
            name: None,
        }
    }

    fn dec(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 12, d).unwrap()
    }

    #[test]
    fn sums_runs_of_the_month_in_berlin_time() {
        let acts = [
            act("run", "2026-12-01T07:00:00Z", 10.0, 3000.0),
            act("treadmill_run", "2026-12-02T18:00:00Z", 5.0, 1500.0),
            act("ride", "2026-12-02T09:00:00Z", 40.0, 5000.0),
            // 30 Nov 23:30 UTC = 1 Dec 00:30 Berlin -> counts for 1 Dec
            act("run", "2026-11-30T23:30:00Z", 2.0, 600.0),
            // 31 Dec 23:30 UTC = 1 Jan Berlin -> next month
            act("run", "2026-12-31T23:30:00Z", 9.0, 2700.0),
        ];
        let m = month(&acts, MY_RUN_TYPES, dec(14));
        assert_eq!(m.km, 17.0);
        assert_eq!(m.daily_km[0], 12.0);
        assert_eq!(m.daily_km[1], 5.0);
        assert_eq!(m.daily_km.len(), 31);
        assert_eq!(m.pace_s_per_km, Some(300)); // 5100 s / 17 km
    }

    #[test]
    fn dogs_skip_treadmill() {
        let acts = [act("treadmill_run", "2026-12-02T18:00:00Z", 5.0, 1500.0)];
        assert_eq!(month(&acts, DOG_RUN_TYPES, dec(14)).km, 0.0);
    }

    #[test]
    fn pace_ignores_runs_without_timer() {
        let mut untimed = act("run", "2026-12-03T07:00:00Z", 5.0, 0.0);
        untimed.total_timer_time = None;
        let timed = act("run", "2026-12-02T07:00:00Z", 5.0, 1500.0);
        let m = month(&[timed, untimed.clone()], MY_RUN_TYPES, dec(14));
        assert_eq!((m.km, m.pace_s_per_km), (10.0, Some(300)));
        let m = month(&[untimed], MY_RUN_TYPES, dec(14));
        assert_eq!((m.km, m.pace_s_per_km), (5.0, None));
    }

    #[test]
    fn empty_month_has_no_pace() {
        let m = month(&[], MY_RUN_TYPES, dec(1));
        assert_eq!((m.km, m.pace_s_per_km), (0.0, None));
    }
}
