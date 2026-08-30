//! The one rule for where a credential may be sent.
//!
//! Every token exchange and refresh posts a client secret, an authorization
//! code or a refresh token as a form body. The provider defaults are all
//! `https://`, but `OAuthConfig` is data — a config file, a test, a future
//! provider — and nothing else in the crate refused a plain `http://` token
//! endpoint. This does, before the request is built, so a secret is never
//! handed to a transport that would put it on the wire in the clear.
//!
//! Loopback is the one exception: the tests stand up a mock server on
//! `127.0.0.1`, and a request to the machine's own loopback interface never
//! leaves it.

use super::store::CredentialError;

/// Refuse to send credentials to `url` unless the transport is TLS or the
/// destination is the local loopback interface.
pub(crate) fn require_tls(url: &str) -> Result<(), CredentialError> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        // Drop userinfo and port; keep IPv6 brackets intact.
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let host = if let Some(h) = host.strip_prefix('[') {
            h.split(']').next().unwrap_or("")
        } else {
            host.split(':').next().unwrap_or("")
        };
        // A parsed address, not a prefix test: `127.0.0.1.evil.example` is a
        // hostname, and only an actual loopback address stays on this machine.
        let is_loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if is_loopback {
            return Ok(());
        }
    }
    Err(CredentialError::Invalid(format!(
        "refusing to send credentials to {url}: the token endpoint must be https:// \
         (plain http is allowed only for the loopback interface)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_accepted() {
        assert!(require_tls("https://api.linear.app/oauth/token").is_ok());
        assert!(require_tls("https://auth.atlassian.com/oauth/token").is_ok());
    }

    #[test]
    fn loopback_over_http_is_accepted_for_mock_servers() {
        assert!(require_tls("http://127.0.0.1:41823/token").is_ok());
        assert!(require_tls("http://localhost/token").is_ok());
        assert!(require_tls("http://[::1]:8080/token").is_ok());
        assert!(require_tls("http://user:pw@127.0.0.1/token").is_ok());
    }

    #[test]
    fn plain_http_to_anywhere_else_is_refused_and_says_why() {
        for url in [
            "http://api.linear.app/oauth/token",
            "http://127.0.0.1.evil.example/token",
            "http://localhost.evil.example/token",
            "ftp://example.test/token",
            "api.linear.app/oauth/token",
        ] {
            let err = require_tls(url).expect_err(url);
            let msg = err.to_string();
            assert!(msg.contains("must be https://"), "{url}: {msg}");
            assert!(msg.contains(url), "{url}: {msg}");
        }
    }
}
