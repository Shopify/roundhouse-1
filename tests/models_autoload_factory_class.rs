//! A file whose one constant is a factory class (`Enum = Struct.new(:name)
//! do … end`, `Point = Data.define(:x) do … end`) autoloads under its namespace,
//! as a file opening `class Enum` does. Shopify Core's
//! Podding::Priority::Enum was loaded at boot instead: its
//! `module Podding; module Priority` reopen autoloaded priority.rb, whose
//! `require_relative "priority/enum"` was the file already loading, and
//! priority.rb's read of `Enum` raised NameError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const PRIORITY: &str = r#"require_relative "priority/enum"

module Podding
  module Priority
    HIGH = Enum.new("high")

    def self.high
      HIGH
    end
  end
end
"#;

const ENUM: &str = r#"module Podding
  module Priority
    Enum = Struct.new(:name) do
      def to_s
        name
      end
    end
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/podding/priority.rb", PRIORITY)
        .write("app/models/podding/priority/enum.rb", ENUM)
}

#[test]
fn a_factory_class_file_is_an_autoload_not_a_boot_file() {
    let (dir, _) = app().emit(BuildTarget::Ruby);
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    assert!(models.contains(r#"[:Enum, "models/podding/priority/enum", false]"#), "{models}");
    let boot = &models[models.find("BOOT_FILES = [").expect("BOOT_FILES")..];
    let boot = &boot[..boot.find("].freeze").unwrap()];
    assert!(!boot.contains("priority/enum"), "{boot}");
}

#[test]
fn the_factory_class_loads_when_its_namespace_reads_it() {
    let run = app().run_ruby("puts Podding::Priority.high.to_s\nputs Podding::Priority::Enum.new('low').to_s\n");
    run.assert_passes();
    assert_eq!(run.stdout, "high\nlow\n");
}
