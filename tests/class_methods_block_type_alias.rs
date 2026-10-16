//! A type alias naming a Concern's `class_methods do` carrier, Core's
//! `Markets::Platform::Responders::Type::Model`:
//!
//! ```ruby
//! Model = T.type_alias do
//!   T.all(T.class_of(ApplicationRecord), T::Class[Responder], Responder::ClassMethods)
//! end
//! ```
//!
//! `Responder`'s class side is a `class_methods do` block, which builds the
//! `ClassMethods` module at load and declares nothing the analysis indexes,
//! so the read was "constant not supported" and the alias's block raised
//! when a check evaluated it: every `Registry.register(ResponderType)` of
//! Core's initializers failed (`No responder registered for model:
//! Merchandising::Catalog`, 4+ unit-1000-v2 ids). The carrier is a module of
//! the emitted Concern, found by its full name like any nested module.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const RESPONDER: &str = r##"# typed: true
module Platform
  module Responder
    extend ActiveSupport::Concern

    class_methods do
      def hello = "hi"
    end
  end
end
"##;

const TYPE: &str = r##"# typed: true
module Platform
  module Responders
    module Type
      Model = T.type_alias do
        T.all(T.class_of(Widget), T::Class[Responder], Responder::ClassMethods)
      end
    end
  end
end
"##;

const WIDGET: &str = r##"class Widget
  include Platform::Responder
end
"##;

const USER: &str = r##"# typed: true
class Registry
  #: (untyped) -> untyped
  def self.model(value)
    T.let(value, Platform::Responders::Type::Model)
  end
end
"##;

#[test]
fn an_alias_naming_a_class_methods_block_carrier_evaluates() {
    let run = emit_and_run::empty_app()
        .write("app/models/platform/responder.rb", RESPONDER)
        .write("app/models/platform/responders/type.rb", TYPE)
        .write("app/models/widget.rb", WIDGET)
        .write("app/models/registry.rb", USER)
        .run_ruby("p Registry.model(Widget).hello\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "\"hi\"", "{}", run.stderr);
    assert!(!run.stderr.contains("constant not supported"), "{}", run.stderr);
}

#[test]
fn the_alias_is_emitted_naming_the_carrier_in_full() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/models/platform/responder.rb", RESPONDER)
        .write("app/models/platform/responders/type.rb", TYPE)
        .write("app/models/widget.rb", WIDGET)
        .write("app/models/registry.rb", USER)
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    let text = std::fs::read_to_string(emitted.join("app/models/platform/responders/type.rb")).unwrap();
    assert!(text.contains("Platform::Responder::ClassMethods"), "{text}");
    assert!(!text.contains("constant not supported"), "{text}");
}
