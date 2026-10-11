//! `cattr_*` / `mattr_*` storage is one class variable, `@@name`, shared
//! by the declaring class, every subclass, and their instances, as
//! ActiveSupport defines it (core_ext/module/attribute_accessors.rb:
//! `def self.x; @@x; end`, `class_variable_set("@@x", default)`).
//!
//! The emitted tree stored it in the declaring class's own ivar, so a
//! subclass read nil and a subclass write went to the subclass alone.
//! activemerchant 1.137.0's `Gateway` appends each subclass to
//! `@@implementations` in `inherited`, which runs with the subclass's
//! parent as `self`: the first gateway of a gateway subclass
//! (`InContextPaypalExpressGateway < PaypalExpressGateway`) raised
//! NoMethodError while loading.
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
    let dir = std::env::temp_dir().join(format!("rh_cattr_shared_storage_{}_{n}", std::process::id()));
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

const GATEWAY: &str = r##"class Gateway
  cattr_reader :implementations
  @@implementations = []
  cattr_accessor :mode, default: :live
  cattr_accessor :logger
  cattr_accessor(:retries) { 3 }

  def self.inherited(subclass)
    super
    @@implementations << subclass
  end

  def self.reset_mode
    @@mode = :live
  end

  def bump
    @@retries += 1
  end

  def describe = [mode, logger, @@retries]
end
"##;

const BOGUS: &str = "class BogusGateway < Gateway\n  self.logger = :bogus_log\nend\n";
const SUB_BOGUS: &str = "class SubBogusGateway < BogusGateway\nend\n";

const SCRIPT: &str = r##"SubBogusGateway
p [Gateway.implementations, BogusGateway.implementations.equal?(Gateway.implementations)]
p [Gateway.mode, BogusGateway.mode, SubBogusGateway.mode, Gateway.logger, SubBogusGateway.logger, Gateway.retries]
SubBogusGateway.mode = :test
p [Gateway.mode, BogusGateway.mode]
g = SubBogusGateway.new
p [g.mode, g.logger, g.retries, g.bump, Gateway.retries, g.describe]
g.mode = :sandbox
p [Gateway.mode, BogusGateway.reset_mode, SubBogusGateway.mode]
p [Gateway.class_variable_get(:@@mode), Gateway.class_variables.sort, BogusGateway.class_variable_defined?(:@@logger)]
p [Gateway.respond_to?(:implementations=), Gateway.new.respond_to?(:implementations), Gateway.new.respond_to?(:implementations=)]
"##;

#[test]
fn a_class_s_cattr_is_one_class_variable_through_its_subclasses_as_native() {
    let classes = [("lib/gateway.rb", GATEWAY), ("lib/bogus_gateway.rb", BOGUS), ("lib/sub_bogus_gateway.rb", SUB_BOGUS)];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native(&format!("{PRELUDE}{program}{SCRIPT}"));
    assert_eq!(
        expected,
        "[[BogusGateway, SubBogusGateway], true]\n\
         [:live, :live, :live, :bogus_log, :bogus_log, 3]\n\
         [:test, :test]\n\
         [:test, :bogus_log, 3, 4, 4, [:test, :bogus_log, 4]]\n\
         [:sandbox, :live, :live]\n\
         [:live, [:@@implementations, :@@logger, :@@mode, :@@retries], true]\n\
         [false, true, false]\n",
        "native control"
    );
    let run = classes.iter().fold(emit_and_run::empty_app(), |overlay, (path, source)| overlay.write(path, source)).run_ruby(SCRIPT);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// A module's `mattr_accessor`, read and written through the module, its
/// own singleton methods and an includer's instance.
const SETTINGS: &str = r##"module Settings
  mattr_accessor :level, default: 1
  mattr_reader :name

  def self.raise_level
    @@level += 1
  end

  def level_twice = level * 2
end

class Host
  include Settings
end
"##;

#[test]
fn a_module_s_mattr_is_its_class_variable_as_native() {
    let script = "p [Settings.level, Settings.name, Settings.raise_level, Host.new.level, Host.new.level_twice]\n\
                  Host.new.level = 7\np [Settings.level, Settings.class_variables.sort]\n";
    let expected = native(&format!("{PRELUDE}{SETTINGS}{script}"));
    assert_eq!(expected, "[1, nil, 2, 2, 4]\n[7, [:@@level, :@@name]]\n", "native control");
    let run = emit_and_run::empty_app().write("lib/settings.rb", SETTINGS).run_ruby(script);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// The seed runs where the declaration stands, against authored `@@x =`
/// writes before or after it: a non-nil default overwrites an earlier
/// value, a nil or absent one keeps it, a later write wins. A multi-name
/// declaration's one default object is shared; `instance_reader: false`
/// leaves the instance writer only.
const ORDER: &str = r##"class Order
  @@before = :authored
  cattr_accessor :before, default: 41
  @@kept = :authored
  cattr_accessor :kept, default: nil
  @@bare = :authored
  mattr_reader :bare
  cattr_accessor(:after) { 41 }
  @@after = :authored
  cattr_accessor :left, :right, default: []
  mattr_accessor :hidden, instance_reader: false, default: 1
end
"##;

#[test]
fn a_cattr_seed_runs_in_source_order_as_native() {
    let script = "o = Order.new\n\
        p [Order.before, Order.kept, Order.bare, Order.after, o.before, o.bare]\n\
        p [Order.left.equal?(Order.right), o.respond_to?(:hidden), o.respond_to?(:hidden=)]\n\
        o.hidden = 2\np Order.hidden\n";
    let expected = native(&format!("{PRELUDE}{ORDER}{script}"));
    assert_eq!(expected, "[41, :authored, :authored, :authored, 41, :authored]\n[true, false, true]\n2\n", "native control");
    let run = emit_and_run::empty_app().write("lib/order.rb", ORDER).run_ruby(script);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// A class-body call after the declaration may send the writer to the
/// class (`registry.register(owner: self, accessor: :store)` runs
/// `owner.send("store=", client)` at load): ActiveSupport defines the
/// accessors where the declaration stands, so the writer exists by then.
const BINDS_AT_LOAD: &str = r##"class Registry
  def self.register(owner:, accessor:, value:)
    owner.send("#{accessor}=", value)
  end
end

class Bound
  cattr_accessor :store

  Registry.register(owner: self, accessor: :store, value: :client)
end
"##;

#[test]
fn a_class_body_call_after_cattr_accessor_finds_the_writer_as_native() {
    let script = "p [Bound.store, Bound.new.store]\n";
    let expected = native(&format!("{PRELUDE}{BINDS_AT_LOAD}{script}"));
    assert_eq!(expected, "[:client, :client]\n", "native control");
    let run = emit_and_run::empty_app().write("lib/bound.rb", BINDS_AT_LOAD).run_ruby(script);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// A class on a base the compiler does not model replays its class-body
/// calls verbatim. A call that names a declared accessor by keyword
/// (`registry.register(owner: self, accessor: :store)` sends `store=` to the
/// class as it runs) is a tail effect: it stands below the accessors, as in
/// ActiveSupport, where the declaration defines them where it stands.
const BINDS_ON_FOREIGN_BASE: &str = r##"class Bound < ActionDispatch::Session::AbstractStore
  cattr_accessor :store

  Registry.register(owner: self, accessor: :store) do
    :built
  end

  def find(id) = store
end
"##;

#[test]
fn a_replayed_call_naming_a_declared_accessor_by_keyword_is_written_below_the_accessors() {
    let run = emit_and_run::empty_app()
        .write("lib/registry.rb", "class Registry\n  def self.register(owner:, accessor:)\n    owner.send(\"#{accessor}=\", yield)\n  end\nend\n")
        .write("app/utils/bound.rb", BINDS_ON_FOREIGN_BASE)
        .run_ruby("p 1\n");
    let text = std::fs::read_to_string(run.emitted.join("app/models/bound.rb")).expect("the class is emitted");
    let call = text.find("Registry.register").unwrap_or_else(|| panic!("the call is replayed:\n{text}"));
    let writer = text.find("def self.store=").unwrap_or_else(|| panic!("the writer is emitted:\n{text}"));
    assert!(writer < call, "the call runs before the writer it sends:\n{text}");
}
