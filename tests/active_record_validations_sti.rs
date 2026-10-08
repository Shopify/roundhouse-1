//! `validate` forms and STI subclass validations on an ActiveRecord
//! model, emitted on the SQLite CRuby tree against the same app run
//! natively on Core's pinned activerecord (tests/support/rails_oracle.rb).
//!
//! The model declares `validate :m, on:` / `if:` / `unless:`, a
//! `validate do ... end` block, a one-argument lambda, `validates_with K,
//! on:`, an object (`validate Rules.new`), `prepend: true`, `comparison:`
//! and `except_on:`; `valid?(context)` and
//! `save(context:)` pick the context, and a record validates in `:create`
//! when new and `:update` once saved.
//!
//! An STI subclass is a library class to ingest, and its body's
//! `validates` / `validate` were dropped. Rails runs them for that
//! subclass (and the classes below it) only, after the inherited ones:
//! a sibling and the base do not see them. A subclass's `uniqueness:`
//! asks its own relation, which keeps the STI type condition (the
//! subclass and its descendants), while the base's asks the whole
//! table. A Symbol message with no translation names the subclass's
//! I18n keys, then its parents' up to the base (`lookup_ancestors`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const RANK_VALIDATOR: &str = r##"class RankValidator < ActiveModel::Validator
  def validate(record)
    record.errors.add(:rank, "exceeds #{options[:limit]}") if record.rank.to_i > options[:limit]
  end
end
"##;

const TITLE_RULES: &str = r##"class TitleRules
  def validate(record)
    record.errors.add(:title, "has a tab") if record.title.to_s.include?("\t")
  end
end
"##;

const POST: &str = r##"class Post < ApplicationRecord
  validates :title, presence: true
  validates :title, uniqueness: true, on: :publish
  validate :body_present, on: :publish
  validate :rank_sane, if: :ranked?, unless: -> { title == "skip" }
  validate do
    errors.add(:title, "is shouty") if title.to_s.size > 3 && title.to_s == title.to_s.upcase
  end
  validate ->(post) { post.errors.add(:base, "Ends before it starts") if post.ends && post.starts && post.ends < post.starts }
  validates :ends, comparison: { greater_than_or_equal_to: :starts }, allow_nil: true
  validates_with RankValidator, on: :update, limit: 50
  validates :title, length: { maximum: 10 }, except_on: :draft
  validate TitleRules.new, on: :create
  validate -> { errors.add(:base, "Post first") if title == "first" }, prepend: true

  def ranked?
    !rank.nil?
  end

  private

  def body_present
    errors.add(:body, "is missing") if body.to_s.empty?
  end

  def rank_sane
    errors.add(:rank, "is too big") if rank.to_i > 100
  end
end
"##;

const FEATURED: &str = r##"class FeaturedPost < Post
  validates :body, presence: true
  validates :rank, presence: { message: :needs_rank }
  validates :title, uniqueness: true
  validate :not_draft_title, on: :create
  validate :featured_first, prepend: true
  validate do
    errors.add(:base, "Featured needs a longer body") if body.to_s.size < 5
  end

  private

  def not_draft_title
    errors.add(:title, "cannot be draft") if title == "draft"
  end

  def featured_first
    errors.add(:base, "Featured first") if title == "first"
  end
end
"##;

const PINNED: &str = "class PinnedPost < FeaturedPost\n  validates :rank, numericality: { less_than: 10 }\nend\n";

const NOTE: &str = "class NotePost < Post\nend\n";

const PROBE: &str = r##"class Probe
  def self.line(label, record, context = nil)
    valid = context.nil? ? record.valid? : record.valid?(context)
    "#{label}: #{valid} #{record.errors.full_messages.inspect}"
  end

  def self.lines
    out = []
    out << line("post ok", Post.new(title: "Hello", body: "b"))
    out << line("post blank", Post.new)
    out << line("post shouty publish", Post.new(title: "HELLO"), :publish)
    out << line("post rank big", Post.new(title: "t", rank: 101))
    out << line("post rank big skip", Post.new(title: "skip", rank: 101))
    out << line("post ends early", Post.new(title: "t", starts: 5, ends: 2))
    out << line("post long draft", Post.new(title: "a long title here"), :draft)
    out << line("post long", Post.new(title: "a long title here"))
    out << line("post tab", Post.new(title: "a\tb"))
    out << line("post tab update", Post.new(title: "a\tb"), :update)
    saved = Post.new(title: "Saved", body: "b", rank: 60)
    out << "save create: #{saved.save}"
    out << line("post saved update", saved)
    out << "save publish: #{Post.new(title: "Pub").save(context: :publish)}"
    out << line("featured blank", FeaturedPost.new)
    out << line("post first", Post.new(title: "first", rank: 101))
    out << line("featured first", FeaturedPost.new(title: "first", body: "b", rank: 1))
    out << line("featured draft", FeaturedPost.new(title: "draft", body: "long body", rank: 1))
    out << line("featured draft update", FeaturedPost.new(title: "draft", body: "long body", rank: 1), :update)
    out << line("note", NotePost.new(title: "draft"))
    out << line("pinned", PinnedPost.new(title: "Pin", body: "short", rank: 12))
    Post.create!(title: "Shared", body: "b")
    out << line("post dup publish", Post.new(title: "Shared", body: "b"), :publish)
    out << line("featured dup of post", FeaturedPost.new(title: "Shared", body: "long body", rank: 1))
    FeaturedPost.create!(title: "Star", body: "long body", rank: 1)
    out << line("featured dup", FeaturedPost.new(title: "Star", body: "long body", rank: 2))
    out << line("pinned dup of featured", PinnedPost.new(title: "Star", body: "long body", rank: 2))
    PinnedPost.create!(title: "Pinned", body: "long body", rank: 3)
    out << line("featured dup of pinned", FeaturedPost.new(title: "Pinned", body: "long body", rank: 2))
    out << line("note dup publish", NotePost.new(title: "Star", body: "b"), :publish)
    out << line("note dup", NotePost.new(title: "Star", body: "b"))
    out
  end
end
"##;

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "posts", force: :cascade do |t|
    t.string "title"
    t.string "body"
    t.integer "rank"
    t.integer "starts"
    t.integer "ends"
    t.string "type"
  end
end
"#;

/// In load order, for the native run.
const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/rank_validator.rb", RANK_VALIDATOR),
    ("app/models/title_rules.rb", TITLE_RULES),
    ("app/models/post.rb", POST),
    ("app/models/featured_post.rb", FEATURED),
    ("app/models/pinned_post.rb", PINNED),
    ("app/models/note_post.rb", NOTE),
    ("app/models/probe.rb", PROBE),
];

const OBSERVE: &str = "puts Probe.lines\n";

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app();
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    app.write("db/schema.rb", SCHEMA)
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_active_record_validations_sti_native_{}", std::process::id()));
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
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}{OBSERVE}"
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

/// Lines the native run must print (the observations this test is
/// about), before the emitted run is compared with it whole.
const EXPECT: &[&str] = &[
    "post blank: false [\"Title can't be blank\"]",
    "post shouty publish: false [\"Body is missing\", \"Title is shouty\"]",
    "post rank big: false [\"Rank is too big\"]",
    "post rank big skip: true []",
    "post ends early: false [\"Ends before it starts\", \"Ends must be greater than or equal to 5\"]",
    "post long draft: true []",
    "post long: false [\"Title is too long (maximum is 10 characters)\"]",
    "post tab: false [\"Title has a tab\"]",
    "post tab update: true []",
    "save create: true",
    "post saved update: false [\"Rank exceeds 50\"]",
    "save publish: false",
    "featured draft: false [\"Title cannot be draft\"]",
    "featured draft update: true []",
    "note: true []",
    "post first: false [\"Post first\", \"Rank is too big\"]",
    "featured first: false [\"Featured first\", \"Post first\", \"Featured needs a longer body\"]",
    "post dup publish: false [\"Title has already been taken\"]",
    "featured dup of post: true []",
    "featured dup: false [\"Title has already been taken\"]",
    "pinned dup of featured: false [\"Title has already been taken\"]",
    "featured dup of pinned: false [\"Title has already been taken\"]",
    "note dup publish: false [\"Title has already been taken\"]",
    "note dup: true []",
    "en.activerecord.errors.models.featured_post.attributes.rank.needs_rank",
    "en.activerecord.errors.models.post.attributes.rank.needs_rank",
    "Rank must be less than 10",
];

#[test]
fn validate_forms_and_sti_subclass_validations_run_like_activerecord() {
    let native = native();
    for seen in EXPECT {
        assert!(native.contains(seen), "native run lacks {seen:?}:\n{native}");
    }
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = format!("require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\n{OBSERVE}");
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(dir.join("app/models/featured_post.rb")).unwrap_or_default()
    );
    assert_eq!(stdout, native, "emitted vs native activerecord");
}

/// An STI subclass's unmodeled validator is the validates gap, reported
/// at its declaration, not a dropped statement.
#[test]
fn unmodeled_sti_subclass_validation_is_a_check_error() {
    let app = app().edit(
        "app/models/pinned_post.rb",
        "  validates :rank, numericality: { less_than: 10 }\n",
        "  validates :rank, numericality: { less_than: 10 }\n  validates :rank, comparison: { greater_than: -> { minimum } }\n",
    );
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("`validates comparison:` not supported (all targets): on `rank` in STI subclass `PinnedPost`, `greater_than:` lambda that reads `self`")),
        "{errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("STI subclass body call")), "{errors:#?}");
    assert_eq!(errors.len(), 1, "{errors:#?}");
}
