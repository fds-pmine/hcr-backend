//! The deployable HCR server.
//!
//! Every setting comes from the environment, so the same binary runs in every
//! environment and nothing secret is compiled into it:
//!
//! ```sh
//! HCR_SIGNING_KEY="$(openssl rand -hex 32)" \
//! HCR_BIND=0.0.0.0:18623 \
//! HCR_CORS_ORIGIN=https://web.hcr.rs \
//!   hcr-server
//! ```
//!
//! See `docs/DEPLOY.md`, which also covers the two things this binary does
//! **not** solve: it has no authentication layer (it trusts `X-HCR-Player` as
//! sent) and it holds all state in memory (a restart drops every live round).

use std::process::ExitCode;
use std::sync::Arc;

use hcr::deploy::{check_binding, serve, spawn_sweeper};
use hcr::{
    HcrService, ItemRefSigner, ReplayPool, Router, ServerConfig, ServiceConfig, UsageLog,
};

fn main() -> ExitCode {
    let config = match ServerConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            // Refusing to start is the point. A server that defaulted its way
            // past a missing key would look healthy while signing item
            // references anybody could forge.
            eprintln!("hcr-server: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Before doing any work: an occupied port otherwise surfaces as a panic
    // inside the framework and a systemd restart loop with no explanation.
    if let Err(error) = check_binding(&config.binding) {
        eprintln!("hcr-server: {error}");
        return ExitCode::FAILURE;
    }

    let catalog = hcr::seed::seed_catalog();

    let mut service = HcrService::new(
        catalog.clone(),
        Arc::new(ReplayPool::with_default_concurrency()),
        ItemRefSigner::new(config.signing_key.clone()),
        ServiceConfig::default(),
    );

    if let Some(path) = &config.usage_log_path {
        match UsageLog::open(path) {
            Ok(log) => {
                println!("recording usage to {}", log.path().display());
                service = service.with_usage_log(Arc::new(log));
            }
            // Not fatal. A server that refused to start because telemetry could
            // not be written would be prioritising the data over the users.
            Err(error) => eprintln!(
                "hcr-server: could not open usage log {}: {error}. Continuing without it.",
                path.display()
            ),
        }
    }
    let service = Arc::new(service);

    println!(
        "hcr-server listening on {} — {} challenge versions, CORS {}",
        config.binding,
        catalog.len(),
        config
            .cors_allow_origin
            .as_deref()
            .unwrap_or("disabled (same-origin only)"),
    );

    spawn_sweeper(service.clone());
    serve(Arc::new(Router::new(service)), &config);
    ExitCode::SUCCESS
}
