//! `include M` inside a Concern's `class_methods do … end` (e2e sweep-1,
//! round 4). Core's InMemoryElasticSearch::UseInMemoryElasticSearch
//!
//! ```ruby
//! class_methods do
//!   include TestModeDeclaration
//!
//!   def use_in_memory_elasticsearch
//!     return unless declare_elasticsearch_test_mode!(:in_memory)
//! ```
//!
//! The block is `module_eval`ed on the concern's `ClassMethods`, so M's
//! methods reach the includer's class side. Ingest read the `include` as a
//! singleton-class include and the emit wrote it on the concern module
//! itself (`class << self; include M; end`), so `ClassMethods` never had
//! it: NoMethodError `declare_elasticsearch_test_mode!` for the test class.
//!
//! CRuby is the oracle: the same script runs on the source and on the
//! emitted code, and the outputs must agree.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const DECLARATION: &str = r##"module ProbeModeDeclaration
  def declared_mode
    @declared_mode
  end

  private

  def declare_mode!(mode)
    return false if @declared_mode == mode

    @declared_mode = mode
    true
  end
end
"##;

const CONCERN: &str = r##"module ProbeUseMode
  extend ActiveSupport::Concern

  class_methods do
    include ProbeModeDeclaration

    def use_probe_mode
      return unless declare_mode!(:in_memory)

      (@uses ||= []) << :used
    end

    def uses
      @uses
    end
  end
end
"##;

const HOST: &str = r##"class ProbeModeHost
  include ProbeUseMode

  use_probe_mode
  use_probe_mode
end
"##;

const SCRIPT: &str = r##"
p ProbeModeHost.declared_mode
p ProbeModeHost.uses
p ProbeModeHost.respond_to?(:declare_mode!, true)
p ProbeUseMode.respond_to?(:declared_mode)
"##;

#[test]
fn an_include_in_a_class_methods_block_is_the_carriers() {
    let expected = ":in_memory\n[:used]\ntrue\nfalse\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!(
            "require 'active_support'\nrequire 'active_support/concern'\n{DECLARATION}\n{CONCERN}\n{HOST}\n{SCRIPT}"
        ))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/models/probe_mode_declaration.rb", DECLARATION)
        .write("app/models/probe_use_mode.rb", CONCERN)
        .write("app/models/probe_mode_host.rb", HOST)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}
