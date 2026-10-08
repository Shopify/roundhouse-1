//! A compact class header's superclass is read where Ruby reads it: in
//! the lexical nesting at the `class` keyword, which the header's own
//! `A::` prefix does not enter. The Ruby emit nests every header (`module
//! Errs` around `class Unavailable`), so a bare superclass there found
//! the namespace's same-named class first: `class Errs::Unavailable <
//! Fallback` inherited from `Errs::Fallback` and required its file.
//!
//! CRuby is the oracle: the same script runs on the source files and on
//! the emitted app, and the outputs must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const FALLBACK: &str = "class Fallback < StandardError\n  def top = :top\nend\n";

const ERRS: &str = r##"module Errs
  class Fallback < StandardError
    def inner = :inner
  end

  module Api
  end

  # Nested: the lexical nesting is [Errs], so this is Errs::Fallback.
  class Nested < Fallback
  end
end
"##;

/// Compact at the top level: no nesting, so `::Fallback`.
const UNAVAILABLE: &str = "class Errs::Unavailable < Fallback\nend\n";

/// Inside a compact module body: the nesting is [Errs::Api] alone.
const CLIENT: &str = "module Errs::Api\n  class Client < Fallback\n  end\nend\n";

const SCRIPT: &str = r##"
[Errs::Unavailable, Errs::Nested, Errs::Api::Client].each do |k|
  p [k.name, k.superclass.name, k.new.respond_to?(:top), k.new.respond_to?(:inner)]
end
"##;

#[test]
fn a_compact_header_reads_its_superclass_in_the_source_nesting() {
    let expected = "[\"Errs::Unavailable\", \"Fallback\", true, false]\n\
                    [\"Errs::Nested\", \"Errs::Fallback\", false, true]\n\
                    [\"Errs::Api::Client\", \"Fallback\", true, false]\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("{FALLBACK}\n{ERRS}\n{UNAVAILABLE}\n{CLIENT}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/fallback.rb", FALLBACK)
        .write("app/lib/errs.rb", ERRS)
        .write("app/lib/errs/unavailable.rb", UNAVAILABLE)
        .write("app/lib/errs/api/client.rb", CLIENT)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
