//! An initializer's `require` of a file a natively loaded gem owns is
//! kept.
//!
//! Core's graph_model graphql_open_telemetry.rb initializer opens with
//! `require "opentelemetry/instrumentation/graphql/tracers/graphql_trace"`.
//! The gem's entry file does not load that one and nothing autoloads it,
//! but `rewrite_requires` accepted only runtime ports, app lib files and
//! stdlib: the require was refused (`initializer_unsupported`) and
//! dropped, so every schema's `trace_with(OpenTelemetry::Instrumentation::
//! GraphQL::Tracers::GraphQLTrace)` raised `uninitialized constant
//! ...::Tracers` (6 ids in unit-1000-v2). A feature the installed gem
//! has under its require path now stays a `require`; one no loaded gem
//! owns still refuses by name.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::PathBuf;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    otelish (0.32.0)\n\nDEPENDENCIES\n  otelish\n";

fn gem_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("rh_initializer_native_gem_require_{}", std::process::id()));
    let files: [(&str, &str); 3] = [
        (
            "specifications/otelish-0.32.0.gemspec",
            "Gem::Specification.new do |s|\n  s.name = \"otelish\"\n  s.version = \"0.32.0\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\nend\n",
        ),
        ("gems/otelish-0.32.0/lib/otelish.rb", "module Otelish\n  VERSION = \"0.32.0\"\nend\n"),
        (
            "gems/otelish-0.32.0/lib/otelish/tracers/trace.rb",
            "module Otelish\n  module Tracers\n    module Trace\n    end\n  end\nend\n",
        ),
    ];
    for (path, content) in files {
        let full = home.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    home
}

fn emit(home: &std::path::Path, initializer: &str) -> (emit_and_run::Emitted, Vec<String>) {
    emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!("test_paths:\n  - test\ndatabase_adapter: postgresql\ngem_home: {}\n", home.display()),
        )
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_initializer_native_gem_require\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"otelish\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("config/initializers/otel.rb", initializer)
        .emit(BuildTarget::Ruby)
}

fn holder(dir: &emit_and_run::Emitted) -> String {
    std::fs::read_to_string(dir.join("app/models/roundhouse_initializer_config_initializers_otel.rb"))
        .expect("initializer holder")
}

#[test]
fn a_require_of_a_natively_loaded_gems_file_stays() {
    let home = gem_home();
    let (dir, errors) = emit(&home, "require \"otelish/tracers/trace\"\n\nOtelish::Tracers::Trace\n");
    let _ = std::fs::remove_dir_all(&home);
    let holder = holder(&dir);
    assert!(holder.contains("require \"otelish/tracers/trace\""), "{holder}");
    assert!(!errors.iter().any(|e| e.contains("initializer_unsupported")), "{errors:?}");
}

#[test]
fn a_require_no_loaded_gem_owns_still_refuses() {
    let home = gem_home();
    let (_dir, errors) = emit(&home, "require \"otelish/tracers/missing\"\n\nOtelish::Tracers\n");
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        errors.iter().any(|e| e.contains("otelish/tracers/missing") && e.contains("not a runtime port")),
        "{errors:?}"
    );
}
