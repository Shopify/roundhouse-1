//! A bare constant a subclass inherits from a gem superclass.
//!
//! Core's `RequestStatsMiddleware < ShopifyMetrics::Middlewares::
//! RequestMetrics` reads `env[REQUEST_TIME]`, a constant of the gem's
//! class that only its RBI declares. The source index cannot see that
//! class, so the bare read was refused as `constant not supported`. Ruby
//! finds it on the ancestor; so does the read now, at its full name.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::ingest::ingest_app_from_tree;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    shopify_metrics (2.0.0)\n\nDEPENDENCIES\n  shopify_metrics\n";

const RBI: &str = r#"# typed: true
module ShopifyMetrics; end
module ShopifyMetrics::Middlewares; end
class ShopifyMetrics::Middlewares::RequestMetrics
  sig { params(env: T::Hash[String, T.untyped]).returns(T.untyped) }
  def call(env); end
end
ShopifyMetrics::Middlewares::RequestMetrics::REQUEST_TIME = T.let(T.unsafe(nil), String)
module ShopifyMetrics::KeyUtilities
  sig { params(x: String).returns(String) }
  def self.convert(x); end
end
ShopifyMetrics::KeyUtilities::CURVE = T.let(T.unsafe(nil), String)
module ShopifyMetrics::Adt; end
module ShopifyMetrics::Adt::Result
  sig { params(x: String).returns(String) }
  def self.error(x); end
end
"#;

/// Core's `ServerPixelService` includes shopify-adt's `Shopify::Adt` and
/// reads its `Result` bare.
const INCLUDER: &str = r#"
class PixelService
  include ShopifyMetrics::Adt

  #: -> String
  def failed = Result.error("x")

  #: -> untyped
  def missing = Outcome
end
"#;

/// Core's `TokenProtocol::Crypto` patch reopens the gem's namespace and
/// reads its `KeyUtilities` lexically.
const PATCH: &str = r#"
module ShopifyMetrics
  class CacheEntry
    #: -> String
    def a = KeyUtilities.convert("x")

    #: -> untyped
    def b = KeyUtilities::CURVE

    #: -> untyped
    def c = KeyUtilities::NOPE
  end
end
"#;

const MIDDLEWARE: &str = r#"
class StatsMiddleware < ShopifyMetrics::Middlewares::RequestMetrics
  #: (Hash[String, untyped]) -> untyped
  def time(env) = env[REQUEST_TIME]
end

class TimedStatsMiddleware < StatsMiddleware
  #: (Hash[String, untyped]) -> untyped
  def again(env) = env[REQUEST_TIME]

  #: -> untyped
  def nope = NOT_THERE
end
"#;

fn errors() -> Vec<String> {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("Gemfile.lock", LOCK),
        ("sorbet/rbi/gems/shopify_metrics@2.0.0.rbi", RBI),
        ("app/models/stats_middleware.rb", MIDDLEWARE),
        ("app/models/shopify_metrics/cache_entry.rb", PATCH),
        ("app/models/pixel_service.rb", INCLUDER),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    diagnose(&app).into_iter().map(|d| d.to_string()).collect()
}

#[test]
fn a_bare_constant_resolves_on_a_gem_superclass() {
    let errors = errors();
    assert!(!errors.iter().any(|e| e.contains("REQUEST_TIME")), "{errors:?}");
}

#[test]
fn a_name_no_ancestor_declares_stays_refused() {
    let errors = errors();
    assert!(errors.iter().any(|e| e.contains("NOT_THERE")), "{errors:?}");
}

#[test]
fn a_bare_constant_resolves_in_an_included_gem_module() {
    let errors = errors();
    assert!(!errors.iter().any(|e| e.contains("Result")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("Outcome")), "{errors:?}");
}

#[test]
fn a_gem_declaration_resolves_from_an_enclosing_scope() {
    let errors = errors();
    assert!(!errors.iter().any(|e| e.contains("KeyUtilities\n") || e.ends_with("KeyUtilities") || e.contains("CURVE")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("KeyUtilities::NOPE")), "{errors:?}");
}
