//! Module protocol inside a module that classes `extend`.
//!
//! `extend M` in a class body makes M's instance methods the class's
//! singleton methods, so `self` in them is a class: Core's
//! `SymbolEnum#symbol_enum` calls `define_method`, and
//! `AttributeOptions#valid_options` reads `constants`. When nothing
//! includes M, extends a value with it, or extends from inside a method,
//! `check` admits the protocol there. Any of those uses gives `self` an
//! instance, so the refusal stays.

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

#[test]
fn any_instance_side_use_keeps_the_refusal() {
    for (path, other) in [
        ("lib/plain.rb", "class Plain\n  include Macros\nend\n"),
        ("lib/plain.rb", "class Plain\n  def self.build = Object.new.extend(Macros)\nend\n"),
        ("lib/plain.rb", "class Plain\n  def adopt\n    extend Macros\n  end\nend\n"),
    ] {
        let (ok, text) = check(&[("lib/gadget.rb", GADGET), (path, other)]);
        assert!(!ok, "accepted with {other}: {text}");
        assert!(text.contains("Module protocol not supported"), "{other}: {text}");
    }
}
