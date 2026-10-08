//! A constant written in a block passed to a class-body call.
//!
//! Core's `FunctionsGraph::Pricing::DiscountSchema` declares its targets
//! in `targets do |t| CartRunTarget = t.target(…) end`. Ruby puts the
//! constant in the class (the block's cref), as Sorbet reads it; every read,
//! bare in the class or qualified elsewhere, was refused as `constant not
//! supported`, though the emitted body ran the block and defined it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r#"module Fg
  class Schema
    #: () { (Schema) -> void } -> void
    def self.targets(&block) = block.call(new)

    #: (String) -> String
    def target(name) = name
  end
end
"#;

const DISCOUNT: &str = r#"module Fg
  class Discount < Schema
    targets do |t|
      CartRun = t.target("cart.run") #: String
    end

    #: -> String
    def self.cart = CartRun.upcase
  end
end
"#;

const USER: &str = r#"module Fg
  class User
    #: -> String
    def self.cart = Discount::CartRun
  end
end
"#;

#[test]
fn a_class_body_block_constant_types_and_runs() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/fg/schema.rb", SCHEMA)
        .write("app/models/fg/discount.rb", DISCOUNT)
        .write("app/models/fg/user.rb", USER)
        .run_ruby("puts Fg::Discount.cart\nputs Fg::User.cart\n");
    run.assert_passes();
    assert_eq!(run.stdout, "CART.RUN\ncart.run\n");
}

#[test]
fn a_constant_no_block_writes_stays_refused() {
    use roundhouse::det::HashMap;
    use std::path::PathBuf;
    let missing = "module Fg\n  class Other\n    #: -> String\n    def self.cart = Discount::FetchRun\n  end\nend\n";
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/fg/schema.rb", SCHEMA),
        ("app/models/fg/discount.rb", DISCOUNT),
        ("app/models/fg/other.rb", missing),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(errors.iter().any(|e| e.contains("FetchRun")), "{errors:?}");
    assert!(!errors.iter().any(|e| e.contains("CartRun")), "{errors:?}");
}
