//! `private_constant` in a model body (Core: 79 statements).
//!
//! Ruby keeps every lexical read of the constant (the class's methods,
//! scopes, validations) and `const_get`, and raises NameError on a
//! scoped read (`Widget::LIMIT`) anywhere. The lowered program reads a
//! model's constants through the class, so the marker is not replayed;
//! the scoped reads Ruby refuses are check errors instead
//! (`lower::model_to_library::private_constant`). The allowed reads are
//! pinned against Rails on Core's pinned activerecord, and the emitted
//! program must answer the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const WIDGET: &str = r#"class Widget < ApplicationRecord
  LIMIT = 3
  private_constant :LIMIT
  PAIR = [1, 2].freeze
  NAMES = %w[a b].freeze
  private_constant :PAIR, "NAMES"

  class Helper
    def self.go = "helped"
  end
  private_constant :Helper

  scope :big, -> { where("id > ?", LIMIT) }
  validates :name, length: { maximum: LIMIT }

  def limit = LIMIT
  def point = PAIR.last
  def names = NAMES
  def self.helped = Helper.go
end
"#;

const PROBE: &str = r#"class Probe
  def self.lines
    w = Widget.new(name: "abcd")
    out = ["reads: #{w.limit} #{w.point.inspect} #{w.names.inspect} #{Widget.helped}"]
    out << "valid: #{w.valid?} #{w.errors.full_messages.inspect}"
    4.times { |i| Widget.create!(name: "w#{i}") }
    out << "big: #{Widget.big.count}"
    out << "const_get: #{Widget.const_get(:LIMIT)}"
    out
  end
end
"#;

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  create_table :widgets do |t|\n    t.string :name\n  end\nend\n";

const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/widget.rb", WIDGET),
    ("app/models/probe.rb", PROBE),
];

const EXPECTED: &str = "reads: 3 2 [\"a\", \"b\"] helped
valid: false [\"Name is too long (maximum is 3 characters)\"]
big: 1
const_get: 3
";

fn app() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_model_private_constant_native_{}", std::process::id()));
    for (path, text) in FILES.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}puts Probe.lines\n\
         begin\n  Widget::LIMIT\nrescue NameError => e\n  puts \"scoped: #{{e.message}}\"\nend\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

/// Rails' answers, and the NameError a scoped read raises.
#[test]
fn rails_keeps_lexical_reads_of_a_private_model_constant() {
    assert_eq!(native(), format!("{EXPECTED}scoped: private constant Widget::LIMIT referenced\n"));
}

#[test]
fn the_emitted_model_answers_the_lexical_reads() {
    let run = app().run_ruby("puts Probe.lines");
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}

/// A scoped read Ruby refuses, and a marker naming nothing, are errors.
#[test]
fn a_scoped_read_and_an_undefined_marker_are_check_errors() {
    let app = app()
        .write("app/models/reader.rb", "class Reader\n  def self.read = Widget::LIMIT\n  def self.ok = Widget.helped\nend\n")
        .write(
            "app/models/gadget.rb",
            "class Gadget < ApplicationRecord\n  self.table_name = \"widgets\"\n  private_constant :MISSING\nend\n",
        );
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert!(errors.iter().any(|e| e.contains("Widget::LIMIT not supported") && e.contains("raises NameError")), "{errors:#?}");
    assert!(errors.iter().any(|e| e.contains("on `Gadget`: `MISSING` not defined")), "{errors:#?}");
}
