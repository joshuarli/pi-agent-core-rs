use crate::render;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tea_core::Usage;

use super::state::AppState;

/// Format only provider-reported values; missing fields remain absent rather than zero.
pub fn format_usage(usage: &Usage) -> String {
    let mut fields = Vec::new();
    if let Some(value) = usage.input_tokens {
        fields.push(format!("in {value}"));
    }
    if let Some(value) = usage.output_tokens {
        fields.push(format!("out {value}"));
    }
    if let Some(value) = usage.reasoning_tokens {
        fields.push(format!("reasoning {value}"));
    }
    if let Some(value) = usage.cache_read_tokens {
        fields.push(format!("cache-read {value}"));
    }
    if let Some(value) = usage.cache_write_tokens {
        fields.push(format!("cache-write {value}"));
    }
    if let Some(value) = usage.cost.as_deref() {
        fields.push(format!("cost {value}"));
    }
    if fields.is_empty() {
        "provider reported no accounting".into()
    } else {
        fields.join(", ")
    }
}

/// Format every persistent-footer accounting field without ever substituting zero for unknown.
pub(super) fn format_footer_usage(usage: &Usage) -> String {
    format!(
        "in {} out {} reasoning {} cache-read {} cache-write {} cost {}",
        usage
            .input_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into()),
        usage
            .output_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into()),
        usage
            .reasoning_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into()),
        usage
            .cache_read_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into()),
        usage
            .cache_write_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into()),
        usage.cost.as_deref().unwrap_or("unknown"),
    )
}

pub(super) fn composer_cursor(state: &AppState, width: u16, height: u16) -> Option<(u16, u16)> {
    if state.picker.is_some() {
        return None;
    }
    render::composer_cursor_position(state, width, height)
}

/// Format today's UTC civil date without adding a date/time dependency.
pub(super) fn utc_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's public-domain civil-date conversion, expressed locally to
// keep Command Code host metadata explicit without a time crate.
pub(super) fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}
