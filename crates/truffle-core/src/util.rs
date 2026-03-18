/// Current time in milliseconds since Unix epoch.
pub fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn util_current_timestamp_ms() {
        let ts = current_timestamp_ms();
        assert!(ts > 1_700_000_000_000); // After 2023
    }

    /// CS-9: Verify current_timestamp_ms returns reasonable values.
    #[test]
    fn current_timestamp_ms_in_reasonable_range() {
        let ts = current_timestamp_ms();
        // Must be after 2020-01-01 (1577836800000)
        assert!(ts > 1_577_836_800_000, "Timestamp should be after 2020-01-01");
        // Must be before 2100-01-01 (4102444800000)
        assert!(ts < 4_102_444_800_000, "Timestamp should be before 2100-01-01");
    }

    /// CS-9: Two consecutive calls should be monotonically non-decreasing.
    #[test]
    fn current_timestamp_ms_monotonic() {
        let ts1 = current_timestamp_ms();
        let ts2 = current_timestamp_ms();
        assert!(ts2 >= ts1, "Consecutive timestamps should be non-decreasing");
    }
}
