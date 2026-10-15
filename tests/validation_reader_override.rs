//! A validation reads its attribute through the reader (`send(attr)`), so
//! a module the model includes that redefines the reader answers it: Core's
//! `Shop::Extension#domain` is `super || permanent_domain`, and
//! `Shop.create!(permanent_domain: ...)` saves in Rails. The lowering read
//! the slot (`@domain`) and rejected it with "Domain can't be blank".

use roundhouse::det::HashMap;
use std::path::PathBuf;

fn validate_of(shop: &str) -> &str {
    let from = shop.find("  def validate\n").expect("validate emitted");
    let rest = &shop[from..];
    &rest[..rest.find("\n  end\n").expect("validate ends") + 1]
}

fn emitted_shop(extension: &str) -> String {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/structure.sql",
            "CREATE TABLE shops (\n    id bigint NOT NULL,\n    domain character varying,\n    permanent_domain character varying,\n    name character varying\n);\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/shop/extension.rb", extension),
        (
            "app/models/shop.rb",
            "class Shop < ApplicationRecord\n  include Shop::Extension\n  validates :name, :domain, presence: true\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    roundhouse::emit::ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("models/shop.rb"))
        .map(|f| f.content)
        .expect("shop.rb emitted")
}

const OVERRIDING: &str = "class Shop\n  module Extension\n    def domain\n      super || permanent_domain\n    end\n  end\nend\n";
const PLAIN: &str = "class Shop\n  module Extension\n    def label\n      name\n    end\n  end\nend\n";

#[test]
fn an_included_module_that_redefines_the_reader_answers_the_validation() {
    let shop = emitted_shop(OVERRIDING);
    let validate = validate_of(&shop);
    assert!(validate.contains("Domain can't be blank"), "{validate}");
    assert!(!validate.contains("@domain"), "{validate}");
    assert!(validate.contains("self.domain.nil?"), "{validate}");
    // The name the module leaves alone keeps its slot read.
    assert!(validate.contains("@name.nil?"), "{validate}");
}

#[test]
fn a_module_that_leaves_the_readers_alone_keeps_the_slot_reads() {
    let shop = emitted_shop(PLAIN);
    let validate = validate_of(&shop);
    assert!(validate.contains("@domain.nil?"), "{validate}");
}
