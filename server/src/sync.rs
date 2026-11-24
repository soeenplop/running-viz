//! Periodic stats refresh and its status (reported by `/healthz`).

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};

use crate::{
    display::{self, Display},
    endurain::Account,
    stats,
};

/// Outcome of the latest refreshes; cheap to clone.
#[derive(Clone, Default)]
pub struct SyncStatus(Arc<RwLock<Snapshot>>);

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Snapshot {
    pub last_success: Option<DateTime<Utc>>,
    /// Set while the latest refresh failed (no secrets: status + title only).
    pub last_error: Option<String>,
    /// Outcome of the latest Dropbox import: `None` = not run yet / disabled.
    pub import_ok: Option<bool>,
}

impl SyncStatus {
    pub fn snapshot(&self) -> Snapshot {
        self.0.read().expect("sync lock").clone()
    }

    pub fn set_import_ok(&self, ok: bool) {
        self.set(|st| st.import_ok = Some(ok));
    }

    fn set(&self, f: impl FnOnce(&mut Snapshot)) {
        f(&mut self.0.write().expect("sync lock"));
    }
}

/// One refresh: fetch the stats, update the display on success, record the outcome.
/// On failure the display keeps its last good image.
pub async fn refresh(accounts: &[Account], display: &Display, status: &SyncStatus) {
    let Some((me, dogs)) = accounts.split_first() else {
        return;
    };
    match stats::fetch(me, dogs, display::today()).await {
        Ok(s) => {
            display.update(&s);
            status.set(|st| {
                st.last_success = Some(Utc::now());
                st.last_error = None;
            });
        }
        Err(err) => {
            tracing::warn!("stats refresh failed: {err:#}");
            status.set(|st| st.last_error = Some(format!("{err}")));
        }
    }
}
