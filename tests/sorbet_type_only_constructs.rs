//! The two sorbet constructs left in the emit, which are not the same
//! kind of thing.
//!
//! `NAME = T.type_alias { … }` is a TYPE. It generates no class. A
//! `sig` reads it before sigs are dropped, and a sorbet check
//! (`T.let(x, NAME)`) reads it at runtime, so the Ruby trees carry the
//! type object that check reads (`ingest::sorbet_check::alias_value`)
//! and only where a check reads it, since sorbet-runtime resolves an
//! alias at its first check (`settle_type_aliases`); the other trees
//! drop it with the checks they refuse.
//!
//! `T.absurd(x)` is BEHAVIOR. It sits in the branch a case analysis is
//! supposed to have made unreachable, and it raises there. Dropping it
//! would turn "this cannot happen" into "this returns nil", in the one
//! branch where a wrong answer travels furthest before anything
//! notices, so it lowers to the raise sorbet performs.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn emitted(source: &str, file: &str) -> String {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"gauges\", force: :cascade do |t|\n    t.string \"label\", null: false\n  end\nend\n",
        ),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        ("app/services/gauge.rb", source),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  get \"/gauges\", to: \"gauges#index\"\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_library(&app)
        .into_iter()
        .find(|f| f.path.display().to_string().ends_with(file))
        .map(|f| f.content)
        .unwrap_or_else(|| panic!("{file} is emitted"))
}

#[test]
fn a_type_alias_constant_is_the_type_object_the_check_reads() {
    let emitted = emitted(
        r#"class Gauge
  Reading = T.type_alias { T.any(Integer, Float) }
  Label = T.type_alias { T.nilable(String) }
  LIMIT = 100

  def over?(value)
    T.let(value, Reading) > LIMIT
  end
end
"#,
        "gauge.rb",
    );
    assert!(
        emitted.contains("Reading = SorbetRuntimeCheck.type_alias { SorbetRuntimeCheck.any(Integer, Float) }"),
        "got:\n{emitted}"
    );
    assert!(!emitted.contains("T.type_alias") && !emitted.contains("T.any"), "got:\n{emitted}");
    // Only `sig`s could read `Label`, and the tree drops those, so its
    // block would never run there: it goes with them.
    assert!(!emitted.contains("Label"), "got:\n{emitted}");
    // An ordinary constant in the same body is untouched — the rule is
    // about what the initializer IS, not about where it sits.
    assert!(emitted.contains("LIMIT = 100"), "got:\n{emitted}");
    assert!(emitted.contains("def over?"), "got:\n{emitted}");
}

#[test]
fn a_type_member_goes_with_the_extend_that_enabled_it() {
    // `EventType = type_member { { upper: Event } }` declares a type
    // PARAMETER through a receiverless call `T::Generic` provides. The
    // annotations pass drops that `extend`, correctly, and left this
    // behind — so the emitted class called a method nothing defines
    // and the tree stopped LOADING there, which no amount of checking
    // shows.
    let emitted = emitted(
        r#"class Handler
  extend T::Generic

  EventType = type_member { { upper: Integer } }
  LIMIT = 100

  def run
    LIMIT
  end
end
"#,
        "handler.rb",
    );
    assert!(!emitted.contains("type_member"), "got:\n{emitted}");
    assert!(!emitted.contains("EventType"), "got:\n{emitted}");
    // An ordinary constant beside it is untouched, as ever.
    assert!(emitted.contains("LIMIT = 100"), "got:\n{emitted}");
}

#[test]
fn a_type_alias_in_a_controller_goes_too() {
    // A controller body has its own ingest path, and it carried the
    // alias when the class-body one had stopped — the emitted
    // controller then died at load on a constant initializer.
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"gauges\", force: :cascade do |t|\n    t.string \"label\", null: false\n  end\nend\n",
        ),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        (
            "app/controllers/gauges_controller.rb",
            "class GaugesController < ApplicationController\n  Reading = T.type_alias { T.any(Integer, Float) }\n  LIMIT = 100\n\n  def index\n    render plain: LIMIT.to_s\n  end\nend\n",
        ),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  get \"/gauges\", to: \"gauges#index\"\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let emitted = ruby::emit_lowered_controllers(&app)
        .into_iter()
        .find(|f| f.path.display().to_string().ends_with("gauges_controller.rb"))
        .map(|f| f.content)
        .expect("the controller is emitted");
    assert!(!emitted.contains("type_alias"), "got:\n{emitted}");
    assert!(emitted.contains("LIMIT = 100"), "got:\n{emitted}");
}

#[test]
fn absurd_raises_where_it_stood() {
    let emitted = emitted(
        r#"class Gauge
  def label(kind)
    if kind == :low
      "low"
    else
      T.absurd(kind)
    end
  end
end
"#,
        "gauge.rb",
    );
    // Dropped, this branch would answer nil instead of raising. (The
    // CALL is gone; the name survives inside sorbet's own message.)
    assert!(!emitted.contains("T.absurd(kind)"), "got:\n{emitted}");
    assert!(emitted.contains("raise"), "got:\n{emitted}");
    assert!(emitted.contains("TypeError"), "got:\n{emitted}");
    // sorbet's own message, with the value that got there — a bare
    // "unreachable" says nothing at the point it is reached.
    assert!(
        emitted.contains("Control flow reached T.absurd. Got value: "),
        "got:\n{emitted}"
    );
    assert!(emitted.contains("kind.to_s"), "got:\n{emitted}");
}

#[test]
fn an_unimplemented_assertion_emits_a_named_refusal() {
    // A nil assertion is behavior, so it cannot become an unchecked read.
    // A reason block with parameters has no lowering (a plain one does:
    // `t_must_because_lowers_to_sorbets_nil_check`).
    let emitted = emitted(
        r#"class Gauge
  def label(kind)
    T.must_because(kind) { |x| "a kind" }.to_s
  end
end
"#,
        "gauge.rb",
    );
    assert!(emitted.contains("raise"), "got:\n{emitted}");
    assert!(!emitted.contains("T.must_because"), "got:\n{emitted}");
    assert!(emitted.contains("non-nil assertion"), "got:\n{emitted}");
}

#[test]
fn t_must_because_lowers_to_sorbets_nil_check() {
    let emitted = emitted(
        r#"class Gauge
  def label(kind)
    T.must_because(kind) { "a kind" }.to_s
  end
end
"#,
        "gauge.rb",
    );
    assert!(!emitted.contains("T.must_because"), "got:\n{emitted}");
    assert!(emitted.contains("kind.nil?"), "got:\n{emitted}");
    assert!(emitted.contains("Unexpected `nil` because #{\"a kind\"}"), "got:\n{emitted}");
    assert!(!emitted.contains("non-nil assertion"), "got:\n{emitted}");
}

#[test]
fn t_must_lowers_to_sorbets_nil_check() {
    let emitted = emitted(
        r#"class Gauge
  def label(kind)
    T.must(kind).to_s
  end
end
"#,
        "gauge.rb",
    );
    // The call is gone; the name survives inside sorbet's own message.
    assert!(!emitted.contains("T.must("), "got:\n{emitted}");
    assert!(emitted.contains("kind.nil?"), "got:\n{emitted}");
    assert!(emitted.contains("Passed `nil` into T.must"), "got:\n{emitted}");
    assert!(!emitted.contains("non-nil assertion"), "got:\n{emitted}");
}

#[test]
fn a_type_alias_a_class_body_names_at_load_is_defined() {
    // Core's cart_next.rb: `attribute :pending_artifacts,
    // T::Hash[CheckoutPlatform::GenericArtifactHandle::HandleNameT, ...]`
    // evaluates the alias constant when the class loads, so the tree must
    // define it, as sorbet-runtime's `T.type_alias` does. Dropped, 803 of
    // the unit-1000 ids failed in setup with NameError.
    let source = r#"module Handle
  HandleNameT = T.type_alias { String }
  Label = T.type_alias { T.nilable(String) }

  sig { returns(Label) }
  def self.label; end
end

class Gauge
  Count = T.type_alias { Integer }
  KEY_TYPE = Handle::HandleNameT

  configure do
    requires(:count, Count)
  end
end
"#;
    let handle = emitted(source, "handle.rb");
    assert!(handle.contains("HandleNameT = SorbetRuntimeCheck.type_alias { String }"), "got:\n{handle}");
    // Only a `sig` names `Label`; the tree drops that, so it goes too.
    assert!(!handle.contains("Label ="), "got:\n{handle}");
    let gauge = emitted(source, "gauge.rb");
    assert!(gauge.contains("KEY_TYPE = Handle::HandleNameT"), "got:\n{gauge}");
    // A bare alias a class-body block names (Core's pipeline steps:
    // `interface do requires :x, T::Hash[LocationId, AllocationSet] end`).
    assert!(gauge.contains("Count = SorbetRuntimeCheck.type_alias { Integer }"), "got:\n{gauge}");
}
