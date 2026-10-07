//! A Concern that only gathers other Concerns hands each includer the
//! gathered Concerns' `included do` items.
//!
//! Core's `MerchantSubscriptions::Concerns::ResolvableCapabilityColumn` is
//! `extend ActiveSupport::Concern; include CapabilityPayloadColumn`. Ingest
//! dropped the module (no def), so its includers reported "includes
//! unresolved"; surfaced, it still handed them nothing, because
//! ActiveSupport::Concern defers a Concern's own `include Other` until the
//! outer Concern is included and then runs `Other`'s `included` block on
//! the includer (`append_features`). The splice follows those
//! dependencies transitively and depth first, each Concern once, and a
//! plain module's include still arrives only through ancestry.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const GATHER: &str = r#"module Gather
  extend ActiveSupport::Concern
  include Gatherable
  include Inner
end
"#;

const GATHERABLE: &str = r#"module Gatherable
  def gathered?
    true
  end
end
"#;

const INNER: &str = r#"module Inner
  extend ActiveSupport::Concern
  include Leaf

  included do
    validates :title, length: { maximum: 5 }
    scope :titled, ->(title) { where(title: title) }
  end
end
"#;

const LEAF: &str = r#"module Leaf
  extend ActiveSupport::Concern

  included do
    scope :greeting, -> { where(title: "Hi") }
  end
end
"#;

const SCRIPT: &str = r#"a = Article.new(title: "Too long", body: "Body text here")
p [a.valid?, a.errors.count, Article.new(title: "Short", body: "Body text here").valid?]
Article.create!(title: "Hi", body: "Body text here")
p [Article.titled("Hi").count, Article.greeting.count, Article.new.gathered?]
"#;

#[test]
fn a_gathering_concern_s_dependencies_run_their_included_blocks_on_the_includer() {
    let native_article = "class Article < ActiveRecord::Base\n  include Gather\n  validates :title, presence: true\n  validates :body, presence: true, length: { minimum: 10 }\nend\n";
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_record\"\nActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\nActiveRecord::Schema.verbose = false\nActiveRecord::Schema.define {{ create_table(:articles) {{ |t| t.string :title; t.text :body; t.timestamps }} }}\n{LEAF}\n{INNER}\n{GATHERABLE}\n{GATHER}\n{native_article}\n{SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[false, 1, true]\n[1, 1, true]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::real_blog()
        .write("app/models/concerns/gather.rb", GATHER)
        .write("app/models/concerns/gatherable.rb", GATHERABLE)
        .write("app/models/concerns/inner.rb", INNER)
        .write("app/models/concerns/leaf.rb", LEAF)
        .edit("app/models/article.rb", "class Article < ApplicationRecord\n", "class Article < ApplicationRecord\n  include Gather\n")
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
