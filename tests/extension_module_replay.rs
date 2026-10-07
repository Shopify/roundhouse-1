//! A plain class's body calls into an app module it extends.
//!
//! `extend SymbolEnum` followed by `symbol_enum :state, [:on, :off]` in
//! a class with no superclass: no runtime base answers either call, so
//! the app's own module is the receiver's, and the emitted body has to
//! run both in source order. They were dropped as "not modelled", and
//! the class then lacked everything the macro defines.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const REGISTRY: &str = r#"module Registry
  #: (Symbol name) -> Array[Symbol]
  def register(name)
    registered << name
  end

  #: () -> Array[Symbol]
  def registered
    @registered ||= [] #: Array[Symbol]?
  end
end
"#;

const WIDGET: &str = r#"class Widget
  extend Registry

  register :a
  register :b

  #: () -> Array[Symbol]
  def self.report
    registered
  end
end
"#;

#[test]
fn an_extended_app_module_s_macros_run_in_the_class_body() {
    let run = emit_and_run::empty_app()
        .write("app/models/registry.rb", REGISTRY)
        .write("app/models/widget.rb", WIDGET)
        .run_ruby("p Widget.report\n");
    run.assert_passes();
    assert_eq!(run.stdout, "[:a, :b]\n");
}

const LAZY: &str = r#"module LazyAccessor
  def lazy_attr_accessor(*attrs)
    public_send(:attr_writer, *attrs)
    attrs.each do |attr|
      define_method(attr) do
        value = instance_variable_get(:"@#{attr}")
        value.is_a?(Proc) ? instance_variable_set(:"@#{attr}", value.call) : value
      end
    end
  end
end
"#;

const SETTINGS: &str = r#"module Settings
  extend self

  extend LazyAccessor

  attr_accessor :group_id

  self.group_id = 36

  lazy_attr_accessor :store

  self.store = -> { "built" }

  def label
    "group #{group_id}"
  end
end
"#;

/// Core's `Shopify::Config`: a module with `extend self` whose body sets
/// its own accessors (`self.x = v`) and calls a macro of an app module
/// it extends. Ruby runs all of it on the module; dropped, every
/// `Settings.group_id` was a NoMethodError or nil.
#[test]
fn an_extend_self_module_s_body_sets_its_own_accessors() {
    let script = "p Settings.group_id, Settings.store, Settings.label\nSettings.group_id = 7\np Settings.label\n";
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{LAZY}\n{SETTINGS}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "36\n\"built\"\n\"group 36\"\n\"group 7\"\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/lazy_accessor.rb", LAZY)
        .write("app/models/settings.rb", SETTINGS)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
