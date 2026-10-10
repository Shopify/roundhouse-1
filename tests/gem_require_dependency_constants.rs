//! A gem a `GEM_REQUIRES` boot require loads as its dependency:
//! `require "rails-html-sanitizer"` loads loofah (measured in the emitted
//! Core tree). Core's `String#sanitize_without_defaults` reads
//! `Loofah.fragment(dup).text(encode_special_chars: false)`, which was
//! "Loofah is declared by the loofah gem, which no emitted tree loads" and
//! raised in every caller, though the CRuby tree had loaded the gem.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    crass (1.0.7)\n    loofah (2.25.2)\n      crass (~> 1.0.2)\n      nokogiri (>= 1.12.0)\n    nokogiri (1.18.10)\n    rails-html-sanitizer (1.7.1)\n      loofah (~> 2.25, >= 2.25.2)\n      nokogiri (>= 1.15.7)\n\nDEPENDENCIES\n  rails-html-sanitizer\n";

const MODEL: &str = "class PlainText\n  def self.of(html)\n    Loofah.fragment(html).text(encode_special_chars: false)\n  end\nend\n";

fn app(lock: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"rails-html-sanitizer\"\n")
        .write("Gemfile.lock", lock)
        .write("sorbet/rbi/gems/loofah@2.25.2.rbi", "# typed: true\n\nmodule Loofah\n  class << self\n    def fragment(*args); end\n  end\nend\n")
        .write("app/models/plain_text.rb", MODEL)
}

#[test]
fn loofah_reads_as_the_gem_rails_html_sanitizer_loads() {
    let (tree, errors) = app(LOCK).emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("constant not supported")), "{errors:#?}");
    let emitted = std::fs::read_to_string(tree.join("app/models/plain_text.rb")).expect("plain_text.rb");
    assert!(emitted.contains("Loofah.fragment(html)"), "{emitted}");
    assert!(!emitted.contains("roundhouse:"), "{emitted}");
}

#[test]
fn without_rails_html_sanitizer_in_the_lock_loofah_stays_refused() {
    let lock = "GEM\n  remote: https://rubygems.org/\n  specs:\n    crass (1.0.7)\n    loofah (2.25.2)\n      crass (~> 1.0.2)\n\nDEPENDENCIES\n  loofah\n";
    let (_, errors) = app(lock).emit(BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("Loofah is declared by the loofah gem")), "{errors:#?}");
}
