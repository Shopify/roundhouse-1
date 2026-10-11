//! A paren-less call whose first argument starts like an operator.
//!
//! `add_filter %r{…}` printed as `add_filter /…/` warns "ambiguous
//! first argument" under `ruby -w` (parse.y `warn_balanced`), and a
//! harness that raises on warnings rejects the file the source loads
//! cleanly. Such a call now takes its arguments in parentheses. Native
//! Ruby on the same source is the oracle: both run warning-free.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const FILTERS: &str = r##"module Warnlab
  class Filters
    def self.add_filter(arg)
      (@seen ||= []) << arg
    end

    def self.seen
      @seen
    end

    def self.install
      add_filter %r{^((?!/skip/).)*$}
      add_filter %r{/test/}
      add_filter -1
      seen.map { |s| s.is_a?(Regexp) ? s.source : s }
    end
  end
end
"##;

#[test]
fn a_command_call_with_a_regexp_first_argument_loads_without_warnings() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/warnlab/filters.rb", FILTERS)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let path = emitted.join("app/models/warnlab/filters.rb");
    let source = std::fs::read_to_string(&path).expect("emitted filters");
    let check = emit_and_run::ruby().arg("-wc").arg(&path).output().expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "{stderr}\n{source}");
    assert!(!stderr.contains("warning"), "{stderr}\n{source}");
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-W2")
        .arg("-e")
        .arg(format!(
            "require {main:?}\nMain.configure_default_adapter!\nWarning.singleton_class.prepend(Module.new {{ def warn(m, **) = raise(m) }})\nrequire_relative \"app/models/warnlab/filters\"\np Warnlab::Filters.install"
        ))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "{}\n{source}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "[\"^((?!/skip/).)*$\", \"/test/\", -1]\n",
        "{source}"
    );
}
