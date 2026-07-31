//! A runnable HCR HTTP server.
//!
//! ```sh
//! cargo run -p hcr_service --features hotaru --example serve
//! curl -s localhost:8080/api/v1/time
//! curl -s localhost:8080/api/v1/challenges
//! ```
//!
//! The whole binding is the loop below: every route funnels into the same
//! [`Router`], which is tested without a socket in `tests/binding.rs`. hotaru
//! carries bytes; it makes no routing decisions of its own.

use std::sync::Arc;

use hcr_service::hotaru_binding::{ROUTES, make_handler};
use hcr_service::{
    CatalogStore, HcrService, ItemRefSigner, ReplayPool, Router, ServiceConfig,
};
use hotaru::http::*;
use hotaru::prelude::*;

const BINDING: &str = "127.0.0.1:8080";

fn main() {
    let service = Arc::new(HcrService::new(
        Arc::new(CatalogStore::new()),
        Arc::new(ReplayPool::with_default_concurrency()),
        // A real deployment loads this from configuration. With a known key,
        // every item reference in the system is forgeable.
        ItemRefSigner::new(*b"replace-me-with-a-real-secret"),
        ServiceConfig::default(),
    ));
    let router = Arc::new(Router::new(service));

    let app = <Server>::new()
        .binding(BINDING)
        .single_protocol(ProtocolBuilder::new(HTTP::server(HttpSafety::default())))
        .build();

    for (pattern, name) in ROUTES {
        app.insert(Endpoint::<HTTP>::endpoint(
            *pattern,
            *name,
            make_handler(router.clone()),
        ))
        .expect("insert endpoint");
    }

    println!("HCR service listening on http://{BINDING}");
    for (pattern, _) in ROUTES {
        println!("  {pattern}");
    }

    // What `run_server!(APP)` expands to; called directly so the example needs
    // no proc-macro ceremony.
    hotaru::hotaru_core::app::server::run_server(app.clone());
}
