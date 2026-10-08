//! A global-variable write in a carried method body runs as Ruby runs it.
//!
//! Core's `gems/ci/lib/ci/test_onboarding/grind/forking_batch_runner.rb`
//! restores `$stdout = STDOUT` / `$stderr = STDERR` in a forked child;
//! the expression ingest refused a global write and the tree carried a
//! bare `nil` in its place, which Core's strict warnings then raised on
//! at boot ("possibly useless use of nil in void context"). A global
//! write is an `LValue::Var` with its `$` name, as a read is; no local
//! pass (the file program's and a railtie body's pre-declared locals)
//! takes it for a local.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIB: &str = r#"require "stringio"

class StdioRestore
  def self.run
    out = StringIO.new
    $stdout = out
    $stderr = out
    puts "captured"
    warn "warned"
    $stdout = STDOUT
    $stderr = STDERR
    [out.string, $stdout.equal?(STDOUT), $stderr.equal?(STDERR)]
  end

  def self.memo
    $rh_memo ||= []
    $rh_memo << :hit
    $rh_count ||= 0
    $rh_count += 1
    $rh_gate &&= :never
    [$rh_memo, $rh_count, $rh_gate]
  end

  def self.in_block
    [1, 2].each { |i| $rh_last = i * 10 }
    $rh_last
  end
end

$rh_file_level ||= :file
$rh_file_level_count = 0
$rh_file_level_count += 1
"#;

const SCRIPT: &str = "p StdioRestore.run\np StdioRestore.memo\np StdioRestore.memo\np StdioRestore.in_block\np [$rh_file_level, $rh_file_level_count]\n";

#[test]
fn global_variable_writes_in_a_method_run_as_natively() {
    let dir = std::env::temp_dir().join(format!("rh_gvar_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stdio_restore.rb"), LIB).unwrap();
    let native = emit_and_run::ruby()
        .arg("-I").arg(&dir).arg("-rstdio_restore").arg("-e").arg(SCRIPT)
        .output()
        .expect("ruby");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(
        native,
        "[\"captured\\nwarned\\n\", true, true]\n[[:hit], 1, nil]\n[[:hit, :hit], 2, nil]\n20\n[:file, 1]\n"
    );
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("lib/stdio_restore.rb", LIB)
        .run_ruby(SCRIPT);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, native);
}
