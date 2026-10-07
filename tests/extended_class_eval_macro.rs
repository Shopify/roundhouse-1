//! A class macro an app base class gets through `extend M` defines its
//! subclass's methods with a `class_eval` heredoc.
//!
//! Core's `CustomerFoundations::ComplexStruct::Base` does
//! `extend(ClassMethods)`, and `ClassMethods#attribute(name, type, …)`
//! records the attribute and runs `class_eval(<<~RUBY)` with
//! `def #{name}; @attributes[:#{name}]; end`. Each struct
//! (`CustomerFoundations::Id`) then calls `attribute :value, String`.
//! The emitted base dropped the `extend`, because `ClassMethods` resolves
//! from the class's namespace to `Cs::ClassMethods`, and the subclass
//! dropped the `attribute` call, so `Id#value` did not exist.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CLASS_METHODS: &str = r##"module Cs
  module ClassMethods
    include Helpers

    #: (Symbol, untyped, ?optional: bool) -> void
    def attribute(name, type, optional: false)
      raise("Attribute #{name} is already defined") if _internal_attributes.key?(name)
      _internal_attributes[name] = type
      caller = caller_locations(1, 1)&.first
      class_eval(<<~RUBY, caller&.path, caller&.lineno)
        def #{name}
          @attributes[:#{name}]
        end
      RUBY
    end

    #: -> Hash[Symbol, untyped]
    def _internal_attributes
      @_internal_attributes ||= {}
    end
  end
end
"##;

const HELPERS: &str = r##"module Cs
  module Helpers
    #: (Symbol, untyped) -> void
    def optional_attribute(name, type)
      attribute(name, type, optional: true)
    end
  end
end
"##;

const BASE: &str = r##"module Cs
  class Base
    extend ClassMethods

    #: (Hash[Symbol, untyped]) -> void
    def initialize(attributes)
      @attributes = attributes
    end
  end
end
"##;

const ID: &str = r##"module Cs
  class Id < Base
    attribute :value, String
    optional_attribute :scope, Symbol
  end
end
"##;

const SCRIPT: &str = r##"id = Cs::Id.new({ value: "x", scope: :shop })
p [id.value, id.scope, Cs::Id._internal_attributes, Cs::Base._internal_attributes]
p Cs::Id.public_instance_methods(false).sort
"##;

#[test]
fn an_extended_macro_s_class_eval_defines_the_subclass_reader() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{HELPERS}\n{CLASS_METHODS}\n{BASE}\n{ID}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[\"x\", :shop, {value: String, scope: Symbol}, {}]\n[:scope, :value]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/cs/helpers.rb", HELPERS)
        .write("app/models/cs/class_methods.rb", CLASS_METHODS)
        .write("app/models/cs/base.rb", BASE)
        .write("app/models/cs/id.rb", ID)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// Analysis sees the reader the heredoc defines: `Cs::Id` carries `value`
/// as a method, so a typed caller's `id.value` resolves. (`scope` comes
/// through a wrapper macro calling `attribute`, which the expansion does
/// not follow; it still runs in the Ruby tree.) The Ruby tree writes the replayed call rather than the copies.
#[test]
fn the_heredoc_readers_are_methods_of_the_calling_class_for_analysis() {
    use roundhouse::det::HashMap;
    use std::path::PathBuf;
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("app/models/cs/helpers.rb", HELPERS),
        ("app/models/cs/class_methods.rb", CLASS_METHODS),
        ("app/models/cs/base.rb", BASE),
        ("app/models/cs/id.rb", ID),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let id = app.library_classes.iter().find(|c| c.name.0.as_str() == "Cs::Id").expect("Cs::Id");
    let names: Vec<&str> = id.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"value"), "{names:?}");
    let emitted = roundhouse::emit::ruby::emit_library(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("cs/id.rb"))
        .expect("cs/id.rb")
        .content;
    assert!(emitted.contains("attribute :value, String") && !emitted.contains("def value"), "{emitted}");
}
