//! A nested module constant read in a `case/when` clause of an instance
//! method keeps its lexical scope.
//!
//! Core's `Metafields::Lifecycle::Metafield` declares `module Type`
//! (`private_constant :Type`) and its `notify` instance method does
//! `case event when Type::NOTIFY_OWNER_ON_METAFIELD_CHANGE`. The emit
//! rooted the `when` constants at `::Type`: NameError, uninitialized
//! constant Type.
//!
//! The rooting pass looks a bare head up in the namespaces the emit opens
//! around the class (`Metafields`, `Metafields::Lifecycle`), where the
//! sibling `Metafields::Type` is known and `Metafields::Type::NOTIFY_*`
//! is not: it read the head as the compact-form shadow it guards against.
//! The class's own constants are the innermost scope, in the source as in
//! the emit, and bind first.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIFECYCLE_MODEL: &str = r#"module Lifecycle
  module Model
  end
end
"#;

const SIBLING_TYPE: &str = r#"module Metafields
  module Type
    BOOLEAN = "boolean"
  end
end
"#;

const LIFECYCLE: &str = r#"module Metafields
  module Lifecycle
    class Metafield
      include ::Lifecycle::Model

      class InvalidNotifyParameter < StandardError; end

      module Type
        NOTIFY_OWNER_ON_METAFIELD_CHANGE = :notify_owner_on_metafield_change
        NOTIFY_ON_METAFIELD_OWNER_DELETE = :notify_on_metafield_owner_delete
      end
      private_constant :Type

      class << self
        #: (Integer) -> void
        def notify_owner_on_metafield_change(metafield)
          new.notify(Type::NOTIFY_OWNER_ON_METAFIELD_CHANGE, metafield)
        end

        def notify_on_metafield_owner_delete(owner)
          new.notify(Type::NOTIFY_ON_METAFIELD_OWNER_DELETE, owner)
        end
      end

      # @override
      #: (Symbol, untyped) -> void
      def notify(event, object)
        @was_notified = true
        case event
        when Type::NOTIFY_OWNER_ON_METAFIELD_CHANGE
          "changed #{object}"
        when Type::NOTIFY_ON_METAFIELD_OWNER_DELETE
          @owner_delete_manager ||= Object.new #: Object?
          "deleted #{object}"
        else
          raise InvalidNotifyParameter, "unsupported #{event}"
        end
      end
    end
  end
end
"#;

#[test]
fn a_nested_module_constant_in_a_when_clause_stays_scoped() {
    let run = emit_and_run::empty_app()
        .write("app/models/lifecycle/model.rb", LIFECYCLE_MODEL)
        .write("app/models/metafields/type.rb", SIBLING_TYPE)
        .write("app/models/metafields/lifecycle/metafield.rb", LIFECYCLE)
        .run_ruby(
            "m = Metafields::Lifecycle::Metafield\n\
             puts m.notify_owner_on_metafield_change(1)\n\
             puts m.notify_on_metafield_owner_delete(2)\n\
             begin; m.new.notify(:x, 3); rescue m::InvalidNotifyParameter => e; puts e.message; end\n",
        );
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "changed 1\ndeleted 2\nunsupported x", "{}", run.stdout);
}
