//! A module body's `require_relative` loop over a literal list it
//! assigns loads each file the list names, as Ruby does.
//!
//! job-iteration 1.15.1's interruption_adapters.rb
//!
//! ```ruby
//! BUNDLED_ADAPTERS = [:delayed_job, :good_job, :resque, :sidekiq, :solid_queue, :sqs].freeze
//! BUNDLED_ADAPTERS.each do |name|
//!   require_relative "interruption_adapters/#{name}_adapter"
//! end
//! ```
//!
//! was kept as written in its declarations file. The path was read
//! relative to the emitted file, where nothing exists: the adapters,
//! file programs under models/roundhouse_files, were UNREACHED_PROGRAMS
//! (unit-1000 r4), and the require raised LoadError.
//!
//! The fixture's gem `adaptkit` loops over `%w[alpha beta].freeze` in a
//! brace block and over a symbol array in a `do` block. One element names
//! a file program (a statement at its top), the other a declarations-only
//! file. The script runs against CRuby with the gem's own lib/.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;

const ADAPTKIT: &str = "module Adaptkit\n  LOADED = []\nend\n\nrequire_relative \"adaptkit/adapters\"\nrequire_relative \"adaptkit/codecs\"\n";
const ADAPTERS: &str = r##"module Adaptkit
  module Adapters
    LIST = %w[alpha beta].freeze
    LIST.each { |n| require_relative "adapters/#{n}_adapter" }

    def self.lookup(name)
      const_get("#{name.capitalize}Adapter")
    end
  end
end
"##;
// A file program: a statement before its declarations.
const ALPHA: &str = r#"Adaptkit::LOADED << :alpha

module Adaptkit
  module Adapters
    module AlphaAdapter
      def self.label = "alpha"
    end
  end
end
"#;
// Declarations only.
const BETA: &str = "module Adaptkit\n  module Adapters\n    class BetaAdapter\n      def self.label = \"beta\"\n    end\n  end\nend\n";
const CODECS: &str = r##"module Adaptkit
  module Codecs
    BUNDLED = [:plain, :gzip].freeze
    BUNDLED.each do |name|
      require_relative "codecs/#{name}_codec"
    end
  end
end
"##;
const PLAIN: &str = "Adaptkit::LOADED << :plain\n\nmodule Adaptkit\n  module Codecs\n    class PlainCodec\n      def self.label = \"plain\"\n    end\n  end\nend\n";
const GZIP: &str = "Adaptkit::LOADED << :gzip\n\nmodule Adaptkit\n  module Codecs\n    class GzipCodec\n      def self.label = \"gzip\"\n    end\n  end\nend\n";

const SCRIPT: &str = "require \"adaptkit\" unless defined?(Adaptkit)\n\
puts Adaptkit::LOADED.inspect\n\
puts Adaptkit::Adapters.lookup(\"alpha\").label\n\
puts Adaptkit::Adapters.lookup(\"beta\").label\n\
puts Adaptkit::Codecs::PlainCodec.label\n\
puts Adaptkit::Codecs::GzipCodec.label\n\
puts Adaptkit::LOADED.inspect\n";

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    adaptkit (1.0.0)\n\nDEPENDENCIES\n  adaptkit\n";

const FILES: [(&str, &str); 7] = [
    ("adaptkit.rb", ADAPTKIT),
    ("adaptkit/adapters.rb", ADAPTERS),
    ("adaptkit/adapters/alpha_adapter.rb", ALPHA),
    ("adaptkit/adapters/beta_adapter.rb", BETA),
    ("adaptkit/codecs.rb", CODECS),
    ("adaptkit/codecs/plain_codec.rb", PLAIN),
    ("adaptkit/codecs/gzip_codec.rb", GZIP),
];

// The adapters' programs ran when the loop did, before any constant
// lookup reached them.
const EXPECTED: [&str; 6] =
    ["[:alpha, :plain, :gzip]", "alpha", "beta", "plain", "gzip", "[:alpha, :plain, :gzip]"];

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

fn native(lib: &Path) -> Vec<String> {
    let out = emit_and_run::ruby().arg("-I").arg(lib).arg("-e").arg(SCRIPT).output().expect("CRuby control");
    assert!(out.status.success(), "native control failed\n{}", String::from_utf8_lossy(&out.stderr));
    lines(&out.stdout)
}

#[test]
fn a_require_relative_loop_over_a_literal_list_loads_each_file_it_names() {
    let root = std::env::temp_dir().join(format!("roundhouse-require-relative-literal-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let lib = root.join("adaptkit/lib");
    for (path, text) in FILES {
        let file = lib.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    assert_eq!(native(&lib), EXPECTED, "native control");
    let _ = std::fs::remove_dir_all(&root);

    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  adaptkit: vendor/gems/adaptkit-1.0.0\n")
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"adaptkit\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n");
    for (path, text) in FILES {
        app = app.write(&format!("vendor/gems/adaptkit-1.0.0/lib/{path}"), text);
    }
    let run = app.run_ruby(SCRIPT);
    assert!(
        run.success,
        "emitted tree failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}",
        run.emitted.display(),
        run.stdout,
        run.stderr
    );
    assert_eq!(lines(run.stdout.as_bytes()), EXPECTED, "emitted boot diverges from CRuby\n{}", run.stderr);

    // Each loop became one require per element, of what the tree
    // carries for the file: its program, or its declarations.
    let programs = "models/roundhouse_files/vendor/gems/adaptkit-1.0.0/lib/adaptkit";
    let adapters = std::fs::read_to_string(run.emitted.join("app/models/adaptkit/adapters.rb")).unwrap();
    let alpha = adapters.find(&format!("require RoundhouseAutoload.path({:?})", format!("{programs}/adapters/alpha_adapter")));
    let beta = adapters.find("require RoundhouseAutoload.path(\"models/adaptkit/adapters/beta_adapter\")");
    assert!(alpha.is_some() && beta.is_some() && alpha < beta, "{adapters}");
    assert!(!adapters.contains("require_relative \"adapters/#{n}"), "{adapters}");
    let codecs = std::fs::read_to_string(run.emitted.join("app/models/adaptkit/codecs.rb")).unwrap();
    let plain = codecs.find(&format!("require RoundhouseAutoload.path({:?})", format!("{programs}/codecs/plain_codec")));
    let gzip = codecs.find(&format!("require RoundhouseAutoload.path({:?})", format!("{programs}/codecs/gzip_codec")));
    assert!(plain.is_some() && gzip.is_some() && plain < gzip, "{codecs}");
    assert!(!codecs.contains("require_relative \"codecs/#{name}"), "{codecs}");
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).unwrap();
    let unreached = models.split("UNREACHED_PROGRAMS").nth(1).and_then(|rest| rest.split(']').next()).unwrap_or("");
    assert!(!unreached.contains("adaptkit"), "{models}");
}
