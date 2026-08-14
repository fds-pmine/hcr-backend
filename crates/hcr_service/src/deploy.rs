//! Server assembly and configuration.
//!
//! Everything the binary and the example share, so a deployed server and a
//! development one differ only in the values they are handed — not in the code
//! that reads them.
//!
//! Enable with `--features hotaru`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::binding::Router;
use crate::hotaru_binding::{ROUTES, make_handler};

/// Runtime configuration, read from the environment.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `address:port` to listen on.
    pub binding: String,
    /// Origin permitted by CORS, or `None` to send no CORS headers at all.
    pub cors_allow_origin: Option<String>,
    /// HMAC key for item references.
    pub signing_key: Vec<u8>,
    /// Where to append the usage log, or `None` to collect nothing.
    pub usage_log_path: Option<PathBuf>,
}

/// Default listen address: loopback, so an unconfigured server is not exposed.
pub const DEFAULT_BINDING: &str = "127.0.0.1:18623";

/// Minimum key length accepted from configuration.
///
/// Not a policy invented here — HMAC-SHA256 offers nothing beyond the entropy of
/// its key, and a short one is guessable no matter how the algorithm is chosen.
pub const MIN_SIGNING_KEY_LEN: usize = 32;

/// Why a configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `HCR_SIGNING_KEY` was absent.
    ///
    /// Deliberately fatal rather than defaulted. A server that invents its own
    /// key on boot mints item references that every restart invalidates; one
    /// that falls back to a constant has no security at all, and would do so
    /// silently.
    MissingSigningKey,
    /// `HCR_SIGNING_KEY` was too short to be worth having.
    SigningKeyTooShort {
        /// What was supplied.
        len: usize,
    },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ConfigError::MissingSigningKey => write!(
                f,
                "HCR_SIGNING_KEY is required. Generate one with: \
                 openssl rand -hex 32"
            ),
            ConfigError::SigningKeyTooShort { len } => write!(
                f,
                "HCR_SIGNING_KEY is {len} bytes; at least {MIN_SIGNING_KEY_LEN} are required."
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl ServerConfig {
    /// Read configuration from the environment.
    ///
    /// | Variable | Default | Meaning |
    /// | --- | --- | --- |
    /// | `HCR_BIND` | `127.0.0.1:18623` | Listen address. |
    /// | `HCR_CORS_ORIGIN` | none | Comma-separated browser origins allowed to call this API. |
    /// | `HCR_SIGNING_KEY` | **required** | HMAC key for item references. |
    /// | `HCR_USAGE_LOG` | none | Path to append usage events to. |
    ///
    /// `HCR_CORS_ORIGIN` defaults to *absent*, meaning no CORS headers are sent.
    /// A permissive default on a scoring API is not something to ship by
    /// accident, and a same-origin deployment needs none. When set, it is an
    /// allowlist: the request's own `Origin` is echoed back if it appears on the
    /// list, and ignored otherwise. More than one is supported because the
    /// hosted site and the desktop build have different origins and neither can
    /// be expressed as the other.
    pub fn from_env() -> Result<Self, ConfigError> {
        let signing_key = std::env::var("HCR_SIGNING_KEY")
            .map_err(|_| ConfigError::MissingSigningKey)?
            .into_bytes();
        if signing_key.len() < MIN_SIGNING_KEY_LEN {
            return Err(ConfigError::SigningKeyTooShort {
                len: signing_key.len(),
            });
        }

        Ok(Self {
            binding: std::env::var("HCR_BIND").unwrap_or_else(|_| DEFAULT_BINDING.to_string()),
            cors_allow_origin: std::env::var("HCR_CORS_ORIGIN")
                .ok()
                .map(|origin| origin.trim().to_string())
                .filter(|origin| !origin.is_empty()),
            signing_key,
            // Off unless asked for. Collecting usage is a decision a deployment
            // makes and discloses, not a default it inherits.
            usage_log_path: std::env::var("HCR_USAGE_LOG")
                .ok()
                .map(|path| path.trim().to_string())
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
        })
    }
}

/// Check the listen address is actually available.
///
/// hotaru panics with `Failed to bind inbound transport` and no address, deep
/// inside `run_server`, which under systemd becomes `status=101` and a restart
/// loop with nothing in the journal to explain it. Binding here first turns that
/// into one sentence naming the port and the command to find the squatter.
///
/// There is a race — the port could be taken between this check and the real
/// bind — but this is a diagnostic, not a lock. Losing the race gets the old
/// panic, which is no worse than not having checked.
pub fn check_binding(binding: &str) -> Result<(), String> {
    match std::net::TcpListener::bind(binding) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err(format!(
            "{binding} is already in use by another process.\n\
             \n\
             Find it with:   ss -lntp | grep {}\n\
             Then either stop it, or set HCR_BIND to a free port in\n\
             /etc/hcr-server.env and point the reverse proxy at the new one.",
            binding.rsplit(':').next().unwrap_or(binding)
        )),
        Err(error) => Err(format!("cannot bind {binding}: {error}")),
    }
}

/// How often idle sessions and finished rounds are swept.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Start the background sweeper.
///
/// Nothing in [`crate::HcrService`] evicts on its own — eviction is a policy the
/// deployment owns, and the service is deliberately free of timers so it stays
/// testable against a `ManualClock`. But a server that never calls it leaks:
/// every abandoned lobby and finished round is retained forever, and on a public
/// deployment anyone can create those.
///
/// Runs on its own thread with its own single-threaded runtime rather than a
/// `tokio::spawn`, because `run_server` owns the main runtime and this must not
/// depend on how it chooses to set one up.
pub fn spawn_sweeper(service: Arc<crate::HcrService>) {
    std::thread::Builder::new()
        .name("hcr-sweeper".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("hcr-sweeper: could not start ({error}); memory will not be reclaimed");
                    return;
                }
            };
            loop {
                std::thread::sleep(SWEEP_INTERVAL);
                let (sessions, rounds) = runtime.block_on(service.evict_idle());
                if sessions > 0 || rounds > 0 {
                    println!("hcr-sweeper: evicted {sessions} session(s), {rounds} round(s)");
                }
            }
        })
        .expect("spawn sweeper thread");
}

/// Register every route on a hotaru app and start serving.
///
/// Blocks for the lifetime of the process.
/// Largest request body the server will read, bytes.
///
/// Sized for the biggest legitimate Cutter Grid submission with headroom, not
/// for comfort: the trajectory is bounded by the 500-command cap and the
/// planner's resampling rules, which together put the worst case near 2.5 MB
/// uncompressed. 8 MiB leaves room for that to grow without inviting a body
/// large enough to be a memory-exhaustion primitive.
pub const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

pub fn serve(router: Arc<Router>, config: &ServerConfig) {
    use hotaru::http::*;
    use hotaru::prelude::*;

    // Stated rather than inherited. A Cutter Grid submission carries its whole
    // frozen trajectory — a few hundred KB typically, and around 2.5 MB for a
    // program at the 500-command cap — so the body ceiling stopped being an
    // irrelevant framework default the moment that shipped. Writing it down
    // means a future framework bump cannot quietly lower it and turn large but
    // legitimate submissions into truncated-body errors.
    let mut safety = HttpSafety::default();
    safety.set_max_body_size(Some(MAX_REQUEST_BODY_BYTES));

    let app = <Server>::new()
        .binding(config.binding.as_str())
        .single_protocol(ProtocolBuilder::new(HTTP::server(safety)))
        .build();

    for (pattern, name) in ROUTES {
        app.insert(Endpoint::<HTTP>::endpoint(
            *pattern,
            *name,
            make_handler(router.clone(), config.cors_allow_origin.clone()),
        ))
        .expect("insert endpoint");
    }

    // What `run_server!(APP)` expands to; called directly so no proc-macro
    // ceremony is needed.
    hotaru::hotaru_core::app::server::run_server(app.clone());
}
