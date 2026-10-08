//! `Model.where(...).exists?` is a terminal on the relation, not on its
//! rows: Rails answers `SELECT 1 ... LIMIT 1`. The arel pass lifted the
//! `where` to a hydrated Array and left `exists?` on it (NoMethodError).
//! The emitted app is compared with native Rails (the pinned oracle,
//! tests/support/rails_oracle.rb) on the same methods and rows.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

const METHODS: &str = "  def self.titled?(title) = where(title: title).exists?\n  \
     def self.probe(id) = Article.where(id: id).exists?\n  \
     def self.any_articles? = Article.exists?\n  \
     def self.limited?(title) = where(title: title).limit(3).exists?\n  \
     def self.first_one? = Article.where(id: 1).exists?\n  \
     def self.titled_one? = where(title: \"One\").order(:id).exists?\n  \
     def self.limited_one? = Article.where(title: \"One\").limit(2).exists?\n";

const SCRIPT: &str = "p [Article.titled?(\"One\"), Article.probe(1), Article.any_articles?, Article.limited?(\"One\")]\n\
     a = Article.create!(title: \"One\", body: \"a body long enough\")\n\
     p [Article.titled?(\"One\"), Article.titled?(\"Two\"), Article.probe(a.id), Article.probe(a.id + 1)]\n\
     p [Article.any_articles?, Article.limited?(\"One\"), Article.limited?(\"Two\")]\n\
     p [Article.first_one?, Article.titled_one?, Article.limited_one?, a.id == 1]\n";

#[test]
fn where_then_exists_answers_a_boolean_like_rails() {
    let dir = std::env::temp_dir().join(format!(
        "rh_relation_exists_terminal_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.rb"),
        format!(
            "require {ORACLE:?}\nRailsOracle.activate(\"activesupport\", \"activemodel\", \"activerecord\")\n\
             require \"active_record\"\n\
             ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
             ActiveRecord::Schema.verbose = false\n\
             ActiveRecord::Schema.define {{ create_table(:articles) {{ |t| t.string :title; t.text :body; t.timestamps }} }}\n\
             class Article < ActiveRecord::Base\n{METHODS}end\n{SCRIPT}"
        ),
    )
    .unwrap();
    let native = std::process::Command::new("ruby")
        .arg("main.rb")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
    let expected = String::from_utf8(native.stdout).unwrap();
    assert_eq!(
        expected.lines().collect::<Vec<_>>(),
        [
            "[false, false, false, false]",
            "[true, false, true, false]",
            "[true, true, false]",
            "[true, true, true, true]"
        ],
        "native control"
    );

    let run = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true\n",
            &format!("  validates :title, presence: true\n\n{METHODS}"),
        )
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected, "emitted Ruby diverges from native");
}
