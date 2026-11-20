//! Command-line parsing for the `zcall-native` entry.

use std::path::PathBuf;

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The token Zalo passes on the command line.
    pub token: String,
    /// Engine -> Zalo TCP port.
    pub recv_port: u16,
    /// Zalo -> engine TCP port.
    pub send_port: u16,
    /// Path to the user's copy of main.js (holds the transport key).
    pub key_path: PathBuf,
    /// Media mode: "stub" (signalling only) or "zrtc" (real UDP).
    pub media: String,
    /// Whether UDP packets actually leave the machine.
    pub send_udp: bool,
    /// Run the local audio self-test instead of connecting to Zalo.
    pub selftest: bool,
    /// Extra logging.
    pub verbose: bool,
}

impl Args {
    /// Parses from an argument list (without the program name).
    pub fn parse_from(args: &[String]) -> Result<Self, String> {
        let selftest = args.iter().any(|a| a == "--selftest");
        let token = opt(args, "--token").unwrap_or_default();
        if token.is_empty() && !selftest {
            return Err("need --token <TK>".to_owned());
        }
        let recv_port = opt(args, "--recv")
            .map(|s| s.parse().unwrap_or(29631))
            .unwrap_or(29631);
        let send_port = opt(args, "--send")
            .map(|s| s.parse().unwrap_or(29632))
            .unwrap_or(29632);
        let key_path = opt(args, "--key")
            .map(PathBuf::from)
            .unwrap_or_else(default_main_js);
        let media = opt(args, "--media").unwrap_or_else(|| "stub".to_owned());
        let send_udp = args.iter().any(|a| a == "--send-udp");
        let verbose = args.iter().any(|a| a == "--verbose");
        Ok(Self {
            token,
            recv_port,
            send_port,
            key_path,
            media,
            send_udp,
            selftest,
            verbose,
        })
    }
}

/// Zalo's main.js location, as `defaultMainJs` does.
pub fn default_main_js() -> PathBuf {
    if let Ok(path) = std::env::var("ZALO_MAIN_JS") {
        return PathBuf::from(path);
    }
    let base = std::env::var("ZALO_LINUX_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/Zalo-Linux")
    });
    PathBuf::from(base).join("app/main-dist/main.js")
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_full_command_line() {
        let parsed = Args::parse_from(&args(&[
            "--token",
            "tk",
            "--recv",
            "1111",
            "--send",
            "2222",
            "--key",
            "/tmp/main.js",
            "--verbose",
        ]))
        .expect("parse");
        assert_eq!(parsed.token, "tk");
        assert_eq!(parsed.recv_port, 1111);
        assert_eq!(parsed.send_port, 2222);
        assert_eq!(parsed.key_path, PathBuf::from("/tmp/main.js"));
        assert_eq!(parsed.media, "stub");
        assert!(!parsed.send_udp);
        assert!(parsed.verbose);
    }

    #[test]
    fn defaults_ports_and_requires_a_token() {
        let parsed = Args::parse_from(&args(&["--token", "tk"])).expect("parse");
        assert_eq!(parsed.recv_port, 29631);
        assert_eq!(parsed.send_port, 29632);
        assert_eq!(parsed.media, "stub");
        assert!(!parsed.send_udp);
        assert!(!parsed.verbose);
        assert!(Args::parse_from(&args(&["--recv", "1"])).is_err());
    }

    #[test]
    fn selftest_needs_no_token() {
        let parsed = Args::parse_from(&args(&["--selftest"])).expect("parse");
        assert!(parsed.selftest);
        assert!(parsed.token.is_empty());
    }

    #[test]
    fn parses_media_zrtc_and_send_udp() {
        let parsed = Args::parse_from(&args(&["--token", "tk", "--media", "zrtc", "--send-udp"]))
            .expect("parse");
        assert_eq!(parsed.media, "zrtc");
        assert!(parsed.send_udp);
    }
}
