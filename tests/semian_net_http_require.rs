//! `require "semian/net_http"` at the top of a file the PostgreSQL CRuby
//! tree compiles (Shopify Core reaches it through opentelemetry-shopify
//! 1.22.0's exporters/otlp.rb:4, a gem source). The emitted file does not
//! carry a bare gem require, and the gem's own file must never load: it
//! begins `require "semian"`, whose lib/semian.rb redefines `Semian` over
//! the runtime port and registers an `ActiveSupport.on_load(:active_record)`
//! that requires semian/rails. So runtime/gem_facades.rb loads the port
//! (runtime/semian_net_http.rb, held to the gem by tests/semian_runtime.rb
//! part net_http) with the gems, where Bundler.require would have loaded
//! it, after `ActiveSupport.on_load` is defined.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    otelish (1.0.0)\n    semian (0.28.3)\n\nDEPENDENCIES\n  otelish\n  semian\n";

fn app(require: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            "test_paths:\n  - test\ngem_sources:\n  otelish: vendor/gems/otelish-1.0.0\ndatabase_adapter: postgresql\n",
        )
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_semian_net_http\n")
        .write("Gemfile.lock", LOCK)
        .write(
            "vendor/gems/otelish-1.0.0/lib/otelish.rb",
            &format!("{require}\n\nmodule Otelish\n  def self.client(host, port)\n    Net::HTTP.new(host, port, semian: false)\n  end\nend\n"),
        )
        .write("lib/oracle.rb", "require \"otelish\"\n\nclass Oracle\n  def ask\n    Otelish.client(\"127.0.0.1\", 1).disabled?\n  end\nend\n")
}

fn facades(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb")
}

#[test]
fn a_top_level_semian_net_http_require_loads_the_port_with_the_gems() {
    let (dir, _) = app("require 'semian/net_http'").emit(BuildTarget::Ruby);
    let facades = facades(&dir);
    let hooks = facades.find("require_relative \"active_support_lazy_load_hooks\"").expect("hooks first");
    // semian's features are the ports for a natively loaded gem too.
    let features = facades.find("$LOAD_PATH.unshift(File.expand_path(\"features\", __dir__))").unwrap_or_else(|| panic!("{facades}"));
    assert!(hooks < features, "{facades}");
    for (feature, port) in [("semian", "../semian"), ("semian/net_http", "../../semian_net_http"), ("semian/adapter", "../../semian_adapter")] {
        let shim = std::fs::read_to_string(dir.join(format!("runtime/features/{feature}.rb"))).expect("feature file");
        assert!(shim.contains(&format!("require_relative \"{port}\"")), "{shim}");
    }
    let gems = facades.find("require gem_name").expect("guarded gem requires");
    let port = facades.find("require_relative \"semian_net_http\"").unwrap_or_else(|| panic!("{facades}"));
    assert!(hooks < gems && gems < port, "{facades}");
    let carried = std::fs::read_to_string(dir.join("runtime/semian_net_http.rb")).expect("port carried");
    assert!(carried.contains("require_relative \"semian_adapter\""), "{carried}");
    assert!(dir.join("runtime/semian_adapter.rb").exists());
    // The authored call is carried as written.
    let otelish = std::fs::read_to_string(dir.join("app/models/otelish.rb")).expect("otelish.rb");
    assert!(otelish.contains("Net::HTTP.new(host, port, semian: false)"), "{otelish}");
}

#[test]
fn without_a_load_time_require_the_port_is_not_loaded() {
    let (dir, _) = app("# no semian").emit(BuildTarget::Ruby);
    assert!(!facades(&dir).contains("semian_net_http"));
    // An indented (lazy) require is not a load-time one.
    let (dir, _) = app("module Later\n  def self.load!\n    require \"semian/net_http\"\n  end\nend").emit(BuildTarget::Ruby);
    assert!(!facades(&dir).contains("semian_net_http"));
}

#[test]
#[ignore = "needs the semian 0.28.3 gem (its compiled bulkhead) on GEM_PATH"]
fn the_emitted_port_answers_and_the_gem_lib_never_loads() {
    let (dir, _) = app("require 'semian/net_http'").emit(BuildTarget::Ruby);
    let out = emit_and_run::ruby()
        .args([
            "-e",
            // The port as gem_facades.rb loads it (the rest of that file is
            // the guarded gem list, environment-dependent here), then the
            // gem's own features, as a natively loaded gem requires them.
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/semian_net_http'\n\
             require 'semian'\n\
             require 'semian/adapter'\n\
             require 'semian/net_http'\n\
             p Net::HTTP.new('127.0.0.1', 1, semian: false).disabled?\n\
             p ActiveSupport.respond_to?(:on_load)\n\
             p $LOADED_FEATURES.grep(%r{/gems/semian-[^/]+/lib/}).map { |f| f[%r{/gems/semian-[^/]+/(lib/.*)}, 1] }\n",
        ])
        .current_dir(&*dir)
        .output()
        .expect("spawn ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "true\ntrue\n[\"lib/semian/semian.so\"]\n");
}
