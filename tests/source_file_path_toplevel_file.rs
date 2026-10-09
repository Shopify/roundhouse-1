//! `__FILE__` from a source file the Ruby family emits elsewhere is
//! written `::File.expand_path(<path>, __dir__)`: inside a namespace that
//! has its own `File` (rack's `Rack::File`, which rack-oauth2's
//! `module Rack; module OAuth2; VERSION = ::File.read(::File.join(
//! ::File.dirname(__FILE__), "../../VERSION"))` sits in), a bare `File`
//! is that constant, and the boot raised NoMethodError on `expand_path`.
//!
//! CRuby running the authored source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"module Shelf
  class File
  end

  module Reader
    HERE = ::File.basename(__FILE__)
  end
end
"#;

#[test]
fn source_file_path_reads_the_top_level_file_inside_a_namespace_with_its_own() {
    let run = emit_and_run::real_blog().write("app/services/shelf.rb", SOURCE).run_ruby("p Shelf::Reader::HERE");
    run.assert_passes();
    assert_eq!(run.stdout, "\"shelf.rb\"\n", "{}", run.stderr);
}
