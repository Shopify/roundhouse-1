//! Unscoped `has_one` participates in Relation `preload` / `includes`
//! batching through the load-once cache.
//!
//! Unclaimed: scoped `has_one` batch scope application; polymorphic
//! `as:` preload batch; separate `eager_load` emit_and_run pin.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/has_one_overlay.rs"]
mod has_one_overlay;

use has_one_overlay::profile_schema_edit;

#[test]
fn preloading_has_one_distributes_each_owners_child() {
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
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :profile\n  scope :with_profiles, -> { preload(:profile) }",
        )
        .run_ruby(
            r#"
a = Article.create!(title: "Alpha", body: "abcdefghij")
b = Article.create!(title: "Beta", body: "abcdefghij")
c = Article.create!(title: "Gamma", body: "abcdefghij")
Profile.create!(article_id: a.id, bio: "a-bio")
Profile.create!(article_id: c.id, bio: "c-bio")
rows = Article.with_profiles.order(:id).to_a
raise "count" unless rows.size == 3
raise "alpha" unless rows[0].profile.bio == "a-bio"
raise "beta should be nil" unless rows[1].profile.nil?
raise "gamma" unless rows[2].profile.bio == "c-bio"
puts "has_one preload passed"
"#,
        )
        .assert_passes();
}

/// `includes(:assoc)` — sibling Relation spelling of the same batch path.
#[test]
fn includes_has_one_distributes_each_owners_child() {
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
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :profile\n  scope :including_profiles, -> { includes(:profile) }",
        )
        .run_ruby(
            r#"
a = Article.create!(title: "Alpha", body: "abcdefghij")
b = Article.create!(title: "Beta", body: "abcdefghij")
Profile.create!(article_id: a.id, bio: "via-includes")
rows = Article.including_profiles.order(:id).to_a
raise "alpha" unless rows[0].profile.bio == "via-includes"
raise "beta nil" unless rows[1].profile.nil?
puts "has_one includes passed"
"#,
        )
        .assert_passes();
}

/// Renamed association: `class_name:` + `foreign_key:` preloads under the
/// declared name, not the target class name.
#[test]
fn preload_honors_class_name_and_foreign_key() {
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
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :spotlight, class_name: \"Profile\", foreign_key: :article_id\n  scope :with_spotlights, -> { preload(:spotlight) }",
        )
        .run_ruby(
            r#"
a = Article.create!(title: "Alpha", body: "abcdefghij")
b = Article.create!(title: "Beta", body: "abcdefghij")
Profile.create!(article_id: a.id, bio: "spot")
rows = Article.with_spotlights.order(:id).to_a
raise "spotlight" unless rows[0].spotlight.bio == "spot"
raise "missing is nil" unless rows[1].spotlight.nil?
puts "has_one preload class_name/foreign_key passed"
"#,
        )
        .assert_passes();
}
