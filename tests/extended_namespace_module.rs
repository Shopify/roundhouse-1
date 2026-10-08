//! A module whose whole body extends it with a gem module.
//!
//! Core's `Metafields::Namespaces` is only `extend
//! ShopifyMetafields::Namespaces`; 82 call sites send it the gem
//! module's methods. Ingest surfaced a module for an `include` but not
//! for an `extend`, so the module was dropped and each read refused as
//! `constant not supported`. A marker extend (`T::Sig`) still leaves a
//! namespace wrapper of classes alone.

use roundhouse::analyze::diagnose;
use roundhouse::det::HashMap;
use roundhouse::ingest::ingest_app_from_tree;
use std::path::PathBuf;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    shopify_metafields (0.12.31)\n\nDEPENDENCIES\n  shopify_metafields\n";

const RBI: &str = r#"# typed: true
module ShopifyMetafields; end
module ShopifyMetafields::Namespaces
  sig { params(namespace: String, api_client_id: Integer).returns(String) }
  def resolve_app_namespace(namespace, api_client_id); end
end
"#;

const NAMESPACES: &str = "module Metafields\n  module Namespaces\n    extend ShopifyMetafields::Namespaces\n  end\nend\n";

const MARKER: &str = "module Wrapper\n  extend T::Sig\n\n  class Inner\n    def x = 1\n  end\nend\n";

const LOADER: &str = r#"
class Loader
  #: -> untyped
  def ns = Metafields::Namespaces.resolve_app_namespace("a", 1)

  #: -> untyped
  def rooted = ::Metafields::Namespaces.resolve_app_namespace("a", 1)
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("Gemfile.lock", LOCK),
        ("sorbet/rbi/gems/shopify_metafields@0.12.31.rbi", RBI),
        ("app/models/metafields/namespaces.rb", NAMESPACES),
        ("app/models/wrapper.rb", MARKER),
        ("app/models/loader.rb", LOADER),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

#[test]
fn an_extended_module_is_read_as_the_module() {
    let app = app();
    let errors: Vec<String> = diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("Metafields::Namespaces")), "{errors:?}");
    assert!(app.library_classes.iter().any(|lc| lc.name.0.as_str() == "Metafields::Namespaces"));
}

#[test]
fn a_marker_extend_does_not_surface_a_namespace() {
    let app = app();
    assert!(!app.library_classes.iter().any(|lc| lc.name.0.as_str() == "Wrapper"));
}
