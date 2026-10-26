//! The compiler observes the concern-accessor surface on the app it
//! analyzes, instead of the probe analyzing a clone of the whole app inside
//! ingest (`ingest::defer_accessor_surface`). Ingest refuses every name the
//! probe would have read; when the analyzed app occupies all of them, the
//! ingested app and its survey gaps must be exactly the probe's. When it
//! does not, the refusal is overturned and the compiler ingests again with
//! the probe, so the emitted tree is still the probe's.

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::det::HashMap;
use roundhouse::ingest::{defer_accessor_surface, ingest_app_from_tree, survey};

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table :widgets do |t|\n    t.string :title, null: false\n  end\n  create_table :gadgets do |t|\n    t.string :name\n  end\nend\n";

fn concern(names: &str) -> String {
    format!("module Virtual\n  extend ActiveSupport::Concern\n  included {{ attr_accessor {names} }}\nend\n")
}

const JSON_WIDGET: &str = "class Widget < ApplicationRecord\n  include Virtual\n  def as_json(options = {})\n    keys = [:title]\n    json = {}\n    keys.each { |key| json[key] = send(key) }\n    json\n  end\nend\n";

const JSON_CONTROLLER: &str = "class WidgetsController < ApplicationController\n  def show\n    @widget = Widget.find(1)\n    render json: @widget\n  end\nend\n";

fn app(files: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut all = vec![
        ("db/schema.rb".to_string(), SCHEMA.to_string()),
        (
            "app/models/application_record.rb".to_string(),
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string(),
        ),
        ("config/routes.rb".to_string(), "Rails.application.routes.draw { resources :widgets }\n".to_string()),
    ];
    for (path, source) in files {
        all.retain(|(existing, _)| existing != path);
        all.push((path.to_string(), source.to_string()));
    }
    all
}

fn tree(files: &[(String, String)]) -> HashMap<PathBuf, Vec<u8>> {
    files.iter().map(|(path, source)| (PathBuf::from(path), source.clone().into_bytes())).collect()
}

fn probed(files: &[(String, String)]) -> (roundhouse::App, Vec<String>) {
    survey::activate();
    let app = ingest_app_from_tree(tree(files)).expect("survey ingest");
    (app, survey::drain().iter().map(ToString::to_string).collect())
}

/// Whether the deferred observation confirmed ingest's refusals.
fn deferral_matches_probe(files: &[(String, String)]) -> bool {
    let (probe_app, probe_gaps) = probed(files);
    let (again, _) = probed(files);
    assert!(again == probe_app, "two ingests of one tree must compare equal");

    survey::activate();
    let (app, deferred) = defer_accessor_surface(|| ingest_app_from_tree(tree(files)));
    let app = app.expect("deferred survey ingest");
    let gaps: Vec<String> = survey::drain().iter().map(ToString::to_string).collect();
    let deferred = deferred.expect("a name reaches the surface check");
    let mut analyzed = app.clone();
    roundhouse::session::analyze_and_lower(&mut analyzed);
    let confirmed = deferred.confirm(&analyzed);
    if confirmed {
        assert_eq!(gaps, probe_gaps, "confirmed refusals are the probe's");
        assert!(app == probe_app, "a confirmed deferral must ingest the probe's app");
    } else {
        assert!(
            app != probe_app,
            "an overturned refusal must be one the probe decides differently"
        );
    }
    confirmed
}

#[test]
fn a_schema_column_reader_confirms_the_refusal() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":title")),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Virtual\nend\n"),
    ]);
    assert!(deferral_matches_probe(&files));
}

#[test]
fn an_association_reader_confirms_the_refusal() {
    let files = app(&[
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :widgets do |t|\n    t.string :title\n  end\n  create_table :parts do |t|\n    t.integer :widget_id\n  end\nend\n"),
        ("app/models/concerns/virtual.rb", &concern(":parts")),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Virtual\n  has_many :parts\nend\n"),
        ("app/models/part.rb", "class Part < ApplicationRecord\n  belongs_to :widget\nend\n"),
    ]);
    assert!(deferral_matches_probe(&files));
}

#[test]
fn a_post_analysis_json_writer_confirms_the_refusal() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":as_json_str")),
        ("app/models/widget.rb", JSON_WIDGET),
        ("app/controllers/widgets_controller.rb", JSON_CONTROLLER),
    ]);
    assert!(deferral_matches_probe(&files));
}

#[test]
fn an_ancestors_demanded_raw_helper_confirms_the_refusal() {
    let files = app(&[
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :messages do |t|\n    t.string :body\n  end\nend\n"),
        ("app/models/concerns/virtual.rb", &concern(":decorate_raw")),
        ("app/models/message_base.rb", "class MessageBase < ApplicationRecord\n  self.abstract_class = true\n  self.table_name = 'messages'\n  def decorate(value)\n    value\n  end\nend\n"),
        ("app/models/message.rb", "class Message < MessageBase\n  include Virtual\nend\n"),
        ("app/helpers/application_helper.rb", "module ApplicationHelper\n  def decorate(value)\n    value\n  end\nend\n"),
        ("app/views/messages/index.html.erb", "<%= decorate(raw('hello')) %>"),
        ("config/routes.rb", "Rails.application.routes.draw { resources :messages }\n"),
    ]);
    assert!(deferral_matches_probe(&files));
}

#[test]
fn a_fresh_name_overturns_the_refusal() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":scratch")),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Virtual\nend\n"),
    ]);
    assert!(!deferral_matches_probe(&files));
}

#[test]
fn one_fresh_name_among_occupied_ones_overturns_the_refusals() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":title, :scratch")),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Virtual\nend\n"),
        ("app/models/gadget.rb", "class Gadget < ApplicationRecord\n  include Virtual\nend\n"),
    ]);
    assert!(!deferral_matches_probe(&files));
}

/// Without survey mode a refusal is an ingest error, so nothing is deferred.
#[test]
fn strict_ingest_does_not_defer() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":title")),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Virtual\nend\n"),
    ]);
    let (result, deferred) = defer_accessor_surface(|| ingest_app_from_tree(tree(&files)));
    assert!(deferred.is_none());
    let error = result.expect_err("an occupied name is refused");
    assert!(error.to_string().contains("concern attr_accessor :title"), "{error}");
}

fn transpile(root: &Path, files: &[(String, String)], defer: bool) -> (Vec<(PathBuf, Vec<u8>)>, String) {
    let input = root.join("app");
    let out = root.join("out");
    for (path, source) in files {
        let file = input.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby", "--survey", "--allow-unsupported", "-o"])
        .arg(&out)
        .arg(&input)
        .env("ROUNDHOUSE_DEFER_ACCESSOR_SURFACE", if defer { "1" } else { "0" })
        .env("ROUNDHOUSE_TIMINGS", "1")
        .output()
        .unwrap();
    let mut emitted = Vec::new();
    let mut pending = vec![out.clone()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else { continue };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                emitted.push((path.strip_prefix(&out).unwrap().to_path_buf(), std::fs::read(&path).unwrap()));
            }
        }
    }
    emitted.sort();
    let report = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    (emitted, report)
}

/// Every line but the timings, with the scratch directory elided.
fn findings(report: &str, root: &Path) -> Vec<String> {
    report
        .lines()
        .filter(|line| !line.starts_with("roundhouse-timing"))
        .map(|line| line.replace(&root.display().to_string(), "<root>"))
        .collect()
}

fn assert_compiler_matches_probe(name: &str, files: &[(String, String)]) -> String {
    let scratch = std::env::temp_dir().join(format!("rh_deferred_surface_{}_{name}", std::process::id()));
    let (probe_root, deferred_root) = (scratch.join("probe"), scratch.join("deferred"));
    let (probe_tree, probe_report) = transpile(&probe_root, files, false);
    let (tree, report) = transpile(&deferred_root, files, true);
    std::fs::remove_dir_all(&scratch).unwrap();
    assert!(!probe_tree.is_empty(), "{probe_report}");
    assert_eq!(findings(&report, &deferred_root), findings(&probe_report, &probe_root));
    assert!(tree == probe_tree, "the deferred compile must write the probe's tree");
    report
}

#[test]
fn a_confirmed_compile_writes_the_probes_tree_without_probing_in_ingest() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":as_json_str")),
        ("app/models/widget.rb", JSON_WIDGET),
        ("app/controllers/widgets_controller.rb", JSON_CONTROLLER),
    ]);
    let report = assert_compiler_matches_probe("confirmed", &files);
    let phases: Vec<&str> = report
        .lines()
        .filter_map(|line| line.strip_prefix("roundhouse-timing: "))
        .filter(|line| line.starts_with("concern-accessor-surface:") || line.starts_with("ingest:"))
        .collect();
    assert_eq!(phases.len(), 2, "{report}");
    assert!(phases[0].starts_with("ingest:"), "the surface is observed after ingest: {report}");
}

#[test]
fn an_overturned_compile_writes_the_probes_tree() {
    let files = app(&[
        ("app/models/concerns/virtual.rb", &concern(":title, :scratch")),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Virtual\nend\n"),
    ]);
    assert_compiler_matches_probe("overturned", &files);
}
