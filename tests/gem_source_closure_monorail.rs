//! A gem whose dependency is a gem source is a gem source too (the
//! harness's bin/gem-sources.rb closure, e2e round 5). monorail 4.0.1 is
//! Bundler.require'd by Core; its entry requires kafka-shopify's
//! `kafka-shopify/monorail/v2` (kafka-shopify is compiled from source) and
//! then runs `Object.const_set("Monorail", MonorailV2)`. Required natively
//! it was a RAILS_DEPENDENT_GEMS refusal: Core's WebhookTestHelper
//! `include Monorail::Assertions` was error[gem_requires_rails] (38 load
//! rows), and Monorail::Assertions was uninitialized (20 rows).
//!
//! The emitted boot is not there yet, so bin/gem-sources.rb holds monorail
//! (its closure names the `Object.const_set` over Core's lib/monorail/):
//! RoundhouseAutoload has set an implicit Monorail module before the boot
//! list loads the gem, and Core's StrictWarning raises Ruby's "already
//! initialized constant Monorail" (the ignored tests below).
//!
//! Core's own lib/monorail/ reopens the gem's constant: config/environments/
//! test.rb requires "monorail/shitlist" (`module Monorail; class
//! Shitlist`), which under Rails reopens MonorailV2, since Bundler.require
//! ran first. The fixture has that shape, cut down: both gems carried as
//! source, the app's namespace file under lib/, and a test that includes
//! the gem's assertions through the `Monorail` alias.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    kafka-shopify (5.2.3)\n      activesupport\n    monorail (4.0.1)\n      kafka-shopify (~> 5.2)\n\nDEPENDENCIES\n  activesupport\n  kafka-shopify\n  monorail\n";

const KAFKA_ENTRY: &str = "module KafkaShopify\n  VERSION = \"5.2.3\"\nend\n";

const KAFKA_V2: &str = "require \"kafka-shopify/monorail/v2/monorail\"\n";

const KAFKA_MONORAIL: &str = r#"module MonorailV2
  extend self

  def produced
    @produced ||= []
  end

  def produce(schema_id, payload)
    produced << [schema_id, payload]
    nil
  end
end
"#;

const KAFKA_TEST_HELPER: &str = r#"require "kafka-shopify/monorail/v2/monorail"

module MonorailV2
  module Assertions
    def assert_monorail_produced(schema_id)
      assert MonorailV2.produced.any? { |schema, _| schema == schema_id }, "no #{schema_id} event produced"
    end
  end
end
"#;

const MONORAIL_ENTRY: &str = "require \"kafka-shopify/monorail/v2\"\n\nObject.const_set(\"Monorail\", MonorailV2)\n";

const MONORAIL_TEST_HELPER: &str = "require \"kafka-shopify/monorail/v2/test_helper\"\n";

const SHITLIST: &str = "module Monorail\n  class Shitlist\n    DEPRECATED = [\"old_event/1.0\"].freeze\n  end\nend\n";

const APPLICATION: &str = "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule MonorailProbeApp\n  class Application < Rails::Application\n  end\nend\n";

const CARRIED: &str = "test_paths:\n  - test\ngem_sources:\n  monorail: vendor/gems/monorail-4.0.1\n  kafka-shopify: vendor/gems/kafka-shopify-5.2.3\n";

/// The r5 symptom: the include was error[gem_requires_rails] at load.
const ASSERTIONS_TEST: &str = r#"require "test_helper"
require "monorail/test_helper"

class AssertionsTest < ActiveSupport::TestCase
  include Monorail::Assertions

  test "the gem's assertions through the Monorail alias" do
    Monorail.produce("checkout_created/1.0", { "id" => 1 })
    assert_monorail_produced("checkout_created/1.0")
    assert_equal true, AssertionsTest.include?(MonorailV2::Assertions)
  end
end
"#;

/// Core's config/environments/test.rb requires "monorail/shitlist" after
/// Bundler.require has aliased Monorail, so the app's `module Monorail`
/// reopens MonorailV2.
const EVENT_TEST: &str = r#"require "test_helper"

class EventTest < ActiveSupport::TestCase
  test "the gem's alias is the boot's Monorail, reopened by the app" do
    Monorail.produce("checkout_created/1.0", { "id" => 1 })
    assert_equal [["checkout_created/1.0", { "id" => 1 }]], Monorail.produced
    assert_same MonorailV2, Monorail
    assert_equal ["old_event/1.0"], Monorail::Shitlist::DEPRECATED
  end
end
"#;

const TEST_ENVIRONMENT: &str = "require \"monorail/shitlist\"\n\nRails.application.configure do\n  config.eager_load = false\nend\n";

fn app(config: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", config)
        .write(
            "Gemfile",
            "source \"https://rubygems.org\"\n\ngem \"activesupport\"\ngem \"monorail\", \"~> 4.0\"\ngem \"kafka-shopify\", \"~> 5.2.3\"\n",
        )
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/kafka-shopify-5.2.3/lib/kafka-shopify.rb", KAFKA_ENTRY)
        .write("vendor/gems/kafka-shopify-5.2.3/lib/kafka-shopify/monorail/v2.rb", KAFKA_V2)
        .write("vendor/gems/kafka-shopify-5.2.3/lib/kafka-shopify/monorail/v2/monorail.rb", KAFKA_MONORAIL)
        .write("vendor/gems/kafka-shopify-5.2.3/lib/kafka-shopify/monorail/v2/test_helper.rb", KAFKA_TEST_HELPER)
        .write("vendor/gems/monorail-4.0.1/lib/monorail.rb", MONORAIL_ENTRY)
        .write("vendor/gems/monorail-4.0.1/lib/monorail/test_helper.rb", MONORAIL_TEST_HELPER)
        .write("lib/monorail/shitlist.rb", SHITLIST)
        .write("config/boot.rb", "require \"bundler/setup\"\n")
        .write("config/application.rb", APPLICATION)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("config/environment.rb", "require_relative \"application\"\n\nRails.application.initialize!\n")
        .write("config/environments/test.rb", TEST_ENVIRONMENT)
        .write("test/models/event_test.rb", EVENT_TEST)
        .write("test/models/assertions_test.rb", ASSERTIONS_TEST)
}

#[test]
fn a_gem_over_a_gem_source_carried_as_source_is_not_refused() {
    let (emitted, errors) = app(CARRIED).emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("gem_requires_rails")), "{errors:#?}");
    let test = std::fs::read_to_string(emitted.join("test/models/assertions_test.rb")).expect("assertions_test.rb");
    assert!(!test.contains("gem_requires_rails"), "{test}");
    assert!(test.contains("include Monorail::Assertions"), "{test}");
}

/// The emitted boot loads the compiled monorail entry (Bundler.require)
/// after RoundhouseAutoload has registered Object's children, so the
/// entry's `Object.const_set` replaces the implicit `Monorail` module set
/// for lib/monorail/ and Ruby warns "already initialized constant
/// Monorail", where native Rails, which sets up Zeitwerk after
/// Bundler.require, does not. The run still has one Monorail, the gem's.
#[test]
fn a_gem_over_a_gem_source_runs_its_alias_in_emitted_ruby() {
    app(CARRIED).run_test("test/models/event_test.rb").assert_passes();
}

/// Not yet: under native Rails Zeitwerk sets up lib/monorail/ after
/// Bundler.require, so Monorail is the gem's constant with no warning; the
/// emitted boot replaces its implicit module with a warning, which Core's
/// StrictWarning (config/boot.rb's Warning hook) raises at boot.
#[test]
#[ignore = "RoundhouseAutoload sets an implicit Monorail before the boot list runs the gem's const_set"]
fn a_gem_over_a_gem_source_aliases_without_a_constant_warning() {
    let run = app(CARRIED).run_test("test/models/event_test.rb");
    run.assert_passes();
    assert!(!run.stderr.contains("already initialized constant Monorail"), "{}", run.stderr);
}

/// Not yet: the emitted test does not carry the test file's `require
/// "monorail/test_helper"` (a compiled gem's feature; only
/// `require_relative`s of declarations files are synthesized), and the
/// compiler cannot see that `Monorail` is MonorailV2, so nothing loads
/// kafka-shopify's v2/test_helper and Monorail::Assertions is
/// uninitialized. In Core the require is in
/// platform/testing/test_runtime.rb, which the emitted tree lists as
/// unreached.
#[test]
#[ignore = "a test file's require of a compiled gem feature is not carried (Monorail::Assertions uninitialized)"]
fn a_gem_over_a_gem_source_runs_its_assertions_in_emitted_ruby() {
    app(CARRIED).run_test("test/models/assertions_test.rb").assert_passes();
}
