//! Declared types (`#: X`) that name a real class by a route Ruby's
//! constant lookup takes and the cast's resolution did not:
//!
//! - a module's method typed with its sole includer as `self` still
//!   reads constants from the module's cref (Shopify Core's
//!   `Functions::Graph::FieldExtension#initialize`:
//!   `@merged_execution_mapping = nil #: MergedExecutionMapping?`);
//! - `NAME = Class.new` is `class NAME; end`
//!   (`Shipping::PurchaseLabelErrorHandleable::ShippingIntentIdNotProvided`);
//! - a constant alias of a namespace on the path
//!   (`PaymentsPartners::Capture::State::Unprocessed`, where `Capture`
//!   writes `State = CaptureState`).
//!
//! Each was refused (`… has no indexed declaration or modeled
//! dependency`) and the emitted constant or initializer raised.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const FILES: &[(&str, &str)] = &[
    ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
    ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "app/models/fn/graph/mapping.rb",
        "module Fn\n  module Graph\n    class Mapping\n      def initialize(field:)\n        @field = field\n      end\n      attr_reader :field\n    end\n  end\nend\n",
    ),
    (
        "app/models/fn/graph/field_ext.rb",
        r#"module Fn
  module Graph
    module FieldExt
      #: Mapping?
      attr_reader :mapping

      #: (?complexity: Integer?) -> void
      def initialize(complexity: nil)
        @mapping = nil #: Mapping?
        super()
        @complexity = complexity
      end

      def map!(field)
        @mapping = Mapping.new(field: field)
      end
    end
  end
end
"#,
    ),
    ("app/models/other/base_field.rb", "module Other\n  class BaseField\n    def initialize\n      @name = nil\n    end\n  end\nend\n"),
    ("app/models/other/field.rb", "module Other\n  class Field < BaseField\n    include Fn::Graph::FieldExt\n  end\nend\n"),
    (
        "app/models/sh/handleable.rb",
        r#"module Sh
  module Handleable
    NotProvided = Class.new
    NOT_PROVIDED = NotProvided.new.freeze #: NotProvided

    #: ((Integer | NotProvided)?) -> bool
    def provided?(id = NOT_PROVIDED)
      !NOT_PROVIDED.equal?(id)
    end
  end
end
"#,
    ),
    ("app/models/pp/capture_state.rb", "module Pp\n  class CaptureState\n    class Unprocessed < CaptureState\n    end\n  end\nend\n"),
    ("app/models/pp/capture.rb", "module Pp\n  class Capture\n    State = CaptureState\n    def self.fresh = State::Unprocessed.new\n  end\nend\n"),
    (
        "app/models/pp/capture_user.rb",
        "module Pp\n  class CaptureUser\n    UNPROCESSED = Capture::State::Unprocessed.new #: Pp::Capture::State::Unprocessed\n    def self.state = UNPROCESSED\n  end\nend\n",
    ),
];

fn app(extra: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "rh_declared_type_misc_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in FILES.iter().chain(extra) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    root
}

fn check(root: &Path) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["check", "--continue"])
        .arg(root)
        .output()
        .unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn each_declared_type_resolves_as_ruby_looks_it_up() {
    let root = app(&[]);
    let output = check(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!output.contains("declared type not supported"), "{output}");
    assert!(!output.contains("constant not supported"), "{output}");
    assert!(output.contains(" 0 error(s)"), "{output}");
}

/// An alias whose namespace declares no such class names no class.
#[test]
fn an_alias_to_a_namespace_without_the_class_stays_refused() {
    let root = app(&[(
        "app/models/pp/dangling.rb",
        "module Pp\n  class Dangling\n    State = Capture\n    X = Object.new #: Pp::Dangling::State::Unprocessed\n  end\nend\n",
    )]);
    let output = check(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(output.contains("Pp::Dangling::State::Unprocessed has no indexed declaration or modeled dependency"), "{output}");
}

#[test]
fn class_new_emits_a_class_and_the_casts_their_values() {
    let root = app(&[]);
    let out = root.with_extension("out");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby"])
        .arg(&root)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    let read = |path: &str| std::fs::read_to_string(out.join(path)).unwrap_or_default();
    let (class, handleable, user, ext) = (
        read("app/models/sh/handleable/not_provided.rb"),
        read("app/models/sh/handleable.rb"),
        read("app/models/pp/capture_user.rb"),
        read("app/models/fn/graph/field_ext.rb"),
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&out);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(class.contains("class NotProvided\n"), "{class}");
    assert!(handleable.contains("NOT_PROVIDED = Sh::Handleable::NotProvided.new.freeze"), "{handleable}");
    assert!(user.contains("UNPROCESSED = ::Pp::CaptureState::Unprocessed.new"), "{user}");
    assert!(ext.contains("@mapping = nil\n"), "{ext}");
    for text in [&handleable, &user, &ext] {
        assert!(!text.contains("roundhouse: declared type not supported"), "{text}");
    }
}
