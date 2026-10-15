//! An `included do` block of a concern runs after the `ClassMethods` of
//! every include above it are extended, as `append_features` leaves the
//! class. Core's `BaseColorRoles` includes `HasDefaultFullAccessConcern`,
//! whose `ClassMethods#field` gives each field a default `access:`, and
//! then `ColorRolesFields`, whose block declares the fields. The emit
//! wrote the first's `extend` after the block's `field` calls, so those
//! fields had no access controls and `AccessCheck.override_field_access_level`
//! raised "undefined method 'add' for nil", which failed the schema build.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "app/models/object_type.rb",
            "class ObjectType\n  def self.field(name, access: nil)\n    (@fields ||= []) << [name, access]\n  end\n\n  def self.fields\n    @fields\n  end\nend\n",
        )
        .write(
            "app/models/default_access.rb",
            "module DefaultAccess\n  extend ActiveSupport::Concern\n\n  module ClassMethods\n    def field(name, **kwargs)\n      kwargs[:access] = :full unless kwargs.key?(:access)\n      super(name, **kwargs)\n    end\n  end\nend\n",
        )
        .write(
            "app/models/color_fields.rb",
            "module ColorFields\n  extend ActiveSupport::Concern\n\n  included do\n    field :background\n    field :text, access: :limited\n  end\nend\n",
        )
        .write(
            "app/models/base_colors.rb",
            "class BaseColors < ObjectType\n  include DefaultAccess\n  include ColorFields\nend\n",
        )
}

#[test]
fn a_concerns_block_runs_after_the_class_methods_of_the_includes_above_it() {
    let run = app().run_ruby("p BaseColors.fields\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[[:background, :full], [:text, :limited]]\n", "{}", run.stderr);
}
