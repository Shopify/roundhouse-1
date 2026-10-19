//! A support file holding one constant write and nothing else
//! (e2e sweep-1, round 4). Core's
//! components/platform/essentials/test/support/helpers/
//! fixture_timing_reporter.rb is
//!
//! ```ruby
//! require "method_timing_recorder"
//!
//! FixtureTimingReporter = MethodTimingRecorder.new(:load_fixtures) do … end
//! ```
//!
//! Ingest carries it as a `FileConstants` holder named for the constant,
//! with a file program for the `require`. The program listed no
//! declaration for the holder, so the `require` of the file (from
//! active_record_fixtures_helper.rb) loaded `method_timing_recorder` and
//! left `FixtureTimingReporter` undefined: NameError in
//! `use_active_record_fixtures`, which failed 11 ids that pass natively.
//! A constant a program's own classes include by name now loads where the
//! program writes it.
//!
//! CRuby is the oracle: the same script runs on the source and on the
//! emitted code, and the outputs must agree.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const RECORDER: &str = r##"class ProbeRecorder < Module
  def initialize(label, &block)
    super(&block)
    @label = label
  end

  def label
    @label
  end
end
"##;

const REPORTER: &str = r##"require_relative "probe_recorder"

ProbeReporter = ProbeRecorder.new(:load_fixtures) do
  def greeting
    "reported"
  end
end
"##;

const ENTRY: &str = r##"require_relative "probe_support/reporter"

module ProbeHost
  def self.host
    Class.new { prepend(ProbeReporter) }
  end
end
"##;

const SCRIPT: &str = r##"
p ProbeHost.host.new.greeting
p ProbeReporter.label
p ProbeReporter.class.name
"##;

#[test]
fn a_file_of_one_constant_loads_with_its_programs_require() {
    let expected = "\"reported\"\n:load_fixtures\n\"ProbeRecorder\"\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(native_script())
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("lib/probe_support/probe_recorder.rb", RECORDER)
        .write("lib/probe_support/reporter.rb", REPORTER)
        .write("lib/probe_host.rb", ENTRY)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// The three files as one script: the `require_relative` lines name files
/// that are inline here.
fn native_script() -> String {
    let inline = |source: &str| -> String {
        source.lines().filter(|line| !line.starts_with("require_relative")).collect::<Vec<_>>().join("\n")
    };
    format!("{}\n{}\n{}\n{SCRIPT}", inline(RECORDER), inline(REPORTER), inline(ENTRY))
}
