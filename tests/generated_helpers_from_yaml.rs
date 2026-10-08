//! A helper object built at load time from a YAML table has methods no
//! `def` declares: core's `WebUrlHelpers` gets `<key>_path` / `<key>_url`
//! for every entry of `config/web_paths.yml`, because its factory
//! `define_method`s them. The set is the table's keys crossed with the
//! factory's name suffixes, so a call to a key the table has resolves
//! and a call to one it lacks is still a dispatch failure.
//!
//! The factory is Core's `WebUrlHelpersFactory`, unchanged: blocks with
//! keyword parameters (`do |params: {}, anchor: nil, **args|`) calling a
//! `protected` `def replace_named_args` written inside the `Class.new`
//! block, and building each string with `ActionDispatch::Http::URL.
//! path_for` / `full_url_for` (runtime/spinel/action_dispatch_http_url.rb).
//! Emitted for Ruby, the helpers it defines answer as they do natively
//! under the pinned actionpack, and the def keeps its visibility.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{Analyzer, DiagnosticKind, diagnose};
use roundhouse::ingest::ingest_app_from_tree;

/// Core's components/platform/essentials/app/models/web_url_helpers_factory.rb,
/// verbatim.
const FACTORY: &str = r###"# typed: true
# frozen_string_literal: true

# Add new paths on components/platform/essentials/config/web_paths.yml
class WebUrlHelpersFactory
  class << self
    def create(paths)
      Class.new do
        paths.each do |config_name, path|
          define_method "#{config_name}_path" do |params: {}, anchor: nil, **args|
            #: self as singleton(WebUrlHelpersFactory)
            finalized_path = replace_named_args(path, **args)
            ActionDispatch::Http::URL.path_for(path: finalized_path, params: params, anchor: anchor)
          end

          define_method(
            "#{config_name}_url"
          ) do |params: {}, anchor: nil, host: nil, protocol: "https", port: nil, **args|
            #: self as singleton(WebUrlHelpersFactory)
            finalized_path = replace_named_args(path, **args)
            ActionDispatch::Http::URL.full_url_for(
              path: finalized_path,
              params: params,
              anchor: anchor,
              host: host,
              protocol: protocol,
              port: port
            )
          end
        end

        protected

        def replace_named_args(path, **args)
          named_args = path.scan(/(?<=:)[a-z_]+/)
          named_args.each do |named_arg|
            unless args.key?(named_arg.to_sym)
              raise ArgumentError, "[WebUrlHelpersFactory] Named arg <#{named_arg}> is not provided."
            end

            path = path.gsub(":" + named_arg, args[named_arg.to_sym].to_s.tr(" ", "+"))
          end

          path
        end
      end.new
    end
  end
end
"###;

fn app(controller_body: &str) -> roundhouse::App {
    let files = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n".to_string()),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n".to_string(),
        ),
        ("app/models/web_url_helpers_factory.rb", FACTORY.to_string()),
        (
            "app/models/web_paths.rb",
            "WebPaths = WebUrlHelpersFactory.create(YAML.load_file(File.join(Rails.root, \"config/web_paths.yml\"))[\"paths\"])\n"
                .to_string(),
        ),
        (
            "config/web_paths.yml",
            "# comment\npaths:\n  admin_orders: /admin/orders\n  admin_order_show: /admin/orders/:id\n".to_string(),
        ),
        (
            "app/controllers/foo_controller.rb",
            format!(
                "class FooController < ApplicationController\n  def show\n{controller_body}\n    render plain: \"\"\n  end\nend\n"
            ),
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.into_bytes()))
        .collect();
    ingest_app_from_tree(tree).expect("ingest")
}

fn failed_sends(mut app: roundhouse::App) -> Vec<String> {
    Analyzer::new(&app).analyze(&mut app);
    diagnose(&app)
        .into_iter()
        .filter_map(|d| match d.kind {
            DiagnosticKind::SendDispatchFailed { method, .. } => Some(method.as_str().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn table_keys_crossed_with_factory_suffixes_are_the_method_set() {
    let app = app("");
    let names: Vec<&str> = app
        .generated_helper_methods
        .values()
        .flatten()
        .map(|n| n.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["admin_order_show_path", "admin_order_show_url", "admin_orders_path", "admin_orders_url"]
    );
}

#[test]
fn calls_the_table_defines_resolve() {
    let failed = failed_sends(app(
        "    @a = WebPaths.admin_orders_path\n    @b = WebPaths.admin_order_show_url(id: 1, host: \"x\")",
    ));
    assert!(failed.is_empty(), "{failed:?}");
}

#[test]
fn a_call_the_table_lacks_still_fails() {
    let failed = failed_sends(app("    @a = WebPaths.admin_nothing_path"));
    assert_eq!(failed, vec!["admin_nothing_path"]);
}

/// Core's WebUrlHelpersFactoryTest observations, and more: query
/// params with nesting and escaping, escaped anchors, explicit and
/// default ports and protocols, named args with spaces, the missing
/// named arg's ArgumentError and full_url_for's missing host.
const OBSERVE: &str = r###"helpers = WebUrlHelpersFactory.create("home" => "/admin", "payments_settings_index" => "/admin/settings/payments")
p helpers.home_path
p helpers.home_path(params: { search: "search value" }, anchor: "anchor")
p helpers.payments_settings_index_url(params: { search: "search value" }, anchor: "anchor", host: "www.shop1.com", protocol: "https", port: "80")
p helpers.home_url(params: { search: "search value" }, anchor: "anchor", host: "www.shop1.com", port: "80")
p helpers.home_url(host: "www.shop1.com", port: 443)
p helpers.home_url(host: "http://shop.test:3000", protocol: nil)
p helpers.home_path(params: { b: [2, 1], a: { y: nil, x: "1 2" }, c: nil, d: [] }, anchor: "a b#c")
p helpers.home_path(params: "raw", anchor: :top)
named = WebUrlHelpersFactory.create("admin_discounts_code_show" => "/admin/discounts/:discount_id/codes/:code_id")
p named.admin_discounts_code_show_path(discount_id: 123, code_id: 456)
p named.admin_discounts_code_show_url(discount_id: 123, code_id: 456, host: "example.org")
spaces = WebUrlHelpersFactory.create("admin_discounts_code_show" => "/admin/discounts/:discount_id?code=:code_id")
p spaces.admin_discounts_code_show_path(discount_id: 123, code_id: "My Code")
p [helpers.respond_to?(:home_path), helpers.respond_to?(:home_url), helpers.class.protected_method_defined?(:replace_named_args)]
begin
  helpers.replace_named_args("/x")
rescue NoMethodError => e
  p e.class
end
begin
  named.admin_discounts_code_show_path(discount_id: 123)
rescue ArgumentError => e
  p e.message
end
begin
  helpers.home_url
rescue ArgumentError => e
  p e.message
end
"###;

#[test]
fn core_factory_helpers_run_as_native_actionpack() {
    // Native: the factory under the pinned actionpack and activesupport's
    // core_ext (tests/support/rails_oracle.rb), as Core runs it.
    let scratch = std::env::temp_dir().join(format!("rh_web_url_native_{}.rb", std::process::id()));
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle");
    std::fs::write(
        &scratch,
        format!(
            "require_relative {oracle:?}\nRailsOracle.activate(\"activesupport\", \"actionpack\")\n\
             require \"active_support\"\nrequire \"active_support/core_ext\"\nrequire \"action_dispatch\"\n\
             {FACTORY}\n{OBSERVE}"
        ),
    )
    .expect("write native script");
    let native = emit_and_run::ruby().arg(&scratch).output().expect("spawn ruby");
    let _ = std::fs::remove_file(&scratch);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(native.contains("\"https://www.shop1.com:80/admin?search=search+value#anchor\""), "{native}");
    let run = emit_and_run::real_blog()
        .write("app/lib/web_url_helpers_factory.rb", FACTORY)
        .run_ruby(&format!(
            "require_relative \"app/models/web_url_helpers_factory\"\n{OBSERVE}\n\
             $stderr.puts ActionDispatch::Http::URL.method(:path_for).source_location.first\n"
        ));
    run.assert_passes();
    assert_eq!(run.stdout, native);
    // The port answered, not a native actionpack.
    assert!(run.stderr.trim_end().ends_with("runtime/action_dispatch_http_url.rb"), "{}", run.stderr);
    let tree = std::fs::read_to_string(run.emitted.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(tree.contains("require_relative \"action_dispatch_http_url\""), "{tree}");
}
