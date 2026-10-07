//! An include whose `included(base)` hook names a method the class
//! defines above the include.
//!
//! asset_cloud 2.7.3's `AssetExtension` defines `store` and `delete`, then
//! `include AssetCloud::Validations`, whose hook runs
//! `base.send(:alias_method, :store_without_validation, :store)` and
//! prepends a `store` that validates first. The emitted class opened its
//! body with the include, ahead of `def store`, so Core's boot raised
//! NameError (undefined method 'store') in the hook. The include must run
//! after the defs it names, as in the source.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const VALIDATIONS: &str = r#"module Guard
  module Validations
    class << self
      def included(base)
        base.send(:alias_method, :store_without_validation, :store)
        base.prepend(PrependedMethods)
      end
    end

    module PrependedMethods
      def store
        valid? ? "validated #{super}" : "invalid"
      end
    end

    def valid?
      true
    end
  end
end
"#;

const LOGGING: &str = r#"module Guard
  module Logging
    def self.included(base)
      base.alias_method :raw_delete, :delete
    end

    def logged
      "logged #{raw_delete}"
    end
  end
end
"#;

const EXTENSION: &str = r#"module Guard
  class Extension
    class Mismatch < StandardError
    end

    def store
      "stored"
    end

    def delete
      "deleted"
    end

    include Guard::Logging
    include Guard::Validations

    def label
      "extension"
    end
  end
end
"#;

const SCRIPT: &str = r#"e = Guard::Extension.new
p [e.store, e.store_without_validation, e.logged, e.label]
p Guard::Extension.ancestors.take(4)
"#;

#[test]
fn an_included_hook_aliasing_a_method_defined_above_the_include_runs_after_it() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{LOGGING}\n{VALIDATIONS}\n{EXTENSION}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[\"validated stored\", \"stored\", \"logged deleted\", \"extension\"]\n[Guard::Validations::PrependedMethods, Guard::Extension, Guard::Validations, Guard::Logging]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/guard/validations.rb", VALIDATIONS)
        .write("app/models/guard/logging.rb", LOGGING)
        .write("app/models/guard/extension.rb", EXTENSION)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
