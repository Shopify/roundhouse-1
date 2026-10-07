//! `NAME = value unless const_defined?(:NAME)` registers the constant
//! and keeps its guard.
//!
//! Core's `TaxesApi` writes its timeouts as `FILING_OPERATION_TIMEOUT =
//! 20.0 unless const_defined?(:FILING_OPERATION_TIMEOUT)`, so that a value
//! defined before the module body runs (a test's shorter timeout) wins.
//! The class-body walk skipped every `unless` statement that held no
//! `def`: no constant was registered, each `TaxesApi::…` read reported
//! "constant not supported", and the emitted module did not define it.
//! Now the constant is typed from its value and the Ruby tree writes the
//! guard back. The control runs the same script on CRuby against the
//! source file.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const TAXES_API: &str = r#"module TaxesApi
  GRAPHQL_RETRY_TIMES = 1 unless const_defined?(:GRAPHQL_RETRY_TIMES)
  FILING_OPERATION_TIMEOUT = 20.0 unless self.const_defined?("FILING_OPERATION_TIMEOUT")
  BANNER = <<-TXT unless const_defined?(:BANNER)
    taxes
  TXT
  DOUBLED = [1, 2].map do |k|
    k * 2
  end unless const_defined?(:DOUBLED)

  def self.timeout
    FILING_OPERATION_TIMEOUT * 2
  end
end
"#;

const USER: &str = r#"class TaxesUser
  def retries
    TaxesApi::GRAPHQL_RETRY_TIMES + 1
  end
end
"#;

/// `$taxes_api` names the file that defines the module: the source for
/// the control, the emitted file for the emitted tree. Loading it again
/// over a predefined value must keep that value, without the warning an
/// unguarded reassignment prints.
const SCRIPT: &str = r#"p [TaxesApi::GRAPHQL_RETRY_TIMES, TaxesApi::FILING_OPERATION_TIMEOUT, TaxesApi.timeout, TaxesApi::BANNER]
p [TaxesUser.new.retries, TaxesApi::DOUBLED]
TaxesApi.send(:remove_const, :FILING_OPERATION_TIMEOUT)
TaxesApi.const_set(:FILING_OPERATION_TIMEOUT, 0.5)
TaxesApi.send(:remove_const, :BANNER)
TaxesApi.const_set(:BANNER, "short")
$stderr = StringIO.new
load $taxes_api
warnings = $stderr.string
$stderr = STDERR
p [TaxesApi::FILING_OPERATION_TIMEOUT, TaxesApi.timeout, TaxesApi::BANNER, TaxesApi::GRAPHQL_RETRY_TIMES]
p warnings
"#;

const EXPECTED: &str = "[1, 20.0, 40.0, \"    taxes\\n\"]\n[2, [2, 4]]\n[0.5, 1.0, \"short\", 1]\n\"\"\n";

#[test]
fn a_guarded_constant_is_registered_and_a_predefined_value_wins() {
    let dir = std::env::temp_dir().join(format!("guarded-constant-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let source = dir.join("taxes_api.rb");
    std::fs::write(&source, TAXES_API).expect("write source");
    let native = emit_and_run::ruby()
        .arg("-rstringio")
        .arg("-e")
        .arg(format!("$taxes_api = {:?}\nload $taxes_api\n{USER}\n{SCRIPT}", source.to_str().unwrap()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);

    let run = emit_and_run::empty_app()
        .write("app/models/taxes_api.rb", TAXES_API)
        .write("app/models/taxes_user.rb", USER)
        .run_ruby(&format!(
            "require \"stringio\"\n$taxes_api = File.expand_path(\"app/models/taxes_api.rb\")\n{SCRIPT}"
        ));
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/taxes_api.rb")).expect("taxes_api.rb");
    assert!(
        emitted.contains("GRAPHQL_RETRY_TIMES = 1 unless const_defined?(:GRAPHQL_RETRY_TIMES)"),
        "{emitted}"
    );
}
