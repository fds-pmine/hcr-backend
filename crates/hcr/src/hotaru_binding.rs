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
//! # use hcr::{Router, hotaru_binding};
//! # fn build(router: Arc<Router>) {
//! // Each entry is (pattern, name, handler) ready for
//! // `Endpoint::<HTTP>::endpoint(...)` followed by `APP.insert(...)`.
//! for (pattern, name) in hcr::hotaru_binding::ROUTES {
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
///
/// Parameters use hotaru's angle-bracket syntax (`<id>`), not braces — the
/// `{id}` form in `QUICK_TUTORIAL.md` predates the current URL lexer, which
/// emits `AngleStart` tokens. A brace pattern is treated as a literal segment
/// and simply never matches.
///
/// The names are irrelevant here: the handler passes the full path to the router
/// rather than reading captured parameters, so hotaru only has to decide *that*
/// a request belongs to this service, not how to carve it up.
pub const ROUTES: &[(&str, &str)] = &[
    ("/api/v1/challenges", "hcr.challenges.list"),
    ("/api/v1/challenges/<id>", "hcr.challenges.get"),
    (
        "/api/v1/challenges/<id>/<version>",
        "hcr.challenges.version",
    ),
    ("/api/v1/score", "hcr.score"),
    ("/api/v1/cutter-grid/plans", "hcr.cutter-grid.plan"),
    ("/api/v1/submissions", "hcr.submissions.create"),
    ("/api/v1/submissions/<id>", "hcr.submissions.get"),
    ("/api/v1/sessions", "hcr.sessions.start"),
    ("/api/v1/sessions/<id>", "hcr.sessions.get"),
    ("/api/v1/sessions/<id>/next", "hcr.sessions.next"),
    ("/api/v1/sessions/<id>/responses", "hcr.sessions.respond"),
    ("/api/v1/sessions/<id>/finalize", "hcr.sessions.finalize"),
    ("/api/v1/time", "hcr.time"),
    ("/api/v1/matches", "hcr.matches.create"),
    ("/api/v1/matches/<id>", "hcr.matches.get"),
    ("/api/v1/matches/<id>/challenge", "hcr.matches.challenge"),
    ("/api/v1/matches/<id>/results", "hcr.matches.results"),
    ("/api/v1/matches/<id>/join", "hcr.matches.join"),
    ("/api/v1/matches/<id>/start", "hcr.matches.start"),
    ("/api/v1/matches/<id>/submissions", "hcr.matches.submit"),
];

/// Header carrying the authenticated player.
///
/// Identity must come from the authentication layer, never a request body — a
/// caller that could name itself could act as somebody else. In a real
/// deployment this is set by whatever validates the bearer token.
pub const PLAYER_HEADER: &str = "x-hcr-player";

/// Header carrying the player's display name.
///
/// Purely cosmetic — it names a row on the leaderboard and nothing else — so
/// unlike [`PLAYER_HEADER`] a client may choose it. See
/// [`crate::binding::HttpCall::display_name`].
pub const PLAYER_NAME_HEADER: &str = "x-hcr-player-name";

/// Request headers used by the browser clients.
///
/// Keep this list in sync with `researchHeaders()` in the frontend. A custom
/// header that is missing here makes the browser reject the CORS preflight
/// before the submission can reach the service, which otherwise looks like a
/// generic network failure to the user.
const CORS_ALLOW_HEADERS: &str = concat!(
    "Content-Type, Authorization, X-HCR-Player, X-HCR-Player-Name, ",
    "X-HCR-Research-Program-And-Scores, ",
    "X-HCR-Research-Language-Consent, ",
    "X-HCR-Research-Utc-Offset-Consent, ",
    "X-HCR-Research-Language, ",
    "X-HCR-Research-Utc-Offset-Minutes",
);

/// Serve one request.
///
/// The single function a hotaru endpoint needs to call.
///
/// `cors_allow_origin` is a comma-separated allowlist of browser origins — a
/// Vite dev server on `:5173` talking to this service on `:18623`, the hosted
/// site, or the desktop build's own scheme. Only an origin on the list is
/// echoed back. Leave it `None` to send no CORS headers at all: a permissive
/// `Access-Control-Allow-Origin` on a scoring API is not something to ship by
/// accident.
pub async fn handle<TS>(
    router: &Router,
    ctx: &mut HttpContext<TS>,
    cors_allow_origin: Option<&str>,
) -> HttpResponse
where
    TS: hotaru_core::connection::TransportSpec,
{
    // Lowercase: hotaru normalizes header names on the way in and `header_str`
    // matches exactly, so "Origin" would silently never be found.
    let request_origin = ctx.header_str("origin").map(str::to_owned);
    let cors_allow_origin = matching_origin(cors_allow_origin, request_origin.as_deref());
    let method = match ctx.method() {
        HttpMethod::GET => Method::Get,
        HttpMethod::POST => Method::Post,
        // Preflight. Answered here rather than routed, since it addresses the
        // transport rather than the service.
        HttpMethod::OPTIONS => {
            return with_cors(
                response_templates::normal_response(StatusCode::NO_CONTENT, Vec::new()),
                cors_allow_origin,
            );
        }
        // The binding uses only GET and POST; anything else is not a route.
        _ => {
            return with_cors(
                response_templates::normal_response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    br#"{"error":{"code":"FORBIDDEN","message":"Only GET and POST are supported.","retryable":false}}"#.to_vec(),
                )
                .content_type(HttpContentType::ApplicationJson()),
                cors_allow_origin,
            );
        }
    };

    let path = ctx.path();
    let player_id = ctx.header_str(PLAYER_HEADER).map(str::to_owned);
    let display_name = ctx.header_str(PLAYER_NAME_HEADER).map(str::to_owned);
    let body = take_body(ctx);

    let reply = router
        .dispatch(HttpCall {
            method,
            path,
            body,
            player_id,
            display_name,
        })
        .await;

    let response = reply.headers.iter().fold(
        response_templates::normal_response(StatusCode::from(reply.status), reply.body)
            .content_type(HttpContentType::ApplicationJson()),
        |response, (name, value)| response.add_header(name, value),
    );
    with_cors(response, cors_allow_origin)
}

/// Pick the allowed origin to echo, if the caller's origin is one of them.
///
/// `configured` is a comma-separated allowlist rather than a single value
/// because there is now more than one legitimate front end: the hosted site,
/// and the desktop build, which loads over its own scheme and therefore has its
/// own origin. Neither can be expressed as the other.
///
/// The request's `Origin` is echoed rather than the configuration reflected
/// back verbatim, which is what a browser requires — `Access-Control-Allow-Origin`
/// has to name the caller, and a list is not a legal value. Sending a configured
/// origin to a caller that did not claim it, as this did before, told browsers
/// nothing and non-browser clients something untrue.
fn matching_origin<'a>(
    configured: Option<&'a str>,
    request_origin: Option<&str>,
) -> Option<&'a str> {
    let (configured, request_origin) = (configured?, request_origin?);
    configured
        .split(',')
        .map(str::trim)
        .filter(|allowed| !allowed.is_empty())
        .find(|allowed| *allowed == request_origin)
}

fn with_cors(response: HttpResponse, allow_origin: Option<&str>) -> HttpResponse {
    match allow_origin {
        None => response,
        Some(origin) => response
            .add_header("Access-Control-Allow-Origin", origin)
            .add_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            .add_header("Access-Control-Allow-Headers", CORS_ALLOW_HEADERS)
            // The response now varies by request origin, so a shared cache must
            // not serve one front end's response to the other.
            .add_header("Vary", "Origin"),
    }
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
    cors_allow_origin: Option<String>,
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
        let cors = cors_allow_origin.clone();
        Box::pin(async move { handle(&router, ctx, cors.as_deref()).await })
    }
}

#[cfg(test)]
mod tests {
    use super::{CORS_ALLOW_HEADERS, matching_origin};

    #[test]
    fn cors_allows_every_frontend_submission_header() {
        for header in [
            "Content-Type",
            "Authorization",
            "X-HCR-Player",
            "X-HCR-Player-Name",
            "X-HCR-Research-Program-And-Scores",
            "X-HCR-Research-Language-Consent",
            "X-HCR-Research-Utc-Offset-Consent",
            "X-HCR-Research-Language",
            "X-HCR-Research-Utc-Offset-Minutes",
        ] {
            assert!(
                CORS_ALLOW_HEADERS
                    .split(',')
                    .map(str::trim)
                    .any(|allowed| allowed.eq_ignore_ascii_case(header)),
                "CORS preflight does not allow {header}",
            );
        }
    }

    #[test]
    fn echoes_only_an_origin_on_the_list() {
        let allowed = Some("https://web.hcr.rs,hcr://app");
        assert_eq!(
            matching_origin(allowed, Some("https://web.hcr.rs")),
            Some("https://web.hcr.rs")
        );
        assert_eq!(
            matching_origin(allowed, Some("hcr://app")),
            Some("hcr://app")
        );
        assert_eq!(matching_origin(allowed, Some("https://evil.test")), None);
    }

    #[test]
    fn tolerates_spacing_in_the_configured_list() {
        let allowed = Some(" https://web.hcr.rs , hcr://app ,, ");
        assert_eq!(
            matching_origin(allowed, Some("hcr://app")),
            Some("hcr://app")
        );
        // An empty entry must never match an empty or absent origin.
        assert_eq!(matching_origin(allowed, Some("")), None);
    }

    #[test]
    fn sends_nothing_when_unconfigured_or_when_the_caller_claims_no_origin() {
        assert_eq!(matching_origin(None, Some("https://web.hcr.rs")), None);
        // curl and the integration tests: no Origin, so no CORS headers. The
        // previous behaviour returned them unconditionally, which told a browser
        // nothing and a non-browser client something untrue.
        assert_eq!(matching_origin(Some("https://web.hcr.rs"), None), None);
    }

    #[test]
    fn a_prefix_is_not_a_match() {
        // `https://web.hcr.rs.evil.test` must not pass because the allowed value
        // is a prefix of it.
        assert_eq!(
            matching_origin(
                Some("https://web.hcr.rs"),
                Some("https://web.hcr.rs.evil.test")
            ),
            None
        );
    }
}
