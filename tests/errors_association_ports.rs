//! Two Rails reads Core makes on a record, ported for the CRuby and JRuby
//! trees from Core's pinned Rails (Shopify/rails 5813559f, 8.2.0.alpha):
//!
//! - `record.errors.attribute_names` (activemodel errors.rb): the
//!   attributes of the current errors, each once, in order, frozen
//!   (runtime/spinel/active_model_errors.rb). The emitted `errors` is an
//!   Array of messages; where the app reads `attribute_names`, the
//!   validation lowering's pushes record their attribute alongside.
//! - `record.association(:name)` (activerecord associations.rb and
//!   associations/*_association.rb): the association object, whose
//!   `loaded?`, `target`, `target =`, `reset`, `reload`, `reader` and
//!   `load_target` read and write the record's association caches
//!   (runtime/spinel/active_record_associations.rb). An undeclared name
//!   raises ActiveRecord::AssociationNotFoundError.
//!
//! The probe app below runs natively on Core's pinned activerecord over
//! in-memory SQLite (tests/support/rails_oracle.rb) and emitted on CRuby,
//! and must print the same. Spinel carries neither port and refuses both
//! reads by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "authors", force: :cascade do |t|
    t.string "name"
  end
  create_table "posts", force: :cascade do |t|
    t.string "title"
    t.integer "author_id"
  end
  create_table "comments", force: :cascade do |t|
    t.string "body"
    t.integer "post_id"
  end
  create_table "covers", force: :cascade do |t|
    t.string "url"
    t.integer "post_id"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ("author", "class Author < ApplicationRecord\n  has_many :posts\nend\n"),
    ("comment", "class Comment < ApplicationRecord\n  belongs_to :post\nend\n"),
    ("cover", "class Cover < ApplicationRecord\n  belongs_to :post\nend\n"),
    (
        "post",
        r#"class Post < ApplicationRecord
  belongs_to :author, optional: true
  has_many :comments
  has_one :cover
  validates :title, presence: true
  validates :title, length: { maximum: 12 }
  validate :title_not_shouting

  def title_not_shouting
    errors.add(:base, "no shouting") if title == "LOUD"
    errors.add(:author_id, "must be set") if title == "LOUD"
    flag(:author_id) if title == "FLAG"
    flag("title") if title == "FLAGS"
  end

  def flag(attribute)
    errors.add(attribute, "is flagged")
  end
end
"#,
    ),
];

/// App code, so both reads go through the compiler. `show` prints one
/// answer, or the error it raised.
const PROBE: &str = r##"module PortsProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.names_after(attrs)
    post = Post.new(attrs)
    post.valid?
    post.errors.attribute_names
  end

  def self.run
    show("blank title") { names_after(title: "") }
    show("shouting") { names_after(title: "LOUD") }
    show("too long") { names_after(title: "a title that is too long") }
    show("flagged by a local") { names_after(title: "FLAG") }
    show("flagged by a string") { names_after(title: "FLAGS") }
    show("valid") { names_after(title: "fine") }
    show("frozen") { names_after(title: "LOUD").frozen? }
    show("never validated") { Post.new(title: "LOUD").errors.attribute_names }
    show("revalidated") do
      post = Post.new(title: "")
      post.valid?
      post.title = "fine"
      post.valid?
      post.errors.attribute_names
    end
    show("required belongs_to") do
      comment = Comment.new(body: "orphan")
      comment.valid?
      comment.errors.attribute_names
    end
    show("cleared") do
      post = Post.new(title: "LOUD")
      post.valid?
      post.errors.clear
      post.errors.attribute_names
    end

    post = Post.create!(title: "first")
    comment = Comment.create!(body: "a", post_id: post.id)
    Cover.create!(url: "u", post_id: post.id)

    fresh = Post.find(post.id)
    comments = fresh.association(:comments)
    show("has_many class") { comments.class.name }
    show("has_many loaded? before") { comments.loaded? }
    show("has_many target before") { comments.target }
    show("has_many load_target") { comments.load_target.map(&:body) }
    show("has_many loaded? after load_target") { comments.loaded? }
    show("has_many target after") { fresh.association(:comments).target.map(&:body) }
    show("has_many target length") { fresh.association(:comments).target.length }
    show("has_many reset") { comments.reset }
    show("has_many loaded? after reset") { comments.loaded? }
    show("has_many target after reset") { comments.target }
    show("has_many reload") { fresh.association(:comments).reload.class.name }
    show("has_many loaded? after reload") { fresh.association(:comments).loaded? }
    show("by a String name") { Post.find(post.id).association("comments").class.name }

    seeded = Post.find(post.id)
    seeded.association(:comments).target = [comment]
    show("target= loaded?") { seeded.association(:comments).loaded? }
    show("target= read through the reader") { seeded.comments.map(&:body) }

    c = Comment.find(comment.id)
    parent = c.association(:post)
    show("belongs_to class") { parent.class.name }
    show("belongs_to loaded? before") { parent.loaded? }
    show("belongs_to target before") { parent.target }
    show("belongs_to reader read") { c.post.title }
    show("belongs_to loaded? after read") { parent.loaded? }
    show("belongs_to target after read") { parent.target.title }
    show("belongs_to reset") { parent.reset }
    show("belongs_to after reset") { [parent.loaded?, parent.target] }
    show("belongs_to association reader") { parent.reader.title }
    show("belongs_to loaded? after reader") { parent.loaded? }

    h = Post.find(post.id)
    cover = h.association(:cover)
    show("has_one class") { cover.class.name }
    show("has_one before") { [cover.loaded?, cover.target] }
    show("has_one target never read") { Post.find(post.id).association(:cover).target }
    show("has_one reader") { cover.reader.url }
    show("has_one after reader") { [cover.loaded?, cover.target.url] }
    show("has_one reload") { cover.reload.target.url }

    preloaded = Post.includes(:comments, :cover).to_a.first
    show("preloaded has_many") { [preloaded.association(:comments).loaded?, preloaded.association(:comments).target.map(&:body)] }
    show("preloaded has_one") { [preloaded.association(:cover).loaded?, preloaded.association(:cover).target.url] }

    show("another model") { Author.new(name: "x").association(:posts).class.name }
    show("unknown association") { fresh.association(:nope) }
    show("unknown on another model") { Cover.new.association(:comments) }
  end
end
"##;

/// Rails' answers, spelled out so a run that drifts from native in the
/// same way on both sides (an oracle that loads the wrong Rails) still fails.
const EXPECTED: &str = r#"blank title: [:title]
shouting: [:base, :author_id]
too long: [:title]
flagged by a local: [:author_id]
flagged by a string: [:title]
valid: []
frozen: true
never validated: []
revalidated: []
required belongs_to: [:post]
cleared: []
has_many class: "ActiveRecord::Associations::HasManyAssociation"
has_many loaded? before: false
has_many target before: []
has_many load_target: ["a"]
has_many loaded? after load_target: true
has_many target after: ["a"]
has_many target length: 1
has_many reset: nil
has_many loaded? after reset: false
has_many target after reset: []
has_many reload: "ActiveRecord::Associations::HasManyAssociation"
has_many loaded? after reload: true
by a String name: "ActiveRecord::Associations::HasManyAssociation"
target= loaded?: true
target= read through the reader: ["a"]
belongs_to class: "ActiveRecord::Associations::BelongsToAssociation"
belongs_to loaded? before: false
belongs_to target before: nil
belongs_to reader read: "first"
belongs_to loaded? after read: true
belongs_to target after read: "first"
belongs_to reset: false
belongs_to after reset: [false, nil]
belongs_to association reader: "first"
belongs_to loaded? after reader: true
has_one class: "ActiveRecord::Associations::HasOneAssociation"
has_one before: [false, nil]
has_one target never read: nil
has_one reader: "u"
has_one after reader: [true, "u"]
has_one reload: "u"
preloaded has_many: [true, ["a"]]
preloaded has_one: [true, "u"]
another model: "ActiveRecord::Associations::HasManyAssociation"
unknown association: ["ActiveRecord::AssociationNotFoundError", "Association named 'nope' was not found on Post; perhaps you misspelled it?"]
unknown on another model: ["ActiveRecord::AssociationNotFoundError", "Association named 'comments' was not found on Cover; perhaps you misspelled it?"]
"#;

/// An authored test reading both, as Core's tests do.
const POST_TEST: &str = r#"require "test_helper"

class PostTest < ActiveSupport::TestCase
  test "attribute_names names each invalid attribute once" do
    post = Post.new(title: "LOUD")
    assert_not post.valid?
    assert_equal [:base, :author_id], post.errors.attribute_names
    post.title = ""
    post.valid?
    assert_equal [:title], post.errors.attribute_names
  end

  test "association answers what is in memory" do
    post = Post.create!(title: "first")
    Comment.create!(body: "a", post_id: post.id)
    record = Post.find(post.id)
    assert_not record.association(:comments).loaded?
    assert_equal [], record.association(:comments).target
    record.association(:comments).load_target
    assert record.association(:comments).loaded?
    assert_equal ["a"], record.association(:comments).target.map(&:body)
    assert_raises(ActiveRecord::AssociationNotFoundError) { record.association(:nope) }
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/ports_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_errors_association_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("ports_probe.rb"), PROBE).unwrap();
    let mut requires = String::new();
    for (name, source) in MODELS {
        std::fs::write(dir.join(format!("{name}.rb")), source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         # A Rails 5+ app's defaults (`load_defaults`): a belongs_to is required.\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}require_relative \"ports_probe\"\n\
         PortsProbe.run\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn attribute_names_and_association_run_like_rails() {
    let run = app().run_ruby("PortsProbe.run");
    run.assert_passes();
    let probe = std::fs::read_to_string(run.emitted.join("app/models/ports_probe.rb")).unwrap();
    // The reads are Rails' own, not collapsed onto the reader (which
    // would load where Rails answers what is in memory).
    assert!(probe.contains("fresh.association(:comments).target.length"), "{probe}");
    assert!(probe.contains("Post.find(post.id).association(:cover).target"), "{probe}");
    assert_eq!(run.stdout, EXPECTED, "emitted on the ports vs Rails' answers");
    assert_eq!(run.stdout, native(), "emitted on the ports vs native activerecord");
}

#[test]
fn an_authored_test_reads_both() {
    let run = app().write("test/models/post_test.rb", POST_TEST).run_test("test/models/post_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("PostTest: 2 tests passed"), "{}", run.stdout);
}

/// The PostgreSQL CRuby tree carries the same two ports.
#[test]
fn the_postgresql_tree_carries_both_ports() {
    let (dir, errors) = app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_e2e_errors_association\n")
        .emit(BuildTarget::Ruby);
    let errors: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    for file in ["runtime/active_model_errors.rb", "runtime/active_record_associations.rb"] {
        assert!(dir.join(file).is_file(), "{file} is carried");
    }
    let probe = std::fs::read_to_string(dir.join("app/models/ports_probe.rb")).unwrap();
    assert!(probe.contains("ActiveModel::ErrorAttributes.attribute_names(post.errors)"), "{probe}");
}

/// Spinel emits neither port and says so for each read.
#[test]
fn spinel_refuses_both_reads() {
    let (dir, errors) = app().emit(BuildTarget::Spinel);
    for (read, file) in [
        ("ActiveModel::Errors#attribute_names", "runtime/active_model_errors.rb"),
        ("#association", "runtime/active_record_associations.rb"),
    ] {
        let expected = format!("declared by the Ruby-family runtime ({file}), which spinel does not emit");
        assert!(
            errors.iter().any(|e| e.contains(read) && e.contains(&expected)),
            "spinel {read}: {errors:#?}"
        );
        assert!(!dir.join(file).exists(), "{file} stays off the spinel tree");
    }
}
