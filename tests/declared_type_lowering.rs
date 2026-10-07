//! A source type ascription (`x = value #: as T`, `#: T` on an ivar
//! write) is checked against the analyzer's registry, the one
//! `roundhouse check` reports from. The model lowerer later retypes the
//! model bodies, the class methods a concern's `class_methods` block
//! gives its includer among them, in a registry of the lowered models
//! and runtime stubs. That registry names neither `ActiveRecord::Base` nor
//! `Module`, so Shopify Core's `ApplicationRecord` came out with
//! `klass = (raise "roundhouse: declared type not supported")` for
//! `klass = self #: as singleton(ActiveRecord::Base)` (GenerateOid) and
//! `@generated_model_uuid_literal_methods ||= (raise ...)` for
//! `Module.new.tap { ... } #: Module[top]?` (ModelUuidColumns), the latter on
//! every model's class body. A declaration the analyzer resolved is not
//! re-decided there; one it could not resolve still raises.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const CONCERN: &str = r#"module OidGen
  extend ActiveSupport::Concern

  class_methods do
    def only_bulk!
      klass = self #: as singleton(ActiveRecord::Base)
      klass.name
    end

    def literal_methods
      @literal_methods ||= Module.new.tap { |mod| include(mod) } #: Module[top]?
    end

    def missing
      @missing = nil #: Missing::Thing?
    end
  end
end
"#;

#[test]
fn a_declaration_the_analyzer_resolved_is_not_re_decided_by_the_model_lowerer() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("app/models/concerns/oid_gen.rb", CONCERN)
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  include OidGen\n  self.abstract_class = true\nend\n",
        )
        .emit(BuildTarget::Ruby);
    let record = std::fs::read_to_string(dir.join("app/models/application_record.rb")).expect("application_record.rb");
    assert!(record.contains("    klass = self\n"), "{record}");
    assert!(record.contains("@literal_methods ||= Module.new.tap { |mod| include(mod) }"), "{record}");
    // Not relaxed: the type no registry declares is still reported
    // and still raises where the value would be.
    assert!(record.contains("@missing = (raise \"roundhouse: declared type not supported"), "{record}");
    assert_eq!(record.matches("declared type not supported").count(), 1, "{record}");
    assert!(errors.iter().any(|e| e.contains("Missing::Thing has no indexed declaration")), "{errors:#?}");
    assert!(!errors.iter().any(|e| e.contains("ActiveRecord::Base has no indexed") || e.contains("Module has no indexed")), "{errors:#?}");
}
