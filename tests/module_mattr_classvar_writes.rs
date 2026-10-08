//! A module's singleton methods writing the class variable that its
//! `mattr_accessor` declares (activemerchant 1.137.0's
//! `ActiveMerchant::Billing::Base`, where `gateway_mode=` writes `@@mode`
//! and `mattr_accessor :mode` reads it). Ingest refused the file
//! ("native class-variable writes alongside cattr/mattr storage"), so
//! `Billing::Base` was missing from the tree and every
//! `Base.test?` raised at run time.
//!
//! The control is CRuby running the same source against the pinned
//! ActiveSupport (tests/support/rails_oracle.rb).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

fn native(program: &str) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_module_mattr_classvar_writes_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rb"), format!("require {ORACLE:?}\n{program}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

const PRELUDE: &str = "RailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\n\
     require \"active_support/core_ext/module/attribute_accessors\"\n";

const BASE: &str = r##"module Bill
  module Base
    mattr_accessor :mode
    mattr_accessor :ticks

    def self.gateway_mode=(mode)
      @@mode = mode
    end

    def self.gateway_mode
      @@mode
    end

    class << self
      def tick
        @@ticks += 1
      end
    end

    self.mode = :production
    self.ticks = 0

    def self.test?
      mode == :test
    end
  end
end
"##;

const SCRIPT: &str = r##"b = Bill::Base
p [b.mode, b.gateway_mode, b.test?]
b.gateway_mode = :test
p [b.mode, b.test?]
b.mode = :live
p [b.gateway_mode, b.tick, b.tick, b.ticks]
"##;

#[test]
fn a_module_s_singleton_methods_write_its_mattr_storage_as_native() {
    let expected = native(&format!("{PRELUDE}{BASE}{SCRIPT}"));
    assert_eq!(expected, "[:production, :production, false]\n[:test, true]\n[:live, 1, 2, 2]\n", "native control");
    let run = emit_and_run::empty_app().write("lib/bill.rb", BASE).run_ruby(SCRIPT);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// A class's singleton method can run with a subclass as `self`, whose
/// own ivar is not the shared storage; a module's instance method writes
/// the class variable `mattr_accessor` stores under its own ivar here.
/// Both stay refused.
#[test]
fn a_class_s_or_an_instance_method_s_write_stays_refused() {
    for source in [
        "class Probe\n  mattr_accessor :mode\n  def self.set(m)\n    @@mode = m\n  end\nend\n",
        "module Probe\n  mattr_accessor :mode\n  def set(m)\n    @@mode = m\n  end\nend\n",
    ] {
        let err = roundhouse::ingest::ingest_library_classes(source.as_bytes(), "probe.rb")
            .expect_err("native and modeled storage cannot silently diverge");
        assert!(err.to_string().contains("alongside cattr/mattr storage"), "{source}: {err}");
    }
    let classes = roundhouse::ingest::ingest_library_classes(BASE.as_bytes(), "bill.rb").unwrap();
    let base = classes.iter().find(|c| c.name.0.as_str() == "Bill::Base").expect("Bill::Base is carried");
    assert!(base.methods.iter().any(|m| m.name.as_str() == "gateway_mode="), "{:?}", base.methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>());
}

/// A class-body `@@x = value` of a `cattr_reader` name is the storage the
/// lowered reader answers: activemerchant 1.137.0's `Gateway` declares
/// `cattr_reader :implementations; @@implementations = []`, and its
/// `inherited` hook appends each gateway. Ingest dropped the initializer
/// whatever its value, so the first gateway subclass raised NoMethodError
/// (`<<` for nil) while loading.
const GATE: &str = r##"class Gate
  cattr_reader :impls
  @@impls = []
  cattr_accessor :unset
  @@unset = nil

  def self.inherited(subclass)
    super
    @@impls << subclass
  end
end
"##;

#[test]
fn a_class_body_storage_initializer_is_the_readers_value_as_native() {
    let script = "class Bogus < Gate; end\nclass Other < Gate; end\np [Gate.impls, Gate.unset]\n";
    let expected = native(&format!("{PRELUDE}{GATE}{script}"));
    assert_eq!(expected, "[[Bogus, Other], nil]\n", "native control");
    let run = emit_and_run::empty_app().write("lib/gate.rb", GATE).run_ruby(script);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}
