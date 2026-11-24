//! The current display image, shared between the renderer and HTTP handlers.

use std::sync::{Arc, RwLock};

use chrono::{NaiveDate, Utc};
use chrono_tz::Europe::Berlin;

use crate::render::{Stats, render};

/// A rendered frame in both output formats.
pub struct Frame {
    /// Raw e-paper framebuffer (15 000 bytes).
    pub bin: Vec<u8>,
    /// Same image as PNG (native size).
    pub png: Vec<u8>,
    /// Quoted ETag derived from the pixels.
    pub etag: String,
}

impl Frame {
    pub fn new(stats: &Stats) -> Self {
        let bitmap = render(stats);
        Self {
            png: bitmap.to_png(1),
            etag: format!("\"{:016x}\"", bitmap.hash()),
            bin: bitmap.bytes().to_vec(),
        }
    }
}

/// Holds the latest frame; cheap to clone.
#[derive(Clone)]
pub struct Display(Arc<RwLock<Arc<Frame>>>);

impl Display {
    pub fn new(stats: &Stats) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(Frame::new(stats)))))
    }

    pub fn current(&self) -> Arc<Frame> {
        self.0.read().expect("display lock").clone()
    }

    /// Renders `stats`; replaces the frame only if the pixels changed.
    pub fn update(&self, stats: &Stats) {
        let frame = Frame::new(stats);
        let mut cur = self.0.write().expect("display lock");
        if cur.etag != frame.etag {
            *cur = Arc::new(frame);
        }
    }
}

/// Today's date in Europe/Berlin (the challenge's time zone).
pub fn today() -> NaiveDate {
    Utc::now().with_timezone(&Berlin).date_naive()
}
