//! A `**rest` beside a required keyword stays a keyword-rest, in the
//! `def` and in its RBS sidecar.

use roundhouse::det::HashMap;
use std::path::{Path, PathBuf};

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::{target_files, BuildTarget};

const APPLICATION_RECORD: &str =
    "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";
const APPLICATION_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";

/// The spinel tree for a small app: the two base classes plus `files`.
#[allow(dead_code)]
fn spinel(files: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    tree.insert(PathBuf::from("app/models/application_record.rb"), APPLICATION_RECORD.as_bytes().to_vec());
    tree.insert(
        PathBuf::from("app/controllers/application_controller.rb"),
        APPLICATION_CONTROLLER.as_bytes().to_vec(),
    );
    for (path, content) in files {
        tree.insert(PathBuf::from(path), content.as_bytes().to_vec());
    }
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    target_files(&app, Path::new("."), BuildTarget::Spinel).expect("spinel files")
}

#[allow(dead_code)]
fn file<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
    &files
        .iter()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("{path} not emitted"))
        .1
}

#[allow(dead_code)]
fn assert_parses(files: &[(String, String)], path: &str) {
    let source = file(files, path);
    let result = ruby_prism::parse(source.as_bytes());
    let errors: Vec<String> = result.errors().map(|e| e.message().to_string()).collect();
    assert!(errors.is_empty(), "{path} does not parse: {errors:?}\n{source}");
}

const TOOL: &str = r#"class BaseTool
  def self.call(server_context:, **arguments)
    { context: server_context, arguments: arguments }
  end
end
"#;

fn files() -> Vec<(String, String)> {
    spinel(&[
        ("app/services/base_tool.rb", TOOL),
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ])
}

#[test]
fn the_def_keeps_the_keyword_rest() {
    let files = files();
    assert_parses(&files, "app/models/base_tool.rb");
    let emitted = file(&files, "app/models/base_tool.rb");
    assert!(emitted.contains("def self.call(server_context:, **arguments)"), "{emitted}");
}

#[test]
fn the_sidecar_declares_it_as_a_keyword_rest() {
    let files = files();
    let rbs = file(&files, "app/models/base_tool.rbs");
    assert!(rbs.contains("**untyped arguments"), "{rbs}");
    assert!(!rbs.contains("?Hash[untyped, untyped] arguments"), "{rbs}");
}
