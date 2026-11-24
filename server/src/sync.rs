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
    /// Set while the latest Dropbox import failed.
    pub import_error: Option<String>,
}

impl SyncStatus {
    pub fn snapshot(&self) -> Snapshot {
        self.0.read().expect("sync lock").clone()
    }

    pub fn set_import_error(&self, err: Option<String>) {
        self.set(|st| st.import_error = err);
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
