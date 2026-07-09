use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DateConfidence {
    High,
    Medium,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_at: Option<DateTime<Utc>>,
    pub date_confidence: DateConfidence,
    #[serde(default)]
    pub raw: serde_json::Value,
}

/// Sorts results by publication date, newest first; results without a date go last
/// (they are displayed in a separate "unknown date" section, see ADR-011).
pub fn sort_by_publication_date(results: &mut [SearchResult]) {
    results.sort_by(|a, b| match (b.published_at, a.published_at) {
        (Some(b_date), Some(a_date)) => b_date.cmp(&a_date),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    });
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
            raw: serde_json::Value::Null,
        }
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
