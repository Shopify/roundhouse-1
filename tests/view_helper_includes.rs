//! `include ActionView::Helpers::*` in an app class.
//!
//! No target ships `ActionView::Helpers`, so the include is dropped and
//! each bare call to a member the runtime ports is qualified to
//! `ActionView::ViewHelpers.<name>`. The include used to be dropped
//! unconditionally: a call to any other member emitted bare, checked
//! clean, and raised NoMethodError when it ran. Now the include is
//! dropped only when every member its class (its `class << self`, its
//! subclasses, its includers) calls through it is answered, and refused
//! with a diagnostic otherwise.
//!
//! The oracle is the pinned Rails (8.2.0.alpha, tests/support/
//! rails_oracle.rb): the same classes under native actionview.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

/// Runs `program` (a file: the oracle re-executes it under jemalloc).
fn native(name: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rh_view_helper_includes_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rb"), format!("require {ORACLE:?}\n{program}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native {name} failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

/// The helper-member table is the pinned actionview's, regenerated.
#[test]
fn actionview_member_table_matches_the_pinned_rails() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/actionview_helper_members.rb");
    let out = Command::new("ruby").arg(script).output().expect("ruby is on PATH");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let table = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/view_helpers/actionview_members.txt"))
        .expect("member table");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        table,
        "src/view_helpers/actionview_members.txt is stale: regenerate it with tests/support/actionview_helper_members.rb"
    );
}

const STAMP: &str = r##"module Cs
  class Stamp
    include ActionView::Helpers::DateHelper

    def initialize(at)
      @at = at
    end

    #: -> String
    def lifetime = distance_of_time_in_words_to_now(@at, include_seconds: true)

    #: -> String
    def ago = time_ago_in_words(@at)

    #: (Time) -> String
    def until_then(later) = distance_of_time_in_words(@at, later)

    # Its own method, ahead of the helper's in Ruby's lookup: no helper
    # call, nothing to refuse.
    #: (Integer) -> String
    def select_year(year) = "Y#{year}"

    #: -> String
    def year_label = select_year(@at.year)
  end
end
"##;

const LONG_STAMP: &str = r##"module Cs
  class LongStamp < Stamp
    #: -> String
    def both = "#{ago} / #{distance_of_time_in_words(@at, @at + 40 * 86400)}"
  end
end
"##;

const CONTACT: &str = r##"module Cs
  module Contact
    class << self
      include ActionView::Helpers::DateHelper

      #: (Time) -> String
      def label(at) = distance_of_time_in_words_to_now(at)
    end
  end
end
"##;

const SPAN_WORDS: &str = r##"module Cs
  module SpanWords
    include ActionView::Helpers::DateHelper

    #: -> String
    def span = distance_of_time_in_words(@from, @to)
  end
end
"##;

const RANGE: &str = r##"module Cs
  class Range
    include SpanWords

    def initialize(from, to)
      @from = from
      @to = to
    end
  end
end
"##;

const ARTICLE_AGE: &str = "  include ActionView::Helpers::DateHelper\n\n  def age_at(now) = distance_of_time_in_words(created_at, now)\n";

const SCRIPT: &str = r##"now = Time.now
p Cs::Stamp.new(now - 15).lifetime
p Cs::Stamp.new(now - 3 * 3600).ago
p Cs::Stamp.new(Time.at(0)).until_then(Time.at(400 * 86400))
p Cs::Stamp.new(Time.utc(2020, 6, 1)).year_label
p Cs::LongStamp.new(now - 50 * 60).both
p Cs::Contact.label(now - 2 * 86400)
p Cs::Range.new(Time.at(0), Time.at(3 * 365 * 86400 + 200 * 86400)).span
"##;

const ARTICLE_SCRIPT: &str = r##"a = Article.create!(title: "Dates", body: "a body long enough")
p a.age_at(a.created_at + 95 * 60)
"##;

#[test]
fn ported_date_helpers_through_an_include_match_native_actionview() {
    let expected = native(
        "ported",
        &format!(
            "RailsOracle.activate(\"activesupport\", \"actionview\", \"actionpack\", \"activemodel\", \"activerecord\")\n\
             require \"action_view\"\nrequire \"active_record\"\n\
             ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
             ActiveRecord::Schema.verbose = false\n\
             ActiveRecord::Schema.define {{ create_table(:articles) {{ |t| t.string :title; t.text :body; t.timestamps }} }}\n\
             class Article < ActiveRecord::Base\n{ARTICLE_AGE}end\n\
             {STAMP}{LONG_STAMP}{CONTACT}{SPAN_WORDS}{RANGE}{SCRIPT}{ARTICLE_SCRIPT}"
        ),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#""less than 20 seconds""#,
            r#""about 3 hours""#,
            r#""about 1 year""#,
            r#""Y2020""#,
            r#""about 1 hour / about 1 month""#,
            r#""2 days""#,
            r#""over 3 years""#,
            r#""about 2 hours""#,
        ],
        "native control"
    );
    let run = emit_and_run::real_blog()
        .write("app/lib/cs/stamp.rb", STAMP)
        .write("app/lib/cs/long_stamp.rb", LONG_STAMP)
        .write("app/lib/cs/contact.rb", CONTACT)
        .write("app/lib/cs/span_words.rb", SPAN_WORDS)
        .write("app/lib/cs/range.rb", RANGE)
        .edit("app/models/article.rb", "  validates :title, presence: true\n", &format!("  validates :title, presence: true\n{ARTICLE_AGE}"))
        .run_ruby(&format!("{SCRIPT}{ARTICLE_SCRIPT}"));
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from native actionview line for line");
}

/// Members the runtime does not port: the include is refused, naming
/// each, wherever the call sits — the class, its `class << self`, a
/// subclass, an includer of a module, a model.
#[test]
fn an_unported_helper_call_refuses_the_include() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/cs/stamp.rb", STAMP)
        .write(
            "app/lib/cs/tagged_stamp.rb",
            "module Cs\n  class TaggedStamp < Stamp\n    #: -> String\n    def tagged = time_tag(@at)\n  end\nend\n",
        )
        .write("app/lib/cs/contact.rb", &CONTACT.replace("distance_of_time_in_words_to_now(at)", "relative_time_in_words(at)"))
        .write("app/lib/cs/span_words.rb", SPAN_WORDS)
        .write("app/lib/cs/range.rb", &RANGE.replace("    def initialize", "    #: -> String\n    def year = select_year(@from)\n\n    def initialize"))
        .write(
            "app/lib/cs/translated.rb",
            "module Cs\n  class Translated\n    include ActionView::Helpers::TranslationHelper\n\n    #: -> String\n    def title = t(\"cs.title\")\n  end\nend\n",
        )
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true\n",
            "  validates :title, presence: true\n  include ActionView::Helpers::TextHelper\n\n  def share = highlight(\"share\", \"s\")\n",
        )
        .emit(BuildTarget::Ruby);
    for (file, expected) in [
        ("cs/stamp.rb", "Cs::Stamp includes ActionView::Helpers::DateHelper and calls `time_tag` (in Cs::TaggedStamp) through it"),
        ("cs/contact.rb", "Cs::Contact extends ActionView::Helpers::DateHelper and calls `relative_time_in_words` through it"),
        ("cs/span_words.rb", "Cs::SpanWords includes ActionView::Helpers::DateHelper and calls `select_year` (in Cs::Range) through it"),
        ("cs/translated.rb", "Cs::Translated includes ActionView::Helpers::TranslationHelper and calls `t` through it"),
        ("models/article.rb", "Article includes ActionView::Helpers::TextHelper and calls `highlight` through it"),
    ] {
        assert!(
            errors.iter().any(|e| e.contains(expected) && e.contains("does not port")),
            "no refusal at {file}: {expected:?} in\n{}",
            errors.join("\n")
        );
    }
    assert_eq!(errors.len(), 5, "one refusal per include, nothing else:\n{}", errors.join("\n"));
}

const CONTEXT_MANAGER: &str = r##"module Cs
  class Manager
    include ActionView::Helpers::TagHelper
    include ActionView::Context

    #: -> String
    def label = "manager"
  end
end
"##;

/// `include ActionView::Context` beside `TagHelper` (Core's AppManager)
/// is a marker, not an `uninitialized constant ActionView::Context` at
/// class-definition time that takes the whole boot with it.
#[test]
fn action_view_context_include_is_dropped_when_no_member_is_called() {
    let expected = native(
        "context",
        &format!(
            "RailsOracle.activate(\"activesupport\", \"actionview\", \"actionpack\", \"activemodel\")\n\
             require \"action_view\"\n{CONTEXT_MANAGER}p Cs::Manager.new.label\np Cs::Manager.include?(ActionView::Context)\n"
        ),
    );
    assert_eq!(expected.lines().collect::<Vec<_>>(), [r#""manager""#, "true"], "native control");
    let run = emit_and_run::real_blog()
        .write("app/lib/cs/manager.rb", CONTEXT_MANAGER)
        .run_ruby("p Cs::Manager.new.label\n");
    run.assert_passes();
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), [r#""manager""#]);
}

/// A member of `ActionView::Context` the runtime does not answer
/// (`output_buffer`) is refused by name rather than emitted bare.
#[test]
fn an_unanswered_action_view_context_member_refuses_the_include() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .write(
            "app/lib/cs/manager.rb",
            &CONTEXT_MANAGER.replace("def label = \"manager\"", "def label = output_buffer.to_s"),
        )
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("Cs::Manager includes ActionView::Context and calls `output_buffer` through it") && e.contains("does not port")),
        "no refusal in\n{}",
        errors.join("\n")
    );
}
