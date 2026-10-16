//! An in-repo engine's `config/routes.rb` draws into the application's
//! route set, as Rails does (`Rails.application.routes.draw`), and the
//! helper names follow Rails' naming for the DSL Shopify Core's
//! components use.
//!
//! Core's own `config/routes.rb` is a comment ("Please add any new routes
//! to component or package engines instead"), so before this the emitted
//! `RouteHelpers` was empty and `admin_payments_complete_connected_paypal_path`
//! and friends were undefined. The expected names below were read off a
//! real ActionDispatch route set drawn from the same files.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::lower::flatten_routes;

const LOCK: &str = "PATH\n  remote: components/shop\n  specs:\n    shop (1.0)\n\n\
    PATH\n  remote: components/pages\n  specs:\n    pages (1.0)\n\n\
    GEM\n  remote: https://rubygems.org/\n  specs:\n    rails (8.0.2)\n\n\
    DEPENDENCIES\n  pages!\n  shop!\n  rails\n";

const APP_ROUTES: &str = "# Please add any new routes to component engines instead.\n";

const SHOP_ROUTES: &str = r#"Rails.application.routes.draw do
  namespace :admin do
    resources :charges do
      member do
        post :activate
      end
      collection do
        get :recent
      end
    end
    resources :fulfillment_request, only: [:create] do
      collection do
        post :accept
      end
    end
    resource :subscription, only: [:show] do
      member do
        post :extend_trial
      end
    end
    namespace :api, path: nil do
      resources :pings, only: [:index]
    end
  end
end
"#;

const PAGES_ROUTES: &str = r#"Rails.application.routes.append do
  get "/*path", to: "pages#show", as: :catch_all
end

Rails.application.routes.draw do
  get "/about", to: "pages#about", as: :about
end
"#;

fn engine(name: &str, camel: &str) -> (String, String) {
    (
        format!("components/{name}/lib/{name}/engine.rb"),
        format!("module {camel}\n  class Engine < ::Rails::Engine\n  end\nend\n"),
    )
}

fn flat() -> Vec<(String, String)> {
    let (shop_engine_path, shop_engine) = engine("shop", "Shop");
    let (pages_engine_path, pages_engine) = engine("pages", "Pages");
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("Gemfile.lock", LOCK),
        ("config/routes.rb", APP_ROUTES),
        ("components/shop/config/routes.rb", SHOP_ROUTES),
        ("components/pages/config/routes.rb", PAGES_ROUTES),
        (shop_engine_path.as_str(), shop_engine.as_str()),
        (pages_engine_path.as_str(), pages_engine.as_str()),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let app = ingest_app_from_tree(tree).expect("ingest tree");
    flatten_routes(&app)
        .into_iter()
        .filter(|r| r.named)
        .map(|r| (r.as_name, r.path))
        .collect()
}

fn path_of(routes: &[(String, String)], name: &str) -> Option<String> {
    routes.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone())
}

#[test]
fn engine_route_files_name_the_apps_helpers() {
    let routes = flat();
    assert_eq!(path_of(&routes, "about").as_deref(), Some("/about"), "{routes:?}");
    assert_eq!(path_of(&routes, "admin_charges").as_deref(), Some("/admin/charges"), "{routes:?}");
}

#[test]
fn member_and_collection_routes_are_verb_first_after_the_namespace() {
    let routes = flat();
    assert_eq!(
        path_of(&routes, "activate_admin_charge").as_deref(),
        Some("/admin/charges/:id/activate"),
        "{routes:?}"
    );
    assert_eq!(
        path_of(&routes, "recent_admin_charges").as_deref(),
        Some("/admin/charges/recent"),
        "{routes:?}"
    );
}

#[test]
fn a_resource_whose_singular_is_its_plural_names_its_collection_index() {
    let routes = flat();
    assert_eq!(
        path_of(&routes, "accept_admin_fulfillment_request_index").as_deref(),
        Some("/admin/fulfillment_request/accept"),
        "{routes:?}"
    );
    assert_eq!(
        path_of(&routes, "admin_fulfillment_request_index").as_deref(),
        Some("/admin/fulfillment_request"),
        "{routes:?}"
    );
}

#[test]
fn a_singular_resources_members_carry_no_id() {
    let routes = flat();
    assert_eq!(
        path_of(&routes, "extend_trial_admin_subscription").as_deref(),
        Some("/admin/subscription/extend_trial"),
        "{routes:?}"
    );
}

#[test]
fn a_namespace_with_a_nil_path_has_no_url_segment() {
    let routes = flat();
    assert_eq!(path_of(&routes, "admin_api_pings").as_deref(), Some("/admin/pings"), "{routes:?}");
}

#[test]
fn an_appended_block_draws_after_every_draw() {
    let (shop_engine_path, shop_engine) = engine("shop", "Shop");
    let (pages_engine_path, pages_engine) = engine("pages", "Pages");
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("Gemfile.lock", LOCK),
        ("config/routes.rb", APP_ROUTES),
        ("components/shop/config/routes.rb", SHOP_ROUTES),
        ("components/pages/config/routes.rb", PAGES_ROUTES),
        (shop_engine_path.as_str(), shop_engine.as_str()),
        (pages_engine_path.as_str(), pages_engine.as_str()),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let app = ingest_app_from_tree(tree).expect("ingest tree");
    let order: Vec<String> = flatten_routes(&app).into_iter().map(|r| r.path).collect();
    let catch_all = order.iter().position(|p| p == "/*path").expect("catch-all route");
    let about = order.iter().position(|p| p == "/about").expect("about route");
    assert!(catch_all > about, "append sits after every draw: {order:?}");
}
