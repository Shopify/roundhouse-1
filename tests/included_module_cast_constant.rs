//! An inline type annotation names a class through an included module.
//!
//! Core's `Counters::LiveAggregatedCounter` includes `AggregatedCounter`
//! and writes `@aggregated_counts = {} #: Hash[String, AggregatedData]`.
//! Ruby (and Sorbet) find `AggregatedData` in the class's ancestors after
//! its lexical scopes: it is `Counters::AggregatedCounter::AggregatedData`.
//! The annotation was resolved lexically only and refused as a declared
//! type with no declaration. A name no scope or ancestor declares stays
//! refused.

use roundhouse::det::HashMap;
use std::path::PathBuf;

const COUNTERS: &str = r#"module Counters
  module AggregatedCounter
    class AggregatedData
      #: Integer
      attr_reader :count

      #: (count: Integer) -> void
      def initialize(count:)
        @count = count
      end
    end
  end

  class LiveAggregatedCounter
    include AggregatedCounter

    #: -> void
    def initialize
      @aggregated_counts = {} #: Hash[String, AggregatedData]
      @missing = {} #: Hash[String, NoSuchData]
    end

    #: (String) -> Integer
    def count(key) = @aggregated_counts.fetch(key).count
  end
end
"#;

#[test]
fn an_annotation_names_a_class_of_an_included_module() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/counters.rb", COUNTERS),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("AggregatedData")), "{errors:?}");
    assert!(!errors.iter().any(|e| e.contains("`count`")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("NoSuchData")), "{errors:?}");
}
