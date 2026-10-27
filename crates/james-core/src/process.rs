//! Helpers for spawning external tools (ffmpeg, yt-dlp, …).

/// Environment variables through which tools pick up (or bypass) proxies.
const PROXY_ENV: &[&str] = &[
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "ftp_proxy",
    "FTP_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// Removes inherited proxy settings (including `no_proxy`, which would let targets bypass the
/// egress proxy) and, if given, routes all HTTP(S) traffic through `proxy`.
///
/// The proxy URL carries a credential, so it is passed in the environment, which other local
/// users cannot read, instead of on the command line.
pub fn apply_proxy_env(cmd: &mut tokio::process::Command, proxy: Option<&str>) {
    for var in PROXY_ENV {
        cmd.env_remove(var);
    }
    if let Some(proxy) = proxy {
        // Every protocol a tool might use is routed to the proxy (which refuses non-HTTP).
        for var in [
            "http_proxy",
            "https_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ftp_proxy",
            "FTP_PROXY",
            "all_proxy",
            "ALL_PROXY",
        ] {
            cmd.env(var, proxy);
        }
    }
}
