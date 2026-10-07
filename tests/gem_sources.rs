//! Gem sources an app carries as its own code (`gem_sources:` in
//! `roundhouse.yml`). Shopify Core's verdict, hedwig and country_db
//! depend on Rails, so no emitted tree loads them natively; their
//! installed trees are copied into the app and compiled like a `PATH`
//! gem: `lib/` is a support root and the gem's RBI is not the boundary.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    widget (1.0.0)\n\nDEPENDENCIES\n  activesupport\n  widget\n";

const WIDGET_RBI: &str = "# typed: true\nmodule Widget\n  def self.answer; end\nend\n";

fn widget_app(config: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", config)
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/widget@1.0.0.rbi", WIDGET_RBI)
        .write("vendor/gems/widget-1.0.0/lib/widget.rb", "require \"widget/answer\"\n\nmodule Widget\n  def self.answer\n    Answer.new.value\n  end\nend\n")
        .write("vendor/gems/widget-1.0.0/lib/widget/answer.rb", "module Widget\n  class Answer\n    def value\n      42\n    end\n  end\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/oracle.rb", "require \"widget\"\n\nclass Oracle\n  def ask\n    Widget.answer\n  end\nend\n")
        .write("test/oracle_test.rb", r#"require "test_helper"
require "oracle"
class OracleTest < ActiveSupport::TestCase
  test "the carried gem answers" do
    assert_equal 42, Oracle.new.ask
  end
end
"#)
}

const CARRIED: &str = "test_paths:\n  - test\ngem_sources:\n  widget: vendor/gems/widget-1.0.0\n";

#[test]
fn a_carried_gem_runs_in_emitted_ruby() {
    widget_app(CARRIED).run_test("test/models/oracle_test.rb").assert_passes();
}

#[test]
fn a_carried_gem_is_not_refused_as_the_gems() {
    let (_, errors) = widget_app(CARRIED).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn without_the_key_the_gem_stays_behind_its_rbi() {
    let (_, errors) = widget_app("test_paths:\n  - test\n").emit(BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("Widget is declared by the widget gem")), "{errors:#?}");
}

#[test]
fn a_gem_source_outside_the_app_is_refused() {
    for path in ["../widget", "/srv/widget", "vendor/gems/absent"] {
        let tree: HashMap<PathBuf, Vec<u8>> = [
            ("roundhouse.yml", format!("gem_sources:\n  widget: {path}\n")),
            ("vendor/gems/widget-1.0.0/lib/widget.rb", "module Widget\nend\n".to_string()),
        ]
        .into_iter()
        .map(|(p, s)| (PathBuf::from(p), s.into_bytes()))
        .collect();
        let error = ingest_app_from_tree(tree).err().unwrap_or_else(|| panic!("{path} accepted"));
        assert!(error.to_string().contains("gem_sources entry \"widget\""), "{error}");
    }
}

/// graphql-shopify 0.20.3 requires shopify-cloud inside
/// `begin ... rescue LoadError end` and runs on without it. The boot's
/// load of the gem source's dependencies keeps that rescue for that
/// require, and only for it.
#[test]
fn a_gem_sources_rescued_require_stays_optional() {
    const WIDGET: &str = "require \"set\"\nbegin\n  require \"rh_absent_optional_feature\"\nrescue LoadError\nend\nrequire \"widget/answer\"\n\nmodule Widget\n  def self.answer\n    Answer.new.value\n  end\nend\n";
    let app = || widget_app(CARRIED).write("vendor/gems/widget-1.0.0/lib/widget.rb", WIDGET);
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(
        facades.contains("[[\"set\", false], [\"rh_absent_optional_feature\", true]].each do |feature, optional|"),
        "{facades}"
    );
    app().run_test("test/models/oracle_test.rb").assert_passes();
}

/// kafka-shopify 5.2.3's classic entry runs
/// `require "kafka-shopify/legacy/lib/rspec_matcher" if defined?(RSpec)`,
/// and the matcher requires `rspec/expectations`. A Minitest boot never
/// loads it (RSpec is not defined), so rspec need not be installed. The
/// boot's load of the gem source's dependencies left the condition out
/// and raised LoadError. A conditional require is the carried program's.
#[test]
fn a_gem_sources_conditional_require_loads_only_where_its_condition_holds() {
    const WIDGET: &str = "require \"set\"\nrequire \"widget/answer\"\nrequire \"widget/matcher\" if defined?(RSpec)\nrequire \"rh_absent_unless_feature\" unless defined?(Set)\n\nmodule Widget\n  def self.answer\n    Answer.new.value\n  end\nend\n";
    const MATCHER: &str = "require \"rh_absent_test_group_feature\"\n\nmodule Widget\n  MATCHER = true\nend\n";
    let app = || {
        widget_app(CARRIED)
            .write("vendor/gems/widget-1.0.0/lib/widget.rb", WIDGET)
            .write("vendor/gems/widget-1.0.0/lib/widget/matcher.rb", MATCHER)
    };
    let (emitted, _) = app().emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("[\"set\"].each do |feature|"), "{facades}");
    assert!(!facades.contains("rh_absent_"), "{facades}");
    app().run_test("test/models/oracle_test.rb").assert_passes();
}

/// A class whose superclass is a gem source's class, itself a subclass
/// of a natively loaded gem's: the class-body DSL is the native gem's,
/// on the native gem's class, and replays as the source has it. Core's
/// `GraphApi::ObjectType < GraphQL::Shopify::Types::BaseObject <
/// GraphQL::Schema::Object` (graphql-shopify a gem source, graphql
/// native), with concurrent-ruby's `Synchronization::Object` and its
/// `safe_initialization!` standing in. A module replays when it
/// includes, through the gem source's modules, a native gem's module
/// (`GraphApi::InterfaceType` and graphql's `Schema::Interface`; here
/// `Concurrent::Concern::Logging`). Native Ruby on the same files gave
/// the asserted values.
#[test]
fn a_class_body_on_a_native_base_reached_through_a_gem_source_replays() {
    const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    concurrent-ruby (1.3.8)\n    widget (1.0.0)\n      concurrent-ruby\n\nDEPENDENCIES\n  activesupport\n  concurrent-ruby\n  widget\n";
    const RBI: &str = "# typed: true\nmodule Concurrent\n  module Synchronization\n    class Object\n      def self.safe_initialization!; end\n      def self.safe_initialization?; end\n    end\n  end\n\n  module Concern\n    module Logging\n      def log(level, progname, message = T.unsafe(nil), &block); end\n    end\n  end\nend\n";
    const WIDGET: &str = "require \"concurrent\"\nrequire \"widget/box\"\nrequire \"widget/taggable\"\n\nmodule Widget\nend\n";
    const BOX: &str = "module Widget\n  class Box < Concurrent::Synchronization::Object\n    def self.kind = \"widget\"\n  end\nend\n";
    const TAGGABLE: &str = "module Widget\n  module Taggable\n    include Concurrent::Concern::Logging\n\n    def self.included(base)\n      base.extend(ClassMethods)\n    end\n\n    module ClassMethods\n      def tag(value = nil)\n        @tag = value if value\n        @tag\n      end\n    end\n  end\nend\n";
    const APP_BOX: &str = "require \"widget\"\n\nclass AppBox < Widget::Box\n  safe_initialization!\n\n  def self.greet = \"hello\"\nend\n";
    const APP_TAGGED: &str = "require \"widget\"\n\nmodule AppTagged\n  include Widget::Taggable\n  tag \"app\"\nend\n";
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", CARRIED)
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/concurrent-ruby@1.3.8.rbi", RBI)
        .write("vendor/gems/widget-1.0.0/lib/widget.rb", WIDGET)
        .write("vendor/gems/widget-1.0.0/lib/widget/box.rb", BOX)
        .write("vendor/gems/widget-1.0.0/lib/widget/taggable.rb", TAGGABLE)
        .write("lib/app_box.rb", APP_BOX)
        .write("lib/app_tagged.rb", APP_TAGGED)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/box_test.rb", r#"require "test_helper"
require "app_box"
require "app_tagged"
class BoxTest < ActiveSupport::TestCase
  test "the native gem's class-body DSL ran" do
    assert_equal true, AppBox.safe_initialization?
    assert_nil Widget::Box.safe_initialization?
    assert_equal "hello", AppBox.greet
    assert_equal "widget", AppBox.kind
    assert_equal "app", AppTagged.tag
  end
end
"#)
        .run_test("test/models/box_test.rb");
    run.assert_passes();
}

#[test]
fn mixes_in_class_methods_extends_each_includer() {
    // sorbet's `mixes_in_class_methods` is behavior: every `include` of
    // the module extends the includer. graphql-shopify's BaseInterface
    // gets `GraphQL::Schema::Interface::DefinitionMethods` onto each
    // implementing type this way. It was dropped as an unmodeled call
    // (and `extend T::Helpers` with it), so `AppThing.describe` was a
    // NoMethodError. `requires_ancestor`'s block never runs: sorbet-runtime
    // 0.6.13502 defines the method with an empty body. Native oracle
    // (ruby -rsorbet-runtime, same files): ["described AppThing", :extra,
    // 1, false, true].
    const DESCRIBABLE: &str = "module Widget\n  module Describable\n    extend T::Helpers\n    requires_ancestor { Widget::NotDefinedAnywhere }\n\n    module ClassMethods\n      def describe = \"described #{name}\"\n    end\n\n    module Extra\n      def extra = :extra\n    end\n\n    mixes_in_class_methods(ClassMethods)\n    mixes_in_class_methods Extra\n  end\nend\n";
    const APP_MIXIN: &str = "module AppMixin\n  extend T::Helpers\n\n  module Counting\n    def count = 1\n  end\n\n  mixes_in_class_methods(Counting)\nend\n";
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", CARRIED)
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/widget-1.0.0/lib/widget.rb", "require \"widget/describable\"\n\nmodule Widget\nend\n")
        .write("vendor/gems/widget-1.0.0/lib/widget/describable.rb", DESCRIBABLE)
        .write("lib/app_mixin.rb", APP_MIXIN)
        .write("lib/app_thing.rb", "require \"widget\"\n\nclass AppThing\n  include Widget::Describable\nend\n")
        .write("lib/app_other.rb", "require \"app_mixin\"\n\nclass AppOther\n  include AppMixin\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/mixin_test.rb", r#"require "test_helper"
require "app_thing"
require "app_other"
class MixinTest < ActiveSupport::TestCase
  test "each includer is extended with the mixed-in class methods" do
    assert_equal "described AppThing", AppThing.describe
    assert_equal :extra, AppThing.extra
    assert_equal 1, AppOther.count
    assert_equal false, Widget::Describable.respond_to?(:describe)
    assert_equal true, AppThing.singleton_class.include?(Widget::Describable::ClassMethods)
  end
end
"#)
        .run_test("test/models/mixin_test.rb");
    run.assert_passes();
}
