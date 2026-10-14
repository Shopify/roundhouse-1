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
