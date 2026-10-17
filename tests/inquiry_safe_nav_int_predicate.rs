//! `buyer_signals.country&.casecmp(code)&.zero?` is Integer#zero? on a
//! safe-navigation temp, not a StringInquirer predicate.
//!
//! The inquiry fold turns `<String>.<label>?` into `<String> == "<label>"`
//! when no String answers `<label>?`. The temp binding the first `&.`
//! (`__safe_nav = x.country`) is a String, and the second `&.` re-binds the
//! `casecmp` result; folding on the first temp's type emitted
//! `(...) && __safe_nav == "zero"`, which is never true, so Core's
//! `Markets::Region::Country#match?` answered false for every country and
//! `draft_order.market_id` stayed nil.

use roundhouse::App;
use roundhouse::analyze::Analyzer;
use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_library_classes;

fn emitted(source: &str) -> String {
    let classes = ingest_library_classes(source.as_bytes(), "test.rb").expect("ingest test source");
    let mut app = App::new();
    for lc in classes {
        app.library_classes.push(lc);
    }
    Analyzer::new(&app).analyze(&mut app);
    roundhouse::session::analyze_and_lower(&mut app);
    emit_library(&app)
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn zero_after_a_chained_safe_navigation_is_not_an_inquiry_comparison() {
    let out = emitted(
        r#"
class Signals
  #: String?
  attr_reader :country

  #: (String? country) -> void
  def initialize(country)
    @country = country
  end
end

class CountryRegion
  #: String
  attr_reader :code

  #: (String code) -> void
  def initialize(code)
    @code = code
  end

  #: (Signals signals) -> bool
  def match?(signals)
    signals.country&.casecmp(code)&.zero? || false
  end
end
"#,
    );
    assert!(!out.contains("== \"zero\""), "zero? must stay a method call:\n{out}");
    assert!(out.contains("zero?"), "{out}");
}

/// The same chain in a model whose `code` is a String column — the shape
/// that actually reached the fold.
#[test]
fn chained_safe_navigation_binds_a_local_per_site() {
    use roundhouse::det::HashMap;
    use std::path::PathBuf;
    let files: HashMap<PathBuf, Vec<u8>> = [
        (PathBuf::from("db/schema.rb"), b"ActiveRecord::Schema.define do\n  create_table \"regions\", force: :cascade do |t|\n    t.string \"code\"\n    t.string \"type\"\n  end\nend\n".to_vec()),
        (PathBuf::from("app/models/region.rb"), b"class Region < ApplicationRecord\nend\n".to_vec()),
        (PathBuf::from("app/models/signals.rb"), b"class Signals\n  #: String?\n  attr_reader :country\n  #: (?country: String?) -> void\n  def initialize(country: nil)\n    @country = country\n  end\nend\n".to_vec()),
        (PathBuf::from("app/models/country.rb"), b"class Country < Region\n  #: (Signals) -> bool\n  def match?(buyer_signals)\n    buyer_signals.country&.casecmp(code)&.zero? || false\n  end\nend\n".to_vec()),
    ].into_iter().collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(files).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let mut files = roundhouse::emit::ruby::emit_lowered_models(&app);
    files.extend(roundhouse::emit::ruby::emit_library(&app));
    let out = &files.iter().find(|f| f.path.ends_with("country.rb")).expect("country.rb").content;
    assert!(!out.contains("== \"zero\""), "zero? must stay a method call:\n{out}");
    assert!(out.contains(".zero?"), "{out}");
}
