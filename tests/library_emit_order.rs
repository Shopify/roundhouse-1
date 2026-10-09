//! The library emit writes each class's body in the order native load
//! order runs it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

const BASE: &str = "class Base\n  def self.field(name, type)\n    (@fields ||= []) << [name, type]\n  end\n\n  def self.fields\n    @fields\n  end\nend\n";

/// GraphApiTestSchemaHelper::UpdateProduct::Input: a class nested two
/// levels below its file's owner, written above the statement of its
/// enclosing class that reads it (`field :variants, [VariantInput]`).
/// Emitted as a block of its own after the enclosing class's, the read
/// raised NameError as the file loaded.
#[test]
fn a_class_nested_in_a_nested_class_is_defined_before_its_enclosing_body_reads_it() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("lib/base.rb", BASE)
        .write(
            "lib/helper.rb",
            "module Helper\n  class Update < Base\n    class Input < Base\n      class VariantInput < Base\n        field :options, :string\n      end\n\n      field :name, :string\n      field :variants, [VariantInput]\n    end\n\n    field :input, Input\n  end\nend\n",
        )
        .run_ruby("p Helper::Update.fields\np Helper::Update::Input.fields\np Helper::Update::Input::VariantInput.fields\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "[[:input, Helper::Update::Input]]\n[[:name, :string], [:variants, [Helper::Update::Input::VariantInput]]]\n[[:options, :string]]\n",
        "{}",
        run.stderr
    );
}
