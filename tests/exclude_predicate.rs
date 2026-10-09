//! `lower::exclude_predicate`: ActiveSupport's `exclude?` grounds to
//! `!recv.include?(arg)` while no app class defines its own `exclude?`,
//! and stands down (the send stays as written, for the runtime's
//! core_ext to answer) once one does. Shopify Core is the second case:
//! its `[...].exclude?(..)` sites reach the CRuby overlay's
//! `Enumerable#exclude?` (tests/core_ext_surface_runtime.rs).

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn emit(extra: &[(&str, &str)]) -> String {
    let mut files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        (
            "app/models/pod_config.rb",
            "class PodConfig\n  def self.missing(names, known)\n    \
             names.select { |n| known.exclude?(n) }\n  end\n\n  \
             def self.word(text)\n    text.exclude?(\"x\")\n  end\nend\n",
        ),
    ];
    files.extend_from_slice(extra);
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .into_iter()
        .chain(ruby::emit_library(&app))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn exclude_grounds_to_negated_include_when_the_app_does_not_define_it() {
    let src = emit(&[]);
    assert!(src.contains("!(known.include?(n))"), "{src}");
    assert!(src.contains("!(text.include?(\"x\"))"), "{src}");
    assert!(!src.contains(".exclude?("), "{src}");
}

#[test]
fn an_app_defined_exclude_leaves_every_site_for_the_runtime() {
    let src = emit(&[(
        "app/models/feature_set.rb",
        "class FeatureSet\n  def initialize(names)\n    @names = names\n  end\n\n  \
         def exclude?(name)\n    !@names.include?(name)\n  end\nend\n",
    )]);
    assert!(src.contains("known.exclude?(n)"), "{src}");
    assert!(src.contains("text.exclude?(\"x\")"), "{src}");
    assert!(!src.contains("known.include?(n)"), "{src}");
}
