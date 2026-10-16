//! `x_path(…, host: "example.com")` → `x_path(…)`.
//!
//! Rails partitions a route helper's option hash into the keys
//! `url_for` consumes ITSELF and the keys it forwards to the path
//! generator. `ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS`
//! (actionpack 8.1.3, `route_set.rb:838`) is that first list:
//!
//! ```text
//! [:host, :protocol, :port, :subdomain, :domain, :tld_length,
//!  :trailing_slash, :anchor, :params, :only_path, :script_name,
//!  :original_script_name]
//! ```
//!
//! `url_for` deletes every one of them from `path_options` before
//! generating (`route_set.rb:864`), so none can ever reach the query
//! string. We forwarded them all, and a leftover option is a query key
//! here: campfire's `room_at_message_url(@room, msg, host:
//! "once.campfire.test")` rendered `/rooms/1/@5?host=once.campfire.test`
//! where Rails renders `http://once.campfire.test/rooms/1/@5`. That is
//! the app's own broadcast assertion failing, and note it is wrong
//! TWICE — a query key that should not be there, and an authority that
//! should. Dropping `host:` alone fixes only the first half and leaves
//! a bare path being compared against an absolute URL.
//!
//! **A rule table, not a special case for `host:`.** The list splits
//! three ways once you ask what each key does to a PATH:
//!
//!   * [`HOST_ONLY_OPTIONS`] — the seven that only describe the host
//!     half of a URL. `path_for` renders no host at all, so dropping
//!     them is EXACT rather than an approximation. This pass strips
//!     them from the call site; the demand survey in
//!     `routes_to_library` refuses to make a parameter out of them.
//!
//!     **On the `_url` spelling they are not dropped — they are the
//!     answer.** `x_url(…, host: h)` renders `"#{Rails.application.protocol}#{h}#{x_path(…)}"`,
//!     the same shape the view lowerer grounds a hostless `_url` with
//!     (`Rails.application.domain` in place of `h`) and the same one
//!     `emit::ruby::library::rewrite_url_helpers_absolute` builds for
//!     the explicit `…routes.url_helpers.x_url(…, host:)` chain.
//!     Dropping the host here would be the second half of the campfire
//!     bug rather than its fix: the button that assertion compares
//!     against holds an ABSOLUTE URL.
//!   * `anchor:` — the fragment. It DOES belong on a path, and
//!     `routes_to_library` now renders it (`#tag`, after the query
//!     string, exactly where `path_for` puts it). Four lobsters call
//!     sites and two writebook ones were passing it to a helper that
//!     had no such parameter.
//!   * `format:` — owned by [`super::route_format_suffix`], which
//!     monomorphizes the helper instead of widening its signature.
//!
//!   * `params:` — the query itself. `url_for` merges its value into the
//!     generated query string, which is what an erased `**splat` renders
//!     too, so `rewrites::route_helper_query_splat_index` reads the two
//!     as one shape: `x_path(…) + RouteHelpers.query_suffix(h)`. campfire's
//!     tests write it twice, both with a nested value.
//!
//! The three left over — `script_name:`, `original_script_name:` and
//! `trailing_slash:` — each genuinely change the path, and none is
//! modeled. They stay query keys, which is visibly wrong rather than
//! silently wrong, and no corpus app writes one on an app route.
//! Ledgered in `docs/pipeline/runtime.md`.
//!
//! Scope, and ordering: bare `*_path` / `*_url` calls whose name
//! matches a route in the app's own table, rewritten before
//! `lower_routes_to_library_functions` surveys those same call sites.
//! `rails_blob_path(…, only_path: true)` is an ActiveStorage helper
//! with its own declared parameter and names no app route, so it is
//! untouched.

use crate::app::App;
use crate::expr::{Expr, ExprNode, InterpPart, Literal};
use crate::ident::Symbol;

/// The `RESERVED_OPTIONS` entries that describe only the HOST half of
/// a URL — protocol, authority, and the choice of whether to render
/// one at all.
///
/// `path_for` is `url_for(…, PATH, …)`, and the `PATH` strategy
/// returns `options[:path]` with the script name, params and anchor
/// applied and nothing else (`http/url.rb`). Every key here feeds
/// `build_host_url`, which that strategy never calls. So a `_path`
/// helper drops them, and drops them exactly.
pub(crate) const HOST_ONLY_OPTIONS: &[&str] = &[
    "host",
    "protocol",
    "port",
    "subdomain",
    "domain",
    "tld_length",
    "only_path",
];

pub fn apply_route_url_options_lowering(app: &mut App) {
    let helpers = super::route_format_suffix::route_helper_names(app);
    if helpers.is_empty() {
        return;
    }
    bare_url_helper_receivers(app, &helpers);
    let params = helper_path_params(app);
    super::for_each_hook_body(app, &mut |e| position_path_params(e, &params));
    for view in &mut app.views {
        position_path_params(&mut view.body, &params);
    }
    super::for_each_hook_body(app, &mut |e| rewrite(e, &helpers));
    for view in &mut app.views {
        rewrite(&mut view.body, &helpers);
    }
    // `for_each_hook_body` does not reach test bodies, and the call
    // site this pass exists for is one — campfire's
    // `MessagesControllerTest`.
    for tm in &mut app.test_modules {
        if let Some(setup) = &mut tm.setup {
            rewrite(setup, &helpers);
        }
        for t in &mut tm.tests {
            rewrite(&mut t.body, &helpers);
        }
        for m in &mut tm.helpers {
            rewrite(&mut m.body, &helpers);
        }
    }
}

/// `UrlHelpers.admin_export_download_url(token: t, filename: f, host: h)`
/// is the bare helper call: a class that singleton-includes
/// `Rails.application.routes.url_helpers` (the include is recorded as
/// RouteHelpers, Core's `UrlHelpers`) answers every named route helper, so
/// the receiver adds nothing. Dropping it hands the call to the passes
/// below that spell path params positionally and ground `host:` /
/// `protocol:`; left qualified, the emit retargeted it to `RouteHelpers`
/// afterwards and the keyword path params reached a positional helper.
pub(crate) fn bare_url_helper_receivers(app: &mut App, helpers: &crate::det::HashSet<String>) {
    let classes: crate::det::HashSet<Symbol> = app
        .library_classes
        .iter()
        .filter(|lc| lc.includes.iter().any(|i| i.0.as_str() == "RouteHelpers"))
        .map(|lc| lc.name.0.clone())
        .collect();
    if classes.is_empty() {
        return;
    }
    let strip = &mut |e: &mut Expr| strip_receivers(e, &classes, helpers);
    super::for_each_hook_body(app, strip);
    for view in &mut app.views {
        strip_receivers(&mut view.body, &classes, helpers);
    }
    for tm in &mut app.test_modules {
        if let Some(setup) = &mut tm.setup {
            strip_receivers(setup, &classes, helpers);
        }
        for t in &mut tm.tests {
            strip_receivers(&mut t.body, &classes, helpers);
        }
        for m in &mut tm.helpers {
            strip_receivers(&mut m.body, &classes, helpers);
        }
    }
}

fn strip_receivers(
    expr: &mut Expr,
    classes: &crate::det::HashSet<Symbol>,
    helpers: &crate::det::HashSet<String>,
) {
    expr.node.for_each_child_mut(&mut |c| strip_receivers(c, classes, helpers));
    let ExprNode::Send { recv, method, .. } = &mut *expr.node else { return };
    let Some(r) = recv else { return };
    let ExprNode::Const { path } = &*r.node else { return };
    if path.len() == 1 && classes.contains(&path[0]) && helpers.contains(method.as_str()) {
        *recv = None;
    }
}

/// Each named helper's path params, in slot order. A name several
/// flattened routes share (lobsters' `/s/:id/(:title)` flattens to
/// `/s/:id/:title` and `/s/:id`) answers its longest list — the helper
/// the generator builds takes every slot.
pub(crate) fn helper_path_params(app: &App) -> crate::det::HashMap<String, Vec<String>> {
    let mut out: crate::det::HashMap<String, Vec<String>> = Default::default();
    for route in super::routes::flatten_routes_for_helpers(app) {
        if !route.named {
            continue;
        }
        for suffix in ["_path", "_url"] {
            let slot = out.entry(format!("{}{suffix}", route.as_name)).or_default();
            if route.path_params.len() > slot.len() {
                *slot = route.path_params.clone();
            }
        }
    }
    out
}

/// `story_short_id_path(id, title: slug)` → `story_short_id_path(id,
/// slug)`. Rails fills a path param from the option hash as readily as
/// from a positional argument — lobsters names the optional `(:title)`
/// segment on every story link — but the generated helper takes path
/// params positionally, and the keyword reached it as an unknown one.
/// A keyword naming a path param moves into that param's slot; a slot
/// skipped on the way (an optional one nothing names) is filled with
/// `nil`, which is what the helper's own default would have been.
fn position_path_params(
    expr: &mut Expr,
    params: &crate::det::HashMap<String, Vec<String>>,
) {
    expr.node.for_each_child_mut(&mut |c| position_path_params(c, params));
    rewrite_position_node(expr, params);
}

pub(crate) fn rewrite_position_node(
    expr: &mut Expr,
    params: &crate::det::HashMap<String, Vec<String>>,
) {
    let span = expr.span;
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &mut *expr.node else {
        return;
    };
    let Some(names) = params.get(method.as_str()) else { return };
    let Some(last) = args.last() else { return };
    let is_url = method.as_str().ends_with("_url");
    if let Some(spread) = splat_options(last, args.len() - 1, names, is_url) {
        args.pop();
        args.extend(spread);
        return;
    }
    if let Some(spread) = spread_options_local(last, args.len() - 1, names) {
        args.pop();
        args.extend(spread);
        return;
    }
    let ExprNode::Hash { entries, kwargs: true } = &*last.node else { return };
    let key_of = |k: &Expr| match &*k.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        _ => None,
    };
    let positional = args.len() - 1;
    let named: Vec<usize> = (positional..names.len())
        .filter(|i| entries.iter().any(|(k, _)| key_of(k).as_deref() == Some(names[*i].as_str())))
        .collect();
    let Some(&furthest) = named.last() else { return };
    let Some(last) = args.pop() else { return };
    let ExprNode::Hash { mut entries, .. } = *last.node else { unreachable!() };
    for i in positional..=furthest {
        let taken = entries
            .iter()
            .position(|(k, _)| key_of(k).as_deref() == Some(names[i].as_str()))
            .map(|at| entries.remove(at).1);
        args.push(taken.unwrap_or_else(|| {
            Expr::new(span, ExprNode::Lit { value: Literal::Nil })
        }));
    }
    if !entries.is_empty() {
        args.push(Expr::new(last.span, ExprNode::Hash { entries, kwargs: true }));
    }
}

/// One link of an erased `**` chain: literal pairs, or a hash read.
enum SplatPart<'a> {
    Pairs(&'a [(Expr, Expr)]),
    Spread(&'a Expr),
}

/// The links of `{ origin: o, **options }` — the `merge` chain the
/// ingest desugar made of it — or of a lone Hash-typed local. `None`
/// for anything else, so a record argument is never read as a splat.
fn splat_parts(arg: &Expr, top: bool) -> Option<Vec<SplatPart<'_>>> {
    match &*arg.node {
        ExprNode::Var { .. } | ExprNode::Ivar { .. } => {
            // A bare read is a splat only when it is a Hash: inside a
            // chain `merge` already says so, standing alone the type
            // must (a model local is a record, not options).
            if top && !matches!(&arg.ty, Some(crate::ty::Ty::Hash { .. })) {
                return None;
            }
            Some(vec![SplatPart::Spread(arg)])
        }
        ExprNode::Hash { entries, .. } if !top => Some(vec![SplatPart::Pairs(entries)]),
        ExprNode::Send { recv: Some(base), method, args, block: None, .. }
            if method.as_str() == "merge" && args.len() == 1 =>
        {
            let mut parts = splat_parts(base, false)?;
            parts.extend(splat_parts(&args[0], false)?);
            Some(parts)
        }
        _ => None,
    }
}

fn sym_key(key: &str, span: crate::span::Span) -> Expr {
    Expr::new(span, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(key) } })
}

/// `admin_export_download_url(**options)` where `options` holds the
/// path params (`token:`, `filename:`) and the host: Rails fills a
/// segment from an option hash as readily as from a positional
/// argument, and Core's `CsvOutputCollector.download_url` builds one
/// hash for two route helpers. The generated helper takes segments
/// positionally, and the splat reached it as keywords it does not
/// have.
///
/// Each segment slot the call left open is read off the chain
/// (`options[:token]`; an earlier literal pair becomes the default,
/// `options.fetch(:origin, o)`, since a later `**` wins as in Ruby).
/// A `_url` spelling also reads `host:` and `protocol:` off it, which
/// the host pass below turns into the absolute URL. Whatever the chain
/// holds beyond the segments and the host options is the query, handed
/// on as `params:` for `RouteHelpers.query_suffix`.
///
/// `anchor:` out of a splat is dropped, not rendered: it is a
/// fragment, the query is not, and no Core call site puts one there.
/// Ruby's `**` raises for a missing required key; the read here is nil.
///
/// Declines when the helper has no segments left to fill and is a
/// `_path` (the existing trailing-splat rendering owns that), and for a
/// lone Hash local that the older [`spread_options_local`] handles
/// exactly — unless the helper is a `_url`, which needs the host.
fn splat_options(arg: &Expr, index: usize, names: &[String], is_url: bool) -> Option<Vec<Expr>> {
    let parts = splat_parts(arg, true)?;
    let chain = matches!(&*arg.node, ExprNode::Send { .. });
    let lone = !chain;
    if index > names.len() || (index == names.len() && !is_url) {
        return None;
    }
    if lone && !is_url && index != 0 {
        return None;
    }
    let span = arg.span;
    let value_ty = match &arg.ty {
        Some(crate::ty::Ty::Hash { value, .. }) => Some((**value).clone()),
        _ => None,
    };
    let read_ty = |e: &mut Expr| {
        if let Some(v) = &value_ty {
            e.ty = Some(crate::ty::Ty::Union { variants: vec![v.clone(), crate::ty::Ty::Nil].into() });
        }
    };
    let key_of = |k: &Expr| match &*k.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        _ => None,
    };
    let send = |recv: &Expr, method: &str, args: Vec<Expr>| {
        Expr::new(
            span,
            ExprNode::Send {
                recv: Some(recv.clone()),
                method: Symbol::from(method),
                args,
                block: None,
                parenthesized: true,
            },
        )
    };
    // The value the chain gives `key`: last writer wins.
    let read = |key: &str| -> Option<Expr> {
        let mut acc: Option<Expr> = None;
        for part in &parts {
            match part {
                SplatPart::Pairs(entries) => {
                    if let Some((_, v)) = entries.iter().rev().find(|(k, _)| key_of(k).as_deref() == Some(key)) {
                        acc = Some(v.clone());
                    }
                }
                SplatPart::Spread(h) => {
                    acc = Some(match acc.take() {
                        None => {
                            let mut e = send(h, "[]", vec![sym_key(key, span)]);
                            read_ty(&mut e);
                            e
                        }
                        Some(default) => send(h, "fetch", vec![sym_key(key, span), default]),
                    });
                }
            }
        }
        acc
    };
    let nil = || Expr::new(span, ExprNode::Lit { value: Literal::Nil });
    let mut out: Vec<Expr> = names[index..].iter().map(|n| read(n).unwrap_or_else(nil)).collect();
    let mut kwargs: Vec<(Expr, Expr)> = Vec::new();
    if is_url {
        for key in ["host", "protocol"] {
            if let Some(v) = read(key) {
                kwargs.push((sym_key(key, span), v));
            }
        }
    }
    // The query: every key that is neither a segment nor reserved.
    let reserved = |k: &str| {
        names.iter().any(|n| n == k) || k == "anchor" || k == "params" || k == "format"
            || HOST_ONLY_OPTIONS.contains(&k)
    };
    let has_spread = parts.iter().any(|p| matches!(p, SplatPart::Spread(_)));
    let mut literal_query: Vec<(Expr, Expr)> = Vec::new();
    for part in &parts {
        if let SplatPart::Pairs(entries) = part {
            for (k, v) in entries.iter() {
                if key_of(k).is_some_and(|k| !reserved(&k)) {
                    literal_query.push((k.clone(), v.clone()));
                }
            }
        }
    }
    if !has_spread {
        kwargs.extend(literal_query);
    } else {
        let excluded: Vec<Expr> = names
            .iter()
            .map(String::as_str)
            .chain(["anchor", "params", "format"])
            .chain(HOST_ONLY_OPTIONS.iter().copied())
            .map(|k| sym_key(k, span))
            .collect();
        let mut rest: Option<Expr> = None;
        for part in &parts {
            let piece = match part {
                SplatPart::Spread(h) => send(h, "except", excluded.clone()),
                SplatPart::Pairs(_) => continue,
            };
            rest = Some(match rest {
                None => piece,
                Some(acc) => send(&acc, "merge", vec![piece]),
            });
        }
        let mut rest = rest?;
        if !literal_query.is_empty() {
            let lit = Expr::new(span, ExprNode::Hash { entries: literal_query, kwargs: false });
            rest = send(&lit, "merge", vec![rest]);
        }
        rest.ty = arg.ty.clone();
        kwargs.push((sym_key("params", span), rest));
    }
    if !kwargs.is_empty() {
        out.push(Expr::new(span, ExprNode::Hash { entries: kwargs, kwargs: true }));
    }
    Some(out)
}

/// `story_short_id_path(story, options)` where `options` is a local
/// Hash — lobsters' `comment_target_path` builds `{anchor: …}` and
/// adds `title:` conditionally. Rails reads a trailing Hash as the
/// option hash; the generated helper, with nothing to tell it so, bound
/// the whole Hash to the `title` slot. Spread into the path slots the
/// call left open plus `anchor:` — the keys this lowering knows the
/// helper to take. `None` unless the argument is a Hash-typed local
/// sitting IN a path slot (a local, so reading it once per key is
/// free of effects).
fn spread_options_local(arg: &Expr, index: usize, names: &[String]) -> Option<Vec<Expr>> {
    if index >= names.len() || index == 0 {
        return None;
    }
    let ExprNode::Var { .. } = &*arg.node else { return None };
    let Some(crate::ty::Ty::Hash { value, .. }) = &arg.ty else { return None };
    let read = |key: &str| {
        let mut e = Expr::new(
            arg.span,
            ExprNode::Send {
                recv: Some(arg.clone()),
                method: Symbol::from("[]"),
                args: vec![Expr::new(
                    arg.span,
                    ExprNode::Lit { value: Literal::Sym { value: Symbol::from(key) } },
                )],
                block: None,
                parenthesized: true,
            },
        );
        e.ty = Some(crate::ty::Ty::Union { variants: vec![(**value).clone(), crate::ty::Ty::Nil].into() });
        e
    };
    let mut out: Vec<Expr> = names[index..].iter().map(|n| read(n)).collect();
    let anchor_key = Expr::new(
        arg.span,
        ExprNode::Lit { value: Literal::Sym { value: Symbol::from("anchor") } },
    );
    out.push(Expr::new(
        arg.span,
        ExprNode::Hash { entries: vec![(anchor_key, read("anchor"))], kwargs: true },
    ));
    Some(out)
}

fn rewrite(expr: &mut Expr, helpers: &crate::det::HashSet<String>) {
    expr.node.for_each_child_mut(&mut |c| rewrite(c, helpers));
    rewrite_node(expr, helpers);
}

pub(crate) fn rewrite_node(expr: &mut Expr, helpers: &crate::det::HashSet<String>) {
    ground_symbol_query_values(expr, helpers);
    let Some((stem, host, protocol)) = strip_host_options(expr, helpers) else {
        return;
    };
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { args, parenthesized, .. } = node else { unreachable!() };
    // A BARE `<stem>_path` send, not a qualified one: the demand survey
    // reads these call sites for query keys and the `RouteHelpers.`
    // receiver goes on afterwards, so qualifying here would hide the
    // call from both.
    let path_call = Expr::new(
        span,
        ExprNode::Send {
            recv: None,
            method: Symbol::from(format!("{stem}_path")),
            args,
            block: None,
            parenthesized,
        },
    );
    // `protocol:` rides bare (`"https"`), the convention
    // `rewrite_url_helpers_absolute` already set — Rails'
    // `normalize_protocol` accepts `"https"` and `"https://"` alike and
    // we accept only the first.
    // Without one, the scheme is the request's, as Rails' `url_for`
    // takes it — `Rails.application.protocol` carries its own `://`.
    let mut parts: Vec<InterpPart> = Vec::new();
    match protocol {
        Some(p) => {
            parts.push(InterpPart::Expr { expr: p });
            parts.push(InterpPart::Text { value: "://".to_string() });
        }
        None => parts.push(InterpPart::Expr {
            expr: crate::lower::view_to_library::rails_application_call("protocol"),
        }),
    }
    parts.push(InterpPart::Expr { expr: host });
    parts.push(InterpPart::Expr { expr: path_call });
    *expr.node = ExprNode::StringInterp { parts };
    expr.ty = Some(crate::ty::Ty::Str);
}

/// `x_path(size: :small)` → `x_path(size: "small")`. A query value
/// reaches Rails' generator through `to_param`, and a Symbol's is its
/// spelling, so the literal is exact. The helper's parameter is typed
/// `String?` from the route's demand, and a strict target refuses the
/// Symbol at the call (`no implicit conversion of Symbol into String`
/// took two of campfire's logo tests). `format:` is not here: the
/// suffix pass ran first and consumed it.
fn ground_symbol_query_values(expr: &mut Expr, helpers: &crate::det::HashSet<String>) {
    let ExprNode::Send { recv: None, method, args, .. } = &mut *expr.node else { return };
    if !helpers.contains(method.as_str()) {
        return;
    }
    let Some(last) = args.last_mut() else { return };
    let ExprNode::Hash { entries, kwargs: true } = &mut *last.node else { return };
    for (_, v) in entries.iter_mut() {
        if let ExprNode::Lit { value: Literal::Sym { value } } = &*v.node {
            let text = value.as_str().to_string();
            *v.node = ExprNode::Lit { value: Literal::Str { value: text } };
            v.ty = Some(crate::ty::Ty::Str);
        }
    }
}

/// Remove every [`HOST_ONLY_OPTIONS`] key from one route-helper call's
/// trailing kwargs hash.
///
/// Answers `Some((stem, host, protocol))` for the one case where the
/// removal is not the whole story: a `_url` spelling that named a
/// `host:` and did not ask for `only_path`. Its caller rebuilds that
/// into `"<protocol>://<host><stem>_path(…)"`. Every other call —
/// `_path`, a hostless `_url`, `x_url(only_path: true)` — is finished
/// by the strip alone and answers None.
fn strip_host_options(
    expr: &mut Expr,
    helpers: &crate::det::HashSet<String>,
) -> Option<(String, Expr, Option<Expr>)> {
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &mut *expr.node else {
        return None;
    };
    if !helpers.contains(method.as_str()) {
        return None;
    }
    let last = args.last_mut()?;
    let ExprNode::Hash { entries, kwargs: true } = &mut *last.node else {
        return None;
    };
    // Read the two that BUILD a host before dropping them: on the `_url`
    // spelling they are not dropped at all — they ARE the host half, and
    // this is the last place that knows the caller named one.
    let mut host: Option<Expr> = None;
    let mut protocol: Option<Expr> = None;
    let mut only_path = false;
    for (k, v) in entries.iter() {
        let ExprNode::Lit { value: Literal::Sym { value } } = &*k.node else {
            continue;
        };
        match value.as_str() {
            "host" => host = Some(v.clone()),
            "protocol" => protocol = Some(v.clone()),
            "only_path" => {
                only_path =
                    matches!(&*v.node, ExprNode::Lit { value: Literal::Bool { value: true } });
            }
            _ => {}
        }
    }
    entries.retain(|(k, _)| {
        !matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } }
            if HOST_ONLY_OPTIONS.contains(&value.as_str()))
    });
    // A hash that held ONLY host options is gone entirely, so the call
    // reaches a helper that grew no parameter for them; one that also
    // carried query keys keeps them. The same disposal
    // `route_format_suffix` makes, for the same reason.
    if entries.is_empty() {
        args.pop();
    }
    // `_path` is done: the seven describe a host it does not render.
    let stem = method.as_str().strip_suffix("_url")?;
    // `x_url(…, only_path: true)` is Rails asking the URL spelling for
    // a path, which is the `_path` helper — the strip above already
    // made it one.
    if only_path {
        return None;
    }
    // `x_url(…, host: h)` names the host EXPLICITLY, and dropping it
    // would be the second half of the campfire bug rather than its fix:
    // the copy-link button that assertion compares against holds an
    // ABSOLUTE URL. The view lowerer already grounds a hostless `_url`
    // as `"#{Rails.application.protocol}#{Rails.application.domain}#{…_path}"`; this is that
    // shape with the caller's host in place of the default, and it is
    // what `emit::ruby::library::rewrite_url_helpers_absolute` builds
    // for the explicit `…routes.url_helpers.x_url(…, host:)` chain.
    // Two spellings, one rendering.
    Some((stem.to_string(), host?, protocol))
}
