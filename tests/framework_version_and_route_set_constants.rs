//! Framework values that Core's code reads while it loads. Each read
//! happens in a module body, or in a gem file that boot loads, so a
//! missing one stops the boot before any test runs.
//!
//! - `ActiveRecord.version` / `ActiveSupport.version`, `gem_version`
//!   and `VERSION::*` (activerecord and activesupport gem_version.rb
//!   and version.rb). softer_delete 0.9.0's common.rb branches on
//!   `ActiveRecord.version >= "7.2.0.alpha"` in its module body.
//!   kafka-shopify's transaction tracker returns early when
//!   `::ActiveRecord.version < "7.2"`. Core's schema_migrations.rb
//!   compares against `Gem::Version.new("8.2.0.alpha")`.
//!   active_model_serializers 0.9.13 tests
//!   `ActiveSupport::VERSION::STRING < "5.1"`. Without the readers,
//!   unit-1000 batch 20261009T011553.731224Z-2df7f4 lost 722 booted
//!   tests to NoMethodError.
//! - `ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS`
//!   (actionpack route_set.rb:849). Essentials'
//!   lib/patches/action_dispatch_url_for_param_filter.rb subtracts its
//!   allowed keys from it in a module body. The read was "constant not
//!   supported", emitted as a raise, which aborted the patches
//!   initializer before any of its other patches loaded.
//! - `ActionView::Helpers::AssetUrlHelper` is not ported: a read of it
//!   is refused by name (`analyze::registry::refused_constants`).
//!
//! The values are Shopify/rails 5813559f4aed (8.2.0.alpha), the Rails
//! Core runs (docs/RAILS_VERSION.md). The ignored test at the end checks
//! the expected values below against that checkout.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

/// RouteSet::RESERVED_OPTIONS in the pinned actionpack, in order.
const RESERVED_OPTIONS: &str = "[:host, :protocol, :port, :subdomain, :domain, :tld_length, :trailing_slash, :anchor, :params, :only_path, :script_name, :original_script_name]";
/// The pinned activerecord's and activesupport's VERSION::STRING.
const VERSION: &str = "8.2.0.alpha";

/// The module-body shape of Core's url_for patch (essentials
/// lib/patches/action_dispatch_url_for_param_filter.rb:6-9, :73),
/// cut to the constants.
const URL_FOR_PARAM_FILTER: &str = r#"module UrlForParamFilter
  URL_FOR_RESERVED_OPTIONS_ALLOWED = [
    :protocol, :host, :user, :password, :port, :only_path, :trailing_slash, :anchor, :params, :script_name,
  ].freeze
  DISALLOWED_URL_FOR_OPTIONS = ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS - URL_FOR_RESERVED_OPTIONS_ALLOWED #: Array[untyped]

  def self.reserved_keys
    ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS - [:params]
  end
end
"#;

/// The version comparisons the carried gems and Core make, one in a
/// module body as softer_delete's is.
const FRAMEWORK_VERSIONS: &str = r#"module FrameworkVersions
  ACTIVE_RECORD_7_2 = ActiveRecord.version >= "7.2.0.alpha"

  def self.active_record_before_7_2?
    ActiveRecord.version < "7.2"
  end

  def self.active_record_at_least_8_2?
    ActiveRecord.version >= Gem::Version.new("8.2.0.alpha")
  end

  def self.active_support_before_5_1?
    ActiveSupport::VERSION::STRING < "5.1"
  end
end
"#;

fn constants_test() -> String {
    format!(
        r#"require "test_helper"

class FrameworkConstantsTest < ActiveSupport::TestCase
  test "url_for's reserved options are actionpack's" do
    assert_equal {RESERVED_OPTIONS}, ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS
    assert_equal true, ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS.frozen?
    assert_equal [:subdomain, :domain, :tld_length, :original_script_name], UrlForParamFilter::DISALLOWED_URL_FOR_OPTIONS
    assert_equal {RESERVED_OPTIONS} - [:params], UrlForParamFilter.reserved_keys
  end

  test "the framework versions are the pinned Rails'" do
    assert_equal Gem::Version.new("{VERSION}"), ActiveRecord.version
    assert_equal Gem::Version.new("{VERSION}"), ActiveRecord.gem_version
    assert_equal "Gem::Version", ActiveRecord.version.class.name
    assert_equal "{VERSION}", ActiveRecord::VERSION::STRING
    assert_equal [8, 2, 0, "alpha"], [ActiveRecord::VERSION::MAJOR, ActiveRecord::VERSION::MINOR, ActiveRecord::VERSION::TINY, ActiveRecord::VERSION::PRE]
    assert_equal Gem::Version.new("{VERSION}"), ActiveSupport.version
    assert_equal Gem::Version.new("{VERSION}"), ActiveSupport.gem_version
    assert_equal "{VERSION}", ActiveSupport::VERSION::STRING
    assert_equal [8, 2, 0, "alpha"], [ActiveSupport::VERSION::MAJOR, ActiveSupport::VERSION::MINOR, ActiveSupport::VERSION::TINY, ActiveSupport::VERSION::PRE]
  end

  test "version comparisons take the pinned Rails' branches" do
    assert_equal true, FrameworkVersions::ACTIVE_RECORD_7_2
    assert_equal false, FrameworkVersions.active_record_before_7_2?
    assert_equal true, FrameworkVersions.active_record_at_least_8_2?
    assert_equal false, FrameworkVersions.active_support_before_5_1?
  end
end
"#
    )
}

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema[8.1].define(version: 2026_01_01_000000) do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/url_for_param_filter.rb", URL_FOR_PARAM_FILTER)
        .write("app/models/framework_versions.rb", FRAMEWORK_VERSIONS)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/models/framework_constants_test.rb", &constants_test())
}

/// Analysis is clean (no "constant not supported" for RESERVED_OPTIONS
/// or VERSION::*), and the emitted program answers the pinned values.
#[test]
fn framework_versions_and_reserved_options_run_in_emitted_ruby() {
    let run = app().run_test("test/models/framework_constants_test.rb");
    assert!(!run.errors.iter().any(|e| e.contains("constant not supported")), "{:#?}", run.errors);
    run.assert_passes();
    assert!(run.stdout.contains("FrameworkConstantsTest: 3 tests passed"), "{}", run.stdout);
    let filter = std::fs::read_to_string(run.emitted.join("app/models/url_for_param_filter.rb")).expect("emitted module");
    assert!(!filter.contains("raise"), "{filter}");
    assert!(filter.contains("ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS - "), "{filter}");
}

/// The expected values above are the pinned Rails': its activerecord,
/// activesupport and actionpack, loaded natively through the shared
/// oracle (tests/support/rails_oracle.rb).
#[test]
#[ignore = "requires ruby and Core's pinned Rails checkout"]
fn expected_values_are_the_pinned_rails() {
    let script = r#"require ARGV.first
RailsOracle.activate("activesupport", "activemodel", "activerecord", "actionpack")
require "active_record"
require "action_dispatch"
require "action_dispatch/routing/route_set"
puts ActiveRecord.version.to_s, ActiveRecord.gem_version.class.name, ActiveSupport.version.to_s
puts ActiveRecord::VERSION::STRING, ActiveSupport::VERSION::STRING
puts ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS.inspect
puts ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS.frozen?
puts (ActiveRecord.version >= "7.2.0.alpha"), (ActiveRecord.version < "7.2"), (ActiveSupport::VERSION::STRING < "5.1")
"#;
    let path = std::env::temp_dir().join(format!("rh_framework_constants_{}.rb", std::process::id()));
    std::fs::write(&path, script).expect("write script");
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let out = std::process::Command::new("ruby").arg(&path).arg(oracle).output().expect("ruby");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    let expected = [VERSION, "Gem::Version", VERSION, VERSION, VERSION, RESERVED_OPTIONS, "true", "true", "false", "false"];
    assert_eq!(stdout.lines().collect::<Vec<_>>(), expected);
}

/// A model that reads ActionView::Helpers::AssetUrlHelper is refused by
/// name, with the reason: no tree carries the module (asset helpers
/// compile to calls on the runtime's ViewHelpers). Not covered:
/// essentials' lib/patches/action_view_ext.rb reopens it as a
/// file-level `ActionView::Helpers::AssetUrlHelper.module_eval`, whose
/// receiver is emitted as written and not analyzed, so that file still
/// stops at boot with Ruby's NameError for the constant.
#[test]
fn asset_url_helper_is_refused_by_name() {
    const READER: &str = "class AssetHelperReader\n  def self.helper\n    ActionView::Helpers::AssetUrlHelper\n  end\nend\n";
    let (_emitted, errors) = app().write("app/models/asset_helper_reader.rb", READER).emit(roundhouse::project::BuildTarget::Ruby);
    let expected = "constant not supported (all targets): ActionView::Helpers::AssetUrlHelper — Rails' AssetUrlHelper module is not carried";
    assert!(errors.iter().any(|e| e.contains(expected)), "no {expected:?} in:\n{errors:#?}");
    let refusals: Vec<_> = errors.iter().filter(|e| e.contains("constant not supported")).collect();
    assert_eq!(refusals.len(), 1, "{refusals:#?}");
}
