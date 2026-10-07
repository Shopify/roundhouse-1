//! A class body calls a class macro its own include's `ClassMethods`
//! gives it, naming methods defined above the call.
//!
//! asset_cloud 2.7.3's `AssetExtension` defines `store` and `delete`,
//! includes the Concern `AssetCloud::Callbacks` and `Validations`, then
//! calls `callback_methods :store, :delete, :validate`, which prepends a
//! module wrapping each named method. The emitted class extended the
//! Concern's `ClassMethods` but dropped the call as "not modelled", so it
//! had no callbacks and no `validate`. The call must run after the
//! include (and after an include whose hook prepends too, so the wrappers
//! stack as in Ruby) and after the defs it wraps.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CALLBACKS: &str = r#"module Kit
  module Callbacks
    extend ActiveSupport::Concern

    module ClassMethods
      def callback_methods(*names)
        names.each do |name|
          wrappers.send(:define_method, name) do |*args|
            "cb(#{super(*args)})"
          end
        end
      end

      private

      def wrappers
        @wrappers ||= begin
          mod = Module.new
          prepend(mod)
          mod
        end
      end
    end
  end
end
"#;

const GUARD: &str = r#"module Kit
  module Guard
    def self.included(base)
      base.extend(ClassMethods)
      base.prepend(Prepended)
    end

    module ClassMethods
      def guarded
        @guarded ||= []
      end

      def guard(*names)
        guarded.concat(names)
      end
    end

    module Prepended
      def store
        "guard(#{super})"
      end
    end
  end
end
"#;

const EXTENSION: &str = r#"module Kit
  class Extension
    def store
      "stored"
    end

    def delete(force = false)
      force ? "deleted!" : "deleted"
    end

    include Kit::Callbacks
    include Kit::Guard

    callback_methods :store, :delete
    guard :store

    def label
      "extension"
    end
  end
end
"#;

const SCRIPT: &str = r#"e = Kit::Extension.new
p [e.store, e.delete, e.delete(true), e.label, Kit::Extension.guarded]
p Kit::Extension.ancestors.take(4).map { |a| a.name || :anonymous }
"#;

#[test]
fn an_included_concern_s_class_macro_wraps_methods_defined_above_it() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"active_support/concern\"\n{CALLBACKS}\n{GUARD}\n{EXTENSION}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[\"cb(guard(stored))\", \"cb(deleted)\", \"cb(deleted!)\", \"extension\", [:store]]\n[:anonymous, \"Kit::Guard::Prepended\", \"Kit::Extension\", \"Kit::Guard\"]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/kit/callbacks.rb", CALLBACKS)
        .write("app/models/kit/guard.rb", GUARD)
        .write("app/models/kit/extension.rb", EXTENSION)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
