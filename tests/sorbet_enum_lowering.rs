//! A `T::Enum` is lowered into the plain Ruby it stands for.
//!
//! `enums do Fill = new("fill") end` is not an annotation: it declares
//! the members other code names and the serialization surface other
//! code calls. The members already emit as the constants they are, so
//! what was left was the base class they call `new` on — and every
//! method that base class provided.
//!
//! The surface here is sorbet's own, read off `T::Enum` rather than
//! guessed. `to_s` is the one that punishes guessing: it delegates to
//! `inspect` and answers `#<Mode::Fill>`, NOT the serialized value.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const ENUM: &str = r#"class Mode < T::Enum
  enums do
    Fill = new("fill")
    Drain = new("drain")
  end
end
"#;

fn emitted(source: &str, file: &str) -> String {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"valves\", force: :cascade do |t|\n    t.string \"mode\", null: false\n  end\nend\n",
        ),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        ("app/services/mode.rb", source),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  get \"/valves\", to: \"valves#index\"\nend\n",
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
fn the_base_class_is_gone_and_its_surface_is_in_the_class() {
    let emitted = emitted(ENUM, "mode.rb");
    // The parent is sorbet-runtime's own `T::Enum` only where that is
    // loaded (`a_tree_with_sorbet_runtime_keeps_the_t_enum_ancestor`);
    // the `enums do` it ran is gone either way.
    assert!(
        emitted.contains("class Mode < (defined?(::T::Enum) ? ::T::Enum : ::Object)"),
        "got:\n{emitted}"
    );
    assert!(!emitted.contains("enums do"), "got:\n{emitted}");
    for method in [
        "def serialize",
        "def self.values",
        "def self.try_deserialize",
        "def self.from_serialized",
        "def self.deserialize",
        "def self.has_serialized?",
        "def inspect",
        "def to_s",
    ] {
        assert!(
            emitted.contains(method),
            "`{method}` was provided by the base class that is now gone; got:\n{emitted}"
        );
    }
    // The members are still the constants they were.
    assert!(emitted.contains("Fill = Mode.new"), "got:\n{emitted}");
    assert!(emitted.contains("Drain = Mode.new"), "got:\n{emitted}");
}

#[test]
fn a_member_is_built_before_it_is_bound_not_after() {
    // `Fill = Mode.new("fill")` RUNS at load time. Emitted above the
    // `initialize` it calls, it is an ArgumentError on every enum in
    // the tree — the whole file fails to load, not just this call.
    let emitted = emitted(ENUM, "mode.rb");
    let initialize = emitted.find("def initialize").expect("the constructor is emitted");
    let member = emitted.find("Fill = Mode.new").expect("the member is emitted");
    assert!(
        initialize < member,
        "the constructor must precede the member that calls it; got:\n{emitted}"
    );
}

#[test]
fn to_s_answers_what_sorbets_to_s_answers() {
    // sorbet's `to_s` delegates to `inspect` — `#<Mode::Fill>`, not
    // "fill". A lowering that answered the serialized value here would
    // change what every interpolation of an enum prints, with nothing
    // at the call site to say so.
    let emitted = emitted(ENUM, "mode.rb");
    assert!(emitted.contains(r##""#<Mode::""##), "got:\n{emitted}");
    // Which is why the member carries the constant name it is bound
    // to: sorbet reads that off the constant table, and here it is
    // known at ingest.
    assert!(emitted.contains(r#"Mode.new("fill", "Fill")"#), "got:\n{emitted}");
}

#[test]
fn a_member_without_a_value_derives_one_from_its_name() {
    // `Desktop = new` is the shorthand: sorbet derives the serialized
    // value from the constant name. Missing this, the name landed in
    // the VALUE's slot and the member was built with one argument
    // where the constructor takes two — an ArgumentError at load
    // time, which takes the whole tree with it.
    //
    // Derived by DOWNCASING and nothing else. `PartiallyCompleted` is
    // "partiallycompleted", not "partially_completed" — checked
    // against the gem rather than assumed, because the underscored
    // form is what one expects.
    let emitted = emitted(
        r#"class Mode < T::Enum
  enums do
    Fill = new
    PartiallyCompleted = new
  end
end
"#,
        "mode.rb",
    );
    // Parenthesised or not is the source's own surface — `= new`
    // carried none — so the arguments are what is asserted.
    assert!(emitted.contains(r#""fill", "Fill""#), "got:\n{emitted}");
    assert!(
        emitted.contains(r#""partiallycompleted", "PartiallyCompleted""#),
        "got:\n{emitted}"
    );
}

#[test]
fn an_explicit_value_is_not_overwritten_by_the_derived_one() {
    // The guard: the shorthand fills a gap, it does not re-decide.
    let emitted = emitted(ENUM, "mode.rb");
    assert!(emitted.contains(r#""fill", "Fill""#), "got:\n{emitted}");
    assert!(!emitted.contains(r#""fill", "fill""#), "got:\n{emitted}");
}

#[test]
fn a_missing_key_is_named_in_the_error() {
    // sorbet's `KeyError` message carries the value that was not
    // found. Without it the raise says only that SOMETHING was not a
    // member, at the one place where knowing which would help.
    let emitted = emitted(ENUM, "mode.rb");
    assert!(
        emitted.contains(r#""Enum Mode key not found: " + value.inspect"#),
        "got:\n{emitted}"
    );
}

#[test]
fn a_constant_that_is_not_a_member_is_left_alone() {
    // An enum may hold ordinary constants. They are not members: they
    // must not reach `values`, and must not be handed a member name.
    let emitted = emitted(
        r#"class Mode < T::Enum
  DEFAULT_LABEL = "unset"

  enums do
    Fill = new("fill")
  end
end
"#,
        "mode.rb",
    );
    assert!(emitted.contains(r#"DEFAULT_LABEL = "unset""#), "got:\n{emitted}");
    let values = emitted
        .lines()
        .skip_while(|l| !l.contains("def self.values"))
        .take_while(|l| !l.trim_start().starts_with("end"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!values.is_empty(), "values is emitted; got:\n{emitted}");
    assert!(
        !values.contains("DEFAULT_LABEL"),
        "a plain constant is not a member; got:\n{values}"
    );
}

/// Core's `sorbet_enum_attribute :issue_type, IssueType` first checks
/// `enum_class.ancestors.include?(T::Enum)` and raised TypeError on
/// every lowered enum (GiftCard::IssueType at load). Where sorbet-runtime
/// is loaded the lowered class keeps the real parent, and the
/// synthesized surface still answers over sorbet's; without it the
/// class loads as the plain Ruby it was.
#[test]
fn a_tree_with_sorbet_runtime_keeps_the_t_enum_ancestor() {
    let emitted = emitted(ENUM, "mode.rb");
    let dir = std::env::temp_dir().join(format!("rh_sorbet_enum_parent_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mode.rb"), &emitted).unwrap();
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let run = |prelude: &str, checks: &str| {
        let out = std::process::Command::new("ruby")
            .env("GEM_HOME", &gem_home)
            .env("GEM_PATH", &gem_home)
            .arg("-e")
            .arg(format!("{prelude}\nload {:?}\n{checks}", dir.join("mode.rb")))
            .output()
            .expect("ruby is on PATH");
        assert!(
            out.status.success(),
            "{prelude}\n{}\n{}\nemitted:\n{emitted}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(
        "require \"sorbet-runtime\"",
        r##"raise "not a T::Enum" unless Mode.ancestors.include?(T::Enum) && Mode::Fill.is_a?(T::Enum)
raise "values" unless Mode.values == [Mode::Fill, Mode::Drain]
raise "deserialize" unless Mode.deserialize("drain").equal?(Mode::Drain) && Mode.try_deserialize("x").nil?
raise "serialize" unless Mode::Fill.serialize == "fill" && Mode.serialize(Mode::Fill) == "fill"
raise "to_s" unless Mode::Fill.to_s == "#<Mode::Fill>"
raise "each_value" unless Mode.each_value.to_a == [Mode::Fill, Mode::Drain]
begin; Mode.from_serialized("x"); raise "no KeyError"; rescue KeyError => e; raise e.message unless e.message.include?('"x"'); end"##,
    );
    run("", r#"raise "parent" unless Mode.superclass == Object && Mode::Fill.serialize == "fill""#);
    std::fs::remove_dir_all(&dir).unwrap();
}
