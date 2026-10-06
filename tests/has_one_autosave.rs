//! `has_one …, autosave: true` stashes through the writer and persists
//! the child after the owner saves. Plain `has_one` gets no writer
//! (fail closed — cache-only assignment would silently drop children).
//!
//! Unclaimed: bare `build_`/`create_` without assigning into the cache;
//! implicit `autosave: nil`; `inverse_of` / `touch` / `validate` /
//! `required` / `strict_loading`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/has_one_overlay.rs"]
mod has_one_overlay;

use has_one_overlay::profile_schema_edit;

#[test]
fn assigned_has_one_child_autosaves_with_owner() {
    let (path, from, to) = profile_schema_edit();
    emit_and_run::real_blog()
        .edit(path, from, to)
        .write(
            "app/models/profile.rb",
            "class Profile < ApplicationRecord\n  belongs_to :article\nend\n",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :profile, autosave: true",
        )
        .run_ruby(
            r#"
a = Article.new(title: "Hello world", body: "abcdefghij")
a.profile = Profile.new(bio: "autosaved")
raise "writer stash missed" unless a.profile.bio == "autosaved"
a.save!
raise "owner missing" if a.id.nil? || a.id == 0
child = Profile.find_by(article_id: a.id)
raise "child not persisted" if child.nil?
raise "bio wrong" unless child.bio == "autosaved"
raise "reader after save" unless a.profile.bio == "autosaved"
b = Article.create!(title: "Empty owner", body: "abcdefghij")
b.save!
raise "nil child invented a row" unless Profile.where(article_id: b.id).count == 0
puts "has_one autosave passed"
"#,
        )
        .assert_passes();
}

/// `class_name:` + `foreign_key:` — association name ≠ target / default FK.
#[test]
fn autosave_honors_class_name_and_foreign_key() {
    let (path, from, to) = profile_schema_edit();
    emit_and_run::real_blog()
        .edit(path, from, to)
        .write(
            "app/models/profile.rb",
            "class Profile < ApplicationRecord\n  belongs_to :article\nend\n",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :spotlight, class_name: \"Profile\", foreign_key: :article_id, autosave: true",
        )
        .run_ruby(
            r#"
a = Article.new(title: "Hello world", body: "abcdefghij")
a.spotlight = Profile.new(bio: "named")
a.save!
child = Profile.find_by(article_id: a.id)
raise "child missing" if child.nil?
raise "bio" unless child.bio == "named"
raise "reader" unless a.spotlight.bio == "named"
puts "has_one autosave class_name/foreign_key passed"
"#,
        )
        .assert_passes();
}

/// Polymorphic `as:` stamps the type column on autosave.
#[test]
fn autosave_stamps_polymorphic_as_type() {
    emit_and_run::real_blog()
        .edit(
            "db/schema.rb",
            "  create_table \"comments\", force: :cascade do |t|",
            "  create_table \"notices\", force: :cascade do |t|\n    t.integer \"notable_id\"\n    t.string \"notable_type\"\n    t.string \"body\"\n  end\n\n  create_table \"comments\", force: :cascade do |t|",
        )
        .write(
            "app/models/notice.rb",
            "class Notice < ApplicationRecord\n  belongs_to :notable, polymorphic: true\nend\n",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :notice, as: :notable, autosave: true",
        )
        .run_ruby(
            r#"
a = Article.new(title: "Hello world", body: "abcdefghij")
a.notice = Notice.new(body: "ping")
a.save!
row = Notice.find_by(notable_id: a.id, notable_type: "Article")
raise "row missing" if row.nil?
raise "body" unless row.body == "ping"
raise "reader" unless a.notice.body == "ping"
puts "has_one autosave as: passed"
"#,
        )
        .assert_passes();
}

/// `dependent: :destroy` beside `autosave: true` — both halves coexist.
#[test]
fn autosave_with_dependent_destroy() {
    let (path, from, to) = profile_schema_edit();
    emit_and_run::real_blog()
        .edit(path, from, to)
        .write(
            "app/models/profile.rb",
            "class Profile < ApplicationRecord\n  belongs_to :article\nend\n",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :profile, dependent: :destroy, autosave: true",
        )
        .run_ruby(
            r#"
a = Article.new(title: "Hello world", body: "abcdefghij")
a.profile = Profile.new(bio: "temp")
a.save!
id = a.profile.id
raise "not saved" if id.nil? || id == 0
a.destroy
raise "child survived destroy" unless Profile.find_by(id: id).nil?
puts "has_one autosave+dependent destroy passed"
"#,
        )
        .assert_passes();
}
