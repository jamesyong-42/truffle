/// Generate Tailscale hostname from prefix, device type, and ID.
/// Format: {prefix}-{type}-{id}
pub fn generate_hostname(prefix: &str, device_type: &str, id: &str) -> String {
    format!("{prefix}-{device_type}-{id}")
}

/// Parse device type and ID from Tailscale hostname.
/// Returns None if hostname doesn't match the pattern: {prefix}-{type}-{id}
pub fn parse_hostname(prefix: &str, hostname: &str) -> Option<ParsedHostname> {
    let suffix = hostname.strip_prefix(prefix)?.strip_prefix('-')?;
    let dash_pos = suffix.find('-')?;
    let device_type = &suffix[..dash_pos];
    let id = &suffix[dash_pos + 1..];

    if device_type.is_empty() || id.is_empty() {
        return None;
    }

    Some(ParsedHostname {
        device_type: device_type.to_string(),
        id: id.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHostname {
    pub device_type: String,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_hostname_basic() {
        assert_eq!(
            generate_hostname("app", "desktop", "abc123"),
            "app-desktop-abc123"
        );
    }

    #[test]
    fn parse_hostname_basic() {
        let result = parse_hostname("app", "app-desktop-abc123").unwrap();
        assert_eq!(result.device_type, "desktop");
        assert_eq!(result.id, "abc123");
    }

    #[test]
    fn parse_hostname_with_dashes_in_id() {
        let result = parse_hostname("app", "app-desktop-abc-123-def").unwrap();
        assert_eq!(result.device_type, "desktop");
        assert_eq!(result.id, "abc-123-def");
    }

    #[test]
    fn parse_hostname_wrong_prefix() {
        assert!(parse_hostname("app", "other-desktop-abc123").is_none());
    }

    #[test]
    fn parse_hostname_no_type() {
        assert!(parse_hostname("app", "app--abc123").is_none());
    }

    #[test]
    fn parse_hostname_no_id() {
        assert!(parse_hostname("app", "app-desktop-").is_none());
    }

    #[test]
    fn parse_hostname_just_prefix() {
        assert!(parse_hostname("app", "app").is_none());
    }

    #[test]
    fn roundtrip() {
        let hostname = generate_hostname("myapp", "server", "uuid-v4");
        let parsed = parse_hostname("myapp", &hostname).unwrap();
        assert_eq!(parsed.device_type, "server");
        assert_eq!(parsed.id, "uuid-v4");
    }
}
