//! Route DSL shapes in Shopify Core's component route files that dropped
//! routes, or their URL helpers, before:
//!
//! - `scope controller: :x do … end` and `controller :x do … end` give the
//!   verbs inside a default controller; a bare verb in a namespace maps to
//!   the namespace's module;
//! - a scope inside `member` / `collection` keeps its level, so its path
//!   follows the resource segment;
//! - a lambda or redirect-block endpoint cannot be dispatched, but Rails
//!   names it, so its URL helper stays;
//! - an optional group anywhere in a path (`(/:locale)/cart(/:id)`) makes
//!   the params it names optional while the others stay required;
//! - a `*glob` is a helper param, not literal text in the helper's path.

use roundhouse::App;
use roundhouse::ingest::ingest_routes;
use roundhouse::lower::routes::{FlatRoute, flatten_routes, flatten_routes_for_helpers};

fn app_with(body: &str) -> App {
    let src = format!("Rails.application.routes.draw do\n{body}\nend\n");
    let table = ingest_routes(src.as_bytes(), "config/routes.rb").expect("ingest routes");
    let mut app = App::default();
    app.routes = table;
    app
}

fn named(app: &App) -> Vec<FlatRoute> {
    flatten_routes_for_helpers(app).into_iter().filter(|r| r.named).collect()
}

fn helper<'a>(routes: &'a [FlatRoute], name: &str) -> &'a FlatRoute {
    routes
        .iter()
        .find(|r| r.as_name == name)
        .unwrap_or_else(|| panic!("no `{name}` helper in {:?}", routes.iter().map(|r| &r.as_name).collect::<Vec<_>>()))
}

fn dispatched(app: &App) -> Vec<(String, String, String)> {
    flatten_routes(app)
        .into_iter()
        .map(|r| (r.path, r.controller.0.as_str().to_string(), r.action.as_str().to_string()))
        .collect()
}

#[test]
fn scope_controller_and_controller_block_supply_the_default_controller() {
    let app = app_with(
        "  scope controller: :billing do\n    get :settings\n  end\n  controller :domains do\n    get \"signup/check\", action: :check\n  end\n",
    );
    let routes = dispatched(&app);
    assert!(
        routes.contains(&("/settings".into(), "BillingController".into(), "settings".into())),
        "{routes:?}"
    );
    assert!(
        routes.contains(&("/signup/check".into(), "DomainsController".into(), "check".into())),
        "{routes:?}"
    );
}

#[test]
fn a_bare_verb_in_a_namespace_maps_to_the_module_controller() {
    let app = app_with("  namespace :admin do\n    get :stafff\n    get \"a/b\"\n  end\n");
    let routes = dispatched(&app);
    assert!(
        routes.contains(&("/admin/stafff".into(), "AdminController".into(), "stafff".into())),
        "{routes:?}"
    );
    assert!(
        routes.contains(&("/admin/a/b".into(), "Admin::AController".into(), "b".into())),
        "`get \"a/b\"` is `a#b`: {routes:?}"
    );
}

#[test]
fn a_scope_in_a_collection_goes_after_the_resource_segment() {
    let app = app_with(
        "  resources :extensions, only: [] do\n    collection do\n      scope :approval_requests, controller: \"approval_requests\" do\n        post :submit\n      end\n    end\n  end\n",
    );
    let routes = dispatched(&app);
    assert!(
        routes.contains(&(
            "/extensions/approval_requests/submit".into(),
            "ApprovalRequestsController".into(),
            "submit".into()
        )),
        "{routes:?}"
    );
}

#[test]
fn an_endpoint_the_router_cannot_run_keeps_its_helper_and_is_not_dispatched() {
    let app = app_with(
        "  get \"/b\", to: redirect { |_p, req| req.original_url }, as: :blk\n  get \"/c\", to: ->(env) { [200, {}, []] }, as: :lam\n  get \"/ok\", to: \"c#ok\", as: :ok\n",
    );
    let helpers = named(&app);
    for name in ["blk", "lam", "ok"] {
        helper(&helpers, name);
    }
    assert_eq!(
        helper(&helpers, "lam").refused.as_ref().map(|(p, _)| p.as_str()),
        Some("endpoint")
    );
    let routes = dispatched(&app);
    assert_eq!(routes.len(), 1, "only `/ok` is served: {routes:?}");
}

#[test]
fn optional_groups_leave_the_other_params_required() {
    let app = app_with(
        "  scope \"(/:shop_route_locale)\" do\n    get \"/cart/add(/:id)\", to: \"carts#add\", as: :cart_add\n    get \"/pages/:slug(/:tab/edit)\", to: \"pages#show\", as: :page\n    get \"/mid(/:a)/tail\", to: \"carts#mid\", as: :mid\n  end\n",
    );
    let helpers = named(&app);
    let cart_add = helper(&helpers, "cart_add");
    assert_eq!(cart_add.path, "/:shop_route_locale/cart/add/:id");
    assert_eq!(cart_add.required_params, 0);
    assert_eq!(cart_add.required_path.as_deref(), Some("/cart/add"));
    // Required params come first, then the optional ones in path order.
    let page = helper(&helpers, "page");
    assert_eq!(page.path_params, ["slug", "shop_route_locale", "tab"]);
    assert_eq!(page.required_params, 1);
    // Every variant is routed: with and without each group.
    let paths: Vec<_> = dispatched(&app).into_iter().map(|(p, ..)| p).collect();
    for expected in ["/cart/add", "/fr/cart/add", "/:shop_route_locale/cart/add", "/mid/tail"] {
        let want = expected.replace("/fr/", "/:shop_route_locale/");
        assert!(paths.contains(&want), "{want} in {paths:?}");
    }
}

#[test]
fn a_leading_optional_scope_applies_to_resources() {
    let app = app_with(
        "  scope \"(/:locale)\" do\n    resources :things, only: [:index, :show, :update]\n  end\n",
    );
    let helpers = named(&app);
    let thing = helper(&helpers, "thing");
    assert_eq!(thing.path_params, ["id", "locale"]);
    assert_eq!(thing.required_params, 1);
    assert_eq!(helper(&helpers, "things").required_params, 0);
    let verbs: Vec<_> = flatten_routes(&app)
        .into_iter()
        .map(|r| format!("{:?} {}", r.method, r.path))
        .collect();
    assert!(verbs.contains(&"Put /things/:id".to_string()), "{verbs:?}");
    assert!(verbs.contains(&"Patch /:locale/things/:id".to_string()), "{verbs:?}");
}

#[test]
fn a_glob_is_a_helper_param_the_router_still_serves() {
    let app = app_with("  get \"/a/*path\", to: \"c#a\", as: :glob\n  get \"/z(/*rest)\", to: \"c#z\", as: :opt_glob\n");
    let helpers = named(&app);
    let glob = helper(&helpers, "glob");
    assert_eq!(glob.path_params, ["path"]);
    assert_eq!(glob.required_params, 1);
    let optional = helper(&helpers, "opt_glob");
    assert_eq!(optional.path_params, ["rest"]);
    assert_eq!(optional.required_params, 0);
    let paths: Vec<_> = dispatched(&app).into_iter().map(|(p, ..)| p).collect();
    assert_eq!(paths, ["/a/*path", "/z/*rest", "/z"]);
}
