//! Route helpers called with Rails' url_for options from module
//! concerns and class methods, and the shared test helper's controller
//! dispatch through the drawn route set.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ROUTES: &str = r#"Rails.application.routes.draw do
    scope ":shop", as: :shop, shop: /\d+/ do
      scope module: "checkouts/web" do
        resources :orders, only: [:show] do
          member do
            match :authenticate, via: [:get, :post]
          end
        end
        resources :checkouts, only: [:show] do
          member do
            get :processing
          end
        end
      end
    end
  get "files/:token/:filename/download", to: "orders#authenticate", constraints: { filename: %r{[^/]+} }, as: :export_download
  get "files/:token/:origin/:filename/download", to: "orders#authenticate", as: :allow_inactive_export_download
end
"#;

const CONCERN: &str = r#"module CheckoutUrlHelpers
  def authenticate_order_url(shop, order)
    authenticate_shop_order_path(shop, order.token, key: order.secret_token)
  end

  def show_url(shop, token, h)
    processing_shop_checkout_path(shop, token, params: h)
  end

  def anchored(shop, token)
    processing_shop_checkout_path(shop, token, anchor: "top")
  end
end
"#;

const COLLECTOR: &str = r#"class CsvOutputCollector
  class << self
    def download_url(token:, filename:, host:, origin: nil)
      options = {
        token: token,
        filename: filename,
        host: host,
        protocol: "https",
      }

      if origin
        UrlHelpers.allow_inactive_export_download_url(origin: origin, **options)
      else
        UrlHelpers.export_download_url(**options)
      end
    end
  end
end
"#;

const URL_HELPERS: &str = r#"module UrlHelpers
  class << self
    include(Rails.application.routes.url_helpers)
  end
end
"#;

const CONTROLLER: &str = r#"class OrdersController < ApplicationController
  def authenticate
    render plain: "ok"
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/controllers/concerns/checkout_url_helpers.rb", CONCERN)
        .write("app/controllers/orders_controller.rb", CONTROLLER)
        .write("app/models/csv_output_collector.rb", COLLECTOR)
        .write("app/models/url_helpers.rb", URL_HELPERS)
        .write("app/models/refund_thing.rb", "class RefundThing\n  include CheckoutUrlHelpers\n  def go(o)\n    authenticate_order_url(7, o)\n  end\nend\n")
        .write("config/routes.rb", ROUTES)
}

const SCRIPT: &str = r##"
Order = Struct.new(:token, :secret_token)
puts RefundThing.new.go(Order.new("tok", "sec"))
puts CsvOutputCollector.download_url(token: "t1", filename: "a.csv", host: "x.test")
puts CsvOutputCollector.download_url(token: "t1", filename: "f.csv", host: "x.test", origin: "o9")
class Probe
  include CheckoutUrlHelpers
end
puts Probe.new.show_url(7, "tk", { "a" => "1", "b" => "2" })
puts Probe.new.anchored(7, "tk")
"##;

#[test]
fn library_module_route_helper_options_and_splat_render_as_rails_does() {
    let run = app().run_ruby(SCRIPT);
    run.assert_passes();
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "/7/orders/tok/authenticate?key=sec",
            "https://x.test/files/t1/a.csv/download",
            "https://x.test/files/t1/o9/f.csv/download",
            "/7/checkouts/tk/processing?a=1&b=2",
            "/7/checkouts/tk/processing#top",
        ],
        "{}",
        run.stdout
    );
}

/// Core's storefront `match ":path", constraints: { path: /.*/ }` under
/// `scope("(:shop_route_locale)")` is the route a bare `/` reaches.
#[test]
fn a_catch_all_segment_with_a_dot_star_constraint_recognizes_the_root() {
    let routes = r#"Rails.application.routes.draw do
  scope("(:shop_route_locale)", shop_route_locale: /[a-z]{2}/) do
    match ":path", to: "orders#authenticate", constraints: { path: /.*/ }, via: [:get, :post]
  end
end
"#;
    let run = app().write("config/routes.rb", routes).run_ruby(
        r##"
p Rails.application.routes.recognize_path("/")
p Rails.application.routes.recognize_path("/en/cart")
p Rails.application.routes.recognize_path("/cart")
"##,
    );
    run.assert_passes();
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            r#"{controller: "orders", action: "authenticate", path: ""}"#,
            r#"{controller: "orders", action: "authenticate", shop_route_locale: "en", path: "cart"}"#,
            r#"{controller: "orders", action: "authenticate", path: "cart"}"#,
        ],
        "{}",
        run.stdout
    );
}

/// An unconstrained `/:id` and a `*glob` still refuse the root, as in
/// Rails: only a constraint that admits the empty string matches it.
#[test]
fn the_root_is_not_matched_by_an_unconstrained_segment_or_a_glob() {
    let routes = r#"Rails.application.routes.draw do
  get ":id", to: "orders#authenticate"
  get "*rest", to: "orders#authenticate", as: :glob
end
"#;
    let run = app().write("config/routes.rb", routes).run_ruby(
        r##"
begin
  Rails.application.routes.recognize_path("/")
rescue ActionController::RoutingError
  puts "no route"
end
p Rails.application.routes.recognize_path("/x/y")
"##,
    );
    run.assert_passes();
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines,
        vec!["no route", r#"{controller: "orders", action: "authenticate", rest: "x/y"}"#],
        "{}",
        run.stdout
    );
}
