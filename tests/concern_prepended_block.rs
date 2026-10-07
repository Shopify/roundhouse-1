//! `prepend Concern` on a model: ActiveSupport::Concern's
//! `prepend_features` evaluates the concern's `prepended do` block in
//! the model's body and prepends its `class_methods` onto the model's
//! singleton. Neither was carried: the block's validation and the class
//! method were silently missing. An includer of the same concern runs
//! neither block.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = "module HeadlineGuard\n  extend ActiveSupport::Concern\n\n  prepended do\n    validates :title, presence: true\n  end\n\n  class_methods do\n    def guarded? = true\n  end\n\n  def headline = \"[#{super}]\"\nend\n";
const PREPENDER: &str = "class GuardedPost < ApplicationRecord\n  self.table_name = \"articles\"\n  prepend HeadlineGuard\n\n  def headline = title.to_s.upcase\nend\n";
const INCLUDER: &str = "class IncludedPost < ApplicationRecord\n  self.table_name = \"articles\"\n  include HeadlineGuard\nend\n";
const SCRIPT: &str = r##"
blank = GuardedPost.new(title: "", body: "long enough body")
raise "blank title accepted" if blank.valid?
raise "blank title saved" if blank.save
raise "errors: #{blank.errors.to_a.inspect}" unless blank.errors.to_a == ["Title can't be blank"]
post = GuardedPost.new(title: "hi", body: "long enough body")
raise "valid record rejected: #{post.errors.to_a.inspect}" unless post.save
raise "headline: #{post.headline.inspect}" unless post.headline == "[HI]"
raise "class method missing" unless GuardedPost.guarded? == true
raise "ancestors: #{GuardedPost.ancestors.first.inspect}" unless GuardedPost.ancestors.first == HeadlineGuard
raise "includer ran the prepended block" unless IncludedPost.new(title: "", body: "long enough body").valid?
"##;

const NATIVE_PRELUDE: &str = r#"
require "active_record"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define { create_table(:articles) { |t| t.string :title; t.text :body } }
class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

#[test]
fn prepended_block_and_class_methods_reach_the_prepender_like_native() {
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{NATIVE_PRELUDE}\n{CONCERN}\n{PREPENDER}\n{INCLUDER}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    emit_and_run::real_blog()
        .write("app/models/concerns/headline_guard.rb", CONCERN)
        .write("app/models/guarded_post.rb", PREPENDER)
        .write("app/models/included_post.rb", INCLUDER)
        .run_ruby(SCRIPT)
        .assert_passes();
}
