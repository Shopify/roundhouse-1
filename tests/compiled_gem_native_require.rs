//! A native gem requiring a feature of a gem the tree compiles from
//! source (`gem_sources:`) loads the compiled gem.
//!
//! Shopify Core compiles paquito 1.0.2. shopify_session 7.40.0, a native
//! gem, defines `ShopifySession::Rack::SessionMiddleware` in
//! `shopify_session/rack`, whose `utils/coder.rb` requires "paquito".
//! The gem home's paquito is registered as loaded and kept off the load
//! path (`project::compiled_gem_native_copies_block`), so that require
//! raised LoadError, and developer_dashboard_session_middleware.rb's
//! `require "shopify_session/rack"` left its superclass undefined
//! (NameError in the r17-pre2 boot survey; `cannot load such file --
//! paquito` for the same reason at components/apps/lib/apps.rb). The
//! require of a compiled gem's feature now loads the emitted files of
//! that feature's source, and the gem home's copy is never read.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::PathBuf;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    paqlike (1.0.2)\n    sessionish (7.40.0)\n      paqlike\n\nDEPENDENCIES\n  paqlike\n  sessionish\n";

fn gem_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "rh_compiled_gem_native_require_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let spec = |name: &str, version: &str| {
        format!("Gem::Specification.new do |s|\n  s.name = \"{name}\"\n  s.version = \"{version}\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\nend\n")
    };
    let files: Vec<(String, String)> = vec![
        ("specifications/paqlike-1.0.2.gemspec".into(), spec("paqlike", "1.0.2")),
        // The gem home's copy, which must not load over the compiled one.
        ("gems/paqlike-1.0.2/lib/paqlike.rb".into(), "module Paqlike\n  NATIVE = true\n  ORIGIN = :native\nend\n".into()),
        ("gems/paqlike-1.0.2/lib/paqlike/codec.rb".into(), "module Paqlike\n  class Codec\n    NATIVE = true\n  end\nend\n".into()),
        ("specifications/sessionish-7.40.0.gemspec".into(), spec("sessionish", "7.40.0")),
        ("gems/sessionish-7.40.0/lib/sessionish.rb".into(), "module Sessionish\nend\n".into()),
        // dry-schema's shape: a Zeitwerk-managed gem whose `Macros` is a
        // directory with no macros.rb (an implicit namespace).
        ("specifications/zwish-1.14.1.gemspec".into(), spec("zwish", "1.14.1")),
        ("gems/zwish-1.14.1/lib/zwish.rb".into(), "require \"zeitwerk\"\n\nmodule Zwish\nend\n\nloader = Zeitwerk::Loader.for_gem\nloader.setup\n".into()),
        ("gems/zwish-1.14.1/lib/zwish/version.rb".into(), "module Zwish\n  VERSION = \"1.14.1\"\nend\n".into()),
        ("gems/zwish-1.14.1/lib/zwish/macros/core.rb".into(), "module Zwish\n  module Macros\n    class Core\n      def self.label\n        \"core macro\"\n      end\n    end\n  end\nend\n".into()),
        (
            "gems/sessionish-7.40.0/lib/sessionish/rack.rb".into(),
            "require \"paqlike\"\nrequire \"paqlike/codec\"\n\nmodule Sessionish\n  class Middleware\n    CODER = [Paqlike::ORIGIN, Paqlike::Codec.label]\n  end\nend\n".into(),
        ),
    ];
    for (path, content) in files {
        let full = home.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    home
}

fn emit_app(home: &std::path::Path) -> emit_and_run::Emitted {
    let (dir, errors) = emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!(
                "test_paths:\n  - test\ndatabase_adapter: postgresql\ngem_home: {}\ngem_sources:\n  paqlike: vendor/gems/paqlike-1.0.2\n",
                home.display()
            ),
        )
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_compiled_gem_native_require\n")
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/paqlike-1.0.2/lib/paqlike.rb", "require \"paqlike/codec\"\n\nmodule Paqlike\n  ORIGIN = :compiled\nend\n")
        .write(
            "vendor/gems/paqlike-1.0.2/lib/paqlike/codec.rb",
            "module Paqlike\n  class Codec\n    def self.label\n      \"compiled codec\"\n    end\n  end\nend\n",
        )
        .write("lib/oracle.rb", "class Oracle\n  def self.codec\n    Paqlike::Codec.label\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let own: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(own.is_empty(), "{own:?}");
    dir
}

const BOOT: &str = "Gem.paths = { 'GEM_PATH' => [ENV['RH_TEST_GEMS'], *Gem.path].join(File::PATH_SEPARATOR) }\n\
     require_relative 'runtime/active_support_lazy_load_hooks'\n\
     $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
     require_relative 'runtime/module_delegate'\n\
     require_relative 'runtime/active_support_inflector'\n\
     require_relative 'runtime/rails'\n\
     Rails.env_name = 'test'\n\
     require_relative 'runtime/rails_railtie'\n\
     require_relative 'runtime/rails_engine'\n\
     require_relative 'runtime/active_support_deprecation'\n";

#[test]
fn a_native_gems_require_of_a_compiled_gem_loads_the_compiled_files() {
    let home = gem_home();
    let dir = emit_app(&home);
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    let table = &models[models.find("COMPILED_GEM_FEATURES = {").unwrap_or_else(|| panic!("{models}"))..];
    let table = &table[..table.find("}.freeze").expect("table end")];
    assert!(table.contains("\"paqlike\" => [\"models/roundhouse_files/vendor/gems/paqlike-1.0.2/lib/paqlike\"]"), "{table}");
    assert!(table.contains("\"paqlike/codec\" => [\"models/paqlike/codec\"]"), "{table}");

    let script = "Gem.paths = { 'GEM_PATH' => [ENV['RH_TEST_GEMS'], *Gem.path].join(File::PATH_SEPARATOR) }\n\
         require_relative 'runtime/active_support_lazy_load_hooks'\n\
         $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
         require_relative 'runtime/module_delegate'\n\
         require_relative 'runtime/active_support_inflector'\n\
         require_relative 'runtime/rails'\n\
         Rails.env_name = 'test'\n\
         require_relative 'runtime/rails_railtie'\n\
         require_relative 'runtime/rails_engine'\n\
         require_relative 'runtime/active_support_deprecation'\n\
         require_relative 'app/models'\n\
         require 'sessionish'\n\
         p require('sessionish/rack')\n\
         p Sessionish::Middleware::CODER\n\
         p [defined?(Paqlike::NATIVE), defined?(Paqlike::Codec::NATIVE)]\n\
         p require('paqlike')\n\
         p $LOADED_FEATURES.grep(%r{/paqlike-1.0.2/lib/}).grep_v(%r{/app/models/})\n";
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .env("RH_TEST_GEMS", &home)
        .args(["-e", script])
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&home);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    // As native Ruby runs it with the app's paqlike source on the load
    // path (Bundler's view of a gem the app vendors): the source's
    // ORIGIN and label, the gem home's copy never loaded, and a second
    // require of the feature a no-op.
    assert_eq!(stdout, "true\n[:compiled, \"compiled codec\"]\n[nil, nil]\nfalse\n[]\n", "{stderr}");
}

/// Kernel#require with a module prepended to Kernel (a require hook such
/// as bin/run-target-boot-survey.rb's, which sends a call made under any
/// other name than `require` to RubyGems' own): the compiled-gem require
/// goes on to Kernel's own method, so Zeitwerk's decoration of it still
/// autovivifies an implicit namespace. With an `alias_method` it took the
/// hook instead, and dry-schema's `Dry::Schema::Macros` (r17-pre4 boot
/// survey) was `cannot load such file -- .../dry/schema/macros`.
#[test]
fn a_require_hook_prepended_to_kernel_still_reaches_zeitwerks_implicit_namespaces() {
    let home = gem_home();
    let dir = emit_app(&home);
    let hook = "GEM_REQUIRE = Kernel.instance_method(:require)\n\
         module Hook\n  private def require(path)\n    return GEM_REQUIRE.bind_call(self, path) unless __callee__ == :require\n    (Kernel.instance_method(:require).super_method || GEM_REQUIRE).bind_call(self, path)\n  end\nend\n\
         Kernel.prepend(Hook) if ENV['RH_HOOK']\n";
    let observe = "require 'zwish'\np Zwish::Macros::Core.label\np require('sessionish/rack')\np Sessionish::Middleware::CODER\n";
    let mut outs = Vec::new();
    // Not (hooked, Zeitwerk after app/models): Zeitwerk's own `def
    // require` then replaces Kernel's, and its alias takes the hook,
    // whose renamed call skips to RubyGems'. That is the hook's chain, and
    // Core's boot loads Zeitwerk with the gems, before app/models.
    for (hooked, zeitwerk_first) in [(true, true), (false, true), (false, false)] {
        let early = if zeitwerk_first { "require 'zeitwerk'\n" } else { "" };
        let script = format!("{BOOT}{hook}{early}require_relative 'app/models'\nrequire 'sessionish'\n{observe}");
        let mut cmd = emit_and_run::ruby();
        cmd.current_dir(&*dir).env("RH_TEST_GEMS", &home).args(["-e", &script]);
        if hooked {
            cmd.env("RH_HOOK", "1");
        }
        let out = cmd.output().expect("ruby");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "hooked={hooked} zeitwerk_first={zeitwerk_first}\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        outs.push(stdout);
    }
    let _ = std::fs::remove_dir_all(&home);
    for out in &outs {
        assert_eq!(out, "\"core macro\"\ntrue\n[:compiled, \"compiled codec\"]\n");
    }
}

/// A compiled gem whose own body autoloads its components (polaris 25's
/// `register_component :Badge, "badge"`, an
/// `autoload(name, "components/polaris/#{path}/#{path}")`) keeps the
/// tree's autoload of the constant. Replayed, the gem's autoload named a
/// feature the tree answers with two files, and loading the first (the
/// `Badge` module) required the second inside its own `module Badge`:
/// "uninitialized constant Polaris::Badge" for 74 v2-r50 ids.
#[test]
fn a_compiled_gems_own_autoload_keeps_the_trees() {
    let home = gem_home();
    std::fs::write(
        home.join("specifications/pollike-25.0.1.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"pollike\"\n  s.version = \"25.0.1\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    let lock = LOCK.replace("    paqlike (1.0.2)\n", "    paqlike (1.0.2)\n    pollike (25.0.1)\n").replace("  paqlike\n  sessionish\n", "  paqlike\n  pollike\n  sessionish\n");
    let (dir, errors) = emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!(
                "test_paths:\n  - test\ndatabase_adapter: postgresql\ngem_home: {}\ngem_sources:\n  pollike: vendor/gems/pollike-25.0.1\n",
                home.display()
            ),
        )
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_compiled_gem_own_autoload\n")
        .write("Gemfile.lock", &lock)
        .write(
            "vendor/gems/pollike-25.0.1/lib/pollike.rb",
            "module Pollike\n  def self.register_component(name, path)\n    autoload(name, \"components/pollike/#{path}/#{path}\")\n  end\n\n  register_component :Badge, \"badge\"\nend\n",
        )
        .write(
            "vendor/gems/pollike-25.0.1/lib/components/pollike/badge/badge.rb",
            "module Pollike\n  module Badge\n    class Badge\n      def self.label\n        \"badge\"\n      end\n    end\n\n    def ui_badge\n      Badge.label\n    end\n  end\nend\n",
        )
        .write("app/helpers/labels_helper.rb", "require \"pollike\"\nrequire \"components/pollike/badge/badge\"\n\nmodule LabelsHelper\n  include Pollike::Badge\nend\n")
        .write("lib/oracle.rb", "class Oracle\n  def self.label\n    Object.new.extend(LabelsHelper).ui_badge\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let own: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(own.is_empty(), "{own:?}");
    let script = format!("{BOOT}require_relative 'app/models'\nrequire 'pollike'\np Pollike::Badge::Badge.label\np Oracle.label\n");
    let out = emit_and_run::ruby().current_dir(&*dir).env("RH_TEST_GEMS", &home).args(["-e", &script]).output().expect("ruby");
    let _ = std::fs::remove_dir_all(&home);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "\"badge\"\n\"badge\"\n");
}
