//! The HTTP clients for requests this API makes to third parties on behalf
//! of a user request: Google's OAuth token and userinfo endpoints, and the
//! iCal feeds of calendar imports. Without a timeout, a peer that accepts
//! the connection and never answers holds the user's request open
//! indefinitely (#371).
//!
//! The OAuth calls get a client of their own that follows no redirect: on a
//! 307 or 308, reqwest replays the code exchange's POST body, authorization
//! code and PKCE verifier included, at whatever the `Location` names, and
//! the `client_secret` too when the redirect stays on the same host (it
//! drops the `Authorization` header on a change of host or port) (#389).
//! The `oauth2` crate's documentation advises the same for the code
//! exchange. The iCal import keeps following redirects, feeds move.
//!
//! The recipe import (#405) fetches a page at an address the member typed:
//! its client only reaches public addresses (`public_destination`), checks
//! each redirect the same way, and ignores any proxy configured in the
//! environment, whose address would be resolved instead of the page's.

use std::sync::LazyLock;
use std::time::Duration;

/// Time allowed to establish the TCP (and TLS) connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed for the whole request, connection and body included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    build(
        CONNECT_TIMEOUT,
        REQUEST_TIMEOUT,
        reqwest::redirect::Policy::default(),
    )
});

static OAUTH_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    build(
        CONNECT_TIMEOUT,
        REQUEST_TIMEOUT,
        reqwest::redirect::Policy::none(),
    )
});

/// The shared client, which follows redirects (reqwest's default, up to
/// 10). Built once, so its connection pool is reused across requests.
pub fn client() -> &'static reqwest::Client {
    &CLIENT
}

/// The client for Google's OAuth endpoints: same timeouts, no redirect
/// followed. A redirect comes back as the 3xx response itself.
pub fn oauth_client() -> &'static reqwest::Client {
    &OAUTH_CLIENT
}

static RECIPE_IMPORT_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    recipe_import_builder()
        .build()
        .expect("static reqwest configuration")
});

/// The client for the recipe import: same timeouts, public destinations
/// only, each redirect checked like the first request
/// (`public_destination`).
pub fn recipe_import_client() -> &'static reqwest::Client {
    &RECIPE_IMPORT_CLIENT
}

/// The recipe import client's configuration. Public so that the flow tests
/// build the same client, and pin a test name to their local server with
/// `ClientBuilder::resolve` — a pin set in the tests, never here.
pub fn recipe_import_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(crate::public_destination::redirect_policy())
        .dns_resolver(std::sync::Arc::new(
            crate::public_destination::PublicOnlyResolver,
        ))
        .no_proxy()
}

fn build(
    connect: Duration,
    request: Duration,
    redirect: reqwest::redirect::Policy,
) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(request)
        .redirect(redirect)
        .build()
        .expect("static reqwest configuration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A peer that accepts the connection, then never answers.
    async fn silent_peer() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                held.push(socket);
            }
        });
        (url, handle)
    }

    /// reqwest exposes no getter for its timeouts; its `Debug` output names
    /// the total one (`TotalTimeout` as of 0.12). That timeout bounds the
    /// connection too, so a client that lost it fails here. The connect
    /// timeout does not show, and is not checked.
    #[test]
    fn both_clients_carry_the_request_timeout() {
        for client in [client(), oauth_client(), recipe_import_client()] {
            let shown = format!("{client:?}");
            assert!(
                shown.contains(&format!("TotalTimeout: {REQUEST_TIMEOUT:?}")),
                "{shown}"
            );
        }
    }

    /// Every outbound call goes through `client()`, `oauth_client()` or
    /// `recipe_import_client()`: a client built anywhere else in `src/`
    /// would come without these timeouts. `notifications/push.rs` keeps its
    /// own, with its own timeout and redirect policy.
    #[test]
    fn no_other_reqwest_client_is_built_in_the_api_sources() {
        let src = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        // Paths relative to `src/`: another file named `push.rs` is not
        // allowed by its name alone.
        let allowed = [
            std::path::Path::new("outbound_http.rs"),
            std::path::Path::new("notifications/push.rs"),
        ];
        let mut offenders = Vec::new();
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let relative = path.strip_prefix(&src).unwrap();
                if path.extension() != Some("rs".as_ref()) || allowed.contains(&relative) {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for n in client_constructions(&source) {
                    offenders.push(format!("{}:{n}", relative.display()));
                }
            }
        }
        assert!(offenders.is_empty(), "{offenders:?}");
    }

    /// The 1-based lines of `source` that build a reqwest client, whether
    /// called or named as a function path (`unwrap_or_else(Client::new)`):
    /// `new`, `default` or `builder` on `Client` or `ClientBuilder`, and
    /// `get`, which builds a client of its own for one request. Each is
    /// found under its `reqwest::` path (`reqwest::blocking::` included,
    /// and wherever `reqwest` is a path segment: `::reqwest`,
    /// `oauth2::reqwest`) or under a name a `use` statement binds to it:
    /// the item itself, an alias, a glob, or the `reqwest` or `blocking`
    /// module under another name.
    ///
    /// This reads text, not types: it misses `Default::default()` where the
    /// type is inferred or annotated, `<reqwest::Client>::new()`, a
    /// `type` alias, and a client built inside a macro or by another crate.
    fn client_constructions(source: &str) -> Vec<usize> {
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut roots = vec!["reqwest".to_string()];
        let mut blockings = Vec::new();
        let mut types = Vec::new();
        let mut functions = Vec::new();
        // `use` statements are read, then blanked out of the text searched
        // below: importing `reqwest::get` builds nothing.
        let mut searched = source.as_bytes().to_vec();
        let mut line_start = 0;
        for line in source.split_inclusive('\n') {
            let start = line_start;
            line_start += line.len();
            let Some(body) = use_statement_body(line) else {
                continue;
            };
            let body_start = start + line.len() - body.len();
            let Some(end) = source[body_start..].find(';') else {
                continue;
            };
            let Some(imports) = use_imports(&source[body_start..body_start + end]) else {
                continue;
            };
            for (path, bound) in imports {
                let Some(at) = path.iter().position(|s| s == "reqwest") else {
                    continue;
                };
                let rest: Vec<&str> = path[at + 1..].iter().map(String::as_str).collect();
                let item = match rest.as_slice() {
                    [] => {
                        roots.push(bound);
                        continue;
                    }
                    ["blocking"] => {
                        blockings.push(bound);
                        continue;
                    }
                    [item] | ["blocking", item] => *item,
                    _ => continue,
                };
                match item {
                    "Client" | "ClientBuilder" => types.push(bound),
                    "get" => functions.push(bound),
                    "*" => {
                        types.extend(["Client".into(), "ClientBuilder".into()]);
                        functions.push("get".into());
                    }
                    _ => {}
                }
            }
            for byte in &mut searched[start..body_start + end + 1] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }
        let modules = roots
            .iter()
            .flat_map(|root| [root.clone(), format!("{root}::blocking")])
            .chain(blockings);
        for module in modules {
            types.extend(["Client", "ClientBuilder"].map(|ty| format!("{module}::{ty}")));
            functions.push(format!("{module}::get"));
        }
        let mut patterns = functions;
        for ty in &types {
            patterns.extend(["new", "default", "builder"].map(|m| format!("{ty}::{m}")));
        }
        let searched = String::from_utf8(searched).expect("ASCII blanks keep UTF-8 valid");
        searched
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                patterns.iter().any(|pattern| {
                    line.match_indices(pattern.as_str()).any(|(at, _)| {
                        let before = line[..at].chars().next_back();
                        let after = line[at + pattern.len()..].chars().next();
                        // `BasicClient::new`, `client.get`, `cache::get` and
                        // `Client::new_with_pool` are other items; a path may
                        // lead to `reqwest` itself (`oauth2::reqwest::get`).
                        let path_may_lead = pattern.starts_with("reqwest::");
                        !before.is_some_and(|c| {
                            is_ident(c) || c == '.' || (c == ':' && !path_may_lead)
                        }) && !after.is_some_and(is_ident)
                    })
                })
            })
            .map(|(n, _)| n + 1)
            .collect()
    }

    /// What follows `use` on a line that opens a use statement
    /// (attributes, `pub` and `pub(…)` allowed before it), or `None`.
    fn use_statement_body(line: &str) -> Option<&str> {
        let mut rest = line.trim_start();
        while let Some(attribute) = rest.strip_prefix("#[") {
            let mut depth = 1;
            let end = attribute.find(|c| {
                depth += match c {
                    '[' => 1,
                    ']' => -1,
                    _ => 0,
                };
                depth == 0
            })?;
            rest = attribute[end + 1..].trim_start();
        }
        if let Some(after_pub) = rest.strip_prefix("pub") {
            rest = after_pub.trim_start();
            if rest.starts_with('(') {
                rest = rest[rest.find(')')? + 1..].trim_start();
            }
        }
        let body = rest.strip_prefix("use")?;
        body.starts_with(char::is_whitespace).then_some(body)
    }

    /// The imports of one use statement's tree (the text between `use` and
    /// `;`): each full path, with the name it binds (`*` for a glob, the
    /// module's name for `self`). `None` when the text is not a use tree.
    fn use_imports(tree: &str) -> Option<Vec<(Vec<String>, String)>> {
        let mut tokens = Vec::new();
        let mut chars = tree.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c.is_whitespace() {
                continue;
            }
            if c.is_alphanumeric() || c == '_' {
                let mut end = at + c.len_utf8();
                while let Some(&(next, n)) = chars.peek() {
                    if !(n.is_alphanumeric() || n == '_') {
                        break;
                    }
                    end = next + n.len_utf8();
                    chars.next();
                }
                tokens.push(&tree[at..end]);
            } else if c == ':' && chars.next_if(|&(_, n)| n == ':').is_some() {
                tokens.push("::");
            } else if "{},*".contains(c) {
                tokens.push(&tree[at..at + 1]);
            } else {
                return None;
            }
        }
        let mut imports = Vec::new();
        let mut at = 0;
        use_tree(&tokens, &mut at, &[], &mut imports)?;
        (at == tokens.len()).then_some(imports)
    }

    fn use_tree(
        tokens: &[&str],
        at: &mut usize,
        prefix: &[String],
        imports: &mut Vec<(Vec<String>, String)>,
    ) -> Option<()> {
        let mut path = prefix.to_vec();
        if tokens.get(*at) == Some(&"::") {
            *at += 1;
        }
        loop {
            let token = *tokens.get(*at)?;
            *at += 1;
            match token {
                "{" => loop {
                    if tokens.get(*at) == Some(&"}") {
                        *at += 1;
                        return Some(());
                    }
                    use_tree(tokens, at, &path, imports)?;
                    match *tokens.get(*at)? {
                        "," => *at += 1,
                        "}" => {}
                        _ => return None,
                    }
                },
                "*" => {
                    path.push("*".into());
                    imports.push((path, "*".into()));
                    return Some(());
                }
                "::" | "," | "}" | "as" => return None,
                segment => {
                    if segment != "self" {
                        path.push(segment.into());
                    }
                    if tokens.get(*at) == Some(&"::") {
                        *at += 1;
                        continue;
                    }
                    let bound = if tokens.get(*at) == Some(&"as") {
                        *at += 2;
                        tokens.get(*at - 1)?.to_string()
                    } else {
                        path.last()?.clone()
                    };
                    imports.push((path, bound));
                    return Some(());
                }
            }
        }
    }

    #[test]
    fn client_constructions_finds_each_form_of_construction() {
        for (source, line) in [
            ("let c = reqwest::Client::new();", 1),
            ("let c = reqwest::Client::default();", 1),
            ("let c = reqwest::Client::builder().build();", 1),
            ("let c = reqwest::ClientBuilder::new().build();", 1),
            ("let c = reqwest::ClientBuilder::default().build();", 1),
            ("let c = ::reqwest::Client::new();", 1),
            ("let c = reqwest::blocking::Client::new();", 1),
            ("use reqwest::Client;\nfn f() {\n    Client::new();\n}", 3),
            (
                "use reqwest::Client;\nfn f() -> Client { Client::default() }",
                2,
            ),
            (
                "use reqwest::ClientBuilder;\nfn f() { ClientBuilder::new() }",
                2,
            ),
            (
                "use reqwest::{header, Client};\nfn f() { Client::builder() }",
                2,
            ),
            (
                "use reqwest::{\n    Client,\n    Url,\n};\nfn f() { Client::new() }",
                5,
            ),
            ("use reqwest::Client as Http;\nfn f() { Http::new() }", 2),
            ("use reqwest::*;\nfn f() { Client::new() }", 2),
            (
                "use reqwest::blocking::Client;\nfn f() { Client::new() }",
                2,
            ),
        ] {
            assert_eq!(client_constructions(source), vec![line], "{source}");
        }
    }

    /// `reqwest::get` builds a client of its own, without timeouts; a
    /// constructor named as a function path, without a call, builds one
    /// later; and reqwest is reachable under other names than its own.
    #[test]
    fn client_constructions_finds_free_functions_paths_and_reexports() {
        let missed = [
            ("let p = reqwest::get(url).await?;", 1),
            ("let p = reqwest::blocking::get(url)?;", 1),
            ("use reqwest::get;\nlet p = get(url).await?;", 2),
            ("let p = urls.map(reqwest::get);", 1),
            ("let c = o.unwrap_or_else(reqwest::Client::new);", 1),
            (
                "use reqwest::Client;\nlet c = o.unwrap_or_else(Client::new);",
                2,
            ),
            ("use ::reqwest::Client;\nfn f() { Client::new() }", 2),
            ("use oauth2::reqwest::Client;\nfn f() { Client::new() }", 2),
            ("let c = oauth2::reqwest::Client::new();", 1),
            (
                "use reqwest::{self as rq};\nfn f() { rq::Client::new() }",
                2,
            ),
            ("use reqwest as rq;\nfn f() { rq::get(url) }", 2),
            (
                "use oauth2::{reqwest as rq};\nfn f() { rq::Client::new() }",
                2,
            ),
            (
                "use reqwest::blocking;\nfn f() { blocking::Client::new() }",
                2,
            ),
            (
                "pub(crate) use reqwest::Client;\nfn f() { Client::new() }",
                2,
            ),
            (
                "#[allow(unused_imports)] use reqwest::Client;\nfn f() { Client::new() }",
                2,
            ),
            // A comment holding the word `use` opens no use statement.
            ("// clients use a pool;\nlet c = reqwest::Client::new();", 2),
        ]
        .into_iter()
        .filter(|&(source, line)| client_constructions(source) != vec![line])
        .map(|(source, _)| source)
        .collect::<Vec<_>>();
        assert!(missed.is_empty(), "{missed:#?}");
    }

    #[test]
    fn client_constructions_ignores_other_clients_and_other_reqwest_uses() {
        for source in [
            "fn f(c: &reqwest::Client) { c.get(url) }",
            "use reqwest::get;\nlet r = client.get(url);",
            "use reqwest::Url;\nlet g = map.get(&key);",
            "use other::get;\nlet r = get(url);",
            "let r = cache::get(url);",
            "use reqwest::Url;\nlet u = Url::parse(s);",
            "let o = BasicClient::new(ClientId::new(id));",
            "let d = reqwest::Client::new_with_pool();",
            // Without a `use reqwest::…` bringing it in, a bare `Client` is
            // another crate's.
            "use aws_sdk_s3::Client;\nlet s = Client::new(&config);",
            "use reqwest::Url;\nlet s = aws_sdk_s3::Client::new(&config);",
        ] {
            assert_eq!(
                client_constructions(source),
                Vec::<usize>::new(),
                "{source}"
            );
        }
    }

    #[tokio::test]
    async fn a_peer_that_never_answers_fails_within_the_request_timeout() {
        let (url, peer) = silent_peer().await;
        let client = build(
            Duration::from_secs(1),
            Duration::from_millis(200),
            reqwest::redirect::Policy::default(),
        );
        let outcome = tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await;
        peer.abort();
        let error = outcome
            .expect("the client gave up on its own, before the 5 s guard")
            .expect_err("a silent peer is a failure");
        assert!(error.is_timeout(), "{error}");
    }
}
