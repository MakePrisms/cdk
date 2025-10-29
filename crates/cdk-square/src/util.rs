//! Utility functions and constants for Square integration

/// KV Store namespace for Square backend data
pub const SQUARE_KV_PRIMARY_NAMESPACE: &str = "cdk_square_backend";
/// KV Store secondary namespace for lightning invoice tracking
pub const SQUARE_KV_SECONDARY_NAMESPACE: &str = "lightning_invoices";
/// KV Store secondary namespace for Square webhook configuration
pub const SQUARE_KV_CONFIG_NAMESPACE: &str = "config";
/// Prefix for invoice hash keys in KV store
pub const INVOICE_HASH_PREFIX: &str = "invoice_hash_";
/// Key for storing Square webhook signature key
pub const SIGNATURE_KEY_STORAGE_KEY: &str = "signature_key";
/// Key for storing last payment sync timestamp
pub const LAST_SYNC_TIME_KEY: &str = "last_sync_time";

/// Convert RFC 3339 format to unix timestamp (seconds)
///
/// Returns None if the string is not a valid RFC 3339 timestamp
pub fn rfc3339_to_unix(rfc3339: &str) -> Option<u64> {
    // Expected format: YYYY-MM-DDTHH:MM:SSZ
    let parts: Vec<&str> = rfc3339.split('T').collect();
    if parts.len() != 2 {
        return None;
    }

    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }

    let year: u64 = date_parts[0].parse().ok()?;
    let month: u64 = date_parts[1].parse().ok()?;
    let day: u64 = date_parts[2].parse().ok()?;

    let time_str = parts[1].trim_end_matches('Z');
    let time_parts: Vec<&str> = time_str.split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }

    let hours: u64 = time_parts[0].parse().ok()?;
    let minutes: u64 = time_parts[1].parse().ok()?;
    let seconds: u64 = time_parts[2].parse().ok()?;

    // Calculate days since Unix epoch
    let mut days = 0u64;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }

    let days_in_months = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    for &days_in_month in days_in_months.iter().take((month - 1) as usize) {
        days += days_in_month as u64;
    }

    days += day - 1; // day is 1-indexed

    const SECONDS_PER_DAY: u64 = 86400;
    const SECONDS_PER_HOUR: u64 = 3600;
    const SECONDS_PER_MINUTE: u64 = 60;

    Some(days * SECONDS_PER_DAY + hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE + seconds)
}

/// Convert unix timestamp (seconds) to RFC 3339 format
///
/// Returns a string in the format: YYYY-MM-DDTHH:MM:SSZ
pub fn unix_to_rfc3339(unix_secs: u64) -> String {
    const SECONDS_PER_DAY: u64 = 86400;
    const SECONDS_PER_HOUR: u64 = 3600;
    const SECONDS_PER_MINUTE: u64 = 60;

    // Days since Unix epoch (1970-01-01)
    let mut days = unix_secs / SECONDS_PER_DAY;
    let remainder = unix_secs % SECONDS_PER_DAY;

    let hours = remainder / SECONDS_PER_HOUR;
    let minutes = (remainder % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE;
    let seconds = remainder % SECONDS_PER_MINUTE;

    // Calculate year, month, day from days since epoch
    // Start from 1970
    let mut year = 1970;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    // Now days is the day of year (0-indexed)
    let days_in_months = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    for (m, &days_in_month) in days_in_months.iter().enumerate() {
        if days < days_in_month as u64 {
            month = m + 1;
            break;
        }
        days -= days_in_month as u64;
    }

    let day = days + 1; // Convert from 0-indexed to 1-indexed

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Check if a year is a leap year
pub fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_leap_year() {
        assert!(is_leap_year(2000)); // Divisible by 400
        assert!(is_leap_year(2024)); // Divisible by 4, not by 100
        assert!(!is_leap_year(1900)); // Divisible by 100, not by 400
        assert!(!is_leap_year(2023)); // Not divisible by 4
    }

    #[test]
    fn test_unix_to_rfc3339() {
        // Test epoch
        assert_eq!(unix_to_rfc3339(0), "1970-01-01T00:00:00Z");

        // Test specific known dates
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(unix_to_rfc3339(1704067200), "2024-01-01T00:00:00Z");

        // Test with time components
        // 2024-06-15 14:40:45 UTC = 1718462445
        assert_eq!(unix_to_rfc3339(1718462445), "2024-06-15T14:40:45Z");
    }

    #[test]
    fn test_rfc3339_to_unix() {
        // Test epoch
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));

        // Test specific known dates
        assert_eq!(rfc3339_to_unix("2024-01-01T00:00:00Z"), Some(1704067200));

        // Test with time components
        assert_eq!(rfc3339_to_unix("2024-06-15T14:40:45Z"), Some(1718462445));

        // Test invalid formats
        assert_eq!(rfc3339_to_unix("invalid"), None);
        assert_eq!(rfc3339_to_unix("2024-01-01"), None);
    }

    #[test]
    fn test_rfc3339_roundtrip() {
        // Test roundtrip conversion
        let timestamps = vec![0, 1704067200, 1718462445];
        for ts in timestamps {
            let rfc = unix_to_rfc3339(ts);
            let back = rfc3339_to_unix(&rfc);
            assert_eq!(back, Some(ts), "Roundtrip failed for timestamp {}", ts);
        }
    }
}
