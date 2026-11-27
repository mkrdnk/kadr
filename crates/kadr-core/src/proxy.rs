//! Validation shared by catalog proxy configuration and network boundaries.

use url::Host;

/// Accept only unauthenticated HTTP(S) or SOCKS5 proxy endpoints, not URLs
/// identifying a resource. An omitted port uses the scheme's default.
pub fn is_valid_endpoint(endpoint: &str) -> bool {
    if endpoint
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let Some((scheme, address)) = endpoint.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https" | "socks5" | "socks5h") {
        return false;
    }
    // Check the original authority before parsing: URL parsers may repair
    // backslashes, empty userinfo, empty ports, or dot-segment paths.
    let authority = address.strip_suffix('/').unwrap_or(address);
    if authority.is_empty() || authority.contains(['/', '\\', '@', '?', '#']) {
        return false;
    }
    let (host, port) = if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return false;
        };
        let (host, rest) = authority.split_at(end + 1);
        let port = if rest.is_empty() {
            None
        } else if let Some(port) = rest.strip_prefix(':') {
            Some(port)
        } else {
            return false;
        };
        (host, port)
    } else if let Some((host, port)) = authority.split_once(':') {
        (host, Some(port))
    } else {
        (authority, None)
    };
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|b| b.is_ascii_digit())
            || !matches!(port.parse::<u16>(), Ok(1..=u16::MAX)))
    {
        return false;
    }
    !host.is_empty() && Host::parse(host).is_ok()
}
