//! Module protocol inside a module that classes `extend`.
//!
//! `extend M` in a class body makes M's instance methods the class's
//! singleton methods, so `self` in them is a class: Core's
//! `SymbolEnum#symbol_enum` calls `define_method`, and
//! `AttributeOptions#valid_options` reads `constants`. When nothing
//! includes M, extends a value with it, or extends from inside a method,
//! `check` types M's methods class side.
//!
//! Those uses give `self` an instance too, and alone they keep the
//! refusal. Beside a class's `extend M` (Core's `Denormalize` includes
//! the `AcceleratedBelongsTo` that ApplicationRecord extends), a
//! Module-only call on implicit self is still admitted: an instance
//! that got M has no such method unless its class defines one, so there
//! the call raises NoMethodError in Ruby and in the emitted tree alike,
//! and Module's answer on the extender is the only value it returns.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const MACROS: &str = r##"module Macros
  def flag(name)
    define_method("#{name}?") { true }
    class_eval { attr_accessor :checked }
    note(name)
  end

  def note(name)
    const_defined?(:STRICT) ? "strict #{name}" : "loose #{name}"
  end
end
"##;

fn check(files: &[(&str, &str)]) -> (bool, String) {
    let root = std::env::temp_dir().join(format!("rh_extension_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    for (path, text) in [("roundhouse.yml", "test_paths:\n  - test\n"), ("lib/macros.rb", MACROS)].iter().chain(files) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    std::fs::create_dir_all(root.join("app/models")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

const GADGET: &str = "class Gadget\n  extend Macros\n  STRICT = true\n  flag :shiny\n  def self.report = [new.shiny?, note(:x)]\nend\n";

#[test]
fn a_module_only_extended_onto_classes_runs_module_protocol_on_self() {
    let (ok, text) = check(&[("lib/gadget.rb", GADGET)]);
    assert!(ok, "{text}");
    assert!(!text.contains("Module protocol"), "{text}");
}

#[test]
fn a_hook_extending_its_base_counts_as_a_class_extend() {
    let tracked = "module Tracked\n  def self.included(base)\n    base.extend(Macros)\n  end\nend\n";
    let widget = "class Widget\n  include Tracked\n  flag :tracked\nend\n";
    let (ok, text) = check(&[("lib/tracked.rb", tracked), ("lib/widget.rb", widget)]);
    assert!(ok, "{text}");
}

const INSTANCE_SIDE_USES: [&str; 3] = [
    "class Plain\n  include Macros\nend\n",
    "class Plain\n  def self.build = Object.new.extend(Macros)\nend\n",
    "class Plain\n  def adopt\n    extend Macros\n  end\nend\n",
];

#[test]
fn instance_side_uses_alone_keep_the_refusal() {
    for other in INSTANCE_SIDE_USES {
        let (ok, text) = check(&[("lib/plain.rb", other)]);
        assert!(!ok, "accepted with {other}: {text}");
        assert!(text.contains("Module protocol not supported"), "{other}: {text}");
    }
}

#[test]
fn a_class_extend_beside_instance_side_uses_admits_module_only_calls() {
    for other in INSTANCE_SIDE_USES {
        let (ok, text) = check(&[("lib/gadget.rb", GADGET), ("lib/plain.rb", other)]);
        assert!(ok, "refused with {other}: {text}");
        assert!(!text.contains("Module protocol"), "{other}: {text}");
    }
}

/// An app class that declares one of the names as an instance method
/// could be the includer that answers it, so that name stays refused.
#[test]
fn an_app_instance_method_of_the_name_keeps_its_refusal() {
    let other = "class Plain\n  include Macros\nend\n\nclass Recorder\n  def define_method(name) = name\nend\n";
    let (ok, text) = check(&[("lib/gadget.rb", GADGET), ("lib/plain.rb", other)]);
    assert!(!ok, "{text}");
    assert!(text.contains("define_method requires a proven"), "{text}");
    assert!(!text.contains("class_eval requires") && !text.contains("const_defined? requires"), "{text}");
}

#[test]
fn an_inherited_hook_receives_a_class() {
    // Ruby calls `inherited(subclass)` with the new Class. Core's
    // `GraphApi::GlobalId`, extended by every GraphApi type, runs
    // `subclass.class_eval { @has_numeric_gid = nil }` there; refused as
    // an unproven receiver, the hook raised on each type's definition.
    let registry = "module Registry\n  def inherited(subclass)\n    super\n    subclass.class_eval do\n      @numeric = nil\n    end\n  end\nend\n";
    let base = "class Base\n  extend Registry\n\n  def self.inherited(sub)\n    super\n    sub.class_eval { @seen = true }\n  end\nend\n\nclass Child < Base\nend\n";
    let (ok, text) = check(&[("lib/gadget.rb", GADGET), ("lib/registry.rb", registry), ("lib/base.rb", base)]);
    assert!(ok, "{text}");
    assert!(!text.contains("Module protocol"), "{text}");
}
