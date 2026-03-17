//! CLI for the wallhack daemon.
//!
//! Node role is declared explicitly via subcommand (`entry`, `exit`, `relay`).
//! Transport direction (`--listen` / `--connect`) is independent of role.
//!
//! # Examples
//!
//! ```text
//! wallhack entry --listen :6565
//! wallhack entry --connect host:443
//! wallhack exit --connect host:6565
//! wallhack exit --listen :443
//! wallhack relay --connect up:443 --listen :6565
//! ```

use std::{path::PathBuf, time::Duration};

use argh::FromArgs;
use wallhack_wire::data::{HintLevel, NodeRole as ProtoNodeRole, RoleHint};
use wallhackd::{
    address_spec::AddressSpec,
    daemon_config::{ApiConfig, AutoConfig, DaemonConfig, GlobalConfig, ModeConfig, TlsParams},
};

/// Network pivoting and tunneling tool.
///
/// Auto-negotiates role from `--connect` / `--listen` flags. Use a subcommand
/// (`entry`, `exit`, `relay`) to override with an explicit role.
#[allow(clippy::struct_excessive_bools)] // Independent CLI flags, not related state
#[derive(FromArgs, Debug, Clone)]
pub struct WallhackCli {
    /// daemon socket path (overrides `WALLHACK_HOST` env var)
    #[argh(option, short = 'H')]
    pub host: Option<String>,

    /// TLS certificate file
    #[argh(option)]
    pub cert: Option<PathBuf>,

    /// TLS private key file
    #[argh(option)]
    pub key: Option<PathBuf>,

    /// CA roots file for mTLS verification
    #[argh(option)]
    pub ca: Option<PathBuf>,

    /// DNS server for target resolution
    #[argh(option, short = 'd')]
    pub dns: Option<String>,

    /// TLS hostname for verification (defaults to target hostname)
    #[argh(option)]
    pub hostname: Option<String>,

    /// connection timeout in seconds
    #[argh(option, short = 't', default = "10")]
    pub timeout: u64,

    /// pre-shared key for tunnel authentication (or set `WALLHACK_PSK` env var)
    #[argh(option)]
    pub psk: Option<String>,

    /// connect to a peer for auto-negotiated mode (e.g. "host:6565")
    #[argh(option, short = 'c')]
    pub connect: Option<String>,

    /// listen for connections for auto-negotiated mode (e.g. ":6565")
    #[argh(option, short = 'l')]
    pub listen: Option<String>,

    /// node name for auto-negotiated mode (random if omitted)
    #[argh(option, short = 'n')]
    pub name: Option<String>,

    /// accept server certificate by fingerprint
    #[argh(option)]
    pub accept_fingerprint: Option<String>,

    /// REST API listen address (e.g. "127.0.0.1:6564")
    #[argh(option)]
    pub api: Option<String>,

    /// REST API username for basic auth (default: admin)
    #[argh(option)]
    pub api_user: Option<String>,

    /// REST API secret for basic auth (default: auto-generated)
    #[argh(option)]
    pub api_secret: Option<String>,

    /// maximum number of concurrent peer connections
    #[argh(option)]
    pub max_peers: Option<usize>,

    /// prefer a role during auto-negotiation (entry, exit, relay)
    #[argh(option)]
    pub prefer_role: Option<String>,

    /// exclude a role during auto-negotiation (entry, exit, relay)
    #[argh(option)]
    pub exclude_role: Option<String>,

    /// override the negotiated role (entry, exit, relay)
    #[argh(option)]
    pub role: Option<String>,

    /// verbose output
    #[argh(switch, short = 'v')]
    pub verbose: bool,

    /// extra verbose (debug level tracing)
    #[argh(switch)]
    pub debug: bool,

    /// comma-separated module substring filters for debug tracing
    #[argh(option)]
    pub debug_filter: Option<String>,

    /// trace level tracing (most verbose)
    #[argh(switch)]
    pub trace: bool,

    /// comma-separated module substring filters for trace tracing
    #[argh(option)]
    pub trace_filter: Option<String>,

    /// quiet mode (errors only)
    #[argh(switch, short = 'q')]
    pub quiet: bool,

    /// print version information and exit
    #[argh(switch)]
    pub version: bool,
}

/// Generate a random node name (8-character hex ID).
fn generate_node_name() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let id: u32 = rng.random();
    format!("{id:08x}")
}

/// Extract the binary name from argv[0], like argh does internally.
fn binary_name(argv0: &str) -> &str {
    std::path::Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv0)
}

/// CLI parse error or informational output (help, version).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CliError {
    pub message: String,
    /// 0 for informational output (--help, --version), 1 for parse errors.
    pub exit_code: i32,
}

/// Configuration build error.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConfigError {
    #[error("--prefer, --exclude-role, and --role are mutually exclusive")]
    HintFlagsConflict,
    #[error("--role entry requires TUN capability (CAP_NET_ADMIN)")]
    RoleEntryRequiresTun,
    #[error("--role relay requires both --connect and --listen")]
    RoleRelayRequiresConnectAndListen,
    #[error("invalid role '{0}': expected 'entry', 'exit', or 'relay'")]
    InvalidRole(String),
    #[error("invalid address '{0}'")]
    InvalidAddress(String),
}

/// Parse CLI from explicit arguments.
///
/// Wraps argh with:
/// - Global flag reordering (allows `wallhack entry --debug` in addition to `wallhack --debug entry`)
/// - Better error messages when subcommand-level flags are used without a subcommand
///
/// # Errors
///
/// Returns [`CliError`] for parse errors or informational output (--help).
pub fn parse_cli_from_args(args: &[String]) -> Result<WallhackCli, CliError> {
    let cmd = binary_name(&args[0]);
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();

    WallhackCli::from_args(&[cmd], &strs[1..]).map_err(|early_exit| {
        if early_exit.status.is_err() {
            let message = format!(
                "{}\nRun {cmd} --help for more information.",
                early_exit.output
            );
            CliError {
                message,
                exit_code: 1,
            }
        } else {
            CliError {
                message: early_exit.output,
                exit_code: 0,
            }
        }
    })
}

/// Parse a role string (case-insensitive) into a proto `NodeRole`.
fn parse_role(s: &str) -> Result<ProtoNodeRole, ConfigError> {
    match s.to_ascii_lowercase().as_str() {
        "entry" => Ok(ProtoNodeRole::RoleEntry),
        "exit" => Ok(ProtoNodeRole::RoleExit),
        "relay" => Ok(ProtoNodeRole::RoleRelay),
        _ => Err(ConfigError::InvalidRole(s.to_string())),
    }
}

/// Build a `RoleHint` from the mutually-exclusive hint CLI flags.
fn resolve_hint(cli: &WallhackCli) -> Result<Option<RoleHint>, ConfigError> {
    let hints: Vec<_> = [
        cli.prefer_role.as_deref().map(|s| (HintLevel::Prefer, s)),
        cli.exclude_role.as_deref().map(|s| (HintLevel::Exclude, s)),
        cli.role.as_deref().map(|s| (HintLevel::Fixed, s)),
    ]
    .into_iter()
    .flatten()
    .collect();

    match hints.len() {
        0 => Ok(None),
        1 => {
            let (level, role_str) = hints[0];
            let target = parse_role(role_str)?;
            Ok(Some(RoleHint {
                level: level.into(),
                target: target.into(),
            }))
        }
        _ => Err(ConfigError::HintFlagsConflict),
    }
}

/// Resolve PSK from flag or `WALLHACK_PSK` environment variable.
fn resolve_psk(psk: Option<&String>) -> Option<String> {
    psk.cloned().or_else(|| std::env::var("WALLHACK_PSK").ok())
}

/// Resolve API credentials, generating a random secret if not provided.
fn resolve_api_config(
    api: Option<&String>,
    api_user: Option<&String>,
    api_secret: Option<&String>,
) -> Option<ApiConfig> {
    // Any --api* flag triggers the API — not just --api.
    if api.is_none() && api_user.is_none() && api_secret.is_none() {
        return None;
    }

    let addr = api
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::net::SocketAddr::from(([127, 0, 0, 1], 6564)));

    let user = api_user.cloned().unwrap_or_else(|| "admin".to_string());

    let secret = if let Some(s) = api_secret {
        s.clone()
    } else {
        use rand::Rng;
        const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut rng = rand::rng();
        let secret: String = (0..32)
            .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
            .collect();
        secret
    };

    Some(ApiConfig { addr, user, secret })
}

/// Validate startup constraints for `--role` hints.
fn validate_fixed_hint(
    hint: Option<&RoleHint>,
    connect: Option<&AddressSpec>,
    listen: Option<&AddressSpec>,
) -> Result<(), ConfigError> {
    if let Some(h) = hint
        && h.level == i32::from(HintLevel::Fixed)
    {
        let target = ProtoNodeRole::try_from(h.target).unwrap_or(ProtoNodeRole::RoleIndeterminate);
        if target == ProtoNodeRole::RoleEntry && !wallhackd::detect_tun_capable() {
            return Err(ConfigError::RoleEntryRequiresTun);
        }
        if target == ProtoNodeRole::RoleRelay && (connect.is_none() || listen.is_none()) {
            return Err(ConfigError::RoleRelayRequiresConnectAndListen);
        }
    }
    Ok(())
}

/// Build a [`DaemonConfig`] from parsed CLI arguments.
///
/// Resolves PSK, generates names, parses transport directions, and maps
/// API options. When top-level `--connect` / `--listen` flags are used
/// without a subcommand, auto-negotiation mode is selected.
///
/// # Errors
///
/// Returns error if transport direction parsing fails.
pub fn build_daemon_config(cli: &WallhackCli) -> Result<DaemonConfig, ConfigError> {
    let global = GlobalConfig {
        tls: TlsParams {
            cert: cli.cert.clone(),
            key: cli.key.clone(),
            ca: cli.ca.clone(),
        },
        hostname: cli.hostname.clone(),
        dns_server: cli.dns.clone(),
        timeout: Duration::from_secs(cli.timeout),
        psk: resolve_psk(cli.psk.as_ref()).map(zeroize::Zeroizing::new),
        version: crate::version::version(),
    };

    let hint = resolve_hint(cli)?;

    let connect = cli
        .connect
        .as_ref()
        .map(|s| {
            s.parse::<AddressSpec>()
                .map_err(ConfigError::InvalidAddress)
        })
        .transpose()?;
    let listen = cli
        .listen
        .as_ref()
        .map(|s| {
            s.parse::<AddressSpec>()
                .map_err(ConfigError::InvalidAddress)
        })
        .transpose()?;

    validate_fixed_hint(hint.as_ref(), connect.as_ref(), listen.as_ref())?;

    let mode = ModeConfig::Auto(AutoConfig {
        name: cli.name.clone().unwrap_or_else(generate_node_name),
        connect,
        listen,
        accept_fingerprint: cli.accept_fingerprint.clone(),
        hint,
        api: resolve_api_config(
            cli.api.as_ref(),
            cli.api_user.as_ref(),
            cli.api_secret.as_ref(),
        ),
        max_peers: cli.max_peers,
    });

    Ok(DaemonConfig { global, mode })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse CLI from a list of argument strings.
    fn cli(args: &[&str]) -> Result<WallhackCli, CliError> {
        let mut v = vec!["wallhackd".to_string()];
        v.extend(args.iter().map(|s| (*s).to_string()));
        parse_cli_from_args(&v)
    }

    #[test]
    fn mutually_exclusive_hint_flags() {
        let c = cli(&[
            "--prefer-role",
            "entry",
            "--role",
            "exit",
            "--listen",
            ":6565",
        ])
        .unwrap();
        assert_eq!(
            build_daemon_config(&c).unwrap_err(),
            ConfigError::HintFlagsConflict
        );
    }

    #[test]
    fn role_entry_requires_tun() {
        // Only testable on machines without CAP_NET_ADMIN.
        if !wallhackd::detect_tun_capable() {
            let c = cli(&["--role", "entry", "--listen", ":6565"]).unwrap();
            assert_eq!(
                build_daemon_config(&c).unwrap_err(),
                ConfigError::RoleEntryRequiresTun
            );
        }
    }

    #[test]
    fn role_relay_requires_connect_and_listen() {
        let c = cli(&["--role", "relay", "--listen", ":6565"]).unwrap();
        assert_eq!(
            build_daemon_config(&c).unwrap_err(),
            ConfigError::RoleRelayRequiresConnectAndListen
        );
    }

    #[test]
    fn valid_prefer_hint_produces_auto_config() {
        let c = cli(&["--prefer-role", "entry", "--listen", ":6565"]).unwrap();
        let config = build_daemon_config(&c).unwrap();
        match &config.mode {
            ModeConfig::Auto(auto) => {
                let hint = auto.hint.as_ref().expect("hint should be set");
                assert_eq!(hint.level, i32::from(HintLevel::Prefer));
                assert_eq!(hint.target, i32::from(ProtoNodeRole::RoleEntry));
            }
            other => panic!("expected Auto, got {other:?}"),
        }
    }

    #[test]
    fn invalid_role_string_rejected() {
        let c = cli(&["--prefer-role", "bogus", "--listen", ":6565"]).unwrap();
        assert_eq!(
            build_daemon_config(&c).unwrap_err(),
            ConfigError::InvalidRole("bogus".to_string())
        );
    }
}
