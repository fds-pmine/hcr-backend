//! A **development** server, seeded with the real shipped challenge.
//!
//! ```sh
//! cargo run -p hcr_service --features hotaru --example serve
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

use hcr_service::deploy::{DEFAULT_BINDING, serve, spawn_sweeper};
use hcr_service::{
    HcrService, ItemRefSigner, ReplayPool, Router, ServerConfig, ServiceConfig,
};

/// Origin of the Vite dev server, so a browser on a different port can talk to
/// this one.
const DEV_ORIGIN: &str = "http://localhost:5173";

fn main() {
    let catalog = hcr_service::seed::seed_catalog();
    println!("catalog seeded with {} challenge versions", catalog.len());

    let service = Arc::new(HcrService::new(
        catalog,
        Arc::new(ReplayPool::with_default_concurrency()),
        // Fixed, and printed in the source above. Fine for a laptop, worthless
        // anywhere else: with a known key every item reference is forgeable.
        ItemRefSigner::new(*b"development-only-key-not-a-secret"),
        ServiceConfig::default(),
    ));

    let config = ServerConfig {
        binding: DEFAULT_BINDING.to_string(),
        cors_allow_origin: Some(DEV_ORIGIN.to_string()),
        signing_key: Vec::new(),
        // The development server collects nothing.
        usage_log_path: None,
    };

    println!("HCR development service on http://{}", config.binding);
    println!("CORS allowed for {DEV_ORIGIN}");
    spawn_sweeper(service.clone());
    serve(Arc::new(Router::new(service)), &config);
}
