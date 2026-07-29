use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DateConfidence {
    High,
    Medium,
    Unknown,
}

/// Coarse classification of what a result reports, shown as a badge on the
/// frontend timeline (ADR-027).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Announcement,
    Release,
    Funding,
    Legal,
    Incident,
    Research,
    Opinion,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_at: Option<DateTime<Utc>>,
    pub date_confidence: DateConfidence,
    // Timeline enrichment (ADR-027); defaults keep older payloads parseable.
    #[serde(default)]
    pub event_type: EventType,
    #[serde(default)]
    pub summary: Option<String>,
    /// False when a recurring run already saw this URL (ADR-033); one-shot
    /// searches leave every result new. Default keeps older payloads working.
    #[serde(default = "default_true")]
    pub is_new: bool,
    #[serde(default)]
    pub raw: serde_json::Value,
}

fn default_true() -> bool {
    true
}

/// Sorts results by publication date, newest first; results without a date go last
/// (they are displayed in a separate "unknown date" section, see ADR-011).
pub fn sort_by_publication_date(results: &mut [SearchResult]) {
    // Reverse(Option<_>) sorts Some(newest) first and None last, which is
    // exactly the ADR-011 ordering.
    results.sort_by_key(|r| std::cmp::Reverse(r.published_at));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn result(title: &str, published_at: Option<DateTime<Utc>>) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: format!("https://example.com/{title}"),
            snippet: String::new(),
            published_at,
            date_confidence: if published_at.is_some() {
                DateConfidence::High
            } else {
                DateConfidence::Unknown
            },
            event_type: EventType::default(),
            summary: None,
            is_new: true,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn older_payloads_without_enrichment_still_deserialize() {
        // Pre-ADR-027 wire shape: no event_type, no summary.
        let json = r#"{"title":"t","url":"https://t","snippet":"","published_at":null,"date_confidence":"unknown"}"#;
        let parsed: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.event_type, EventType::Other);
        assert_eq!(parsed.summary, None);
    }

    fn date(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn sorts_newest_first_with_unknown_dates_last() {
        let mut results = vec![
            result("old", Some(date(2023, 1, 1))),
            result("no-date", None),
            result("new", Some(date(2026, 6, 1))),
            result("mid", Some(date(2025, 3, 15))),
        ];

        sort_by_publication_date(&mut results);

        let titles: Vec<&str> = results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["new", "mid", "old", "no-date"]);
    }
}
