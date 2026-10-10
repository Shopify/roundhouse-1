//! Classes, modules and value types nested in an ActiveRecord model's
//! body, read while the model's own body loads.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"status\", null: false\n  end\nend\n";
const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

/// The nested definitions sit above the class-body calls that read them
/// (`validates ... in: Status::ALL`, a constant built from one, a
/// `Struct`/`Data` value, a nested class inheriting from another).
const MODEL: &str = r#"class Widget < ApplicationRecord
  class Status
    ACTIVE = "active"
    PAUSED = "paused"
    ALL = [ACTIVE, PAUSED].freeze
  end

  module Tagging
    TAG = "t"
    def self.tag = TAG
  end

  class Base
    def kind = :base
  end

  class Special < Base
    def kind = :special
  end

  Point = Struct.new(:x, :y)
  Pair = Data.define(:left, :right)

  DEFAULT_STATUS = Status::ACTIVE
  STATUSES = Status::ALL
  ORIGIN = Point.new(0, 0)

  validates :status, inclusion: { in: Status::ALL }

  def self.describe = [Status::ACTIVE, Tagging.tag, Special.new.kind, ORIGIN.to_a, Pair.new(1, 2).right]
end
"#;

fn probe(script: &str) -> emit_and_run::Run {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/widget.rb", MODEL)
        .run_ruby(script)
}

/// Ruby evaluates a model body top to bottom, so a class, module or
/// value type written above a call or constant that reads it is defined
/// when that statement runs, and stays reachable as `Model::Name`. The
/// emitted model requires each nested file ahead of the first statement
/// that reads it.
#[test]
fn nested_definitions_exist_when_the_model_body_reads_them() {
    let run = probe(
        "p Widget.describe, Widget::STATUSES, Widget::DEFAULT_STATUS, Widget::Special.superclass, Widget::Status::ALL.frozen?\n\
         p Widget.new(status: \"paused\").valid?, Widget.new(status: \"nope\").valid?\n",
    );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "[\"active\", \"t\", :special, [0, 0], 2]\n[\"active\", \"paused\"]\n\"active\"\nWidget::Base\ntrue\ntrue\nfalse\n",
        "{}",
        run.stderr
    );
}

/// A block that runs while the body loads (a `state_machine` or `with_options`
/// style DSL) names the nested class by its full path.
#[test]
fn a_dsl_block_in_the_model_body_reads_a_nested_class() {
    let model = r#"class Widget < ApplicationRecord
  class Status
    ACTIVE = "active"
    ALL = [ACTIVE].freeze
  end

  SEEN = []

  def self.each_state
    yield Widget::Status::ACTIVE
  end

  each_state do |state|
    SEEN << [state, Status::ALL]
  end
end
"#;
    let run = emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/widget.rb", model)
        .run_ruby("p Widget::SEEN\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[[\"active\", [\"active\"]]]\n", "{}", run.stderr);
}
