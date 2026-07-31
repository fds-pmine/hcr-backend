//! hotaru adapter for the HTTP binding.
//!
//! Everything that can be got wrong — routing, JSON codecs, the status table —
//! lives in [`crate::binding`] where it is tested without a socket. This module
//! is the remaining glue: pull method, path and body off an `HttpContext`, call
//! [`Router::dispatch`], and write the reply back.
//!
//! Enable with `--features hotaru`.
//!
//! # Assembling a server
//!
//! `hotaru`'s route patterns cannot express a catch-all — `TypeKind::from_ident`
//! accepts only `int|uint|decimal|str|uuid`, and the multi-segment `Path` kind is
//! unreachable from pattern syntax — so each path shape is registered
//! individually. [`ROUTES`] is that list; [`endpoints`] turns it into endpoints
//! that all funnel into the same tested router.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use hcr_service::{Router, hotaru_binding};
//! # fn build(router: Arc<Router>) {
//! // Each entry is (pattern, name, handler) ready for
//! // `Endpoint::<HTTP>::endpoint(...)` followed by `APP.insert(...)`.
//! for (pattern, name) in hcr_service::hotaru_binding::ROUTES {
//!     let _ = (pattern, name, router.clone());
//! }
//! # }
//! ```

use std::sync::Arc;

use hotaru_http::context::HttpContext;
use hotaru_http::message::body::HttpBody;
use hotaru_http::message::http_value::{HttpContentType, HttpMethod, StatusCode};
use hotaru_http::message::response::{HttpResponse, response_templates};

use crate::binding::{HttpCall, Method, Router};

/// Every path shape the binding serves, as `(pattern, endpoint name)`.
///
/// Registered explicitly because hotaru has no usable catch-all pattern. The
/// handler still hands the **full path** to [`Router`], so route parsing stays in
/// one tested place rather than being split between two layers.
pub const ROUTES: &[(&str, &str)] = &[
    ("/api/v1/challenges", "hcr.challenges.list"),
    ("/api/v1/challenges/{id}", "hcr.challenges.get"),
    ("/api/v1/challenges/{id}/{version}", "hcr.challenges.version"),
    ("/api/v1/score", "hcr.score"),
    ("/api/v1/submissions", "hcr.submissions.create"),
    ("/api/v1/submissions/{id}", "hcr.submissions.get"),
    ("/api/v1/sessions", "hcr.sessions.start"),
    ("/api/v1/sessions/{id}", "hcr.sessions.get"),
    ("/api/v1/sessions/{id}/next", "hcr.sessions.next"),
    ("/api/v1/sessions/{id}/responses", "hcr.sessions.respond"),
    ("/api/v1/sessions/{id}/finalize", "hcr.sessions.finalize"),
    ("/api/v1/time", "hcr.time"),
    ("/api/v1/matches", "hcr.matches.create"),
    ("/api/v1/matches/{id}", "hcr.matches.get"),
    ("/api/v1/matches/{id}/challenge", "hcr.matches.challenge"),
    ("/api/v1/matches/{id}/results", "hcr.matches.results"),
    ("/api/v1/matches/{id}/join", "hcr.matches.join"),
    ("/api/v1/matches/{id}/start", "hcr.matches.start"),
    ("/api/v1/matches/{id}/submissions", "hcr.matches.submit"),
];

/// Header carrying the authenticated player.
///
/// Identity must come from the authentication layer, never a request body — a
/// caller that could name itself could act as somebody else. In a real
/// deployment this is set by whatever validates the bearer token.
pub const PLAYER_HEADER: &str = "x-hcr-player";

/// Serve one request.
///
/// The single function a hotaru endpoint needs to call.
pub async fn handle<TS>(router: &Router, ctx: &mut HttpContext<TS>) -> HttpResponse
where
    TS: hotaru_core::connection::TransportSpec,
{
    let method = match ctx.method() {
        HttpMethod::GET => Method::Get,
        HttpMethod::POST => Method::Post,
        // The binding uses only GET and POST; anything else is not a route.
        _ => {
            return response_templates::normal_response(
                StatusCode::METHOD_NOT_ALLOWED,
                br#"{"error":{"code":"FORBIDDEN","message":"Only GET and POST are supported.","retryable":false}}"#.to_vec(),
            )
            .content_type(HttpContentType::ApplicationJson());
        }
    };

    let path = ctx.path();
    let player_id = ctx.header_str(PLAYER_HEADER).map(str::to_owned);
    let body = take_body(ctx);

    let reply = router
        .dispatch(HttpCall {
            method,
            path,
            body,
            player_id,
        })
        .await;

    response_templates::normal_response(StatusCode::from(reply.status), reply.body)
        .content_type(HttpContentType::ApplicationJson())
}

/// Take the request body as raw bytes.
///
/// Deliberately *not* `ctx.parse_body()`: that dispatches on content type and
/// routes `application/json` through akari's `Value`, so the bytes would reach
/// serde only after a parse/serialize round trip. For a service whose payloads
/// carry joint angles and scores, re-formatting floats on the way in is a risk
/// worth avoiding entirely.
fn take_body<TS>(ctx: &mut HttpContext<TS>) -> Vec<u8>
where
    TS: hotaru_core::connection::TransportSpec,
{
    match std::mem::take(&mut ctx.request.body) {
        // The usual case: hotaru buffers the body without interpreting it.
        //
        // `content_coding` is ignored because this binding's clients send
        // uncompressed JSON. Accepting a compressed body would mean decoding
        // here first; until that is needed, a compressed request simply fails to
        // parse and is reported as a malformed body.
        HttpBody::Buffer { data, .. } => data,
        HttpBody::Binary(data) => data,
        HttpBody::Text(text) => text.into_bytes(),
        HttpBody::Empty | HttpBody::Unparsed => Vec::new(),
        // Already parsed by middleware; re-serializing is the best available
        // recovery.
        other => other.raw(),
    }
}

/// Build a handler closure for one route.
///
/// Returned as an owned closure so it can be passed straight to
/// `Endpoint::<HTTP>::endpoint(pattern, name, make_handler(router))`.
pub fn make_handler<TS>(
    router: Arc<Router>,
) -> impl for<'a> Fn(
    &'a mut HttpContext<TS>,
) -> std::pin::Pin<Box<dyn Future<Output = HttpResponse> + Send + 'a>>
+ Clone
+ Send
+ Sync
+ 'static
where
    TS: hotaru_core::connection::TransportSpec + Send + Sync + 'static,
    HttpContext<TS>: Send,
{
    move |ctx: &mut HttpContext<TS>| {
        let router = router.clone();
        Box::pin(async move { handle(&router, ctx).await })
    }
}
