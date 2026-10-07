//! A gem source's file program requires a gem-home feature where its
//! source does, after the statements above it, and nothing loads that
//! feature earlier.
//!
//! Core's kafka-shopify 5.2.3 `patches/avro_stdlib_json.rb` defines
//! `Avro::AvroError`, then requires `avro/logical_types` and `avro/schema`
//! (whose `SchemaParseError < AvroError` needs it), patches
//! `Avro::Schema`, and only then requires `avro`. The boot's load of the
//! gem sources' dependencies (runtime/gem_facades.rb) required every one
//! of those features up front, before the patch ran, and boot died on
//! `uninitialized constant Avro::AvroError`. A feature a carried file
//! program requires is that program's to load, in place.
//!
//! The fixture is a small gem `avrokit` with avro's shape, installed into
//! a scratch gem home, and a gem source `kafkakit` with the patch's
//! shape. The emitted tree must print what CRuby prints.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const AVROKIT: &str = "module Avrokit\n  class AvroError < StandardError; end\nend\nrequire \"avrokit/schema\"\n";
const AVROKIT_SCHEMA: &str =
    "module Avrokit\n  class SchemaParseError < AvroError; end\n\n  class Schema\n    def self.parse(text) = \"original #{text}\"\n  end\nend\n";

const KAFKAKIT: &str = "require \"kafkakit/patch\"\n\nmodule Kafkakit\n  def self.parse(text) = Avrokit::Schema.parse(text)\nend\n";
const PATCH: &str = r#"require "json"

module Avrokit
  class AvroError < StandardError; end
end

require "avrokit/schema"

module Avrokit
  class Schema
    def self.parse(text) = "patched #{text}"
  end
end

require "avrokit"
"#;

const SCRIPT: &str = "puts Kafkakit.parse(\"x\")\nputs Avrokit::SchemaParseError.superclass\n";

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    avrokit (1.0.0)\n    kafkakit (1.0.0)\n      avrokit (= 1.0.0)\n\nDEPENDENCIES\n  kafkakit\n";

fn gem_home(root: &Path) -> PathBuf {
    let home = root.join("gem-home");
    let gem = home.join("gems/avrokit-1.0.0");
    std::fs::create_dir_all(gem.join("lib/avrokit")).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(gem.join("lib/avrokit.rb"), AVROKIT).unwrap();
    std::fs::write(gem.join("lib/avrokit/schema.rb"), AVROKIT_SCHEMA).unwrap();
    std::fs::write(
        home.join("specifications/avrokit-1.0.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"avrokit\"\n  s.version = \"1.0.0\"\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/avrokit.rb\", \"lib/avrokit/schema.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    home
}

/// The fixture gem home ahead of the interpreter's own, so the emitted
/// tree's other gems (sqlite3, pg) still load.
fn gem_path(home: &Path) -> String {
    let out = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    format!("{}:{}", home.display(), String::from_utf8_lossy(&out.stdout))
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_gem_source_program_requires_a_gem_home_feature_in_place() {
    let root = std::env::temp_dir().join(format!("roundhouse-gem-source-require-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let home = gem_home(&root);
    let gem_path = gem_path(&home);

    // CRuby on the same files: the gem source on the load path.
    let source = root.join("kafkakit/lib");
    std::fs::create_dir_all(source.join("kafkakit")).unwrap();
    std::fs::write(source.join("kafkakit.rb"), KAFKAKIT).unwrap();
    std::fs::write(source.join("kafkakit/patch.rb"), PATCH).unwrap();
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &gem_path)
        .arg("-I")
        .arg(&source)
        .args(["-rkafkakit", "-e", SCRIPT])
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(expected, ["patched x", "Avrokit::AvroError"], "native control");

    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("gem_home: {}\ngem_sources:\n  kafkakit: vendor/gems/kafkakit-1.0.0\n", home.display()))
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/kafkakit-1.0.0/lib/kafkakit.rb", KAFKAKIT)
        .write("vendor/gems/kafkakit-1.0.0/lib/kafkakit/patch.rb", PATCH)
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap_or_default();
    assert!(!facades.contains("\"avrokit/schema\""), "the program's own require was preloaded:\n{facades}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\nrequire \"kafkakit\" unless defined?(Kafkakit)\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    // The error class the patch defines is the one the gem's schema
    // subclasses: the patch ran before `avrokit/schema` loaded.
    let got = lines(&run.stdout);
    assert_eq!(got.get(1), expected.get(1), "emitted Ruby diverges from CRuby\nerrors: {errors:#?}");
    // Not yet as CRuby: the tree registers an autoload of
    // `Avrokit::Schema` for the patch's reopen, so the gem's own
    // `class Schema` loads the reopen first and its `parse` wins
    // ("original x"). The autoload belongs to the program, not to the
    // constant; when it does, this is `assert_eq!(got, expected)`.
    assert_eq!(got.first().map(String::as_str), Some("original x"), "now as CRuby: tighten to the full comparison");
    let _ = std::fs::remove_dir_all(&root);
}
