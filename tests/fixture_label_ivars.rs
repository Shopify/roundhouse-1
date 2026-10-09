//! A fixture label is any YAML key; the accessor's cache slot is an
//! instance variable, whose name is not. Core's `domains.yml` labels
//! hosts (`ihatetaxes.myshopify.com`, `wwwstorefront-api_com`), and the
//! slot `@__fx_wwwstorefront-api_com` parsed as a subtraction: a
//! "possibly useless use of - in void context" warning that Core's
//! StrictWarning raises on while loading `test/fixtures/domains.rb`, a
//! file 540 of the unit-1000 tests require (and at run time, `nil.b`).
//!
//! `fixture_cache_ivar` (src/emit/ruby/library.rs) escapes such labels
//! injectively, so `a.b-c` and `a_b_c` are two valid, distinct slots.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const ARTICLES: &str = r#"one:
  title: Getting Started with Rails
  body: Rails is a web application framework running on the Ruby programming language.

two:
  title: Understanding MVC Architecture
  body: MVC stands for Model-View-Controller. Models handle data and business logic.

a.b-c:
  title: Dotted label
  body: A label that is a host name, as Core's domains.yml writes them.

a_b_c:
  title: Plain label
  body: The label the dotted one would collide with under a lossy escape.

a_2e_b:
  title: Escape-shaped label
  body: A plain label spelled like the escape of a dotted one.

a.b:
  title: Short dotted label
  body: Escapes to the spelling of the label above under the dotted prefix only.
"#;

// The per-test fixture load clears every slot, the dotted ones included:
// before the escape, `@__fx_a.b-c = nil` there was `nil.b` and every
// test whose reset loaded the file errored in setup. (A call site that
// names a dotted label, `articles(:"a.b-c")`, is a separate emit: it
// still spells `ArticlesFixtures.a.b-c`, and no unit-1000 test writes
// one.)
const LABEL_TEST: &str = r#"require "test_helper"

class FixtureLabelTest < ActiveSupport::TestCase
  test "a file with dotted labels loads and keeps every row" do
    plain = articles(:a_b_c)
    assert_equal "Plain label", plain.title
    assert_same plain, articles(:a_b_c)
    assert_equal "Escape-shaped label", articles(:a_2e_b).title
    assert_equal 6, Article.count
    assert_equal ["Dotted label", "Short dotted label"], Article.where(title: ["Dotted label", "Short dotted label"]).order(:title).map(&:title)
  end
end
"#;

fn overlay() -> emit_and_run::Overlay {
    emit_and_run::real_blog()
        .write("test/fixtures/articles.yml", ARTICLES)
        .write("test/models/fixture_label_test.rb", LABEL_TEST)
}

#[test]
fn fixture_cache_slots_are_valid_distinct_ivars() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let path = emitted.join("test/fixtures/articles.rb");
    let src = std::fs::read_to_string(&path).expect("emitted articles fixtures");
    for slot in ["@__fxe_a_2e_b_2d_c", "@__fx_a_b_c", "@__fx_a_2e_b", "@__fxe_a_2e_b ", "@__fx_one"] {
        assert!(src.contains(slot), "no slot {slot:?}:\n{src}");
    }
    assert!(!src.contains("@__fx_a.b"), "a label reached an ivar unescaped:\n{src}");
    // Every slot the file names is a whole instance variable: what
    // follows the name is a space, the end of the line, or `.nil?`.
    for ivar in src.split('@').skip(1) {
        let name: String = ivar.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let rest = &ivar[name.len()..];
        assert!(
            rest.starts_with(' ') || rest.starts_with('\n') || rest.starts_with(".nil?"),
            "ivar @{name} is followed by {:?}:\n{src}",
            rest.chars().take(8).collect::<String>(),
        );
    }
    // And the file parses without a warning (`-w`: Core's StrictWarning
    // raises on "possibly useless use of - in void context").
    let check = Command::new(std::env::var("RUBY").unwrap_or_else(|_| "ruby".into()))
        .args(["-wc"])
        .arg(&path)
        .output()
        .expect("ruby -wc");
    let stdout = String::from_utf8_lossy(&check.stdout);
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success() && stdout.contains("Syntax OK"), "{stdout}{stderr}");
    assert!(stderr.trim().is_empty(), "warnings:\n{stderr}");
}

#[test]
fn a_fixture_file_with_dotted_labels_loads() {
    overlay().run_test("test/models/fixture_label_test.rb").assert_passes();
}
