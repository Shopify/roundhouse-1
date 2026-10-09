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

/// Core's `CustomerFoundations::Entity::WithIdentifiers` declares
/// `CustomerEntity#id` from its `self.included(base)` hook with
/// `base.attribute(:id, CustomerFoundations::Id)`, and
/// `Customers::CustomerEntity` reaches the module through its superclass's
/// constants, not lexically.
const WITH_IDS: &str = r##"module Cs
  class Base
    module WithIds
      class << self
        #: (singleton(Base)) -> void
        def included(base)
          base.attribute(:id, Integer)
        end
      end
    end
  end
end
"##;

const THING: &str = r##"module Other
  class Thing < Cs::Base
    include(WithIds)
    attribute :name, String
  end
end
"##;

const THING_SCRIPT: &str = r##"t = Other::Thing.new({ id: 7, name: "n" })
p [t.id, t.name, Other::Thing._internal_attributes]
p Other::Thing.public_instance_methods(false).sort
"##;

fn thing_tree() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/models/cs/helpers.rb", HELPERS),
        ("app/models/cs/class_methods.rb", CLASS_METHODS),
        ("app/models/cs/base.rb", BASE),
        ("app/models/cs/base/with_ids.rb", WITH_IDS),
        ("app/models/other/thing.rb", THING),
    ]
}

#[test]
fn an_included_hook_s_macro_call_defines_the_includer_s_reader() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{HELPERS}\n{CLASS_METHODS}\n{BASE}\n{WITH_IDS}\n{THING}\n{THING_SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[7, \"n\", {id: Integer, name: String}]\n[:id, :name]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let mut app = emit_and_run::empty_app();
    for (path, src) in thing_tree() {
        app = app.write(path, src);
    }
    let run = app.run_ruby(THING_SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// Analysis sees the hook's reader on the includer; the Ruby tree leaves
/// it to the include, as it leaves `name` to the replayed call.
#[test]
fn the_included_hook_s_reader_is_a_method_of_the_includer_for_analysis() {
    use roundhouse::det::HashMap;
    use std::path::PathBuf;
    let tree: HashMap<PathBuf, Vec<u8>> =
        thing_tree().into_iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let thing = app.library_classes.iter().find(|c| c.name.0.as_str() == "Other::Thing").expect("Other::Thing");
    let includes: Vec<&str> = thing.includes.iter().map(|m| m.0.as_str()).collect();
    assert_eq!(includes, ["Cs::Base::WithIds"]);
    let names: Vec<&str> = thing.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"id") && names.contains(&"name"), "{names:?}");
    let emitted = roundhouse::emit::ruby::emit_library(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("other/thing.rb"))
        .expect("other/thing.rb")
        .content;
    assert!(!emitted.contains("def id") && !emitted.contains("def name"), "{emitted}");
}

/// Core's `AcceleratedBelongsTo#accelerated_readonly_belongs_to` shape:
/// a module classes `extend` defines a method with a `class_eval`
/// heredoc carrying `__FILE__, __LINE__ + 1`, then aliases it with
/// `alias_method` on implicit self. ApplicationRecord's `extend
/// AcceleratedBelongsTo` makes both run on the model class.
const ACCEL: &str = r##"module Accel
  def accel(name)
    class_eval <<~RUBY, __FILE__, __LINE__ + 1
      def #{name}_x; 1; end
    RUBY
    alias_method :"#{name}_y", :"#{name}_x"
  end
end
"##;

const ACCEL_CLASS: &str = r##"class Accelerated
  extend Accel
  accel :foo
end
"##;

const ACCEL_SCRIPT: &str = r##"p [Accelerated.new.foo_x, Accelerated.new.foo_y, Accelerated.new.foo_y == 1]
p Accelerated.public_instance_methods(false).sort
"##;

fn assert_no_module_protocol_refusal(run: &emit_and_run::Run) {
    let module = std::fs::read_to_string(run.emitted.join("app/models/accel.rb")).expect("emitted accel.rb");
    assert!(!module.contains("Module protocol not supported"), "{module}");
    assert!(module.contains("class_eval") && module.contains("alias_method"), "{module}");
}

#[test]
fn an_extended_module_s_heredoc_and_alias_method_define_the_class_s_methods() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{ACCEL}\n{ACCEL_CLASS}\n{ACCEL_SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[1, 1, true]\n[:foo_x, :foo_y]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/accel.rb", ACCEL)
        .write("app/models/accelerated.rb", ACCEL_CLASS)
        .run_ruby(ACCEL_SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    assert_no_module_protocol_refusal(&run);
}

/// Core's `Denormalize` concern also runs `include AcceleratedBelongsTo`
/// from its `included do` block, so the module is included as well as
/// extended, and ApplicationRecord's instances get the macro as an
/// instance method. The class side still defines the methods; on an
/// instance the `class_eval` raises NoMethodError, in Ruby and in the
/// emitted tree alike. The concern form makes the class the module's
/// sole includer (its methods type with that class's instance as self);
/// the hook form leaves the module its own self.
const DENORM_CONCERN: &str = r##"module Denorm
  extend ActiveSupport::Concern

  included do
    include Accel
  end
end
"##;

const DENORM_HOOK: &str = r##"module Denorm
  def self.included(base)
    base.include(Accel)
  end
end
"##;

const DENORM_CLASS: &str = r##"class Accelerated
  extend Accel
  include Denorm
  accel :foo
end
"##;

const DENORM_SCRIPT: &str = r##"p [Accelerated.new.foo_x, Accelerated.new.foo_y, Accelerated.new.respond_to?(:accel)]
begin
  Accelerated.new.accel(:bar)
rescue NoMethodError => e
  p [e.class, e.name, Accelerated.method_defined?(:bar_x)]
end
"##;

#[test]
fn a_module_also_included_still_runs_as_the_extending_class_s_macro() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{ACCEL}\n{DENORM_HOOK}\n{DENORM_CLASS}\n{DENORM_SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[1, 1, true]\n[NoMethodError, :class_eval, false]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    for denorm in [DENORM_CONCERN, DENORM_HOOK] {
        let run = emit_and_run::empty_app()
            .write("app/models/accel.rb", ACCEL)
            .write("app/models/denorm.rb", denorm)
            .write("app/models/accelerated.rb", DENORM_CLASS)
            .run_ruby(DENORM_SCRIPT);
        run.assert_passes();
        assert_eq!(run.stdout, expected, "{denorm}");
        assert_no_module_protocol_refusal(&run);
    }
}

/// Included only, the module's methods never run with a class as self:
/// both Module-protocol calls stay refused.
#[test]
fn a_module_nothing_extends_keeps_the_refusal() {
    for denorm in [DENORM_CONCERN, DENORM_HOOK] {
        let run = emit_and_run::empty_app()
            .write("app/models/accel.rb", ACCEL)
            .write("app/models/denorm.rb", denorm)
            .write("app/models/accelerated.rb", "class Accelerated\n  include Denorm\nend\n")
            .run_ruby("p Accelerated.new.respond_to?(:accel)");
        for method in ["class_eval", "alias_method"] {
            assert!(
                run.errors.iter().any(|e| e.contains("Module protocol not supported") && e.contains(method)),
                "{method} accepted with {denorm}: {:?}",
                run.errors
            );
        }
    }
}
