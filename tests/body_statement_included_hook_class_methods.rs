//! A module body statement calling a class method the module's own
//! `include` gives it through the included module's `self.included` hook
//! runs where the body loads (Shopify Core's `GraphQLTestHelper`:
//! `include GraphQLEasyTesting` whose hook `module_eval`s a
//! `self.configuration` onto the includer, then `configuration.tap do |config|
//! config.before_test_case_setup = … end`). Without it the setup never ran
//! and the Admin engine's `on_load(:graphql_test_helper)` read nil.
//!
//! CRuby is the oracle: the same script runs on the source and on the
//! emitted app, and the outputs must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const EASY: &str = r##"module EasyTesting
  Configuration = Struct.new(:before_setup)

  def self.configuration
    @configuration ||= Configuration.new(nil)
  end

  def self.included(base, parent: self)
    base.module_eval do
      def self.configuration
        @configuration ||= EasyTesting.configuration.dup
      end
    end
  end
end
"##;

const HELPER: &str = r##"module Helper
  include EasyTesting

  configuration.tap do |config|
    config.before_setup = lambda { |options| "helper:#{options}" }
  end

  def self.run(options) = configuration.before_setup.call(options)
end
"##;

const SCRIPT: &str = "p Helper.run(1)\np EasyTesting.configuration.before_setup\n";

#[test]
fn module_body_statement_calls_a_class_method_the_included_hook_defined() {
    let expected = "\"helper:1\"\nnil\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("{EASY}\n{HELPER}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/easy_testing.rb", EASY)
        .write("app/lib/helper.rb", HELPER)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// The tap's lambdas `instance_exec` on another object (`test_case_config.
/// test_case.instance_exec do … end`, GraphQLTestHelper's
/// `after_query_test_setup`): the block rebinds `self` to that object, so
/// its calls are not the module's and the statement is still carried.
const HELPER_INSTANCE_EXEC: &str = r##"require "active_support"
require "active_support/concern"

module Helper
  extend ActiveSupport::Concern

  unless ENV.key?("NO_WARMING")
    WARMED = []
  end

  included do
    WARMED << :included
  end

  include EasyTesting

  configuration.tap do |config|
    config.before_setup = lambda do |target|
      target.instance_exec do
        shout("helper")
      end
    end
  end

  def self.run(target) = configuration.before_setup.call(target)
end

class Target
  def shout(word) = "#{word}!"
end
"##;

#[test]
fn module_body_statement_instance_execs_a_block_on_another_object() {
    let script = "p Helper.run(Target.new)\n";
    let expected = "\"helper!\"\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("{EASY}\n{HELPER_INSTANCE_EXEC}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/easy_testing.rb", EASY)
        .write("app/lib/helper.rb", HELPER_INSTANCE_EXEC)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// The carried statement names a sibling file's constant that an included
/// module's namesake would shadow (`config.test_case_config_class =
/// TestCaseConfig`: `GraphQLEasyTesting::TestCaseConfig` through the
/// include, the helper's own `TestCaseConfig` in its file). The emitted
/// module must load its own.
const EASY_WITH_CONFIG: &str = r##"module EasyTesting
  class TestCaseConfig; end
  Configuration = Struct.new(:klass)

  def self.configuration
    @configuration ||= Configuration.new(nil)
  end

  def self.included(base, parent: self)
    base.module_eval do
      def self.configuration
        @configuration ||= EasyTesting.configuration.dup
      end
    end
  end
end
"##;

const HELPER_CONFIG_CONST: &str = r##"require "helper/test_case_config"

module Helper
  include EasyTesting

  configuration.tap do |config|
    config.klass = TestCaseConfig
  end

  def self.klass = configuration.klass
end
"##;

const HELPER_TEST_CASE_CONFIG: &str = r##"module Helper
  class TestCaseConfig < EasyTesting::TestCaseConfig; end
end
"##;

#[test]
fn module_body_statement_names_a_sibling_constant_an_include_shadows() {
    let script = "p Helper.klass\n";
    let expected = "Helper::TestCaseConfig\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("{EASY_WITH_CONFIG}\n{HELPER_TEST_CASE_CONFIG}\n{HELPER_CONFIG_CONST}\n{script}").replace("require \"helper/test_case_config\"\n", ""))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/easy_testing.rb", EASY_WITH_CONFIG)
        .write("app/lib/helper/test_case_config.rb", HELPER_TEST_CASE_CONFIG)
        .write("app/lib/helper.rb", HELPER_CONFIG_CONST)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
