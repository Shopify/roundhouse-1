//! Gem sources an app carries as its own code (`gem_sources:` in
//! `roundhouse.yml`). Shopify Core's verdict, hedwig and country_db
//! depend on Rails, so no emitted tree loads them natively; their
//! installed trees are copied into the app and compiled like a `PATH`
//! gem: `lib/` is a support root and the gem's RBI is not the boundary.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    widget (1.0.0)\n\nDEPENDENCIES\n  activesupport\n  widget\n";

const WIDGET_RBI: &str = "# typed: true\nmodule Widget\n  def self.answer; end\nend\n";

fn widget_app(config: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", config)
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/widget@1.0.0.rbi", WIDGET_RBI)
        .write("vendor/gems/widget-1.0.0/lib/widget.rb", "require \"widget/answer\"\n\nmodule Widget\n  def self.answer\n    Answer.new.value\n  end\nend\n")
        .write("vendor/gems/widget-1.0.0/lib/widget/answer.rb", "module Widget\n  class Answer\n    def value\n      42\n    end\n  end\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/oracle.rb", "require \"widget\"\n\nclass Oracle\n  def ask\n    Widget.answer\n  end\nend\n")
        .write("test/oracle_test.rb", r#"require "test_helper"
require "oracle"
class OracleTest < ActiveSupport::TestCase
  test "the carried gem answers" do
    assert_equal 42, Oracle.new.ask
  end
end
"#)
}

const CARRIED: &str = "test_paths:\n  - test\ngem_sources:\n  widget: vendor/gems/widget-1.0.0\n";

#[test]
fn a_carried_gem_runs_in_emitted_ruby() {
    widget_app(CARRIED).run_test("test/models/oracle_test.rb").assert_passes();
}

#[test]
fn a_carried_gem_is_not_refused_as_the_gems() {
    let (_, errors) = widget_app(CARRIED).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn without_the_key_the_gem_stays_behind_its_rbi() {
    let (_, errors) = widget_app("test_paths:\n  - test\n").emit(BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("Widget is declared by the widget gem")), "{errors:#?}");
}

#[test]
fn a_gem_source_outside_the_app_is_refused() {
    for path in ["../widget", "/srv/widget", "vendor/gems/absent"] {
        let tree: HashMap<PathBuf, Vec<u8>> = [
            ("roundhouse.yml", format!("gem_sources:\n  widget: {path}\n")),
            ("vendor/gems/widget-1.0.0/lib/widget.rb", "module Widget\nend\n".to_string()),
        ]
        .into_iter()
        .map(|(p, s)| (PathBuf::from(p), s.into_bytes()))
        .collect();
        let error = ingest_app_from_tree(tree).err().unwrap_or_else(|| panic!("{path} accepted"));
        assert!(error.to_string().contains("gem_sources entry \"widget\""), "{error}");
    }
}
