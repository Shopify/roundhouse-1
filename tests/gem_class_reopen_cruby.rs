//! An app file that reopens a class of a gem the CRuby tree loads.
//!
//! Shopify Core's essentials `lib/patches/liquid_ext.rb` reopens the
//! liquid gem's `Liquid::Render` to `prepend` a patch, `remove_const
//! :SYNTAX` and assign a new `SYNTAX` built from the gem's own
//! `Liquid::Render::FOR`, and prepends onto `Liquid::Block`'s singleton
//! class and `Liquid::Include` from `module Liquid`'s body. Ruby runs
//! the reopen on the gem's class object after the gem loads. The emitted
//! tree dropped the `prepend` and `remove_const`, refused the gem's
//! `FOR` (the app's reopen hid the RBI's constants), and left the
//! module-body prepends out with no diagnostic.
//!
//! The fixture is a small gem named `liquid` installed into a scratch
//! gem home: the CRuby tree requires `liquid` because the lockfile pins
//! it (`project.rs::LOCK_GATED_REQUIRES`), and its RBI types it. The
//! emitted tree must print what CRuby prints loading the gem and then
//! the authored patch, line for line, and warn no more than CRuby does.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const GEM_LIB: &str = r#"module Liquid
  QuotedString = /"[^"]*"|'[^']*'/
  QuotedFragment = /#{QuotedString}|(?:[^\s,\|'"]|#{QuotedString})+/o
  VariableSegment = /[\w\-]/

  class Error < StandardError; end

  class Tag
    def initialize(name)
      @name = name
    end
  end

  class Block < Tag
    def self.raise_unknown_tag(tag)
      raise Error, "unknown tag #{tag}"
    end
  end

  class Include < Tag
    def render_to_output_buffer(output)
      output << "include:#{@name}"
    end
  end

  class Render < Tag
    FOR = "for"
    SYNTAX = /(#{QuotedString}+)(\s+(with|#{FOR})\s+(#{QuotedFragment}+))?/o

    def render_tag(output)
      output << "render:#{@name}"
    end
  end
end
"#;

/// The parts of a Tapioca RBI for the fixture gem.
const RBI: &str = r#"# typed: true
module Liquid; end
class Liquid::Error < ::StandardError; end
class Liquid::Tag
  def initialize(name); end
end
class Liquid::Block < ::Liquid::Tag
  class << self
    def raise_unknown_tag(tag); end
  end
end
class Liquid::Include < ::Liquid::Tag
  def render_to_output_buffer(output); end
end
class Liquid::Render < ::Liquid::Tag
  def render_tag(output); end
end
Liquid::QuotedFragment = T.let(T.unsafe(nil), Regexp)
Liquid::QuotedString = T.let(T.unsafe(nil), Regexp)
Liquid::Render::FOR = T.let(T.unsafe(nil), String)
Liquid::Render::SYNTAX = T.let(T.unsafe(nil), Regexp)
Liquid::VariableSegment = T.let(T.unsafe(nil), Regexp)
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    liquid (5.13.0)\n\nDEPENDENCIES\n  activesupport\n  liquid (= 5.13.0)\n";

/// The shape of Core's liquid_ext.rb, reduced to this gem's surface.
const PATCH: &str = r#"module Liquid
  module LiquidUnknownTagExt
    def raise_unknown_tag(tag)
      raise Liquid::Error, "'#{tag}' tag must not be nested" if tag == "schema"
      super
    end
  end

  Block.singleton_class.prepend(LiquidUnknownTagExt)

  module LiquidIncludeExt
    def render_to_output_buffer(output)
      output << "["
      super
      output << "]"
    end
  end

  ::Liquid::Include.prepend(LiquidIncludeExt)

  module ThemeExtension
    module RenderTagSyntaxErrorPatch
      def render_tag(output)
        return super if @name.is_a?(String)

        raise Liquid::Error, "render target is not an app block"
      end
    end
  end

  class Render
    prepend(ThemeExtension::RenderTagSyntaxErrorPatch)
    remove_const :SYNTAX
    SYNTAX = /((?:#{Liquid::QuotedString}|#{Liquid::VariableSegment})+)(\s+(with|#{Liquid::Render::FOR})\s+(#{Liquid::QuotedFragment}+))?/o
  end
end
"#;

const SCRIPT: &str = r#"
puts "render ancestors: #{Liquid::Render.ancestors.first(3).inspect}"
puts "render superclass: #{Liquid::Render.superclass}"
puts "render: #{Liquid::Render.new("snippet").render_tag(+"")}"
begin
  Liquid::Render.new(7).render_tag(+"")
rescue Liquid::Error => e
  puts "render raised: #{e.message}"
end
puts "syntax: #{Liquid::Render::SYNTAX.source}"
puts "for: #{Liquid::Render::FOR}"
puts "syntax matches: #{Liquid::Render::SYNTAX.match?("'a' for b")}"
puts "include: #{Liquid::Include.new("x").render_to_output_buffer(+"")}"
["schema", "other"].each do |tag|
  Liquid::Block.raise_unknown_tag(tag)
rescue Liquid::Error => e
  puts "block raised: #{e.message}"
end
"#;

/// A gem home holding the fixture `liquid` 5.13.0, as `gem install`
/// lays one out.
fn gem_home(root: &Path) -> PathBuf {
    let home = root.join("gem-home");
    let gem = home.join("gems/liquid-5.13.0");
    std::fs::create_dir_all(gem.join("lib")).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(gem.join("lib/liquid.rb"), GEM_LIB).unwrap();
    std::fs::write(
        home.join("specifications/liquid-5.13.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"liquid\"\n  s.version = \"5.13.0\"\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/liquid.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
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

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roundhouse-gem-class-reopen-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_reopen_of_a_loaded_gem_class_runs_on_the_gem_class_as_in_cruby() {
    reopen_runs_as_in_cruby(true);
}

/// With no `sorbet/` directory (the RBI-free Core slice): the reopen is
/// known from the gem's installed source in `gem_home:`
/// (`GemSourceConstants::kinds`). Before, the emitter dropped the
/// `prepend` and `remove_const` as lower_residue there.
#[test]
fn a_reopen_is_known_from_the_installed_source_without_an_rbi() {
    reopen_runs_as_in_cruby(false);
}

fn reopen_runs_as_in_cruby(rbi: bool) {
    let root = scratch(if rbi { "rbi" } else { "source" });
    let home = gem_home(&root);
    let gem_path = gem_path(&home);
    std::fs::write(root.join("liquid_ext.rb"), PATCH).unwrap();
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &gem_path)
        .arg("-W1")
        .arg("-e")
        .arg(format!("require \"liquid\"\nload {:?}\n{SCRIPT}", root.join("liquid_ext.rb").display().to_string()))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    for probe in [
        "render superclass: Liquid::Tag",
        "render raised: render target is not an app block",
        "for: for",
        "syntax matches: true",
        "include: [include:x]",
        "block raised: 'schema' tag must not be nested",
        "block raised: unknown tag other",
    ] {
        assert!(expected.iter().any(|l| l == probe), "native control lacks {probe:?}: {expected:#?}");
    }
    assert!(!String::from_utf8_lossy(&native.stderr).contains("already initialized constant"));

    let app = emit_and_run::empty_app().write("Gemfile.lock", LOCK).write("lib/patches/liquid_ext.rb", PATCH);
    let app = if rbi {
        app.write("roundhouse.yml", "test_paths:\n  - test\n").write("sorbet/rbi/gems/liquid@5.13.0.rbi", RBI)
    } else {
        app.write("roundhouse.yml", &format!("test_paths:\n  - test\ngem_home: {}\n", home.display()))
    };
    let (tree, errors) = app.emit(BuildTarget::Ruby);
    let patch_errors: Vec<&String> = errors.iter().filter(|e| !e.contains("FileId(0)")).collect();
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .arg("-W1")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed: {stderr}\nerrors: {patch_errors:#?}");
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby line for line\nerrors: {patch_errors:#?}");
    assert!(!stderr.contains("already initialized constant"), "remove_const is honored: {stderr}");
    assert!(errors.is_empty(), "{errors:#?}");
}
