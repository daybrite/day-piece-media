// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
use serde::{Deserialize, Serialize};

/// A closed range of media time, in seconds. Live streams may expose a moving DVR window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

/// Capabilities of the current backend/source, not a promise about installed codecs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaCapabilities {
    pub seek: bool,
    pub rate: bool,
    pub volume: bool,
    /// No current backend promises sample-accurate transitions between sources.
    pub gapless: bool,
    /// No current backend exposes an audio normalization/limiter pipeline.
    pub normalization: bool,
}

/// Native observations. Unknown duration is `None`; an empty range list is not seekable/buffered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaTimeline {
    pub position: f64,
    pub duration: Option<f64>,
    /// Actual configured speed (even while paused), not zero when paused.
    pub rate: f64,
    pub seekable: Vec<TimeRange>,
    pub buffered: Vec<TimeRange>,
    pub seeking: bool,
    pub capabilities: MediaCapabilities,
}
impl Default for MediaTimeline {
    fn default() -> Self {
        Self {
            position: 0.0,
            duration: None,
            rate: 1.0,
            seekable: vec![],
            buffered: vec![],
            seeking: false,
            capabilities: MediaCapabilities::default(),
        }
    }
}
impl MediaTimeline {
    pub(crate) fn decode(text: &str) -> Option<Self> {
        let mut value: Self = serde_json::from_str(text).ok()?;
        if !value.position.is_finite() || value.position < 0.0 || !super::valid_rate(value.rate) {
            return None;
        }
        value.duration = value.duration.filter(|d| d.is_finite() && *d >= 0.0);
        for ranges in [&mut value.seekable, &mut value.buffered] {
            ranges.retain(|r| {
                r.start.is_finite() && r.end.is_finite() && r.start >= 0.0 && r.end >= r.start
            });
        }
        Some(value)
    }
    #[allow(dead_code)] // used by native Rust backends
    pub(crate) fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    /// Clamp a request into the nearest available range, including discontinuous live ranges.
    pub fn seek_target(&self, seconds: f64) -> Option<f64> {
        if !self.capabilities.seek || !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        self.seekable
            .iter()
            .filter(|r| {
                r.start.is_finite() && r.end.is_finite() && r.start >= 0.0 && r.end >= r.start
            })
            .map(|r| seconds.clamp(r.start, r.end))
            .min_by(|a, b| (a - seconds).abs().total_cmp(&(b - seconds).abs()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaErrorKind {
    InvalidInput,
    Unsupported,
    Playback,
    Command,
}
/// Diagnostic detail is for logging. Applications map `kind`/`operation` to localized messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaError {
    pub kind: MediaErrorKind,
    pub operation: String,
    pub detail: String,
}
impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}: {}", self.operation, self.kind, self.detail)
    }
}
impl std::error::Error for MediaError {}
impl MediaError {
    pub(crate) fn invalid(operation: &str) -> Self {
        Self {
            kind: MediaErrorKind::InvalidInput,
            operation: operation.into(),
            detail: String::new(),
        }
    }
    pub(crate) fn command(text: &str) -> Self {
        let (operation, detail) = text.split_once(':').unwrap_or((text, ""));
        Self {
            kind: if detail == "unsupported" {
                MediaErrorKind::Unsupported
            } else {
                MediaErrorKind::Command
            },
            operation: operation.into(),
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_ranges_are_filtered_and_public_values_cannot_panic() {
        let mut timeline = MediaTimeline::decode(r#"{"position":1,"seekable":[{"start":10,"end":2},{"start":-1,"end":3}],"capabilities":{"seek":true}}"#).unwrap();
        assert_eq!(timeline.seek_target(5.0), None);
        timeline.seekable.push(TimeRange {
            start: f64::NAN,
            end: 3.0,
        });
        assert_eq!(timeline.seek_target(5.0), None);
        assert!(MediaTimeline::decode(r#"{"position":-1}"#).is_none());
    }
    #[test]
    fn live_ranges_and_unknown_duration() {
        let timeline = MediaTimeline::decode(r#"{"position":42,"duration":null,"seekable":[{"start":20,"end":30},{"start":40,"end":60}],"capabilities":{"seek":true}}"#).unwrap();
        assert_eq!(timeline.duration, None);
        assert_eq!(timeline.seek_target(0.0), Some(20.0));
        assert_eq!(timeline.seek_target(38.0), Some(40.0));
        assert_eq!(timeline.seek_target(100.0), Some(60.0));
        assert_eq!(timeline.seek_target(f64::NAN), None);
        assert!(MediaTimeline::decode(r#"{"rate":0}"#).is_none());
    }
}
