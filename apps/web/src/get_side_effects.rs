//! No `GET` route of apps/web asks apps/api for a change (#414), outside the
//! closed list the arbitrage of 2026-10-26 admits. `SameSite=Lax` sends the
//! session cookie on a cross-site navigation, so a `GET` that writes is one a
//! third site can trigger (#223, point 4).
//!
//! apps/web keeps no data: what it changes, it changes through apps/api, by a
//! `POST`, `PUT`, `PATCH` or `DELETE` (apps/api's own `GET` routes are held
//! to no effect by `apps/api/tests/get_side_effects_flow.rs`). So the test
//! reads the source: every function the crate defines, what it calls, and
//! which of them send apps/api such a request. A `GET` handler from which one
//! of those can be reached fails it, with the chain of calls that leads
//! there.
//!
//! The call graph is built by name, and errs on the side of reaching too
//! much: a method call reaches every method of that name in the crate, an
//! unqualified call with no definition in sight every function of that
//! name. It reaches the extractors of a handler's arguments, but not the
//! middleware around the routes, nor what a `view!` holds.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;
use syn::visit::Visit;

/// The function through which `/admin/*` ends the session of a superadmin
/// apps/api asks to reauthenticate (`routes::admin::reauthenticate`): the
/// effect the arbitrage admits on those routes, and on no other.
const ADMIN_REAUTHENTICATION: (&[&str], &str) = (&["routes", "admin"], "reauthenticate");

/// What a function references: a path (a call, or a function passed by
/// name), or a method called by name.
#[derive(Debug)]
enum Ref {
    Path(Vec<String>),
    Method(String),
}

/// One function the crate defines.
#[derive(Debug)]
struct Item {
    module: Vec<String>,
    /// The type of its `impl` block, for a method.
    self_ty: Option<String>,
    name: String,
    refs: Vec<Ref>,
    /// The types of its arguments: a handler's extractors.
    arg_types: Vec<String>,
    /// The request to apps/api it sends itself, if one changes something.
    sink: Option<String>,
}

/// A `GET` route: its path (or the function that builds it) and what its
/// handler expression references.
struct Route {
    path: String,
    module: Vec<String>,
    refs: Vec<Ref>,
}

/// A `use` of a module: names it brings in, and the modules it brings in
/// whole (`*`).
#[derive(Default)]
struct Uses {
    names: BTreeMap<String, Vec<String>>,
    globs: Vec<Vec<String>>,
}

#[derive(Default)]
struct Crate {
    items: Vec<Item>,
    routes: Vec<Route>,
    uses: BTreeMap<Vec<String>, Uses>,
    modules: BTreeSet<Vec<String>>,
}

fn last_ident(path: &syn::Path) -> Option<String> {
    path.segments.last().map(|s| s.ident.to_string())
}

fn segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && {
            let mut test = false;
            let _ = a.parse_nested_meta(|m| {
                test |= m.path.is_ident("test");
                Ok(())
            });
            test
        }
    })
}

/// `crate`, `self` and `super` resolved against `module`.
fn absolute(module: &[String], segs: &[String]) -> Option<Vec<String>> {
    match segs.first().map(String::as_str) {
        Some("crate") => Some(segs[1..].to_vec()),
        Some("self") => Some([module, &segs[1..]].concat()),
        Some("super") => {
            let mut base = module.to_vec();
            let mut rest = segs;
            while rest.first().map(String::as_str) == Some("super") {
                base.pop()?;
                rest = &rest[1..];
            }
            Some([&base[..], rest].concat())
        }
        _ => None,
    }
}

/// Whether `expr` is `<something>.http`: the client apps/web reaches
/// apps/api with.
fn is_http_client(expr: &syn::Expr) -> bool {
    matches!(expr, syn::Expr::Field(f) if matches!(&f.member, syn::Member::Named(n) if n == "http"))
}

/// Whether `expr` names `Method::GET`.
fn is_get_method(expr: &syn::Expr) -> bool {
    matches!(expr, syn::Expr::Path(p) if last_ident(&p.path).as_deref() == Some("GET"))
}

/// Collects what a function body or a handler expression references.
#[derive(Default)]
struct Body {
    refs: Vec<Ref>,
    sink: Option<String>,
    /// The names it binds: arguments, `let`, closures, patterns. A path of
    /// one of these names is the variable, not a function.
    bindings: BTreeSet<String>,
}

impl Body {
    /// What it references, less the variables.
    fn into_refs(self) -> Vec<Ref> {
        let bindings = self.bindings;
        self.refs
            .into_iter()
            .filter(
                |r| !matches!(r, Ref::Path(segs) if segs.len() == 1 && bindings.contains(&segs[0])),
            )
            .collect()
    }
}

impl<'ast> Visit<'ast> for Body {
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        self.bindings.insert(p.ident.to_string());
        syn::visit::visit_pat_ident(self, p);
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        self.refs.push(Ref::Path(segments(&p.path)));
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // `api_request_auth` sends the method it is given: the call site
        // tells whether it changes something, and it is not followed.
        if let syn::Expr::Path(f) = &*call.func {
            if last_ident(&f.path).as_deref() == Some("api_request_auth") {
                if !call.args.iter().nth(1).is_some_and(is_get_method) {
                    self.sink = Some("api_request_auth with a method other than GET".into());
                }
                for arg in &call.args {
                    self.visit_expr(arg);
                }
                return;
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let method = call.method.to_string();
        if is_http_client(&call.receiver) {
            let changes = match method.as_str() {
                "post" | "put" | "patch" | "delete" => true,
                "request" => !call.args.first().is_some_and(is_get_method),
                _ => false,
            };
            if changes {
                self.sink = Some(format!("http.{method}"));
            }
        }
        self.refs.push(Ref::Method(method));
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // `format!`, `vec!`, `matches!`…: their arguments are expressions.
        // A `view!` is not, and is left unread.
        if let Ok(args) =
            mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        {
            for arg in &args {
                match arg {
                    // `format!("{name}", name = value)`: `name` is no path.
                    syn::Expr::Assign(a) => self.visit_expr(&a.right),
                    _ => self.visit_expr(arg),
                }
            }
        }
    }
}

/// The identifiers of a type, generics included: `CurrentUser`,
/// `Option<Query<Q>>` → `Option`, `Query`, `Q`.
fn type_idents(ty: &syn::Type, out: &mut Vec<String>) {
    struct Idents<'a>(&'a mut Vec<String>);
    impl<'ast> Visit<'ast> for Idents<'_> {
        fn visit_path_segment(&mut self, s: &'ast syn::PathSegment) {
            self.0.push(s.ident.to_string());
            syn::visit::visit_path_segment(self, s);
        }
    }
    Idents(out).visit_type(ty);
}

/// The `GET` handler expressions of a method router: `get(h)`,
/// `get(h).post(p)`, `post(p).get(h)`.
fn get_handlers<'a>(expr: &'a syn::Expr, out: &mut Vec<&'a syn::Expr>) {
    match expr {
        syn::Expr::Call(call) => {
            if let syn::Expr::Path(f) = &*call.func {
                if last_ident(&f.path).as_deref() == Some("get") {
                    out.extend(call.args.first());
                }
            }
        }
        syn::Expr::MethodCall(call) => {
            if call.method == "get" {
                out.extend(call.args.first());
            }
            get_handlers(&call.receiver, out);
        }
        _ => {}
    }
}

/// Finds the `.route(path, get(…))` calls of a function body.
struct RouteFinder<'c> {
    module: Vec<String>,
    within: String,
    routes: &'c mut Vec<Route>,
}

impl<'ast> Visit<'ast> for RouteFinder<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "route" && call.args.len() == 2 {
            let path = match &call.args[0] {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => s.value(),
                _ => format!("<a path {} builds>", self.within),
            };
            let mut handlers = Vec::new();
            get_handlers(&call.args[1], &mut handlers);
            for handler in handlers {
                let mut body = Body::default();
                body.visit_expr(handler);
                self.routes.push(Route {
                    path: path.clone(),
                    module: self.module.clone(),
                    refs: body.into_refs(),
                });
            }
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

impl Crate {
    fn read(src: &Path) -> Crate {
        let mut krate = Crate::default();
        krate.read_file(&src.join("main.rs"), Vec::new());
        krate
    }

    /// Reads the module `module` from `file`.
    fn read_file(&mut self, file: &Path, module: Vec<String>) {
        let source =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let parsed = syn::parse_file(&source).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        // A module's children sit beside it (`main.rs`, `mod.rs`) or in a
        // directory named after it (`foo.rs` → `foo/`).
        let dir = match file.file_name().and_then(|n| n.to_str()) {
            Some("main.rs" | "mod.rs") => file.parent().unwrap().to_path_buf(),
            _ => file.with_extension(""),
        };
        self.read_items(&parsed.items, &module, &dir);
    }

    fn read_items(&mut self, items: &[syn::Item], module: &[String], dir: &Path) {
        self.modules.insert(module.to_vec());
        for item in items {
            match item {
                syn::Item::Mod(m) if !is_cfg_test(&m.attrs) => {
                    let child = [module, &[m.ident.to_string()]].concat();
                    let child_dir = dir.join(m.ident.to_string());
                    match &m.content {
                        Some((_, items)) => self.read_items(items, &child, &child_dir),
                        None => {
                            let file = [
                                dir.join(format!("{}.rs", m.ident)),
                                child_dir.join("mod.rs"),
                            ]
                            .into_iter()
                            .find(|f| f.exists())
                            .unwrap_or_else(|| panic!("no file for mod {}", m.ident));
                            self.read_file(&file, child);
                        }
                    }
                }
                syn::Item::Fn(f) if !is_cfg_test(&f.attrs) => {
                    self.add_fn(module, None, &f.sig, &f.block);
                }
                syn::Item::Impl(i) if !is_cfg_test(&i.attrs) => {
                    let self_ty = match &*i.self_ty {
                        syn::Type::Path(p) => last_ident(&p.path),
                        _ => None,
                    };
                    for impl_item in &i.items {
                        if let syn::ImplItem::Fn(f) = impl_item {
                            if !is_cfg_test(&f.attrs) {
                                self.add_fn(module, self_ty.clone(), &f.sig, &f.block);
                            }
                        }
                    }
                }
                syn::Item::Use(u) if !is_cfg_test(&u.attrs) => {
                    self.add_use(module, Vec::new(), &u.tree);
                }
                _ => {}
            }
        }
    }

    fn add_fn(
        &mut self,
        module: &[String],
        self_ty: Option<String>,
        sig: &syn::Signature,
        block: &syn::Block,
    ) {
        let mut body = Body::default();
        body.visit_block(block);
        let mut arg_types = Vec::new();
        for input in &sig.inputs {
            if let syn::FnArg::Typed(arg) = input {
                body.visit_pat(&arg.pat);
                type_idents(&arg.ty, &mut arg_types);
            }
        }
        let name = sig.ident.to_string();
        let mut finder = RouteFinder {
            module: module.to_vec(),
            within: format!("{}::{name}", module.join("::")),
            routes: &mut self.routes,
        };
        finder.visit_block(block);
        self.items.push(Item {
            module: module.to_vec(),
            self_ty,
            name,
            sink: body.sink.clone(),
            refs: body.into_refs(),
            arg_types,
        });
    }

    fn add_use(&mut self, module: &[String], prefix: Vec<String>, tree: &syn::UseTree) {
        let full = |segs: Vec<String>| absolute(module, &segs).unwrap_or(segs);
        match tree {
            syn::UseTree::Path(p) => {
                let mut prefix = prefix;
                prefix.push(p.ident.to_string());
                self.add_use(module, prefix, &p.tree);
            }
            syn::UseTree::Name(n) => {
                let mut segs = prefix;
                segs.push(n.ident.to_string());
                let path = full(segs);
                let name = path.last().unwrap().clone();
                let uses = self.uses.entry(module.to_vec()).or_default();
                uses.names.insert(name, path);
            }
            syn::UseTree::Rename(r) => {
                let mut segs = prefix;
                segs.push(r.ident.to_string());
                let path = full(segs);
                let uses = self.uses.entry(module.to_vec()).or_default();
                uses.names.insert(r.rename.to_string(), path);
            }
            syn::UseTree::Glob(_) => {
                let path = full(prefix);
                self.uses
                    .entry(module.to_vec())
                    .or_default()
                    .globs
                    .push(path);
            }
            syn::UseTree::Group(g) => {
                for tree in &g.items {
                    self.add_use(module, prefix.clone(), tree);
                }
            }
        }
    }

    fn free_fns<'a>(
        &'a self,
        module: &'a [String],
        name: &'a str,
    ) -> impl Iterator<Item = usize> + 'a {
        self.items
            .iter()
            .enumerate()
            .filter(move |(_, i)| i.self_ty.is_none() && i.module == module && i.name == name)
            .map(|(n, _)| n)
    }

    fn methods<'a>(
        &'a self,
        self_ty: Option<&'a str>,
        name: &'a str,
    ) -> impl Iterator<Item = usize> + 'a {
        self.items
            .iter()
            .enumerate()
            .filter(move |(_, i)| {
                i.self_ty.is_some()
                    && i.name == name
                    && self_ty.is_none_or(|t| i.self_ty.as_deref() == Some(t))
            })
            .map(|(n, _)| n)
    }

    /// The functions `r`, met in `module` (in an `impl` of `self_ty`), may
    /// lead to.
    fn resolve(&self, module: &[String], self_ty: Option<&str>, r: &Ref) -> Vec<usize> {
        let segs = match r {
            Ref::Method(name) => return self.methods(None, name).collect(),
            Ref::Path(segs) => segs,
        };
        let uses = self.uses.get(module);
        // The first segment, as a `use` or a relative path names it.
        let first = &segs[0];
        let path: Vec<String> = if let Some(abs) = absolute(module, segs) {
            abs
        } else if first == "Self" {
            return self_ty
                .map(|t| self.methods(Some(t), &segs[segs.len() - 1]).collect())
                .unwrap_or_default();
        } else if let Some(imported) = uses.and_then(|u| u.names.get(first)) {
            [&imported[..], &segs[1..]].concat()
        } else if segs.len() == 1 {
            let local: Vec<usize> = self.free_fns(module, first).collect();
            if !local.is_empty() {
                return local;
            }
            let globbed: Vec<usize> = uses
                .map(|u| {
                    u.globs
                        .iter()
                        .flat_map(|g| self.free_fns(g, first))
                        .collect()
                })
                .unwrap_or_default();
            if !globbed.is_empty() {
                return globbed;
            }
            // Out of sight: every function of that name.
            return self
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.self_ty.is_none() && &i.name == first)
                .map(|(n, _)| n)
                .collect();
        } else if self.modules.contains(&[module, &segs[..1]].concat()) {
            [module, &segs[..]].concat()
        } else {
            segs.clone()
        };
        let (name, parent) = path.split_last().unwrap();
        let mut found: Vec<usize> = self.free_fns(parent, name).collect();
        // `Type::method`, the type named by its last segment.
        if let Some(ty) = parent.last() {
            found.extend(self.methods(Some(ty), name));
        }
        found
    }

    /// The extractors of `item`'s arguments: `from_request_parts` and
    /// `from_request` of the crate's types among them.
    fn extractors(&self, item: &Item) -> Vec<usize> {
        item.arg_types
            .iter()
            .flat_map(|t| {
                self.methods(Some(t), "from_request_parts")
                    .chain(self.methods(Some(t), "from_request"))
            })
            .collect()
    }

    /// The chain of calls from `route`'s handler to a request that changes
    /// something on apps/api, if there is one. `stop` is not followed.
    fn path_to_sink(&self, route: &Route, stop: &[usize]) -> Option<Vec<String>> {
        let mut parent: BTreeMap<usize, Option<usize>> = BTreeMap::new();
        let mut queue = VecDeque::new();
        for r in &route.refs {
            for n in self.resolve(&route.module, None, r) {
                if !stop.contains(&n) && !parent.contains_key(&n) {
                    parent.insert(n, None);
                    queue.push_back(n);
                }
            }
        }
        while let Some(n) = queue.pop_front() {
            let item = &self.items[n];
            if let Some(sink) = &item.sink {
                let mut chain = vec![format!("{} ({sink})", self.name_of(n))];
                let mut at = parent[&n];
                while let Some(p) = at {
                    chain.push(self.name_of(p));
                    at = parent[&p];
                }
                chain.reverse();
                return Some(chain);
            }
            let next = item
                .refs
                .iter()
                .flat_map(|r| self.resolve(&item.module, item.self_ty.as_deref(), r))
                .chain(self.extractors(item));
            for m in next.collect::<Vec<_>>() {
                if !stop.contains(&m) && !parent.contains_key(&m) {
                    parent.insert(m, Some(n));
                    queue.push_back(m);
                }
            }
        }
        None
    }

    fn name_of(&self, n: usize) -> String {
        let i = &self.items[n];
        let mut path = i.module.clone();
        path.extend(i.self_ty.clone());
        path.push(i.name.clone());
        path.join("::")
    }

    fn index_of(&self, module: &[&str], name: &str) -> usize {
        let module: Vec<String> = module.iter().map(|s| s.to_string()).collect();
        let found: Vec<usize> = self.free_fns(&module, name).collect();
        assert_eq!(found.len(), 1, "{}::{name}", module.join("::"));
        found[0]
    }
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The `GET` routes from which a request that changes something on apps/api
/// can be reached, with the chain that leads there.
fn offending_routes(krate: &Crate) -> Vec<String> {
    let (module, name) = ADMIN_REAUTHENTICATION;
    let reauthenticate = krate.index_of(module, name);
    krate
        .routes
        .iter()
        .filter_map(|route| {
            let stop = if route.path.starts_with("/admin") {
                vec![reauthenticate]
            } else {
                vec![]
            };
            krate
                .path_to_sink(route, &stop)
                .map(|chain| format!("GET {}: {}", route.path, chain.join(" -> ")))
        })
        .collect()
}

#[test]
fn no_get_route_outside_the_admitted_list_asks_apps_api_for_a_change() {
    let krate = Crate::read(&src_dir());
    // Sanity bounds on the parse itself: it found the router and the
    // requests that change something.
    assert!(
        krate.routes.len() > 60,
        "{} GET routes found",
        krate.routes.len()
    );
    assert!(
        krate.items.iter().filter(|i| i.sink.is_some()).count() > 20,
        "too few requests to apps/api that change something found"
    );
    let offending = offending_routes(&krate);
    assert!(offending.is_empty(), "{}", offending.join("\n"));
}

/// The parse finds what it is meant to find: on the admin routes, the
/// reauthentication leads to `POST /auth/logout`, and so do the `POST`
/// routes it is not asked about.
#[test]
fn the_parse_finds_the_admin_reauthentication_and_the_post_routes() {
    let krate = Crate::read(&src_dir());
    let admin = krate
        .routes
        .iter()
        .find(|r| r.path == "/admin/users")
        .expect("a GET /admin/users route");
    let chain = krate
        .path_to_sink(admin, &[])
        .expect("the reauthentication");
    assert!(
        chain
            .iter()
            .any(|c| c.ends_with("routes::admin::reauthenticate")),
        "{chain:?}"
    );
    assert!(
        chain.last().unwrap().contains("logout_to (http.post)"),
        "{chain:?}"
    );

    // `POST /logout`, read as if it were a GET route.
    let logout = Route {
        path: "/logout".into(),
        module: Vec::new(),
        refs: vec![Ref::Path(
            ["routes", "home", "logout"].map(String::from).to_vec(),
        )],
    };
    assert!(krate.path_to_sink(&logout, &[]).is_some());
}

/// Each kind of request that changes something is told from a read.
#[test]
fn the_parse_tells_a_change_from_a_read() {
    let sink = |body: &str| {
        let block: syn::Block = syn::parse_str(&format!("{{ {body} }}")).unwrap();
        let mut b = Body::default();
        b.visit_block(&block);
        b.sink
    };
    assert!(sink("state.http.post(url).send()").is_some());
    assert!(sink("state.http.put(url)").is_some());
    assert!(sink("state.http.patch(url)").is_some());
    assert!(sink("state.http.delete(url)").is_some());
    assert!(sink("state.http.request(Method::POST, url)").is_some());
    assert!(sink("state.http.request(method, url)").is_some());
    assert!(sink("api_request_auth(&state, reqwest::Method::DELETE, &p, c, None)").is_some());
    assert!(sink("api_request_auth(&state, method, &p, c, None)").is_some());
    assert!(sink("format!(\"{}\", state.http.post(url))").is_some());

    assert!(sink("state.http.get(url).send()").is_none());
    assert!(sink("state.http.request(reqwest::Method::GET, url)").is_none());
    assert!(sink("api_request_auth(&state, reqwest::Method::GET, &p, c, None)").is_none());
    // A `post` that is not apps/api's client.
    assert!(sink("router.post(handler)").is_none());
}
