//! A declared type (`#: X`) naming a constant the emitted CRuby tree
//! loads at runtime but roundhouse does not index.
//!
//! Shopify Core's essentials engine writes `ShopifyTracer =
//! OpenTelemetry.tracer_provider.tracer("Shopify") #:
//! OpenTelemetry::Trace::Tracer` in `config/initializers/00_tracing.rb`.
//! An RBS-inline annotation has no runtime effect: Ruby evaluates the
//! value and never reads the annotation's constant. opentelemetry-api is
//! Rails-free and the CRuby tree Bundler.requires it natively, so the
//! emitted assignment evaluates the authored expression, and `check`
//! names the declaration it could not index, at its site. A declared
//! type only a Rails-dependent gem declares names a constant no emitted
//! tree has, and stays refused.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

/// opentelemetry-instrumentation-pg is one of the CRuby tree's
/// Bundler.require gems; opentelemetry-api is a dependency requiring it
/// loads (`project::LOCK_GATED_DEPENDENCIES`). shopify_errors requires
/// Rails (`project::RAILS_DEPENDENT_GEMS`).
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    opentelemetry-api (1.10.1)\n    opentelemetry-instrumentation-pg (0.33.0)\n      opentelemetry-api (~> 1.0)\n    shopify_errors (1.0.0)\n\nDEPENDENCIES\n  activesupport\n  opentelemetry-instrumentation-pg\n  shopify_errors\n";

/// The parts of Core's Tapioca RBI for opentelemetry-api this app reaches,
/// with `OpenTelemetry::Trace::Tracer` left out, as an RBI that does not
/// enumerate a class leaves it out: the namespace is the gem's.
const OTEL_RBI: &str = r#"# typed: true
module OpenTelemetry
  class << self
    def tracer_provider; end
  end
end
module OpenTelemetry::Trace; end
"#;

const ERRORS_RBI: &str = r#"# typed: true
module ShopifyErrors; end
class ShopifyErrors::Reporter; end
"#;

const TRACING: &str = "TRACER = OpenTelemetry.tracer_provider.tracer(\"Shopify\") #: OpenTelemetry::Trace::Tracer\n";

const READER: &str = r#"class Traced
  def self.run
    TRACER.in_span("work") { |span| [span.class.name, span.context.valid?] }
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/opentelemetry-api@1.10.1.rbi", OTEL_RBI)
        .write("sorbet/rbi/gems/shopify_errors@1.0.0.rbi", ERRORS_RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("config/initializers/00_tracing.rb", TRACING)
        .write("app/models/traced.rb", READER)
}

const REPORTER: &str = "REPORTER = Object.new #: ShopifyErrors::Client\nNOWHERE = Object.new #: Nowhere::Thing\n";

fn declared_type_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("declared type")).collect()
}

#[test]
fn the_cruby_tree_runs_the_authored_value() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    let named = declared_type_errors(&errors);
    assert_eq!(named.len(), 1, "{errors:#?}");
    assert!(
        named[0].contains(
            "OpenTelemetry::Trace::Tracer has no indexed declaration; the opentelemetry-api gem the CRuby tree loads provides it at runtime"
        ),
        "{}",
        named[0]
    );
    let tracer = std::fs::read_to_string(dir.join("app/models/tracer.rb")).expect("tracer.rb");
    assert_eq!(tracer.trim(), "TRACER = OpenTelemetry.tracer_provider.tracer(\"Shopify\")", "{tracer}");
}

/// The RBI enumerates the class: the declaration is indexed, nothing to
/// name. opentelemetry-api is "infrastructure" by family, and its RBI is
/// read because the lock loads it.
#[test]
fn an_rbi_that_declares_the_class_indexes_it() {
    let rbi = format!("{OTEL_RBI}class OpenTelemetry::Trace::Tracer\n  def in_span(name, &block); end\nend\n");
    let (dir, errors) = app().write("sorbet/rbi/gems/opentelemetry-api@1.10.1.rbi", &rbi).emit(BuildTarget::Ruby);
    assert!(declared_type_errors(&errors).is_empty(), "{errors:#?}");
    let tracer = std::fs::read_to_string(dir.join("app/models/tracer.rb")).expect("tracer.rb");
    assert!(!tracer.contains("raise"), "{tracer}");
}

#[test]
fn other_trees_refuse_it() {
    let (_, errors) = app().emit(BuildTarget::Spinel);
    assert!(
        errors.iter().any(|e| e.contains("OpenTelemetry::Trace::Tracer") && e.contains("the spinel tree does not load the gem")),
        "{errors:#?}"
    );
}

/// A namespace only a Rails-dependent gem declares, and one nothing
/// declares, are constants no emitted tree has: refused, by name.
#[test]
fn a_constant_no_tree_has_stays_refused() {
    let (dir, errors) = app().write("config/initializers/10_reporter.rb", REPORTER).emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains(
            "ShopifyErrors::Client is in a namespace of the shopify_errors gem, whose load closure requires Rails"
        )),
        "{errors:#?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("Nowhere::Thing has no indexed declaration or modeled dependency")),
        "{errors:#?}"
    );
    let mut models = String::new();
    for entry in std::fs::read_dir(dir.join("app/models")).unwrap() {
        models.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    for constant in ["REPORTER", "NOWHERE"] {
        assert!(
            models.contains(&format!("{constant} = (raise \"roundhouse: declared type not supported (all targets)\")")),
            "{constant}: {models}"
        );
    }
}

const SCRIPT: &str = "p [TRACER.class.name, Traced.run]\n";

/// The emitted tree against the same files on plain Ruby with the real
/// gem: Bundler.require loads opentelemetry-api, the initializer runs,
/// then the app reads the constant and calls it.
#[test]
#[ignore = "requires ruby with opentelemetry-api (Core's gem home on GEM_PATH)"]
fn runs_like_native() {
    let native_dir = std::env::temp_dir().join(format!("rh_declared_type_native_{}", std::process::id()));
    std::fs::create_dir_all(&native_dir).unwrap();
    std::fs::write(native_dir.join("tracing.rb"), TRACING).unwrap();
    std::fs::write(native_dir.join("traced.rb"), READER).unwrap();
    let native = std::process::Command::new("ruby")
        .current_dir(&native_dir)
        .args(["-ropentelemetry-api", "-e", &format!("load './tracing.rb'\nload './traced.rb'\n{SCRIPT}")])
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&native_dir);
    let native_out = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(native.status.success(), "{native_out}{}", String::from_utf8_lossy(&native.stderr));

    let run = app().run_ruby(SCRIPT);
    assert!(run.success, "stdout={}\nstderr={}", run.stdout, run.stderr);
    assert_eq!(run.stdout, native_out, "stderr={}", run.stderr);
}
