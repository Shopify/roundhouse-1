//! Real Minitest on the CRuby test path, as Shopify Core's test boot
//! reaches it outside the test bodies: `rescue => e;
//! Minitest::UnexpectedError.new(e)` in its parallelization
//! instrumentation, and reporters that subclass
//! `Minitest::Reporters::BaseReporter`.
//!
//! The CRuby test process runs on the minitest gem
//! (`runtime/spinel/test/minitest_runner.rb`), so minitest and
//! minitest-reporters are typed from their RBIs there, whatever their
//! tooling fate. The Spinel and JRuby test bootstraps are Minitest-free
//! and refuse them as the gems'. ci-queue is not loaded: it patches
//! Minitest when it loads, so it stays behind the require site the app
//! gates (docs/BOOT_BOUNDARIES.md in the Core harness).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    ci-queue (0.101.0)\n    minitest (5.27.0)\n    minitest-reporters (1.8.0)\n\nDEPENDENCIES\n  activesupport\n  ci-queue\n  minitest\n  minitest-reporters\n";

/// Tapioca's shapes, cut to what the app reaches.
const MINITEST_RBI: &str = r#"# typed: true
module Minitest
  class << self
    def parallel_executor; end
  end
end

class Minitest::Assertion < ::Exception
  def location; end
  def result_code; end
end

class Minitest::UnexpectedError < ::Minitest::Assertion
  def initialize(error); end
  def error; end
  def message; end
end

class Minitest::Result
  def klass; end
  def source_location; end
end
"#;

const REPORTERS_RBI: &str = r#"# typed: true
module Minitest::Reporters; end

class Minitest::Reporters::BaseReporter < ::Minitest::StatisticsReporter
  def initialize(options = T.unsafe(nil)); end
  def record(test); end
end
"#;

/// ci-queue reopens minitest's `Minitest::Result`, and sorts first.
const CI_QUEUE_RBI: &str = r#"# typed: true
class Minitest::Result
  def queue_id; end
end

module Minitest::Queue
  class << self
    def queue; end
  end
end
"#;

fn app(lib: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/minitest@5.27.0.rbi", MINITEST_RBI)
        .write("sorbet/rbi/gems/minitest-reporters@1.8.0-c049436a61d339e046da63bfd97d994fa6b70c52.rbi", REPORTERS_RBI)
        .write("sorbet/rbi/gems/ci-queue@0.101.0.rbi", CI_QUEUE_RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/instrumentation.rb", lib)
}

const INSTRUMENTATION: &str = r#"class Instrumentation
  def failure(error)
    Minitest::UnexpectedError.new(error)
  end

  def result_class
    Minitest::Result
  end

  def reporter
    Minitest::Reporters::BaseReporter
  end
end
"#;

#[test]
fn minitest_constants_are_the_cruby_test_gems() {
    let (_, errors) = app(INSTRUMENTATION).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn minitest_constants_are_refused_where_the_test_bootstrap_has_none() {
    for target in [BuildTarget::Jruby, BuildTarget::Rust] {
        let (_, errors) = app(INSTRUMENTATION).emit(target);
        for (konst, gem) in [
            ("Minitest::UnexpectedError", "minitest"),
            // Declared by ci-queue as well; minitest is the gem a tree loads.
            ("Minitest::Result", "minitest"),
            ("Minitest::Reporters::BaseReporter", "minitest-reporters"),
        ] {
            let needle = format!("{konst} is declared by the {gem} gem");
            assert!(errors.iter().any(|e| e.contains(&needle)), "{target:?} {konst}: {errors:#?}");
        }
    }
}

#[test]
fn ci_queue_is_not_loaded() {
    let lib = "class Queueing\n  def queue\n    Minitest::Queue.queue\n  end\nend\n";
    let (_, errors) = app(lib).emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("Minitest::Queue is declared by the ci-queue gem, which no emitted tree loads")),
        "{errors:#?}"
    );
}
