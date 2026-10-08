//! The core extensions native Rails boot loads before any app file, on
//! the PostgreSQL CRuby tree (project.rs::BOOT_LOADED_CORE_EXTENSIONS),
//! against the Rails native Core runs (Shopify/rails 5813559f4aed;
//! tests/support/rails_oracle.rb).
//!
//! `require "rails"` loads Class#class_attribute and Module#mattr_*, and
//! the bootstrap initializer :load_active_support requires
//! active_support/all, so app code (and a gem source compiled with it)
//! calls them with no require: activemerchant 1.137.0's PostsData runs
//! `base.class_attribute :ssl_strict` in its `included` hook. The tree
//! loaded the ports only where a file required their feature, so such a
//! call raised NoMethodError. Now boot loads each port where native boot
//! loads it when emitted app code calls it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\n\
DEPENDENCIES\n  i18n\n";

/// A gem compiled with the app (`gem_sources:`, as Core carries
/// activemerchant 1.137.0), whose class bodies run as the gem's own Ruby
/// does: a mixin whose `included` hook defines class attributes on the
/// includer, as activemerchant's PostsData does, and module attributes.
const POSTSY: &str = r#"module Postsy
  module PostsData
    PROXY = %i[proxy_address proxy_port].freeze

    def self.included(base)
      base.class_attribute :ssl_strict
      base.ssl_strict = true
      base.class_attribute :open_timeout, instance_writer: false
      base.open_timeout = 60
      PROXY.each { |name| base.class_attribute(name) }
      base.proxy_port = 8080
      base.cattr_accessor :retries
      base.retries = 3
    end
  end

  module Billing
    class Gateway
      include Postsy::PostsData

      mattr_reader :service, default: "gateway"
    end

    class StrictGateway < Gateway
    end

    # A second includer, as activemerchant's Braintree TokenNonce.
    class TokenNonce
      include PostsData
    end
  end
end
"#;

const QUOTA: &str = r#"class Quota
  def self.size_for(n)
    n.megabytes
  end

  def self.window_for(n)
    n.days
  end

  def self.lines
    # Through the subclass's writer from outside its body: a gem
    # source's class-body `self.ssl_strict = false` through a writer an
    # included module's hook defines at run time is not replayed yet.
    Postsy::Billing::StrictGateway.ssl_strict = false
    lazy = Postsy::Billing::StrictGateway.new
    [
      "ssl_strict: #{Postsy::Billing::Gateway.ssl_strict} #{Postsy::Billing::StrictGateway.ssl_strict} #{lazy.ssl_strict} #{lazy.ssl_strict?}",
      "open_timeout: #{Postsy::Billing::Gateway.open_timeout} #{lazy.open_timeout} #{lazy.respond_to?(:open_timeout=)}",
      "nonce: #{Postsy::Billing::TokenNonce.ssl_strict} #{Postsy::Billing::TokenNonce.new.open_timeout} #{Postsy::Billing::TokenNonce.proxy_port} #{Postsy::Billing::TokenNonce.proxy_address.inspect}",
      "retries: #{Postsy::Billing::Gateway.retries} #{Postsy::Billing::StrictGateway.retries} #{lazy.retries}",
      "service: #{Postsy::Billing::Gateway.service} #{lazy.service}",
      "bytes: #{size_for(3)} #{size_for(1.5)} #{2.kilobytes}",
      "window: #{window_for(2).class} #{window_for(2).to_i} #{3.hours.to_i} #{90.minutes.in_hours}",
      "to_fs: #{1234567.to_fs(:delimited)} #{12345.to_fs(:human_size)} #{0.5.to_fs(:percentage, precision: 1)}",
    ]
  end
end
"#;

/// The gem's entry, which native Bundler.require loads before the app's
/// files; and the app file.
const FILES: [(&str, &str); 2] = [
    ("vendor/gems/postsy-1.0.0/lib/postsy.rb", POSTSY),
    ("app/models/quota.rb", QUOTA),
];

fn probe() -> emit_and_run::Overlay {
    FILES.iter().fold(
        emit_and_run::empty_app()
            .write(
                "roundhouse.yml",
                "test_paths:\n  - test\ndatabase_adapter: postgresql\ngem_sources:\n  postsy: vendor/gems/postsy-1.0.0\n",
            )
            .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"i18n\"\n")
            .write("Gemfile.lock", LOCK)
            .write("config.ru", "")
            .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
            .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_boot_core_extensions_unused\n")
            .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
            .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        |app, (path, text)| app.write(path, text),
    )
}

fn run(mode: &str, dir: &Path, files: &[&str]) -> String {
    let mut cmd = emit_and_run::ruby();
    cmd.arg(root().join("tests/boot_loaded_core_extensions.rb")).arg("run").arg(mode).arg(dir).args(files);
    let out = cmd.output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{mode} failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

/// What Rails answers (checked natively), so two equal wrong answers
/// still fail.
const EXPECTED: &str = "ssl_strict: true false false false\n\
open_timeout: 60 60 false\n\
nonce: true 60 8080 nil\n\
retries: 3 3 3\n\
service: gateway gateway\n\
bytes: 3145728 1572864.0 2048\n\
window: ActiveSupport::Duration 172800 10800 1.5\n\
to_fs: 1,234,567 12.1 KB 0.5%\n";

#[test]
fn app_code_calls_the_core_extensions_rails_boot_loaded() {
    let (tree, errors) = probe().emit(BuildTarget::Ruby);
    let unexpected: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(unexpected.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    for port in ["active_support_module_attribute_accessors", "active_support_class_attribute", "active_support_numeric"] {
        assert!(facades.contains(&format!("require_relative \"{port}\"\n")), "{port} not loaded at boot:\n{facades}");
    }
    let scratch = tree.parent().expect("scratch").to_path_buf();
    let app = scratch.join("app");
    let files: Vec<&str> = FILES.iter().map(|(path, _)| *path).collect();
    let native = run("native", &app, &files);
    assert_eq!(native, EXPECTED, "native");
    let emitted = run("emitted", &tree, &[]);
    assert_eq!(emitted, native, "emitted");
}

/// App code that calls none of them loads none at boot (a feature's
/// require still loads its port from runtime/features).
#[test]
fn app_code_that_calls_none_loads_none() {
    let (tree, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"i18n\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config.ru", "")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_boot_core_extensions_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/plain.rb", "class Plain\n  # class_attribute :x, 2.days\n  def self.name_of(x) = x.to_s\nend\n")
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    for port in ["active_support_module_attribute_accessors", "active_support_class_attribute", "active_support_numeric"] {
        assert!(!facades.contains(&format!("require_relative \"{port}\"\n")), "{port} loaded at boot:\n{facades}");
    }
    assert!(tree.join("runtime/features/active_support/core_ext/class/attribute.rb").exists());
}
