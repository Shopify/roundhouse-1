//! A gem body that registers its own files' constants by a glob, as
//! activemerchant 1.137.0's billing/gateways.rb does:
//!
//! ```ruby
//! load_path = Pathname.new(__FILE__ + '/../../..')
//! Dir[File.dirname(__FILE__) + '/gateways/**/*.rb'].each do |filename|
//!   gateway_name      = File.basename(filename, '.rb')
//!   gateway_classname = "#{gateway_name}_gateway".camelize
//!   gateway_filename  = Pathname.new(filename).relative_path_from(load_path).sub_ext('')
//!   autoload(gateway_classname, gateway_filename)
//! end
//! ```
//!
//! The emitted module body dropped the loop without a word, so
//! `ActiveMerchant::Billing::BogusGateway` was a NameError unless a file
//! required gateways/bogus.rb itself. The glob is evaluated at compile
//! time over the compiled gem's files, and each constant it names gets
//! the autoload Ruby would register: the constant loads on first
//! reference, not at boot. A gateway file with its own file-level
//! statements (`require "nokogiri"` above the class) autoloads through
//! its file program. And `APPROVED, DECLINED = 1, 2` in a class body,
//! dropped the same silent way, is the constants it writes.
//!
//! A class or module body statement that is still not carried is now a
//! diagnostic, and the emitted file says so where it loads.
//!
//! The control is CRuby running the same gem against the pinned
//! ActiveSupport (tests/support/rails_oracle.rb), which provides the
//! `String#camelize` the gem relies on.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

const FILES: &[(&str, &str)] = &[
    ("lib/paygate.rb", "require \"paygate/base\"\nrequire \"paygate/gateways\"\n"),
    (
        "lib/paygate/base.rb",
        "module Paygate\n  class Base\n    @@all = []\n\n    def self.inherited(subclass)\n      super\n      @@all << subclass.name\n    end\n\n    def self.all = @@all\n  end\nend\n",
    ),
    (
        "lib/paygate/gateways.rb",
        "require \"pathname\"\n\nmodule Paygate\n  load_path = Pathname.new(__FILE__ + \"/../..\")\n  Dir[File.dirname(__FILE__) + \"/gateways/**/*.rb\"].each do |filename|\n    gateway_name      = File.basename(filename, \".rb\")\n    gateway_classname = \"#{gateway_name}_gateway\".camelize\n    gateway_filename  = Pathname.new(filename).relative_path_from(load_path).sub_ext(\"\")\n\n    autoload(gateway_classname, gateway_filename)\n  end\nend\n",
    ),
    ("lib/paygate/gateways/bogus.rb", "module Paygate\n  class BogusGateway < Base\n    CODE = :ok\n  end\nend\n"),
    (
        "lib/paygate/gateways/card_net.rb",
        "require \"set\"\n\nmodule Paygate\n  class CardNetGateway < Base\n    APPROVED, DECLINED = 1, \"declined\"\n    SEEN = Set.new([APPROVED])\n\n    def self.codes = [APPROVED, DECLINED, SEEN.include?(1)]\n  end\nend\n",
    ),
];

const SCRIPT: &str = r#"p Paygate::Base.all
p Paygate::BogusGateway::CODE
p Paygate::CardNetGateway.codes
p Paygate::Base.all
p Paygate.autoload?(:BogusGateway)
"#;

const EXPECTED: &str = "[]\n:ok\n[1, \"declined\", true]\n[\"Paygate::BogusGateway\", \"Paygate::CardNetGateway\"]\nnil\n";

fn native() -> String {
    let dir = std::env::temp_dir().join(format!("rh_gem_glob_autoload_native_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in FILES {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let main = format!(
        "require {ORACLE:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\n\
         require \"active_support/core_ext/string/inflections\"\n$LOAD_PATH.unshift({:?})\nrequire \"paygate\"\n{SCRIPT}",
        dir.join("lib").display().to_string()
    );
    std::fs::write(dir.join("main.rb"), main).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

fn overlay() -> emit_and_run::Overlay {
    let mut overlay = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  paygate: vendor/gems/paygate-1.0.0\n")
        .write("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    paygate (1.0.0)\n\nDEPENDENCIES\n  paygate\n")
        .write("config/application.rb", "require \"paygate\"\n");
    for (path, text) in FILES {
        overlay = overlay.write(&format!("vendor/gems/paygate-1.0.0/{path}"), text);
    }
    overlay
}

#[test]
fn a_gem_glob_autoload_loop_registers_each_constant_as_native() {
    assert_eq!(native(), EXPECTED, "native control");
    let run = overlay().run_ruby(SCRIPT);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
    assert!(!run.stderr.contains("file_statement_not_carried"), "{}", run.stderr);
}

#[test]
fn a_dropped_module_body_statement_is_reported() {
    // A global write in a module body: not carried, and now said so.
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  paygate: vendor/gems/paygate-1.0.0\n")
        .write("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    paygate (1.0.0)\n\nDEPENDENCIES\n  paygate\n")
        .write("config/application.rb", "require \"paygate\"\n")
        .write(
            "vendor/gems/paygate-1.0.0/lib/paygate.rb",
            "module Paygate\n  $paygate_loaded = true\n\n  def self.ready = true\nend\n",
        )
        .run_ruby("p Paygate.ready\n");
    assert!(
        run.errors.iter().any(|e| e.contains("a global variable write in a class or module body is not carried")),
        "{:#?}",
        run.errors
    );
    assert!(run.stderr.contains("roundhouse: error[file_statement_not_carried]"), "{}", run.stderr);
    assert!(run.stdout.ends_with("true\n"), "{}\n{}", run.stdout, run.stderr);
}
