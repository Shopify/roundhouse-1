//! Route shapes that Shopify Core's component route files use and that
//! named or placed helpers differently from Rails (each expectation is the
//! name and path a real `ActionDispatch::Routing::RouteSet` gives):
//!
//! - `namespace` inside `resources` nests the parent first
//!   (`/shops/:shop_id/courier/...`, `services_internal_shop_courier_...`);
//! - a route whose path has no plain action name (`get "p/:id"`) takes the
//!   scope's own name, first route wins;
//! - a canonical action symbol in `member`/`collection` (`get :index`) is
//!   the resource's own path and name, a string path is not;
//! - `root action:` in a namespace, `path:` on a symbol verb, `as: nil`;
//! - an un-composed mount with a literal `as:` keeps its helper;
//! - `scope format: true` makes the helper's format a required last param;
//! - `load(path_local.join("x.rb"))` follows a path held in a local.

use roundhouse::App;
use roundhouse::ingest::ingest_routes;
use roundhouse::lower::routes::{FlatRoute, flatten_routes_for_helpers};

fn named_paths(body: &str) -> Vec<(String, String)> {
    let src = format!("Rails.application.routes.draw do\n{body}\nend\n");
    let table = ingest_routes(src.as_bytes(), "config/routes.rb").expect("ingest routes");
    let mut app = App::default();
    app.routes = table;
    flatten_routes_for_helpers(&app)
        .into_iter()
        .filter(|r: &FlatRoute| r.named)
        .map(|r| (r.as_name, r.path))
        .collect()
}

fn has(routes: &[(String, String)], name: &str, path: &str) {
    assert!(
        routes.iter().any(|(n, p)| n == name && p == path),
        "want `{name}` at {path}; got {routes:?}"
    );
}

fn lacks(routes: &[(String, String)], name: &str) {
    assert!(routes.iter().all(|(n, _)| n != name), "unexpected `{name}` in {routes:?}");
}

#[test]
fn namespace_in_resources_nests_the_parent_first() {
    let r = named_paths(
        "namespace :services do\n namespace :internal do\n  resources :shops, only: [:show] do\n   namespace :courier do\n    resources :validation, only: [:create]\n   end\n  end\n end\nend\nresources :users do\n scope :extra do\n  resource :badge\n end\n namespace :admin do\n  resources :notes, only: [:index]\n end\nend",
    );
    has(&r, "services_internal_shop_courier_validation_index", "/services/internal/shops/:shop_id/courier/validation");
    has(&r, "user_admin_notes", "/users/:user_id/admin/notes");
    // A plain scope's path still prefixes the whole nesting.
    has(&r, "user_badge", "/extra/users/:user_id/badge");
}

#[test]
fn a_route_without_a_plain_action_name_takes_the_scopes_name_once() {
    let r = named_paths(
        "namespace :auth do\n get \"preview-store/:shop_id\", to: \"a#show\"\n get \"preview-store/:shop_id/refresh\", to: \"a#refresh\"\n get \"other\", to: \"a#o\"\nend\nget \"/foo/:id\", to: \"f#show\"\nscope \"(:loc)\" do\n scope \"cart\", as: :cart do\n  get \"checkout(/:id)\", to: \"c#co\"\n end\nend",
    );
    has(&r, "auth", "/auth/preview-store/:shop_id");
    has(&r, "auth_other", "/auth/other");
    has(&r, "cart", "/:loc/cart/checkout/:id");
    assert_eq!(r.iter().filter(|(n, _)| n == "auth").count(), 1, "{r:?}");
    // Top level, no scope name: nothing to name it.
    assert!(r.iter().all(|(_, p)| p != "/foo/:id"), "{r:?}");
}

#[test]
fn canonical_action_symbols_are_the_resource_path_and_strings_are_not() {
    let r = named_paths(
        "resources :shops, only: [:show] do\n collection do\n  get :index\n  post :create\n  get :foo\n  delete \"destroy\", action: :destroy, as: :destroy\n end\n member do\n  patch :update\n  get :multipass\n end\nend\nresource :fraud, only: [] do\n collection { get :index }\nend",
    );
    has(&r, "shops", "/shops");
    has(&r, "shop", "/shops/:id");
    has(&r, "foo_shops", "/shops/foo");
    has(&r, "multipass_shop", "/shops/:id/multipass");
    has(&r, "fraud", "/fraud");
    // `delete "destroy"` is a string path: it keeps its segment.
    has(&r, "destroy_shops", "/shops/destroy");
    assert_eq!(r.iter().filter(|(n, _)| n == "shops").count(), 1, "{r:?}");
}

#[test]
fn root_action_path_option_and_unnamed_routes() {
    let r = named_paths(
        "namespace :search do\n root action: \"new\", via: \"get\"\n get :new\nend\nscope \"admin/api/:version_handle\" do\n root to: \"home#index\"\nend\nresources :shops, only: [] do\n member { get :token, path: \"auth-token\" }\n post :render_it, path: \"retail_receipt\", to: \"r#c\"\nend\nget \"blocked\", to: redirect(\"/x\"), as: nil",
    );
    has(&r, "search_root", "/search");
    has(&r, "root", "/admin/api/:version_handle");
    has(&r, "token_shop", "/shops/:id/auth-token");
    has(&r, "shop_render_it", "/shops/:shop_id/retail_receipt");
    lacks(&r, "blocked");
}

#[test]
fn an_uncomposed_mount_with_a_literal_as_keeps_its_helper() {
    let r = named_paths(
        "constraints host: \"x\" do\n mount Dash::Engine => \"/dashboard\", as: :developer_dashboard\nend\nscope \"customer_account\" do\n mount Reports::Engine, at: \"/\", as: :reports\nend",
    );
    has(&r, "developer_dashboard", "/dashboard");
    has(&r, "reports", "/customer_account");
}

#[test]
fn format_true_requires_the_format_param() {
    let src = "Rails.application.routes.draw do\n namespace :admin do\n  namespace :settings, path: \"\", format: true do\n   resources :locations, only: [:index] do\n    get :count, on: :collection\n   end\n  end\n  get \"plain\", to: \"p#q\"\n end\nend\n";
    let table = ingest_routes(src.as_bytes(), "config/routes.rb").expect("ingest routes");
    let mut app = App::default();
    app.routes = table;
    let routes = flatten_routes_for_helpers(&app);
    let count = routes.iter().find(|r| r.as_name == "count_admin_settings_locations").expect("count");
    assert!(count.format_required);
    let plain = routes.iter().find(|r| r.as_name == "admin_plain").expect("plain");
    assert!(!plain.format_required);
}

#[test]
fn load_follows_a_path_held_in_a_local() {
    let table = roundhouse::ingest::routes::ingest_routes_with_draws(
        b"Rails.application.routes.draw do\n  routes_path = Engine.root.join(\"config\", \"routes\", \"services\")\n  load(routes_path.join(\"external.rb\").to_s)\nend\n",
        "config/routes.rb",
        &roundhouse::det::HashMap::from_iter([(
            "services/external".to_string(),
            (
                b"Rails.application.routes.draw do\n  get \"/ext/ping\", to: \"ext#ping\"\nend\n".to_vec(),
                "config/routes/services/external.rb".to_string(),
            ),
        )]),
    )
    .expect("ingest routes");
    let mut app = App::default();
    app.routes = table;
    let routes: Vec<_> = flatten_routes_for_helpers(&app).into_iter().map(|r| r.path).collect();
    assert!(routes.contains(&"/ext/ping".to_string()), "{routes:?}");
}
