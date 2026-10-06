//! statsd-instrument, as Shopify Core's CI helper reaches it
//! (`gems/ci/lib/ci/stats_d_metrics_helper.rb`): a client built with
//! `StatsD::Instrument::Client.from_env(StatsD::Instrument::Environment
//! .new({...}))`, or the `StatsD` module's singleton client.
//!
//! The analyzer types those constants from the gem's Tapioca RBI
//! (`crate::gem_boundary`). That is a type, not a runtime: the CRuby
//! tree loads the real gem (`project.rs::RUNTIME_GEMS`, guarded-required
//! by runtime/gem_facades.rb and declared in the emitted Gemfile), and
//! every other tree, which loads no gem, refuses the constant.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    statsd-instrument (3.11.1)\n    zork (1.0)\n\nDEPENDENCIES\n  activesupport\n  statsd-instrument\n  zork\n";

/// The part of Tapioca's statsd-instrument@3.11.1 RBI this app reaches,
/// in the generated shape (methods unsigned, so their results are
/// gradual).
const STATSD_RBI: &str = r#"# typed: true
module StatsD
  class << self
    def distribution(*, **, &); end
    def increment(*, **, &); end
    def singleton_client; end
  end
end

module StatsD::Instrument; end

class StatsD::Instrument::Client
  def initialize(prefix: T.unsafe(nil), default_sample_rate: T.unsafe(nil), default_tags: T.unsafe(nil), implementation: T.unsafe(nil), sink: T.unsafe(nil)); end
  def capture(&block); end
  def distribution(name, value = T.unsafe(nil), sample_rate: T.unsafe(nil), tags: T.unsafe(nil), no_prefix: T.unsafe(nil), &block); end
  def increment(name, value = T.unsafe(nil), sample_rate: T.unsafe(nil), tags: T.unsafe(nil), no_prefix: T.unsafe(nil)); end
  def prefix; end
  def sink; end

  class << self
    def from_env(env = T.unsafe(nil), prefix: T.unsafe(nil), default_sample_rate: T.unsafe(nil), default_tags: T.unsafe(nil), implementation: T.unsafe(nil), sink: T.unsafe(nil)); end
  end
end

class StatsD::Instrument::Environment
  def initialize(env); end
end
"#;

/// A gem nothing in any emitted tree loads. It also reopens the
/// framework's `ActiveRecord`, as activemodel-serializers-xml does, and
/// the stdlib's `OpenSSL::PKey::RSA`, as json-jwt does.
const ZORK_RBI: &str = "# typed: true\nmodule Zork\n  class Widget\n    def label; end\n  end\nend\n\nmodule ActiveRecord\n  def self.zork_mode; end\n  module ZorkExtension\n    def zork; end\n  end\nend\n\nclass OpenSSL::PKey::RSA\n  def to_zork; end\nend\n";

fn statsd_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/statsd-instrument@3.11.1.rbi", STATSD_RBI)
        .write("sorbet/rbi/gems/zork@1.0.rbi", ZORK_RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/metrics.rb", r#"require "statsd-instrument"

class Metrics
  def client(prefix)
    StatsD::Instrument::Client.from_env(
      StatsD::Instrument::Environment.new({
        "STATSD_ENV" => "test",
        "STATSD_PREFIX" => prefix,
      }),
    )
  end

  def report(prefix, name, time_ms)
    client = client(prefix)
    client.capture { client.distribution(name, time_ms, tags: { lane: "ruby" }) }
  end

  def singleton
    StatsD
  end
end
"#)
        .write("test/metrics_test.rb", r#"require "test_helper"
require "metrics"
class MetricsTest < ActiveSupport::TestCase
  test "a client from the environment prefixes and captures" do
    metrics = Metrics.new
    assert_equal "pfx", metrics.client("pfx").prefix
    assert_equal "StatsD::Instrument::NullSink", metrics.client("pfx").sink.class.name
    datagrams = metrics.report("pfx", "query", 1.5)
    assert_equal 1, datagrams.size
    assert_equal "pfx.query", datagrams.first.name
    assert_equal 1.5, datagrams.first.value
    assert_equal ["lane:ruby"], datagrams.first.tags
  end
  test "the module is the singleton client's front" do
    assert_equal "StatsD::Instrument::Client", Metrics.new.singleton.singleton_client.class.name
  end
end
"#)
}

#[test]
fn statsd_runs_in_emitted_ruby() {
    statsd_app().run_test("test/models/metrics_test.rb").assert_passes();
}

#[test]
fn the_emitted_ruby_tree_declares_the_gem() {
    let (dir, errors) = statsd_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(gemfile.contains("gem \"statsd-instrument\""), "{gemfile}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.contains("\"statsd-instrument\""), "{facades}");
}

#[test]
fn statsd_is_refused_where_no_tree_loads_the_gem() {
    for target in [BuildTarget::Spinel, BuildTarget::Rust] {
        let (_, errors) = statsd_app().emit(target);
        for konst in ["StatsD::Instrument::Client", "StatsD::Instrument::Environment", "StatsD"] {
            let needle = format!("{konst} is declared by the statsd-instrument gem");
            assert!(errors.iter().any(|e| e.contains(&needle)), "{target:?} {konst}: {errors:#?}");
        }
    }
}

/// A gem only its RBI describes, which no tree loads, is refused on the
/// CRuby tree as well: typing its constant is not loading it.
#[test]
fn an_rbi_only_gem_is_refused_on_the_ruby_tree() {
    let (_, errors) = statsd_app()
        .write("lib/widgets.rb", "class Widgets\n  def make\n    Zork::Widget.new\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("Zork::Widget is declared by the zork gem, which no emitted tree loads")),
        "{errors:#?}"
    );
}

/// A namespace the framework already has is not the gem's because its
/// RBI reopens it: the read runs without the gem.
#[test]
fn a_framework_namespace_a_gem_reopens_is_not_the_gems() {
    let (_, errors) = statsd_app()
        .write("lib/widgets.rb", "class Widgets\n  def framework\n    ActiveRecord\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("ActiveRecord is declared by")), "{errors:#?}");
    // The gem's own constant under it still is.
    let (_, errors) = statsd_app()
        .write("lib/widgets.rb", "class Widgets\n  def ext\n    ActiveRecord::ZorkExtension\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("ActiveRecord::ZorkExtension is declared by the zork gem")),
        "{errors:#?}"
    );
}

/// A stdlib class a gem's RBI patches (json-jwt's `OpenSSL::PKey::RSA`)
/// is the stdlib's: loading the gem would not load it, so it is not
/// refused as the gem's.
#[test]
fn a_stdlib_class_a_gem_reopens_is_not_the_gems() {
    let (_, errors) = statsd_app()
        .write("lib/keys.rb", "class Keys\n  def rsa\n    OpenSSL::PKey::RSA\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("is declared by the zork gem")), "{errors:#?}");
}
