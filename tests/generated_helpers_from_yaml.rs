//! A helper object built at load time from a YAML table has methods no
//! `def` declares: core's `WebUrlHelpers` gets `<key>_path` / `<key>_url`
//! for every entry of `config/web_paths.yml`, because its factory
//! `define_method`s them. The set is the table's keys crossed with the
//! factory's name suffixes, so a call to a key the table has resolves
//! and a call to one it lacks is still a dispatch failure.
//!
//! The factory's blocks take Core's keyword parameters
//! (`do |params: {}, anchor: nil, **args|`); emitted for Ruby, the
//! helpers it defines answer as native Ruby's do.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{Analyzer, DiagnosticKind, diagnose};
use roundhouse::ingest::ingest_app_from_tree;

const FACTORY: &str = r###"class PathsFactory
  class << self
    def create(paths)
      Class.new do
        paths.each do |config_name, path|
          define_method "#{config_name}_path" do |params: {}, anchor: nil, **args|
            finalized_path = PathsFactory.replace_named_args(path, **args)
            query = params.map { |key, value| "#{key}=#{value}" }.join("&")
            finalized_path += "?#{query}" unless query.empty?
            finalized_path += "##{anchor}" if anchor
            finalized_path
          end

          define_method(
            "#{config_name}_url"
          ) do |params: {}, host: nil, protocol: "https", **args|
            "#{protocol}://#{host}#{PathsFactory.replace_named_args(path, **args)}"
          end
        end
      end.new
    end

    # Core's factory defines this inside the `Class.new` block, where a
    # `def` is lifted to nil at ingest (an anonymous class's methods
    # have no IR home yet); here it is the factory's own.
    def replace_named_args(path, **args)
      path.scan(/(?<=:)[a-z_]+/).each do |named_arg|
        raise ArgumentError, "Named arg <#{named_arg}> is not provided." unless args.key?(named_arg.to_sym)

        path = path.gsub(":" + named_arg, args[named_arg.to_sym].to_s)
      end
      path
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
        ("app/models/paths_factory.rb", FACTORY.to_string()),
        (
            "app/models/web_paths.rb",
            "WebPaths = PathsFactory.create(YAML.load_file(File.join(Rails.root, \"config/web_paths.yml\"))[\"paths\"])\n"
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

#[test]
fn keyword_block_helpers_run_as_native() {
    let observe = "helpers = PathsFactory.create(\"admin_orders\" => \"/admin/orders\", \"admin_order_show\" => \"/admin/orders/:id\")\n\
         p helpers.admin_orders_path\n\
         p helpers.admin_orders_path(params: { search: \"x\" }, anchor: \"top\")\n\
         p helpers.admin_order_show_path(id: 7)\n\
         p helpers.admin_order_show_url(id: 7, host: \"shop.test\")\n\
         p helpers.admin_orders_url(host: \"shop.test\", protocol: \"http\", params: { ignored: 1 })\n\
         begin\n  helpers.admin_order_show_path\nrescue ArgumentError => e\n  p e.message\nend\n";
    let native = emit_and_run::ruby().arg("-e").arg(format!("{FACTORY}\n{observe}")).output().expect("spawn ruby");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let run = emit_and_run::real_blog()
        .write("app/lib/paths_factory.rb", FACTORY)
        .run_ruby(&format!("require_relative \"app/models/paths_factory\"\n{observe}"));
    run.assert_passes();
    assert_eq!(run.stdout, String::from_utf8_lossy(&native.stdout));
}
