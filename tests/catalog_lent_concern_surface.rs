//! The AR names lent to a model concern stay out of the folds.
//!
//! A module that some model includes is lent the AR instance surface
//! (`errors`, `save`, ...) so its bodies type against the model. Those
//! names are the model's, not the module's: Core's models
//! `include Kernel`, and `ComplexStruct::ClassMethods` also does
//! `include(Kernel)`. Copying Kernel's lent `errors` on to every class
//! extending ClassMethods typed `ComplexStruct::Base`'s implicit
//! `errors` as `ActiveModel::Errors` instead of its own declared
//! `Errors`, refusing `error_exists?`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(files: &[(&str, &str)]) -> (bool, String) {
    let root = std::env::temp_dir().join(format!("rh_catalog_lent_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let base: &[(&str, &str)] = &[
        ("roundhouse.yml", "test_paths:\n  - test\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/data_source.rb", "class DataSource < ApplicationRecord\n  include Kernel\nend\n"),
        ("lib/kernel_ext.rb", "module Kernel\n  #: (String) -> void\n  def boot_note(msg)\n    nil\n  end\nend\n"),
    ];
    for (path, text) in base.iter().chain(files) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

const ERRORS: &str = r#"module Structs
  class Errors
    #: -> void
    def initialize
      @list = [] #: Array[Symbol]
    end

    #: (Symbol) -> bool
    def error_exists?(attr)
      @list.include?(attr)
    end
  end
end
"#;

const CLASS_METHODS: &str = r#"module Structs
  module ClassMethods
    include(Kernel)

    #: (Symbol) -> void
    def validate(name)
      nil
    end
  end
end
"#;

const BASE: &str = r#"module Structs
  class Base
    extend(ClassMethods)

    #: -> Errors
    def errors
      @errors ||= Errors.new #: Errors?
    end
  end
end
"#;

const CHANGE: &str = r#"module Structs
  class Change < Base
    validate(:named)

    #: -> bool
    def named
      errors.error_exists?(:name)
    end
  end
end
"#;

#[test]
fn an_extender_keeps_its_own_errors_over_a_model_concerns_lent_one() {
    let (ok, text) = check(&[
        ("lib/structs/errors.rb", ERRORS),
        ("lib/structs/class_methods.rb", CLASS_METHODS),
        ("lib/structs/base.rb", BASE),
        ("lib/structs/change.rb", CHANGE),
    ]);
    assert!(ok, "{text}");
    assert!(!text.contains("ActiveModel::Errors"), "{text}");
}

#[test]
fn a_plain_includer_is_not_handed_the_lent_surface() {
    let plain = "class Plain\n  include Kernel\n  include Structs::ClassMethods\n\n  #: -> Structs::Errors\n  def errors = Structs::Errors.new\n\n  #: -> bool\n  def named = errors.error_exists?(:name)\nend\n";
    let (ok, text) = check(&[("lib/structs/errors.rb", ERRORS), ("lib/structs/class_methods.rb", CLASS_METHODS), ("lib/plain.rb", plain)]);
    assert!(ok, "{text}");
}

#[test]
fn the_model_concern_itself_still_types_the_lent_surface() {
    let concern = "module Kernel\n  #: -> bool\n  def blank_errors? = errors.empty?\nend\n";
    let (ok, text) = check(&[("lib/kernel_errors.rb", concern)]);
    assert!(ok, "{text}");
}
