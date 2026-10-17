//! `validates :batch_id, presence: true` on `attribute :batch_id, :suid`: the
//! slot holds the declared type's value (a `Shopify::Uuid`), which has no
//! `empty?`, so the presence check asks only a String or an Array for it as
//! Rails' `blank?` does. The String column's check called `empty?` on every
//! value (`undefined method 'empty?' for an instance of Shopify::Uuid`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SUID: &str = "class SuidType < ActiveRecord::Type::Value\n  def cast(value)\n    value\n  end\nend\nActiveRecord::Type.register(:suid, SuidType)\n";

const MODEL: &str = "class Operation < ApplicationRecord\n  attribute :batch_id, :suid\n  validates :batch_id, :label, presence: true\nend\n";

#[test]
fn a_retypable_column_presence_check_does_not_assume_a_string() {
    let (emitted, _errors) = emit_and_run::real_blog()
        .write("app/models/suid_type.rb", SUID)
        .write("app/models/operation.rb", MODEL)
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema[8.0].define(version: 1) do\n  create_table \"operations\", force: :cascade do |t|\n    t.string \"batch_id\"\n    t.string \"label\"\n  end\nend\n",
        )
        .emit(BuildTarget::Ruby);
    let model = std::fs::read_to_string(emitted.join("app/models/operation.rb")).expect("operation.rb");
    assert!(model.contains("@batch_id.nil? || @batch_id.is_a?(String) && (@batch_id || \"\").empty?"), "{model}");
    assert!(model.contains("@label.nil? || (@label || \"\").empty?"), "{model}");
}
