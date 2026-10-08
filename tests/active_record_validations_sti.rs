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
//! Each line also reads `errors.attribute_names`: every check, block,
//! object and comparison records the attribute it adds to (`:base` for
//! `errors.add(:base, ...)`), and `errors[:title]`.
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
    "#{label}: #{valid} #{record.errors.full_messages.inspect} #{record.errors.attribute_names.inspect} #{record.errors[:title].inspect}"
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
    app_of(FILES, SCHEMA)
}

fn app_of(files: &[(&str, &str)], schema: &str) -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app();
    for (path, text) in files {
        app = app.write(path, text);
    }
    app.write("db/schema.rb", schema)
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

fn native() -> String {
    native_of("sti", FILES, SCHEMA, OBSERVE)
}

fn native_of(label: &str, files: &[(&str, &str)], schema: &str, observe: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir()
        .join(format!("rh_active_record_validations_{label}_native_{}", std::process::id()));
    for (path, text) in files.iter().chain([&("db/schema.rb", schema)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = files.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}{observe}"
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
    "post blank: false [\"Title can't be blank\"] [:title]",
    "post shouty publish: false [\"Body is missing\", \"Title is shouty\"] [:body, :title] [\"is shouty\"]",
    "post rank big: false [\"Rank is too big\"] [:rank] []",
    "post rank big skip: true [] []",
    "post ends early: false [\"Ends before it starts\", \"Ends must be greater than or equal to 5\"] [:base, :ends]",
    "post long draft: true [] []",
    "post long: false [\"Title is too long (maximum is 10 characters)\"] [:title]",
    "post tab: false [\"Title has a tab\"] [:title]",
    "post tab update: true [] []",
    "save create: true",
    "post saved update: false [\"Rank exceeds 50\"] [:rank]",
    "save publish: false",
    "featured draft: false [\"Title cannot be draft\"] [:title] [\"cannot be draft\"]",
    "featured draft update: true [] []",
    "note: true [] []",
    "post first: false [\"Post first\", \"Rank is too big\"] [:base, :rank]",
    "featured first: false [\"Featured first\", \"Post first\", \"Featured needs a longer body\"] [:base]",
    "post dup publish: false [\"Title has already been taken\"] [:title]",
    "featured dup of post: true [] []",
    "featured dup: false [\"Title has already been taken\"] [:title]",
    "pinned dup of featured: false [\"Title has already been taken\"] [:title] [\"has already been taken\"]",
    "featured dup of pinned: false [\"Title has already been taken\"] [:title]",
    "note dup publish: false [\"Title has already been taken\"] [:title]",
    "note dup: true [] []",
    "en.activerecord.errors.models.featured_post.attributes.rank.needs_rank",
    "en.activerecord.errors.models.post.attributes.rank.needs_rank",
    "Rank must be less than 10",
    "featured blank: false [\"Title can't be blank\", \"Body can't be blank\", \"Rank Translation missing.",
    "\"Featured needs a longer body\"] [:title, :body, :rank, :base]",
    "pinned: false [\"Rank must be less than 10\"] [:rank]",
];

#[test]
fn validate_forms_and_sti_subclass_validations_run_like_activerecord() {
    compare(app(), native(), EXPECT, "app/models/featured_post.rb", OBSERVE);
}

fn compare(app: emit_and_run::Overlay, native: String, expect: &[&str], show: &str, observe: &str) {
    for seen in expect {
        assert!(native.contains(seen), "native run lacks {seen:?}:\n{native}");
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = format!("require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\n{observe}");
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
        std::fs::read_to_string(dir.join(show)).unwrap_or_default()
    );
    assert_eq!(stdout, native, "emitted vs native activerecord");
}

const ACCOUNT: &str = r##"class Account < ApplicationRecord
  def self.max_rank
    10
  end

  def self.kinds
    %w[a b]
  end

  validates :terms, acceptance: true
  validates :agreed, acceptance: { accept: "yes" }
  validates :name, presence: true, strict: true
  validates :rank, comparison: { less_than: -> { max_rank } }, allow_nil: true
  validates :kind, inclusion: { in: -> { kinds } }, allow_nil: true
end
"##;

const PREMIUM: &str = r##"class PremiumError < StandardError
end

class Premium < Account
  def self.levels
    %w[gold silver]
  end

  validates :cira, acceptance: true
  validates :code, presence: true, strict: PremiumError
  validates! :level, inclusion: { in: levels }, allow_nil: true
  validates :rank, numericality: { greater_than: -> { min_rank } }, allow_nil: true

  def self.min_rank
    2
  end
end
"##;

const GOLD: &str = r##"class Gold < Premium
  validates :vip, acceptance: { accept: [true, "y"] }
  validates :kind, inclusion: { in: -> { kinds - ["b"] } }, allow_nil: true
end
"##;

const ACCOUNT_PROBE: &str = r##"class AccountProbe
  def self.line(label, record)
    valid = begin
      record.valid?
    rescue StandardError => e
      "raised #{e.class}: #{e.message}"
    end
    "#{label}: #{valid} #{record.errors.full_messages.inspect} #{record.errors.attribute_names.inspect}"
  end

  def self.lines
    out = []
    out << line("account ok", Account.new(name: "n"))
    out << line("account terms 0", Account.new(name: "n", terms: "0"))
    out << line("account terms 1", Account.new(name: "n", terms: "1"))
    out << line("account terms true", Account.new(name: "n", terms: true))
    out << line("account terms false", Account.new(name: "n", terms: false))
    out << line("account agreed yes", Account.new(name: "n", agreed: "yes"))
    out << line("account agreed 1", Account.new(name: "n", agreed: "1"))
    out << line("account unnamed", Account.new(terms: "0"))
    out << line("account rank", Account.new(name: "n", rank: 10))
    out << line("account kind", Account.new(name: "n", kind: "c"))
    out << line("account kind b", Account.new(name: "n", kind: "b"))
    out << line("premium ok", Premium.new(name: "n", code: "c"))
    out << line("premium cira 0", Premium.new(name: "n", code: "c", cira: "0"))
    out << line("premium cira true", Premium.new(name: "n", code: "c", cira: true))
    out << line("premium terms 0", Premium.new(name: "n", code: "c", terms: "0"))
    out << line("premium no code", Premium.new(name: "n"))
    out << line("premium level", Premium.new(name: "n", code: "c", level: "tin"))
    out << line("premium level gold", Premium.new(name: "n", code: "c", level: "gold"))
    out << line("premium rank", Premium.new(name: "n", code: "c", rank: 1))
    out << line("premium rank big", Premium.new(name: "n", code: "c", rank: 11))
    out << line("gold ok", Gold.new(name: "n", code: "c"))
    out << line("gold vip n", Gold.new(name: "n", code: "c", vip: "n"))
    out << line("gold vip y", Gold.new(name: "n", code: "c", vip: "y", cira: "1"))
    out << line("gold cira 0", Gold.new(name: "n", code: "c", cira: "0"))
    out << line("gold kind b", Gold.new(name: "n", code: "c", kind: "b"))
    out << line("gold kind a", Gold.new(name: "n", code: "c", kind: "a"))
    out << "gold type: #{Gold.new.type.inspect}"
    premium = Premium.create!(name: "n", code: "c", cira: "1")
    out << "premium saved cira: #{premium.cira.inspect}"
    out << "premium update cira 0: #{premium.update(cira: "0")} #{premium.errors.full_messages.inspect}"
    gold = Gold.create!(name: "n", code: "c")
    out << "gold update vip n: #{gold.update(vip: "n")} #{gold.errors.full_messages.inspect}"
    begin
      gold.update!(vip: "n")
    rescue ActiveRecord::RecordInvalid => e
      out << "gold update! vip n: #{e.message}"
    end
    account = Account.create!(name: "n")
    out << "account update terms 0: #{account.update(terms: "0")} #{account.errors.full_messages.inspect}"
    out
  end
end
"##;

const ACCOUNT_SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "accounts", force: :cascade do |t|
    t.string "name"
    t.string "code"
    t.integer "rank"
    t.string "kind"
    t.string "level"
    t.string "agreed"
    t.string "type"
  end
end
"#;

const ACCOUNT_FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/account.rb", ACCOUNT),
    ("app/models/premium.rb", PREMIUM),
    ("app/models/gold.rb", GOLD),
    ("app/models/account_probe.rb", ACCOUNT_PROBE),
];

const ACCOUNT_OBSERVE: &str = "puts AccountProbe.lines\n";

/// `acceptance:` (a virtual attribute on the model and on STI
/// subclasses, and a column), `strict:` / `validates!` (raise instead
/// of recording), and `comparison:` / `inclusion:` / `numericality:`
/// values that read the class (a lambda calling a class method, a
/// class-body expression).
const ACCOUNT_EXPECT: &[&str] = &[
    "account ok: true [] []",
    "account terms 0: false [\"Terms must be accepted\"] [:terms]",
    "account terms 1: true [] []",
    "account terms true: true [] []",
    "account terms false: false [\"Terms must be accepted\"] [:terms]",
    "account agreed yes: true [] []",
    "account agreed 1: false [\"Agreed must be accepted\"] [:agreed]",
    "account unnamed: raised ActiveModel::StrictValidationFailed: Name can't be blank",
    "account rank: false [\"Rank must be less than 10\"] [:rank]",
    "account kind: false [\"Kind is not included in the list\"] [:kind]",
    "account kind b: true [] []",
    "premium cira 0: false [\"Cira must be accepted\"] [:cira]",
    "premium cira true: true [] []",
    "premium terms 0: false [\"Terms must be accepted\"] [:terms]",
    "premium no code: raised PremiumError: Code can't be blank",
    "premium level: raised ActiveModel::StrictValidationFailed: Level is not included in the list",
    "premium level gold: true [] []",
    "premium rank: false [\"Rank must be greater than 2\"] [:rank]",
    "premium rank big: false [\"Rank must be less than 10\"] [:rank]",
    "gold vip n: false [\"Vip must be accepted\"] [:vip]",
    "gold vip y: true [] []",
    "gold cira 0: false [\"Cira must be accepted\"] [:cira]",
    "gold kind b: false [\"Kind is not included in the list\"] [:kind]",
    "gold kind a: true [] []",
    "gold type: \"Gold\"",
    "premium update cira 0: false [\"Cira must be accepted\"]",
    "gold update vip n: false [\"Vip must be accepted\"]",
    "gold update! vip n: Validation failed: Vip must be accepted",
    "account update terms 0: false [\"Terms must be accepted\"]",
];

#[test]
fn acceptance_strict_and_class_reading_values_run_like_activerecord() {
    let native = native_of("accounts", ACCOUNT_FILES, ACCOUNT_SCHEMA, ACCOUNT_OBSERVE);
    compare(app_of(ACCOUNT_FILES, ACCOUNT_SCHEMA), native, ACCOUNT_EXPECT, "app/models/gold.rb", ACCOUNT_OBSERVE);
}

/// An STI subclass's unmodeled validator is the validates gap, reported
/// at its declaration, not a dropped statement.
#[test]
fn unmodeled_sti_subclass_validation_is_a_check_error() {
    let app = app().edit(
        "app/models/pinned_post.rb",
        "  validates :rank, numericality: { less_than: 10 }\n",
        "  validates :rank, numericality: { less_than: 10 }\n  validates :rank, comparison: { greater_than: -> { @minimum } }\n",
    );
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("`validates comparison:` not supported (all targets): on `rank` in STI subclass `PinnedPost`, `greater_than:` lambda that reads an instance variable of the class (ActiveModel calls it with the class as `self`)")),
        "{errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("STI subclass body call")), "{errors:#?}");
    assert_eq!(errors.len(), 1, "{errors:#?}");
}

/// A subclass that writes `def validate` shadows the hook the emitted
/// record runs its validations through (Rails' own `validate` is only
/// the `valid?` alias, and never runs it): a check error at each
/// declaration, not a lowering that would drop the method or the rules.
#[test]
fn sti_subclass_with_its_own_validate_is_a_check_error() {
    let app = app().edit(
        "app/models/pinned_post.rb",
        "  validates :rank, numericality: { less_than: 10 }\n",
        "  validates :rank, numericality: { less_than: 10 }\n\n  def validate\n    true\n  end\n",
    );
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("`PinnedPost` defines its own `validate`: in Rails that replaces only the `valid?` alias")),
        "{errors:#?}"
    );
    assert_eq!(errors.len(), 1, "{errors:#?}");
}

/// A strict validation raises `ActiveModel::StrictValidationFailed`,
/// which only the Ruby-family runtime declares: a strict target reports
/// it at each declaration that implies it (`strict: true`, `validates!`)
/// rather than emitting a raise of an undefined class. An app-defined
/// exception class (`strict: PremiumError`) is no such gap.
#[test]
fn strict_validation_failed_is_a_check_error_on_a_strict_target() {
    let (_dir, errors) = app_of(ACCOUNT_FILES, ACCOUNT_SCHEMA).emit(BuildTarget::Crystal);
    let strict: Vec<&String> = errors.iter().filter(|e| e.contains("ActiveModel::StrictValidationFailed")).collect();
    assert!(strict.iter().all(|e| !e.contains("FileId(0)")), "each at its declaration: {errors:#?}");
    assert_eq!(strict.len(), 2, "`strict: true` on Account and `validates!` on Premium: {errors:#?}");
    assert!(!errors.iter().any(|e| e.contains("PremiumError")), "{errors:#?}");
}

const CUSTOMER: &str = r##"class Customer < ApplicationRecord
  has_many :orders
  validates :name, presence: true
  validates :name, length: { minimum: 3 }, on: :audit
  validates_each :name do |record, attr, value, extra|
    record.errors.add(:base, "#{attr} #{value} #{extra.inspect}") if value == "four"
  end
end
"##;

const LINE: &str = r##"class Line < ApplicationRecord
  belongs_to :order
  validates :sku, presence: true
  validates :qty, numericality: { greater_than: 0 }, on: :update
  validates :qty, numericality: { less_than: 100 }, on: :audit
end
"##;

const RECEIPT: &str = r##"class Receipt < ApplicationRecord
  belongs_to :order
  validates :total, presence: true

  def marked_for_destruction?
    total == -1
  end
end
"##;

const ORDER: &str = r##"class Order < ApplicationRecord
  belongs_to :customer, optional: true
  has_many :lines
  has_one :receipt
  validates_associated :customer
  validates_associated :lines
  validates_associated :receipt, unless: -> { is_a?(RushOrder) }
  validates_each :name, :code, allow_nil: true do |record, attr, value|
    record.errors.add(attr, "starts with x") if value.to_s.start_with?("x")
    record.errors.add(:base, "#{attr} is #{record.send(attr).to_s.size} long") if value.to_s.size > 6
  end
  validates_each :status do |record, attr|
    record.errors.add(:base, "No #{attr}") if record.status.nil?
  end
  validates :code, presence: true, strict: true
  validates! :code, length: { maximum: 5 }
end
"##;

const RUSH: &str = r##"class RushOrder < Order
  validates_each :name do |record, attr, value|
    record.errors.add(attr, "is too short for rush") if value.to_s.size < 3
  end
  validates :receipt, associated: true
end
"##;

const ORDER_PROBE: &str = r##"class OrderProbe
  def self.line(label, record, context = nil)
    valid = begin
      context.nil? ? record.valid? : record.valid?(context)
    rescue StandardError => e
      "raised #{e.class}: #{e.message}"
    end
    "#{label}: #{valid} #{record.errors.full_messages.inspect} #{record.errors.attribute_names.inspect}"
  end

  def self.lines
    out = []
    c = Customer.create!(name: "Ann")
    o = Order.create!(name: "o1", code: "c1", status: "new", customer: c)
    Line.create!(order: o, sku: "a", qty: 1)
    Line.create!(order: o, sku: "b", qty: 2)
    Receipt.create!(order: o, total: 5)
    o = Order.find(o.id)
    out << line("fresh", o)
    o.customer.name = ""
    out << line("bad customer", o)
    out << "customer errors: #{o.customer.errors.full_messages.inspect}"
    o.customer.name = "Al"
    out << line("customer default ctx", o)
    out << line("customer audit ctx", o, :audit)
    out << line("customer create ctx", o, :create)
    o.customer.name = "Ann"
    lines = o.lines.to_a
    lines[0].sku = ""
    lines[1].qty = 0
    out << line("bad lines", o)
    out << "line errors: #{lines.map { |l| l.errors.full_messages }.inspect}"
    lines[0].sku = "a"
    out << line("line update ctx", o)
    out << line("line audit ctx", o, :audit)
    lines[1].qty = 200
    out << line("line create ctx big", o, :create)
    out << line("line audit ctx big", o, :audit)
    lines[1].qty = 2
    built = o.lines.build(sku: "")
    out << line("built bad line", o)
    built.sku = "n"
    out << line("built line", o)
    o.receipt.total = nil
    out << line("bad receipt", o)
    o.receipt.total = -1
    out << line("receipt marked", o)
    o.receipt.total = 5
    out << line("new without customer", Order.new(name: "n", code: "c", status: "s"))
    out << line("each x", Order.new(name: "xname", code: "xc", status: "s"))
    out << line("each long", Order.new(name: "a long name", code: "c", status: "s"))
    out << line("each nil", Order.new(code: "c"))
    out << line("strict code", Order.new(name: "n", status: "s"))
    out << line("bang long", Order.new(name: "n", code: "toolong", status: "s"))
    rush = RushOrder.create!(name: "rush", code: "r", status: "s")
    Receipt.create!(order: rush, total: 1)
    rush = RushOrder.find(rush.id)
    out << line("rush ok", rush)
    rush.name = "xy"
    rush.receipt.total = nil
    out << line("rush bad", rush)
    out << line("order sibling", Order.new(name: "xy", code: "c", status: "s"))
    out << line("each four", Customer.new(name: "four"))
    out
  end
end
"##;

const ORDER_SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "customers", force: :cascade do |t|
    t.string "name"
  end
  create_table "orders", force: :cascade do |t|
    t.string "name"
    t.string "code"
    t.string "status"
    t.integer "customer_id"
    t.string "type"
  end
  create_table "lines", force: :cascade do |t|
    t.integer "order_id"
    t.string "sku"
    t.integer "qty"
  end
  create_table "receipts", force: :cascade do |t|
    t.integer "order_id"
    t.integer "total"
  end
end
"#;

const ORDER_FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/customer.rb", CUSTOMER),
    ("app/models/line.rb", LINE),
    ("app/models/receipt.rb", RECEIPT),
    ("app/models/order.rb", ORDER),
    ("app/models/rush_order.rb", RUSH),
    ("app/models/order_probe.rb", ORDER_PROBE),
];

const ORDER_OBSERVE: &str = "puts OrderProbe.lines\n";

/// `validates_associated` (every loaded record valid in the record's
/// custom context, else its own default; nil and marked for destruction
/// skipped; "is invalid" on the attribute), `validates_each` (the block
/// called with the record, the attribute and its value, under
/// `allow_nil:`, on a model and an STI subclass) and `validates!`.
///
/// The records are the ones the association holds: the readers keep
/// what they load, so `order.customer.name = ""` changes the record the
/// next read and the validation see.
///
/// Rails' `has_many` also validates its records itself (`validate:
/// true`, AutosaveAssociation): new ones, and every loaded one in a
/// custom context, which adds a second "Lines is invalid" beside
/// `validates_associated`'s (the two errors differ in their options, so
/// `_ensure_no_duplicate_errors` keeps both).
const ORDER_EXPECT: &[&str] = &[
    "fresh: true [] []",
    "bad customer: false [\"Customer is invalid\"] [:customer]",
    "customer errors: [\"Name can't be blank\"]",
    "customer default ctx: true [] []",
    "customer audit ctx: false [\"Customer is invalid\"] [:customer]",
    "customer create ctx: true [] []",
    "bad lines: false [\"Lines is invalid\"] [:lines]",
    "line errors: [[\"Sku can't be blank\"], [\"Qty must be greater than 0\"]]",
    "line update ctx: false [\"Lines is invalid\"] [:lines]",
    "line audit ctx: true [] []",
    "line create ctx big: true [] []",
    "line audit ctx big: false [\"Lines is invalid\", \"Lines is invalid\"] [:lines]",
    "built bad line: false [\"Lines is invalid\", \"Lines is invalid\"] [:lines]",
    "built line: true [] []",
    "bad receipt: false [\"Receipt is invalid\"] [:receipt]",
    "receipt marked: true [] []",
    "new without customer: true [] []",
    "each x: false [\"Name starts with x\", \"Code starts with x\"] [:name, :code]",
    "each long: false [\"name is 11 long\"] [:base]",
    "each nil: false [\"No status\"] [:base]",
    "strict code: raised ActiveModel::StrictValidationFailed: Code can't be blank [] []",
    "bang long: raised ActiveModel::StrictValidationFailed: Code is too long (maximum is 5 characters) [\"code is 7 long\"] [:base]",
    "rush ok: true [] []",
    "rush bad: false [\"Name starts with x\", \"Name is too short for rush\", \"Receipt is invalid\"] [:name, :receipt]",
    "order sibling: false [\"Name starts with x\"] [:name]",
    "each four: false [\"name four nil\"] [:base]",
];

#[test]
fn validates_associated_each_and_bang_run_like_activerecord() {
    let native = native_of("orders", ORDER_FILES, ORDER_SCHEMA, ORDER_OBSERVE);
    compare(app_of(ORDER_FILES, ORDER_SCHEMA), native, ORDER_EXPECT, "app/models/order.rb", ORDER_OBSERVE);
}

/// The validation context (`on:`, and the one `validates_associated`
/// validates each record in) is run state only the Ruby-family runtimes
/// carry for ActiveRecord: on a strict target each declaration reading
/// it is an error, not an emit naming `validation_context`.
#[test]
fn validation_contexts_are_check_errors_on_a_strict_target() {
    // An ActiveModel class synthesizes its own `valid?(context)`.
    let form = "class Form\n  include ActiveModel::Model\n  attr_accessor :name\n  validates :name, presence: true, on: :audit\nend\n";
    let (_dir, errors) = app_of(ORDER_FILES, ORDER_SCHEMA).write("app/models/form.rb", form).emit(BuildTarget::Crystal);
    let context: Vec<&String> = errors.iter().filter(|e| e.contains("validation_context not supported (crystal)")).collect();
    assert!(context.iter().all(|e| !e.contains("FileId(0)")), "each at its declaration: {errors:#?}");
    assert_eq!(
        context.len(),
        7,
        "Customer's `on: :audit`, Line's `on: :update` and `on: :audit`, Order's three `validates_associated`, RushOrder's `associated: true`: {errors:#?}"
    );
    let (_dir, errors) = app_of(ORDER_FILES, ORDER_SCHEMA).emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("validation_context not supported")), "{errors:#?}");
}

/// What `validates_associated` / `validates_each` cannot be lowered to
/// is a check error at the declaration: a block reading `self` (the
/// class in Rails, the record in the emitted check), a block with a
/// rest parameter, and `validates_associated` outside ActiveRecord.
#[test]
fn unmodeled_validates_associated_and_each_are_check_errors() {
    let form = "class Form\n  include ActiveModel::Model\n  attr_accessor :item, :name\n  validates_associated :item\n  validates_each :name do |*args|\n    args[0].errors.add(args[1], \"bad\")\n  end\nend\n";
    let app = app_of(ORDER_FILES, ORDER_SCHEMA)
        .edit(
            "app/models/rush_order.rb",
            "value.to_s.size < 3",
            "value.to_s.size < min_size",
        )
        .write("app/models/form.rb", form);
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    for seen in [
        "`validates_each` not supported (all targets): on `name` in STI subclass `RushOrder`, a block that reads `self` (the class it was written in, not the record the emitted check runs on)",
        "`validates_each` not supported (all targets): on `name`, a block with a rest or block parameter",
        "`validates_associated` not supported (all targets): on `item`, on a class outside ActiveRecord (ActiveModel has no AssociatedValidator)",
    ] {
        assert!(errors.iter().any(|e| e.contains(seen)), "{seen}: {errors:#?}");
    }
    assert_eq!(errors.len(), 3, "{errors:#?}");
}
