//! `extend self` exposes instance methods on the module, retaining visibility.
//! Unlike `module_function`, it does not make an independent public copy.
//!
//! Ruby makes every instance method of the module a singleton method
//! too, so `PrivateNetworkGuard.resolve(host)` reaches the `def resolve`
//! written without a receiver. Dropped at ingest, the module emitted
//! instance-only methods and every dotted call was a NoMethodError.
//!
//! campfire paid for this a long way from the declaration:
//! `Opengraph::Metadata.from_url` fetches through
//! `RestrictedHTTP::PrivateNetworkGuard.resolve`, so with the module
//! silent the whole fetch chain returned an EMPTY document — and the
//! tests that asserted on the result passed anyway, because nothing
//! else checked that the object had been filled in.
//!
//! The Ruby family writes the defs as the instance methods the source
//! has, under the replayed `extend self`, so the module answers them on
//! itself AND passes them on to whatever `extend`s or `include`s it:
//! frozen_record 0.27.4's `Backends::Yaml` `extend self`s, and
//! api_versioning's `FrozenApplicationRecordBackend` `extend`s it and
//! calls `super` from its own `filename`. Written `def self.filename`,
//! Yaml had no instance methods and the `super` raised NoMethodError.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn emitted(lib_src: &str) -> String {
    let mut app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"posts\", force: :cascade do |t|\n    t.string \"body\", null: false\n  end\nend\n",
        ),
        ("app/models/post.rb", "class Post < ApplicationRecord\nend\n"),
        ("app/models/guard.rb", lib_src),
    ]))
    .expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_library(&app)
        .iter()
        .find(|f| f.path.ends_with("guard.rb"))
        .expect("no guard.rb emitted")
        .content
        .clone()
}

#[test]
fn extend_self_makes_every_def_callable_on_the_module() {
    let src = emitted(
        r#"module Guard
  extend self

  def resolve(host)
    host
  end

  def private?(ip)
    false
  end
end
"#,
    );
    assert!(src.contains("extend self"), "{src}");
    assert!(src.contains("def resolve(host)") && !src.contains("def self.resolve"), "{src}");
    assert!(src.contains("def private?(ip)"), "{src}");
}

#[test]
fn extend_self_retains_private_visibility_and_survives_a_public_marker() {
    let src = emitted(
        r#"module Guard
  extend self
  private
  def helper; 31; end
  protected
  def guarded; 32; end
  public
  def later; 33; end
end
"#,
    );
    assert!(src.contains("private :helper"), "{src}");
    assert!(src.contains("protected :guarded"), "{src}");
    assert!(src.contains("def later"), "{src}");
    assert!(!src.contains(":later"), "{src}");
}

/// Without it the methods stay instance-only — the behaviour every
/// other module keeps.
#[test]
fn a_module_without_extend_self_keeps_instance_methods() {
    let src = emitted(
        r#"module Guard
  def resolve(host)
    host
  end
end
"#,
    );
    assert!(src.contains("def resolve(host)"), "{src}");
    assert!(!src.contains("def self.resolve"), "{src}");
}

/// `extend SomethingElse` is a plain mixin and must not flip anything.
#[test]
fn extend_of_another_module_is_not_extend_self() {
    let src = emitted(
        r#"module Guard
  extend Comparable

  def resolve(host)
    host
  end
end
"#,
    );
    assert!(!src.contains("def self.resolve"), "{src}");
}

/// The module's source and its emitted file, run by CRuby, answer the
/// same: on the module itself, through a module that `extend`s it and
/// calls `super`, through a class that `include`s it, with private and
/// protected copies refused alike. A `def self.` stays on the singleton
/// only, and `module_function` copies keep their own form.
#[test]
fn extend_self_modules_answer_like_ruby_when_extended_and_included() {
    let source = r##"module Guard
  extend self

  def filename(name)
    "#{name.downcase}s.yml"
  end

  def self.only_module
    :module
  end

  def load(path)
    [read(path), path]
  end

  private

  def read(path)
    path.size
  end
end
"##;
    let src = emitted(source);
    let probe = r##"
module Backend
  extend Guard
  extend self

  def filename(name)
    super(name.split("::").last)
  end
end

class Includer
  include Guard
end

def answer
  yield.inspect
rescue NoMethodError => e
  e.class.name
end

puts answer { Guard.filename("Api::Version") }
puts answer { Guard.load("x/y") }
puts answer { Guard.read("x") }
puts answer { Guard.only_module }
puts answer { Backend.filename("Api::Version") }
puts answer { Backend.load("a") }
puts answer { Backend.respond_to?(:only_module) }
puts answer { Includer.new.filename("Thing") }
puts answer { Includer.new.respond_to?(:read, true) }
puts answer { Guard.instance_methods(false).sort }
puts answer { Guard.private_instance_methods(false).sort }
"##;
    let run = |module: &str| {
        let dir = std::env::temp_dir().join(format!("rh_extend_self_{}_{}", std::process::id(), module.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("probe.rb");
        std::fs::write(&file, format!("{module}\n{probe}")).unwrap();
        let out = std::process::Command::new("ruby").arg(&file).output().expect("ruby");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let native = run(source);
    assert!(native.contains("\"versions.yml\""), "{native}");
    assert_eq!(native, run(&src), "emitted:\n{src}");
}

/// `private_class_method :m` after `extend self` names the singleton's
/// view of the module's own instance method, which Ruby accepts. It was
/// refused ("a visibility change in a module requires a method the
/// module defines") because only a class-side `def self.m` counted, so
/// Core's `PreviewStores::StorefrontPreviewLink` never loaded.
#[test]
fn private_class_method_after_extend_self_names_the_instance_method() {
    let source = r##"module Guard
  extend self

  def open(host)
    [normalize(host), validate!(host)]
  end

  def normalize(host)
    host.downcase
  end

  def validate!(host)
    host
  end

  private_class_method :normalize,
    :validate!
end
"##;
    let src = emitted(source);
    assert!(src.contains("def normalize(host)") && !src.contains("def self.normalize"), "{src}");
    assert!(src.contains("private_class_method :normalize") && src.contains("private_class_method :validate!"), "{src}");
    let probe = r##"
def answer
  yield.inspect
rescue NoMethodError => e
  e.class.name
end

puts answer { Guard.open("A.B") }
puts answer { Guard.normalize("A") }
puts answer { Guard.validate!("A") }
puts answer { Guard.instance_method(:normalize).owner }
puts answer { Guard.public_instance_methods(false).sort }
"##;
    let run = |module: &str, tag: &str| {
        let dir = std::env::temp_dir().join(format!("rh_extend_self_pcm_{}_{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("probe.rb");
        std::fs::write(&file, format!("{module}\n{probe}")).unwrap();
        let out = std::process::Command::new("ruby").arg(&file).output().expect("ruby");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let native = run(source, "native");
    assert!(native.contains("NoMethodError"), "{native}");
    assert_eq!(native, run(&src, "emitted"), "emitted:\n{src}");
}
