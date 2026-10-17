//! A declared type (`#: X`) decided against the installed source of the
//! gems the CRuby tree loads natively, with no `sorbet/` directory.
//!
//! Shopify Core's essentials engine writes `ShopifyTracer =
//! OpenTelemetry.tracer_provider.tracer("Shopify") #:
//! OpenTelemetry::Trace::Tracer`. The emitted Bundler.require loads
//! opentelemetry-api (a dependency of opentelemetry-instrumentation-pg,
//! `project::LOCK_GATED_DEPENDENCIES`) from the gem home; that gem's
//! `lib/opentelemetry/trace/tracer.rb` declares the class. roundhouse.yml's
//! `gem_home:` names the home, and the compiler reads each loaded gem's
//! `lib/**/*.rb` there at its locked version. A constant the source does
//! not declare stays refused; one in a namespace the gem fills with
//! `const_set` under computed names is owned by namespace, and said so.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    opentelemetry-api (1.10.1)\n    opentelemetry-instrumentation-pg (0.33.0)\n      opentelemetry-api (~> 1.0)\n    roaring (2.0.0)\n\nDEPENDENCIES\n  activesupport\n  opentelemetry-instrumentation-pg\n  roaring\n";

const TRACING: &str = "TRACER = OpenTelemetry.tracer_provider.tracer(\"Shopify\") #: OpenTelemetry::Trace::Tracer\n";

const READER: &str = r#"class Traced
  def self.run
    TRACER.in_span("work") { |span| [span.class.name, span.context.valid?] }
  end
end
"#;

/// A gem home laid out as RubyGems installs one: `gems/<name>-<version>/`
/// and `specifications/<name>-<version>.gemspec`. Only declarations
/// matter to the read; the native comparison uses the real gem.
fn fake_gem_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "rh_gem_home_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let files: &[(&str, &str)] = &[
        ("specifications/opentelemetry-api-1.10.1.gemspec", "Gem::Specification.new do |s|\n  s.name = \"opentelemetry-api\".freeze\n  s.require_paths = [\"lib\".freeze]\nend\n"),
        ("gems/opentelemetry-api-1.10.1/lib/opentelemetry-api.rb", "require_relative \"opentelemetry\"\n"),
        ("gems/opentelemetry-api-1.10.1/lib/opentelemetry.rb", "module OpenTelemetry\n  extend self\n  def tracer_provider; end\nend\nrequire_relative \"opentelemetry/trace\"\n"),
        ("gems/opentelemetry-api-1.10.1/lib/opentelemetry/trace.rb", "module OpenTelemetry\n  module Trace\n    extend self\n  end\nend\n"),
        ("gems/opentelemetry-api-1.10.1/lib/opentelemetry/trace/tracer.rb", "module OpenTelemetry\n  module Trace\n    class Tracer\n      def in_span(name); end\n    end\n  end\nend\n"),
        // Not read: the gemspec names `lib` only.
        ("gems/opentelemetry-api-1.10.1/test/opentelemetry/trace/nope.rb", "module OpenTelemetry::Trace\n  class Nope; end\nend\n"),
        ("gems/opentelemetry-instrumentation-pg-0.33.0/lib/opentelemetry/instrumentation/pg/constants.rb", "module OpenTelemetry\n  module Instrumentation\n    module PG\n      module Constants\n        %w[Select Insert].each { |name| const_set(name, Class.new) }\n      end\n    end\n  end\nend\n"),
        // A gem with a native extension: its classes are the C code's.
        ("gems/roaring-2.0.0/ext/roaring/extconf.rb", "require \"mkmf\"\n"),
        ("gems/roaring-2.0.0/lib/roaring.rb", "require \"roaring/roaring\"\nmodule Roaring\n  VERSION = \"2.0.0\"\nend\n"),
    ];
    for (path, content) in files {
        let full = home.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    home
}

/// The tracing API the tree loads, and a Bundler.required gem.
const GEMS: &str = "runtime_gems:\n  - {constant: OpenTelemetry, gem: opentelemetry-api}\n  - {constant: \"OpenTelemetry::Instrumentation::PG\", gem: opentelemetry-instrumentation-pg}\nlock_gated_requires:\n  - {require: roaring, gem: roaring}\n";

fn app(home: &Path) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("test_paths:\n  - test\nextensions:\n  - ext\ngem_home: {}\n", home.display()))
        .write("ext/gems.yml", GEMS)
        .write("Gemfile.lock", LOCK)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("config/initializers/00_tracing.rb", TRACING)
        .write("app/models/traced.rb", READER)
}

fn declared_type_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("declared type")).collect()
}

fn models(dir: &Path) -> String {
    let mut out = String::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir.join("app/models")).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for entry in entries {
        out.push_str(&std::fs::read_to_string(entry).unwrap());
    }
    out
}

#[test]
fn the_installed_source_declares_the_class() {
    let home = fake_gem_home();
    let (dir, errors) = app(&home).emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(!dir.join("sorbet").exists());
    let named = declared_type_errors(&errors);
    assert_eq!(named.len(), 1, "{errors:#?}");
    assert!(
        named[0].contains(
            "OpenTelemetry::Trace::Tracer has no indexed declaration; opentelemetry-api 1.10.1 declares OpenTelemetry::Trace::Tracer (lib/opentelemetry/trace/tracer.rb); the CRuby tree loads opentelemetry-api natively, so the value runs and is typed as opaque"
        ),
        "{}",
        named[0]
    );
    let tracer = std::fs::read_to_string(dir.join("app/models/tracer.rb")).expect("tracer.rb");
    // The body naming `OpenTelemetry` carries the gem-facade anchor's
    // `require_relative` ahead of the authored value, which is untouched.
    assert!(
        tracer.trim().ends_with("\nTRACER = OpenTelemetry.tracer_provider.tracer(\"Shopify\")"),
        "{tracer}"
    );
}

/// The namespace is opentelemetry-api's, read whole, and it declares no
/// `Nope` under `lib/` (its test tree does, which no require path loads).
#[test]
fn a_constant_the_source_does_not_declare_stays_refused() {
    let home = fake_gem_home();
    let (dir, errors) = app(&home)
        .write("config/initializers/00_tracing.rb", "TRACER = Object.new #: OpenTelemetry::Trace::Nope\n")
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        errors.iter().any(|e| e.contains(
            "OpenTelemetry::Trace::Nope has no indexed declaration, and no installed source of a gem the CRuby tree loads declares it (its nearest namespace is opentelemetry-api 1.10.1's, read whole)"
        )),
        "{errors:#?}"
    );
    assert!(
        models(&dir).contains("TRACER = (raise \"roundhouse: declared type not supported (all targets)\")"),
        "{}",
        models(&dir)
    );
}

/// `const_set` under a computed name: the source cannot name the
/// constants, only the namespace they land in.
#[test]
fn a_dynamically_filled_namespace_owns_by_namespace() {
    let home = fake_gem_home();
    let (dir, errors) = app(&home)
        .write(
            "config/initializers/00_tracing.rb",
            "STATEMENT = Object.new #: OpenTelemetry::Instrumentation::PG::Constants::Select\n",
        )
        .write("app/models/traced.rb", "class Traced\n  def self.run = STATEMENT\nend\n")
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    let named = declared_type_errors(&errors);
    assert_eq!(named.len(), 1, "{errors:#?}");
    assert!(
        named[0].contains(
            "opentelemetry-instrumentation-pg 0.33.0 declares no OpenTelemetry::Instrumentation::PG::Constants::Select, but creates constants in OpenTelemetry::Instrumentation::PG::Constants under computed names (lib/opentelemetry/instrumentation/pg/constants.rb: const_set), so OpenTelemetry::Instrumentation::PG::Constants::Select is taken as that gem's by namespace ownership"
        ),
        "{}",
        named[0]
    );
    assert!(models(&dir).contains("STATEMENT = Object.new"), "{}", models(&dir));
}

#[test]
fn other_trees_refuse_it() {
    let home = fake_gem_home();
    let (_, errors) = app(&home).emit(BuildTarget::Spinel);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        errors.iter().any(|e| e.contains("opentelemetry-api 1.10.1 declares OpenTelemetry::Trace::Tracer")
            && e.contains("the spinel tree does not load the gem")),
        "{errors:#?}"
    );
}

/// Without a gem home nothing is known about the gem's constants: the
/// declaration stays refused (no RBI either).
#[test]
fn no_gem_home_no_decision() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\nextensions:\n  - ext\n")
        .write("ext/gems.yml", GEMS)
        .write("Gemfile.lock", LOCK)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("config/initializers/00_tracing.rb", TRACING)
        .emit(BuildTarget::Ruby);
    if std::env::var_os("ROUNDHOUSE_GEM_HOME").is_some() {
        return;
    }
    assert!(
        errors.iter().any(|e| e.contains("OpenTelemetry::Trace::Tracer has no indexed declaration or modeled dependency")),
        "{errors:#?}"
    );
    assert!(models(&dir).contains("TRACER = (raise"), "{}", models(&dir));
}

const SCRIPT: &str = "p [TRACER.class.name, Traced.run]\n";

/// The emitted tree, decided against the real gem home (`GEM_HOME`,
/// Core's), against the same files on plain Ruby with the real gem.
#[test]
#[ignore = "requires ruby with opentelemetry-api (Core's gem home as GEM_HOME)"]
fn runs_like_native() {
    let home = PathBuf::from(std::env::var("GEM_HOME").expect("GEM_HOME names Core's gem home"));
    let native_dir = std::env::temp_dir().join(format!("rh_declared_type_source_native_{}", std::process::id()));
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

    let run = app(&home).run_ruby(SCRIPT);
    let named = declared_type_errors(&run.errors);
    assert!(
        named.iter().all(|e| e.contains("opentelemetry-api 1.10.1 declares OpenTelemetry::Trace::Tracer (lib/opentelemetry/trace/tracer.rb)")),
        "{named:#?}"
    );
    assert_eq!(named.len(), 1, "{named:#?}");
    assert!(run.success, "stdout={}\nstderr={}", run.stdout, run.stderr);
    assert_eq!(run.stdout, native_out, "stderr={}", run.stderr);
}

/// Roaring's bitmaps are its C extension's classes: no Ruby file names
/// them, and the gem builds one, inside the namespace named after it. The
/// read and the declaration run on CRuby, each named with that ownership.
#[test]
fn a_native_extension_owns_its_namespace() {
    let home = fake_gem_home();
    let (dir, errors) = app(&home)
        .write("config/initializers/00_tracing.rb", "BITMAP = Roaring::Bitmap32.new #: Roaring::Bitmap32\n")
        .write("app/models/traced.rb", "class Traced\n  def self.run = BITMAP\nend\n")
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    let fact = "roaring 2.0.0 declares no Roaring::Bitmap32 in Ruby, but builds a native extension and owns Roaring, so Roaring::Bitmap32 is taken as that gem's by namespace ownership";
    assert!(errors.iter().any(|e| e.contains("declared type") && e.contains(fact)), "{errors:#?}");
    assert!(errors.iter().any(|e| e.contains("constant not supported") && e.contains(fact) && e.contains("so the read runs there")), "{errors:#?}");
    assert!(models(&dir).contains("BITMAP = Roaring::Bitmap32.new"), "{}", models(&dir));
}

/// A constant read the installed source declares runs as the gem's, with
/// nothing to name; one it does not stays refused.
#[test]
fn a_read_the_source_declares_runs() {
    let home = fake_gem_home();
    let (dir, errors) = app(&home)
        .write("config/initializers/00_tracing.rb", "TRACER = [Roaring::VERSION, OpenTelemetry::Trace::Tracer]\nNOPE = OpenTelemetry::Trace::Nope\n")
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(!errors.iter().any(|e| e.contains("Roaring::VERSION") || e.contains("Trace::Tracer")), "{errors:#?}");
    assert!(errors.iter().any(|e| e.contains("constant not supported (all targets): OpenTelemetry::Trace::Nope")), "{errors:#?}");
    let models = models(&dir);
    assert!(models.contains("TRACER = [Roaring::VERSION, OpenTelemetry::Trace::Tracer]"), "{models}");
    // The namespace is the locked gem's, so the read stays the gem's: Ruby raises NameError naming it.
    assert!(models.contains("NOPE = OpenTelemetry::Trace::Nope"), "{models}");
    assert!(!models.contains("NOPE = (raise"), "{models}");
}
