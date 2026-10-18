//! A validation reads its attribute through the model's own reader when it
//! writes one (`read_attribute_for_validation` is `send(attr)`). Core's
//! `Taxes::TaxRegionConfiguration` has `def tax_invoice_status; ::Status
//! .try_deserialize(super) || Inactive; end` and `validates
//! :tax_invoice_status, inclusion: { in: Status.values, allow_nil: true }`,
//! values that are enum members; the lowering judged the slot's raw string
//! and `save!` raised "Tax invoice status is not included in the list".

use roundhouse::det::HashMap;
use std::path::PathBuf;

fn validate_of(model: &str) -> &str {
    let from = model.find("  def validate\n").expect("validate emitted");
    let rest = &model[from..];
    &rest[..rest.find("\n  end\n").expect("validate ends") + 1]
}

fn emitted_config() -> String {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/structure.sql",
            "CREATE TABLE configs (\n    id bigint NOT NULL,\n    status character varying,\n    amount integer,\n    name character varying\n);\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        (
            "app/models/config.rb",
            "class Config < ApplicationRecord\n  validates :status, inclusion: { in: [:on, :off], allow_nil: true }\n  validates :name, presence: true\n  validates :amount, numericality: true\n\n  def status\n    raw = super\n    raw && raw.to_sym\n  end\n\n  def amount\n    super.to_i\n  end\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    roundhouse::emit::ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("models/config.rb"))
        .map(|f| f.content)
        .expect("config.rb emitted")
}

#[test]
fn a_reader_the_model_writes_answers_the_validation() {
    let config = emitted_config();
    let validate = validate_of(&config);
    // The reader is called, never the slot. The model's own `status` has no
    // declared signature, so the call stays receiverless (it is not
    // annotated onto `self` as a typed column reader is).
    assert!(validate.contains("[:on, :off].include?(status)"), "{validate}");
    assert!(!validate.contains("include?(@status)"), "{validate}");
    // A column the model leaves alone keeps its slot read, and numericality
    // reads the value before type cast whatever the reader answers.
    assert!(validate.contains("@name.nil?"), "{validate}");
    assert!(!validate.contains("self.amount"), "{validate}");
}
