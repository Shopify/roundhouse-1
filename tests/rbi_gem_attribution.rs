//! A class two gem RBIs declare is named as the gem that defines it.
//! Shopify Core's hedwig and semian RBIs reopen redis's error classes,
//! and sort before it; the refusal must still name redis.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    hedwig (1.68.0)\n    redis (5.4.1)\n\nDEPENDENCIES\n  activesupport\n  hedwig\n  redis\n";

const REDIS_RBI: &str = "# typed: true\nclass Redis\n  def get(key); end\nend\n\nclass Redis::BaseError < ::StandardError; end\nclass Redis::CommandError < ::Redis::BaseError; end\n";

/// Reopens redis's class, and sorts first.
const HEDWIG_RBI: &str = "# typed: true\nmodule Hedwig; end\n\nclass Redis::CommandError < ::Redis::BaseError\n  def hedwig_retryable?; end\nend\n";

#[test]
fn a_reopened_class_is_named_as_the_gem_of_its_namespace() {
    let (_, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/hedwig@1.68.0.rbi", HEDWIG_RBI)
        .write("sorbet/rbi/gems/redis@5.4.1.rbi", REDIS_RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/retrier.rb", "class Retrier\n  def failure\n    Redis::CommandError\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("Redis::CommandError is declared by the redis gem")), "{errors:#?}");
    assert!(!errors.iter().any(|e| e.contains("hedwig gem")), "{errors:#?}");
}
