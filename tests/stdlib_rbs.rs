//! Ruby core and stdlib constants resolve through the rbs signatures that
//! ship with the pinned Ruby (`rbs/`, `src/stdlib_rbs.rs`). A namespace is
//! its class, a value constant has its declared type, and an RBS class
//! alias (`class Queue = Thread::Queue`) is its target. An app's own name
//! still wins over Ruby's, a gem's RBI still attributes its constant to the
//! gem, and only the Ruby-family trees admit a name no other runtime
//! defines. The CRuby tree writes the `require` a stdlib library needs.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{diagnose, Analyzer};
use roundhouse::expr::ExprNode;
use roundhouse::ident::ClassId;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;
use roundhouse::ty::Ty;
use roundhouse::App;

fn analyzed(files: &[(&str, &str)]) -> App {
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    app
}

/// Every written constant in the app's library methods: its path as
/// written (after resolution qualifies it) and its type.
fn constants(app: &App) -> Vec<(String, Option<Ty>)> {
    fn walk(expr: &roundhouse::Expr, out: &mut Vec<(String, Option<Ty>)>) {
        if let ExprNode::Const { path } = &*expr.node {
            let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
            out.push((name, expr.ty.clone()));
        }
        expr.node.for_each_child(&mut |child| walk(child, out));
    }
    let mut out = Vec::new();
    for class in &app.library_classes {
        for method in &class.methods {
            walk(&method.body, &mut out);
        }
    }
    out
}

fn type_of(app: &App, written: &str) -> Ty {
    constants(app)
        .into_iter()
        .find(|(name, _)| name == written)
        .unwrap_or_else(|| panic!("no constant {written}: {:?}", constants(app)))
        .1
        .unwrap_or_else(|| panic!("{written} is untyped"))
}

fn class(name: &str) -> Ty {
    Ty::Class { id: ClassId(roundhouse::ident::Symbol::from(name)), args: vec![] }
}

fn errors(app: &App) -> Vec<String> {
    diagnose(app)
        .iter()
        .filter(|d| d.severity == roundhouse::diagnostic::Severity::Error)
        .map(|d| d.message.clone())
        .collect()
}

fn probe(body: &str) -> App {
    analyzed(&[("app/lib/probe.rb", &format!("class Probe\n  def run\n{body}\n  end\nend\n"))])
}

#[test]
fn value_constants_take_their_rbs_types() {
    let app = probe("    [Encoding::UTF_8, File::FNM_EXTGLOB, Math::PI, Float::INFINITY]");
    assert_eq!(type_of(&app, "Encoding::UTF_8"), class("Encoding"));
    // Declared in File::Constants, which File includes.
    assert_eq!(type_of(&app, "File::FNM_EXTGLOB"), Ty::Int);
    assert_eq!(type_of(&app, "Math::PI"), Ty::Float);
    assert_eq!(type_of(&app, "Float::INFINITY"), Ty::Float);
    assert!(errors(&app).is_empty(), "{:#?}", errors(&app));
}

#[test]
fn stdlib_namespaces_resolve_to_their_classes() {
    let app = probe("    [URI::HTTPS, OpenSSL::HMAC, Benchmark, Gem::Version]");
    for name in ["URI::HTTPS", "OpenSSL::HMAC", "Benchmark", "Gem::Version"] {
        assert_eq!(type_of(&app, name), class(name), "{name}");
    }
    assert!(errors(&app).is_empty(), "{:#?}", errors(&app));
}

#[test]
fn a_class_alias_resolves_to_its_target() {
    let app = probe("    Queue");
    assert_eq!(type_of(&app, "Thread::Queue"), class("Thread::Queue"));
    assert!(errors(&app).is_empty(), "{:#?}", errors(&app));
}

/// Ruby's lexical lookup finds the app's own `Foo::NotImplementedError`
/// before Ruby's top-level one; outside `Foo` the name is Ruby's.
#[test]
fn an_app_namesake_wins_over_ruby() {
    let app = analyzed(&[
        ("app/lib/foo.rb", "module Foo\n  class NotImplementedError < StandardError\n  end\n\n  class Job\n    def run\n      NotImplementedError\n    end\n  end\nend\n"),
        ("app/lib/probe.rb", "class Probe\n  def run\n    NotImplementedError\n  end\nend\n"),
    ]);
    let found = constants(&app);
    assert!(found.contains(&("Foo::NotImplementedError".into(), Some(class("Foo::NotImplementedError")))), "{found:?}");
    assert!(found.contains(&("NotImplementedError".into(), Some(class("NotImplementedError")))), "{found:?}");
    assert!(errors(&app).is_empty(), "{:#?}", errors(&app));
}

fn check(config: &str) -> String {
    let root = std::env::temp_dir().join(format!("rh_stdlib_rbs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let file = root.join("app/lib/config.rb");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, config).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["check", "--continue"])
        .arg(&root)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// A declared type that names a Ruby class is indexed like a written one.
#[test]
fn declared_types_may_name_ruby_classes() {
    let config = "class Config\n  #: (untyped, untyped) -> void\n  def run(a, b)\n    range = a #: Range[Integer]\n    meth = b #: UnboundMethod\n    uri = b #: URI::HTTPS\n    klass = b #: Class\n    proc = b #: Proc\n    [range, meth, uri, klass, proc]\n  end\nend\n";
    let text = check(config);
    assert!(!text.contains("has no indexed declaration"), "{text}");
    assert!(text.contains(" 0 error(s)"), "{text}");
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    ostruct (0.6.3)\n\nDEPENDENCIES\n  activesupport\n  ostruct\n";

/// A gem's RBI names its constant before Ruby's signatures do.
#[test]
fn a_gem_constant_stays_the_gems() {
    let (_, errors) = emit_and_run::empty_app()
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/ostruct@0.6.3.rbi", "# typed: true\nclass OpenStruct\n  def initialize(hash = T.unsafe(nil)); end\nend\n")
        .write("lib/payload.rb", "class Payload\n  def account\n    OpenStruct.new({ name: \"Ada\" })\n  end\nend\n")
        .emit(BuildTarget::Rust);
    assert!(errors.iter().any(|e| e.contains("OpenStruct is declared by the ostruct gem")), "{errors:#?}");
    assert!(!errors.iter().any(|e| e.contains("OpenStruct is CRuby's own")), "{errors:#?}");
}

const LINKS: &str = r#"class Links
  def host(text)
    URI.parse(text).host
  end

  def secure?(text)
    URI.parse(text).is_a?(URI::HTTPS)
  end

  def utf8?(text)
    text.encoding == Encoding::UTF_8
  end

  def quoted(text)
    Shellwords.escape(text)
  end
end
"#;

fn links_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/links.rb", LINKS)
        .write("test/links_test.rb", r#"require "test_helper"
require "links"
class LinksTest < ActiveSupport::TestCase
  test "stdlib constants the interpreter defines" do
    links = Links.new
    assert_equal "example.com", links.host("https://example.com/a")
    assert_equal true, links.secure?("https://example.com/a")
    assert_equal false, links.secure?("http://example.com/a")
    assert_equal true, links.utf8?("plain")
    assert_equal "a\\ b", links.quoted("a b")
  end
end
"#)
}

fn emitted_links(dir: &std::path::Path) -> String {
    fn sources(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rb") {
                out.push(std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut files = Vec::new();
    sources(dir, &mut files);
    files.into_iter().find(|f| f.contains("class Links")).expect("emitted Links")
}

/// The Ruby tree writes the requires the emit dropped: `uri` from the
/// bundled-library table, `shellwords` from the signatures' own table.
/// Encoding is core and needs none.
#[test]
fn the_ruby_tree_requires_the_stdlib_it_names() {
    let (dir, errors) = links_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let links = emitted_links(&dir);
    assert!(links.contains("require \"uri\""), "{links}");
    assert!(links.contains("require \"shellwords\""), "{links}");
    assert!(!links.contains("require \"encoding\""), "{links}");
}

/// Run on the pinned interpreter (`ruby` on PATH must be Ruby 4.0).
#[test]
fn stdlib_constants_run_in_emitted_ruby() {
    links_app().run_test("test/models/links_test.rb").assert_passes();
}

#[test]
fn jruby_admits_them() {
    let (_, errors) = links_app().emit(BuildTarget::Jruby);
    assert!(errors.is_empty(), "{errors:#?}");
}

/// No other runtime defines `URI::HTTPS`, so those trees refuse it by
/// name rather than emit a reference to nothing.
#[test]
fn another_runtime_refuses_them_by_name() {
    for target in [BuildTarget::Crystal, BuildTarget::Typescript] {
        let (_, errors) = links_app().emit(target);
        for name in ["URI::HTTPS", "Shellwords"] {
            let needle = format!("{name} is CRuby's own");
            assert!(errors.iter().any(|e| e.contains(&needle)), "{target:?} {name}: {errors:#?}");
        }
    }
}
