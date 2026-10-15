//! Class-side visibility an `ActiveSupport::Concern`'s `included do`
//! block gives the methods its own `class_attribute` generates, as Core's
//! `MutationGuard` writes it:
//!
//! ```ruby
//! class_attribute :guard_gate, instance_accessor: false, instance_predicate: false
//! wrapped_method = method(:guard_gate=)
//! redefine_singleton_method(:guard_gate=) do |value|
//!   wrapped_method.call(value)
//!   singleton_class.class_eval { protected :guard_gate }
//! end
//! class << self
//!   protected :guard_gate=
//! end
//! self.guard_gate = Gate.new
//! ```
//!
//! The net effect on each includer: the writer is protected; calling it
//! writes the includer's storage (the captured method is bound to the
//! includer, whatever class it is called on) and then protects the reader
//! on the receiving class, so after the block's own write the reader is
//! protected too. Before, the concern was refused at ingest ("visibility
//! declarations inside a `included do … end` block are not modeled");
//! dropping just the visibility lines left both methods public.
//!
//! The control is CRuby running the same source against the installed
//! ActiveRecord; the emitted tree must print the same lines.

use std::path::PathBuf;

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r#"module Gated
  extend ActiveSupport::Concern

  class Gate
    def initialize
      @disabled = false
    end

    attr_accessor :disabled

    def allow
      previous = disabled
      self.disabled = true
      yield
    ensure
      self.disabled = previous
    end
  end

  included do
    class_attribute :guard_gate, instance_accessor: false, instance_predicate: false
    # make methods defined by class_attribute protected
    wrapped_method = method(:guard_gate=)
    redefine_singleton_method(:guard_gate=) do |value|
      wrapped_method.call(value)
      singleton_class.class_eval { protected :guard_gate }
    end
    class << self
      protected :guard_gate=
    end

    self.guard_gate = Gate.new
  end

  class_methods do
    def locked?
      !guard_gate.disabled
    end
  end
end
"#;

const VAULT: &str = "class Vault < ApplicationRecord\n  include Gated\nend\n";

const SUB_VAULT: &str = "class SubVault < Vault\nend\n";

const SCRIPT: &str = r#"def visibility
  yield
  :public
rescue NoMethodError => e
  e.message[/protected|private/].to_sym
end
sc = Vault.singleton_class
p [Vault.locked?, visibility { Vault.guard_gate }, visibility { Vault.guard_gate = 1 }]
p [sc.protected_method_defined?(:guard_gate), sc.protected_method_defined?(:guard_gate=), sc.method_defined?(:guard_gate?), Vault.method_defined?(:guard_gate)]
p Vault.method(:guard_gate=).parameters
original = Vault.send(:guard_gate)
p [original.class.name, SubVault.send(:guard_gate).equal?(original)]
replacement = Gated::Gate.new
p SubVault.send(:guard_gate=, replacement)
p [Vault.send(:guard_gate).equal?(replacement), SubVault.instance_variable_defined?(:@__class_attr_guard_gate)]
p [SubVault.singleton_class.protected_method_defined?(:guard_gate), visibility { SubVault.guard_gate }]
sc.send(:public, :guard_gate)
p visibility { Vault.guard_gate }
Vault.send(:guard_gate=, original)
p [visibility { Vault.guard_gate }, Vault.send(:guard_gate).equal?(original)]
Vault.send(:guard_gate).allow { p [Vault.locked?, SubVault.locked?] }
p [Vault.locked?, SubVault.locked?]
"#;

#[test]
fn protected_class_attribute_methods_reach_each_includer_after_emission() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_record\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n{CONCERN}\n{VAULT}\n{SUB_VAULT}\n{SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected: Vec<String> = String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect();
    // The control exercises every part of the net effect, or matching it
    // would prove nothing.
    for probe in [
        "[true, :protected, :protected]",
        "[true, true, false, false]",
        "[[:req, :value]]",
        r#"["Gated::Gate", true]"#,
        ":guard_gate",
        "[true, false]",
        "[true, :protected]",
        ":public",
        "[:protected, true]",
        "[false, false]",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }
    let run = emit_and_run::real_blog()
        // A concrete model reads its table: ActiveRecord raises on the first
        // query without one, and check refuses it (model_table_not_in_schema).
        // The control never queries, so an empty table changes nothing.
        .edit("db/schema.rb", "create_table \"articles\", force: :cascade do |t|", "create_table \"vaults\", force: :cascade do |t|\n  end\n\n  create_table \"articles\", force: :cascade do |t|")
        .write("app/models/concerns/gated.rb", CONCERN)
        .write("app/models/vault.rb", VAULT)
        .write("app/models/sub_vault.rb", SUB_VAULT)
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from ActiveRecord line for line");
}

fn ingest_error(concern: &str) -> String {
    let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> = [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/concerns/gated.rb", concern),
        ("app/models/vault.rb", VAULT),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    match roundhouse::ingest::ingest_app_from_tree(tree) {
        Ok(_) => String::new(),
        Err(err) => err.to_string(),
    }
}

/// Only the shapes whose net effect the expansion reproduces are admitted.
/// A `protected` the later redefinition would make public again, a
/// protected name no `class_attribute` of the block generated, and any
/// other visibility statement in the block keep the refusal.
#[test]
fn other_class_side_visibility_in_included_blocks_stays_refused() {
    let refused = "visibility declarations inside a `included do … end` block are not modeled";
    assert!(ingest_error(CONCERN).is_empty(), "{}", ingest_error(CONCERN));
    let protected_before_redefinition = CONCERN.replace(
        "    redefine_singleton_method(:guard_gate=) do |value|\n      wrapped_method.call(value)\n      singleton_class.class_eval { protected :guard_gate }\n    end\n    class << self\n      protected :guard_gate=\n    end\n",
        "    class << self\n      protected :guard_gate=\n    end\n    redefine_singleton_method(:guard_gate=) do |value|\n      wrapped_method.call(value)\n      singleton_class.class_eval { protected :guard_gate }\n    end\n",
    );
    let foreign_name = CONCERN.replace("      protected :guard_gate=\n", "      protected :locked?\n");
    let extra_marker = CONCERN.replace("    self.guard_gate = Gate.new\n", "    self.guard_gate = Gate.new\n    private :helper\n");
    let captured_elsewhere = CONCERN.replace("    self.guard_gate = Gate.new\n", "    self.guard_gate = Gate.new\n    WRAPPED = wrapped_method\n");
    for (label, source) in [
        ("protected before redefinition", &protected_before_redefinition),
        ("foreign name", &foreign_name),
        ("extra marker", &extra_marker),
        ("capture read elsewhere", &captured_elsewhere),
    ] {
        assert_ne!(source.as_str(), CONCERN, "{label}: edit did not apply");
        let error = ingest_error(source);
        assert!(error.contains(refused), "{label}: expected the refusal, got {error:?}");
    }
}

/// A refused `included do` block is reported at the block. Expanding the
/// block's `class_attribute` defs ingests their generated source while
/// the visibility walk is inside the `included` statement; that nested
/// walk reset the statement it was in, so the refusal fell back to the
/// enclosing module's line (Core's `clickjacking_protection.rb:4:1`
/// instead of `:15:3`).
#[test]
fn a_refused_included_block_is_reported_at_its_own_line() {
    let dir = std::env::temp_dir().join(format!("roundhouse-included-site-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("app/controllers/concerns")).unwrap();
    std::fs::write(dir.join("roundhouse.yml"), "test_paths: []\n").unwrap();
    std::fs::write(
        dir.join("app/controllers/concerns/framed.rb"),
        "module Framed\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :rules, instance_writer: false\n    private :helper\n    self.rules = []\n  end\nend\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .arg("check")
        .arg("--continue")
        .arg(&dir)
        .output()
        .expect("run roundhouse check");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    let line = text
        .lines()
        .find(|l| l.contains("error[ingest_unsupported]") && l.contains("framed.rb"))
        .unwrap_or_else(|| panic!("no ingest_unsupported for framed.rb:\n{text}"));
    assert!(line.contains("framed.rb:4:3:"), "refusal not at the `included do` line: {line}");
}
