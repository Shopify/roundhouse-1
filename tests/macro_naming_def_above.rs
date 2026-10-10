//! A class or module body calls a macro of an extended module that
//! names, by symbol, a def written above the call.
//!
//! Core's ActsAsTheme defines `generate_unique_cityhash` and then calls
//! statsd-instrument's `statsd_measure :generate_unique_cityhash, ...`,
//! which raises "could not find method" unless the method is defined.
//! The emitted module replayed the call ahead of every def, so loading
//! it raised, and with it every `themes` fixture. The call must run
//! after the defs above it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const INSTRUMENT: &str = r#"module Instrument
  def measure(name)
    raise ArgumentError, "could not find method #{name} for #{self.name}" unless method_defined?(name)
    wrapper = Module.new
    wrapper.define_method(name) { |*args| "measured(#{super(*args)})" }
    prepend(wrapper)
  end
end
"#;

const THEMED: &str = r#"module Themed
  extend Instrument

  def assets = [1, 2]

  def cityhash
    assets.sum
  end
  measure :cityhash

  def after = :after
end
"#;

const REPORT: &str = r#"class Report
  extend Instrument

  def total = 7
  measure :total
end
"#;

const SCRIPT: &str = r#"class Host; include Themed; end
p [Host.new.cityhash, Host.new.after, Report.new.total]
"#;

#[test]
fn a_macro_naming_a_def_above_it_runs_after_that_def() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{INSTRUMENT}\n{THEMED}\n{REPORT}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[\"measured(3)\", :after, \"measured(7)\"]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/instrument.rb", INSTRUMENT)
        .write("app/models/themed.rb", THEMED)
        .write("app/models/report.rb", REPORT)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
