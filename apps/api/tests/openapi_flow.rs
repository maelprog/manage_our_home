//! The OpenAPI documentation (#626): off by default, the spec and Swagger UI
//! when `API_DOCS_ENABLED=true`, and a spec that lists every route
//! `build_router` registers.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Method, Response, StatusCode};
use common::{call, test_state};
use http_body_util::BodyExt;
use manage_our_home::api_docs::{openapi, SESSION_SCHEME};
use manage_our_home::build_router;
use sqlx::PgPool;
use syn::visit::Visit;

/// The documentation's routes, and a few paths inside Swagger UI.
const DOC_PATHS: [&str; 6] = [
    "/openapi.json",
    "/docs",
    "/docs/",
    "/docs/index.html",
    "/docs/swagger-initializer.js",
    "/docs/swagger-ui-bundle.js",
];

async fn body_bytes(response: Response<Body>) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn text(response: Response<Body>) -> String {
    String::from_utf8(body_bytes(response).await).unwrap()
}

fn content_type(response: &Response<Body>) -> Option<String> {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
}

fn docs_router(db: PgPool) -> axum::Router {
    let mut state = test_state(db);
    state.api_docs_enabled = true;
    build_router(state)
}

#[sqlx::test]
async fn off_by_default_the_docs_answer_like_an_unknown_route(db: PgPool) {
    let router = build_router(test_state(db));
    let unknown = call(&router, Method::GET, "/no-such-route", None, None).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let unknown_type = content_type(&unknown);
    let unknown_body = body_bytes(unknown).await;
    for path in DOC_PATHS {
        let res = call(&router, Method::GET, path, None, None).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(content_type(&res), unknown_type, "{path}");
        assert_eq!(body_bytes(res).await, unknown_body, "{path}");
    }
}

#[sqlx::test]
async fn on_the_spec_is_openapi_3_1_with_every_reference_defined(db: PgPool) {
    let router = docs_router(db);
    let res = call(&router, Method::GET, "/openapi.json", None, None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type(&res).as_deref(), Some("application/json"));
    let bytes = body_bytes(res).await;

    // Reads back as an OpenAPI document, and as 3.1.
    let spec: utoipa::openapi::OpenApi = serde_json::from_slice(&bytes).unwrap();
    assert!(spec.openapi == utoipa::openapi::OpenApiVersion::Version31);
    assert!(!spec.paths.paths.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["openapi"], "3.1.0");

    // Every `$ref` names a schema the document defines.
    let schemas = json["components"]["schemas"].as_object().unwrap();
    let mut refs = BTreeSet::new();
    collect_refs(&json, &mut refs);
    assert!(!refs.is_empty());
    for r in &refs {
        let name = r
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("unexpected $ref {r}"));
        assert!(schemas.contains_key(name), "$ref {r} is not defined");
    }

    // The session cookie, under the name this process gives it.
    assert_eq!(
        json["components"]["securitySchemes"][SESSION_SCHEME],
        serde_json::json!({
            "type": "apiKey",
            "in": "cookie",
            "name": "session_id",
            "description": "The session cookie `POST /auth/login` sets (HttpOnly, SameSite=Lax).",
        })
    );
    assert_eq!(json["servers"][0]["url"], "/api");
}

fn collect_refs(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == "$ref" {
                    out.insert(v.as_str().unwrap().to_string());
                } else {
                    collect_refs(v, out);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}

#[test]
fn behind_secure_cookies_the_scheme_names_the_host_prefixed_cookie() {
    let json = serde_json::to_value(openapi(true)).unwrap();
    assert_eq!(
        json["components"]["securitySchemes"][SESSION_SCHEME]["name"],
        "__Host-session_id"
    );
}

#[sqlx::test]
async fn on_swagger_ui_is_served_from_the_binary_with_relative_urls(db: PgPool) {
    let router = docs_router(db);

    // Relative, so that behind Caddy `/api/docs` goes to `/api/docs/`.
    let res = call(&router, Method::GET, "/docs", None, None).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers()[header::LOCATION], "docs/");

    // No inline script: every `<script>` loads a file.
    let res = call(&router, Method::GET, "/docs/", None, None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(content_type(&res).unwrap().starts_with("text/html"));
    let html = text(res).await;
    let scripts: Vec<&str> = html.split("<script").skip(1).collect();
    assert_eq!(scripts.len(), 3, "{html}");
    for script in scripts {
        let (open, rest) = script.split_once('>').unwrap();
        assert!(open.contains("src=\"./"), "{open}");
        assert!(
            rest.trim_start().starts_with("</script>"),
            "inline script: {script}"
        );
    }
    assert!(
        !html.contains("http://") && !html.contains("https://"),
        "{html}"
    );

    // The spec, relative to the page; no validator badge from swagger.io.
    let res = call(
        &router,
        Method::GET,
        "/docs/swagger-initializer.js",
        None,
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let initializer = text(res).await;
    assert!(
        initializer.contains(r#""url": "../openapi.json""#),
        "{initializer}"
    );
    assert!(
        initializer.contains(r#""validatorUrl": "none""#),
        "{initializer}"
    );
    // Not `StandaloneLayout`, whose top bar draws an SVG logo holding a
    // `<style>` element the CSP refuses.
    assert!(
        initializer.contains(r#""layout": "BaseLayout""#),
        "{initializer}"
    );
    assert!(!initializer.contains("petstore"), "{initializer}");

    // The stylesheet that replaces the archive's `index.css`.
    let res = call(&router, Method::GET, "/docs/index.css", None, None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type(&res).as_deref(), Some("text/css"));
    assert!(text(res).await.contains(".swagger-ui select"));

    let res = call(
        &router,
        Method::GET,
        "/docs/swagger-ui-bundle.js",
        None,
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(content_type(&res).unwrap().contains("javascript"));
    assert!(body_bytes(res).await.len() > 100_000);

    let res = call(&router, Method::GET, "/docs/no-such-file.js", None, None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Routes `build_router` registers that the spec leaves out on purpose.
const NOT_DOCUMENTED: [(&str, &str); 4] = [
    // The Messagerie WebSocket: an upgrade, not a request/response
    // operation OpenAPI can describe.
    ("GET", "/groups/{id}/messages/ws"),
    // The orchestrator's probes: infrastructure, not the API apps/web calls.
    ("GET", "/healthz"),
    ("GET", "/readyz"),
    // Content-Security-Policy reports: posted by browsers, in the Reporting
    // API's format, not by a client of the API.
    ("POST", "/csp-report"),
    // `/openapi.json` and `/docs` are not in this list: they are merged
    // from `api_docs::router`, the one merge the parse below lets through
    // without reading it.
];

const METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

/// One `(method, path)` of `build_router`, and the handler it calls.
#[derive(Debug)]
struct Route {
    handler: Vec<String>,
}

/// The `Router` methods that register routes some other way than `.route`.
/// The parse reads none of them, so it refuses them all — but for the merge
/// of `api_docs::router`, whose routes are the documentation's own.
const OTHER_REGISTRATIONS: [&str; 7] = [
    "nest",
    "nest_service",
    "route_service",
    "merge",
    "fallback",
    "fallback_service",
    "method_not_allowed_fallback",
];

/// Reads `build_router` in `src/lib.rs`: every `.route("<path>", <method
/// router>)`, with the methods of its chain — `get(h)`, `.post(h)`,
/// `axum::routing::patch(h)` — and what a `.layer(...)` wraps left out.
/// Anything else that would register a route — another method router
/// (`any`, `on`, a `.fallback` in the chain), or one of
/// `OTHER_REGISTRATIONS` — fails the parse rather than going unseen.
struct RouteCollector {
    routes: BTreeMap<(String, String), Route>,
}

impl RouteCollector {
    fn methods(&mut self, path: &str, expr: &syn::Expr) {
        match expr {
            syn::Expr::MethodCall(call) => {
                let method = call.method.to_string();
                if METHODS.contains(&method.as_str()) {
                    self.add(path, &method, call.args.first());
                } else if method != "layer" {
                    panic!("{path}: `.{method}` is not a method router this parse reads");
                }
                self.methods(path, &call.receiver);
            }
            syn::Expr::Call(call) => {
                let syn::Expr::Path(func) = &*call.func else {
                    panic!("{path}: a method router whose function is not a path");
                };
                let method = func.path.segments.last().unwrap().ident.to_string();
                assert!(
                    METHODS.contains(&method.as_str()),
                    "{path}: `{method}` is not a method router this parse reads"
                );
                self.add(path, &method, call.args.first());
            }
            _ => panic!("{path}: a method router that is neither a call nor a method chain"),
        }
    }

    fn add(&mut self, path: &str, method: &str, handler: Option<&syn::Expr>) {
        let Some(syn::Expr::Path(handler)) = handler else {
            panic!("{method} {path}: the handler is not a path");
        };
        let handler = handler
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        let key = (method.to_uppercase(), path.to_string());
        assert!(
            self.routes.insert(key.clone(), Route { handler }).is_none(),
            "{key:?} registered twice"
        );
    }
}

impl<'ast> Visit<'ast> for RouteCollector {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "route" {
            let mut args = call.args.iter();
            let (Some(syn::Expr::Lit(path)), Some(methods)) = (args.next(), args.next()) else {
                panic!("a .route() whose path is not a literal");
            };
            let syn::Lit::Str(path) = &path.lit else {
                panic!("a .route() whose path is not a string");
            };
            self.methods(&path.value(), methods);
        } else {
            let method = call.method.to_string();
            if OTHER_REGISTRATIONS.contains(&method.as_str())
                && !(method == "merge" && merges_the_documentation(call))
            {
                panic!(
                    "build_router registers routes with `.{method}`, which this parse does \
                     not read: read it here, or register them with `.route`"
                );
            }
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

/// Whether `call` is `.merge(api_docs::router(..))`.
fn merges_the_documentation(call: &syn::ExprMethodCall) -> bool {
    let Some(syn::Expr::Call(arg)) = call.args.first() else {
        return false;
    };
    let syn::Expr::Path(func) = &*arg.func else {
        return false;
    };
    let segments: Vec<String> = func
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    segments == ["api_docs", "router"]
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn build_router_routes() -> BTreeMap<(String, String), Route> {
    routes_of(&std::fs::read_to_string(src_dir().join("lib.rs")).unwrap())
}

/// The routes of the `build_router` that `source` defines.
fn routes_of(source: &str) -> BTreeMap<(String, String), Route> {
    let file = syn::parse_file(source).unwrap();
    let build_router = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Fn(f) if f.sig.ident == "build_router" => Some(f),
            _ => None,
        })
        .expect("build_router in src/lib.rs");
    let mut collector = RouteCollector {
        routes: BTreeMap::new(),
    };
    collector.visit_item_fn(build_router);
    collector.routes
}

/// A `build_router` whose body is `body`, for the tests of the parse.
fn routes_of_body(body: &str) -> BTreeSet<(String, String)> {
    routes_of(&format!("fn build_router(state: AppState) -> Router {{ {body} }}"))
        .into_keys()
        .collect()
}

fn route(method: &str, path: &str) -> (String, String) {
    (method.to_string(), path.to_string())
}

#[test]
fn the_parse_reads_method_chains_and_skips_layers() {
    assert_eq!(
        routes_of_body(
            r#"Router::new()
                .route("/a", get(a::list).post(a::create))
                .route("/b", axum::routing::patch(b::edit).layer(DefaultBodyLimit::max(1)))
                .layer(CookieManagerLayer::new())"#
        ),
        BTreeSet::from([route("GET", "/a"), route("POST", "/a"), route("PATCH", "/b")])
    );
}

#[test]
fn the_parse_accepts_the_merge_of_the_documentation() {
    assert_eq!(
        routes_of_body(
            r#"let router = Router::new().route("/a", get(a::list));
            let router = if state.api_docs_enabled {
                router.merge(api_docs::router(&api_docs::openapi(state.secure_cookies)))
            } else {
                router
            };
            router.with_state(state)"#
        ),
        BTreeSet::from([route("GET", "/a")])
    );
}

#[test]
#[should_panic(expected = "/mutant: `any` is not a method router this parse reads")]
fn the_parse_refuses_a_method_router_it_does_not_know() {
    routes_of_body(r#"Router::new().route("/mutant", axum::routing::any(m::h))"#);
}

#[test]
#[should_panic(expected = "/mutant: `.fallback` is not a method router this parse reads")]
fn the_parse_refuses_a_chained_method_it_does_not_know() {
    routes_of_body(r#"Router::new().route("/mutant", get(m::h).fallback(m::other))"#);
}

#[test]
#[should_panic(expected = "build_router registers routes with `.nest`")]
fn the_parse_refuses_a_nested_router() {
    routes_of_body(
        r#"Router::new()
            .route("/a", get(a::list))
            .nest("/n", Router::new().route_service("/mutant", m::service()))"#,
    );
}

#[test]
#[should_panic(expected = "build_router registers routes with `.route_service`")]
fn the_parse_refuses_a_route_service() {
    routes_of_body(r#"Router::new().route_service("/mutant", m::service())"#);
}

#[test]
#[should_panic(expected = "build_router registers routes with `.merge`")]
fn the_parse_refuses_the_merge_of_another_router() {
    routes_of_body(r#"Router::new().route("/a", get(a::list)).merge(m::router())"#);
}

#[test]
#[should_panic(expected = "build_router registers routes with `.fallback`")]
fn the_parse_refuses_a_fallback() {
    routes_of_body(r#"Router::new().route("/a", get(a::list)).fallback(m::h)"#);
}

/// Every `(method, path)` operation of the spec.
fn spec_operations() -> BTreeMap<(String, String), serde_json::Value> {
    let json = serde_json::to_value(openapi(false)).unwrap();
    let mut ops = BTreeMap::new();
    for (path, item) in json["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if METHODS.contains(&method.as_str()) {
                ops.insert((method.to_uppercase(), path.clone()), op.clone());
            }
        }
    }
    ops
}

#[test]
fn the_spec_lists_exactly_the_routes_of_build_router() {
    let mut routes: BTreeSet<_> = build_router_routes().into_keys().collect();
    // A sanity bound on the parse itself: it found the router.
    assert!(routes.len() > 100, "{} routes parsed", routes.len());
    for (method, path) in NOT_DOCUMENTED {
        assert!(
            routes.remove(&(method.to_string(), path.to_string())),
            "{method} {path} is listed as left out but build_router has no such route"
        );
    }
    let ops: BTreeSet<_> = spec_operations().into_keys().collect();
    let undocumented: Vec<_> = routes.difference(&ops).collect();
    let unrouted: Vec<_> = ops.difference(&routes).collect();
    assert!(
        undocumented.is_empty() && unrouted.is_empty(),
        "routes without an operation (add `#[utoipa::path]` and list the handler in \
         `api_docs::ApiDoc`): {undocumented:?}\noperations without a route: {unrouted:?}"
    );
}

/// The extractors that read the session cookie.
const SESSION_EXTRACTORS: [&str; 6] = [
    "AuthUser",
    "SuperAdminUser",
    "AnySession",
    "DeactivatedSession",
    "AgeUndeclaredSession",
    "TermsSession",
];

/// Whether the handler `crate::<path>` takes one of `SESSION_EXTRACTORS`.
fn takes_a_session(handler: &[String]) -> bool {
    let (name, module) = handler.split_last().unwrap();
    let base = module.iter().fold(src_dir(), |dir, m| dir.join(m));
    let file = [base.with_extension("rs"), base.join("mod.rs")]
        .into_iter()
        .find(|f| f.exists())
        .unwrap_or_else(|| panic!("no source file for {handler:?}"));
    let source = std::fs::read_to_string(&file).unwrap();
    let parsed = syn::parse_file(&source).unwrap();
    let f = parsed
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Fn(f) if f.sig.ident == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fn {name} in {}", file.display()));
    f.sig.inputs.iter().any(|arg| match arg {
        syn::FnArg::Typed(arg) => match &*arg.ty {
            syn::Type::Path(ty) => {
                let last = ty.path.segments.last().unwrap().ident.to_string();
                SESSION_EXTRACTORS.contains(&last.as_str())
            }
            _ => false,
        },
        syn::FnArg::Receiver(_) => false,
    })
}

#[test]
fn every_operation_behind_a_session_declares_the_session_cookie() {
    let ops = spec_operations();
    let mut wrong = Vec::new();
    for (key, route) in build_router_routes() {
        let Some(op) = ops.get(&key) else { continue };
        let declared = op["security"]
            .as_array()
            .is_some_and(|s| s.iter().any(|r| r.get(SESSION_SCHEME).is_some()));
        if declared != takes_a_session(&route.handler) {
            wrong.push((key, declared));
        }
    }
    assert!(
        wrong.is_empty(),
        "security does not match the handler's extractor (key, declared): {wrong:?}"
    );
}

#[test]
fn every_operation_documents_its_error_envelope() {
    for (key, op) in spec_operations() {
        let responses = op["responses"].as_object().unwrap();
        assert!(
            responses
                .keys()
                .any(|s| s.starts_with('2') || s.starts_with('3')),
            "{key:?} has no success response"
        );
        let error = &responses
            .get("500")
            .unwrap_or_else(|| panic!("{key:?} has no 500"))["content"]["application/json"]
            ["schema"]["$ref"];
        assert_eq!(error, "#/components/schemas/ErrorResponse", "{key:?}");
    }
}
