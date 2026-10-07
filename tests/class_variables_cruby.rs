//! Native class-variable semantics through ingest, lowering and Ruby emit.
//!
//! A class variable is one slot shared by its class and every subclass,
//! readable and writable from class methods, instance methods and the
//! class body; an unset one raises NameError, and a superclass that
//! defines it after a subclass did "overtakes" it with a RuntimeError.
//! shopify-money's `Money.new` (`@@zero_money ||= {}` in a class method)
//! and `Money::Currency` (`@@mutex = Mutex.new`, `@@loaded_currencies =
//! {}`) need all of that. Each probe prints what it observes; the emitted
//! tree must print the same lines, in the same order, as CRuby running the
//! authored source directly.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SOURCE: &str = r#"class CvarRegistry
  @@mutex = Mutex.new
  @@loaded = {}

  class << self
    def new(key)
      @@loaded[key] || @@mutex.synchronize { @@loaded[key] = super(key) }
    end

    def loaded_keys
      @@loaded.keys
    end

    def reset_loaded
      @@loaded = {}
    end

    def mutex_class
      @@mutex.class.name
    end
  end

  attr_reader :key

  def initialize(key)
    @key = key
  end
end

class CvarBase
  def self.zero
    @@zero ||= {}
    @@zero[name] ||= "zero-#{name}"
  end

  def self.zero_table
    @@zero
  end

  def self.store(value)
    @@shared = value
  end

  def self.shared
    @@shared
  end

  def shared_from_instance
    @@shared
  end

  def bump_from_instance
    @@shared += 1
  end
end

class CvarChild < CvarBase
  def self.child_store(value)
    @@shared = value
  end
end

class CvarUnset
  def self.read
    @@never_set
  end
end

class CvarLate
end

class CvarEarly < CvarLate
  def self.claim
    @@claimed = :child
  end

  def self.claimed
    @@claimed
  end
end

class CvarLate
  def self.claim_parent
    @@claimed = :parent
  end
end
"#;

const SCRIPT: &str = r#"
first = CvarRegistry.new("usd")
again = CvarRegistry.new("usd")
puts "registry memoized: #{first.equal?(again)} key=#{first.key}"
puts "registry keys: #{CvarRegistry.loaded_keys.inspect}"
puts "registry mutex: #{CvarRegistry.mutex_class}"
CvarRegistry.reset_loaded
puts "registry after reset: #{CvarRegistry.loaded_keys.inspect} fresh=#{!CvarRegistry.new("usd").equal?(first)}"

puts "inherited zero from child: #{CvarChild.zero}"
puts "inherited zero from base: #{CvarBase.zero}"
puts "one zero table: #{CvarBase.zero_table.inspect} same=#{CvarBase.zero_table.equal?(CvarChild.zero_table)}"

CvarBase.store(1)
puts "base write seen by child: #{CvarChild.shared}"
CvarChild.child_store(5)
puts "child write seen by parent: #{CvarBase.shared}"
puts "child write seen by instance: #{CvarBase.new.shared_from_instance}"
CvarChild.new.bump_from_instance
puts "instance write seen by both: #{CvarBase.shared} #{CvarChild.shared}"
puts "storage owner: #{CvarBase.class_variable_defined?(:@@shared)} #{CvarChild.class_variables(false).include?(:@@shared)}"

begin
  CvarUnset.read
  puts "uninitialized read returned"
rescue NameError => e
  puts "uninitialized read: #{e.class}: #{e.message}"
end

CvarEarly.claim
puts "child claimed first: #{CvarEarly.claimed}"
CvarLate.claim_parent
begin
  CvarEarly.claimed
  puts "overtaken read returned"
rescue RuntimeError => e
  puts "overtaken read: #{e.class}: #{e.message}"
end
"#;

/// The authored source, run by CRuby itself.
fn native_lines() -> Vec<String> {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{SOURCE}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect()
}

#[test]
fn class_variables_behave_as_in_cruby_after_emission() {
    let expected = native_lines();
    // The control must actually exercise each behaviour, or matching it
    // would prove nothing.
    for probe in [
        "registry memoized: true key=usd",
        "registry mutex: Thread::Mutex",
        "child write seen by parent: 5",
        "instance write seen by both: 6 6",
        "uninitialized read: NameError: uninitialized class variable @@never_set in CvarUnset",
        "overtaken read: RuntimeError: class variable @@claimed of CvarEarly is overtaken by CvarLate",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let run = emit_and_run::real_blog()
        .write("app/services/cvar_probe.rb", SOURCE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from CRuby line for line");
}

/// A class variable stays native Ruby: a target that cannot express the
/// shared storage refuses each read and write by name.
#[test]
fn class_variables_are_refused_by_name_off_the_ruby_family() {
    let (_tree, errors) = emit_and_run::real_blog()
        .write("app/services/cvar_probe.rb", SOURCE)
        .emit(BuildTarget::Spinel);
    for construct in ["class variable write", "class variable read"] {
        assert!(
            errors.iter().any(|e| e.contains(construct)),
            "spinel must refuse {construct}: {errors:#?}"
        );
    }
}

/// A concern's class-side method is copied onto each includer, where `@@x`
/// would resolve against the includer; Ruby resolves it in the concern
/// that wrote the `def`. The copy refuses each class variable at its site.
#[test]
fn class_variables_in_copied_concern_class_methods_are_refused() {
    let (_tree, errors) = emit_and_run::real_blog()
        .write("app/services/cvar_counting.rb", r#"module CvarCounting
  extend ActiveSupport::Concern

  class_methods do
    def bump
      @@bumps ||= 0
      @@bumps += 1
    end
  end
end
"#)
        .write("app/services/cvar_counter.rb", r#"class CvarCounter
  include CvarCounting
end
"#)
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("class variable in a copied concern class method")),
        "the includer's copy must refuse its class variables: {errors:#?}"
    );
}
