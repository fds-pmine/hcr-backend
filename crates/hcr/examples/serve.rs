//! A **development** server, seeded with the real shipped challenge.
//!
//! ```sh
//! cargo run -p hcr --features hotaru --example serve
//! curl -s localhost:18623/api/v1/challenges | jq
//! ```
//!
//! # Not for deployment
//!
//! This binds loopback, allows the Vite dev origin, and uses a fixed signing
//! key so that a fresh checkout runs with no setup. All three are wrong outside
//! a laptop, and the key is published in this file. Deploy `hcr-server`
//! instead, which reads the same settings from the environment and refuses to
//! start without a real key.
//!
//! Neither binary has an authentication layer: both trust `X-HCR-Player` as
//! sent. See `docs/DEPLOY.md`.

use std::sync::Arc;

use hcr::deploy::{DEFAULT_BINDING, serve, spawn_sweeper};
use hcr::{
    HcrService, ItemRefSigner, ReplayPool, Router, ServerConfig, ServiceConfig, UsageLog,
};

/// Origins allowed to call this server from a browser.
///
/// The Vite dev server, so a browser on a different port can talk to this one,
/// and the desktop build's own scheme. A packaged Electron app that loaded from
/// `file://` would send `Origin: null` and be indistinguishable from any
/// sandboxed frame, so it serves itself over `hcr://app` instead and appears
/// here under that name.
const DEV_ORIGINS: &str = "http://localhost:5173,hcr://app";

fn main() {
    let catalog = hcr::seed::seed_catalog();
    println!("catalog seeded with {} challenge versions", catalog.len());

    let mut service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::with_default_concurrency()),
        // Fixed, and printed in the source above. Fine for a laptop, worthless
        // anywhere else: with a known key every item reference is forgeable.
        ItemRefSigner::new(*b"development-only-key-not-a-secret"),
        ServiceConfig::default(),
    );

    // Off unless asked for, as in production — but askable, so the collection
    // can be inspected on a laptop before it is turned on somewhere real. A
    // deployment that cannot see its own telemetry format until it is live is a
    // deployment that finds out too late what it is recording about people.
    let usage_log_path = std::env::var("HCR_USAGE_LOG")
        .ok()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);

    let config = ServerConfig {
        binding: DEFAULT_BINDING.to_string(),
        cors_allow_origin: Some(DEV_ORIGINS.to_string()),
        signing_key: Vec::new(),
        usage_log_path,
    };

    // `ServerConfig::usage_log_path` is only a setting — `serve` never reads it,
    // and the service does no collection unless it is handed a log. Same two
    // steps as `bin/hcr-server.rs`, and skipping the second is how you get a
    // server that announces it is recording and records nothing.
    match &config.usage_log_path {
        Some(path) => match UsageLog::open(path) {
            Ok(log) => {
                println!("usage log appending to {}", log.path().display());
                service = service.with_usage_log(Arc::new(log));
            }
            // Not fatal, for the same reason as in production: a server that
            // refused to start over telemetry would value the data above the
            // people generating it.
            Err(error) => eprintln!("could not open usage log {}: {error}", path.display()),
        },
        None => println!("usage log off — set HCR_USAGE_LOG=<path> to collect"),
    }
    let service = Arc::new(service);

    println!("HCR development service on http://{}", config.binding);
    println!("CORS allowed for {DEV_ORIGINS}");
    spawn_sweeper(service.clone());
    serve(Arc::new(Router::new(service)), &config);
}
