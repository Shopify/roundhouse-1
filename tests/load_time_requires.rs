//! A library class file's header requires are its LOAD-time needs: what
//! its superclass, its includes, its class-body calls and constants
//! read as the file runs, and the method bodies that code runs
//! (`emit::ruby::library`, `load_time_methods`, `walk_load_time_paths`).
//! A constant named only inside a method body that load does not run,
//! or inside a closure the body only stores, is read later, and
//! app/models.rb's autoload loads its file then.
//!
//! Core had both, and each closed a circular require that native load
//! order does not have (Core's StrictWarning raises on it):
//! - `Merchandising::Metafields::FileReferenceType::FileTypes` is a
//!   nested T::Enum. Its values run `initialize`; its `graphql_type` and
//!   `model_class` name `Merchandising::Media` and a GraphApi type. All
//!   of its method bodies counted as load time, so
//!   file_reference_type.rb required media.rb, whose load reached
//!   `StoreLocalization::LocalizableContentTypes`, which required
//!   file_reference_type.rb back (out/app/models/merchandising/media.rb:16).
//! - Shop::Extension's `mark_as_translatable :meta_title, type: proc {
//!   StoreLocalization::LocalizableContentTypes::SingleLineTextFieldType }`
//!   stores the proc, and shop.rb required localizable_content_types.rb.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

/// Core's StrictWarning: $VERBOSE on, and a circular-require warning
/// raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

const RUBYOPT: (&str, &str) = ("RUBYOPT", "-W:deprecated");

const BASE: &str = "class BaseType\n  def self.mark_as_translatable(field, type:)\n    translatable_types[field] = type\n  end\n\n  def self.translatable_types\n    @translatable_types ||= {}\n  end\nend\n";
const LABELS: &str = "module Labels\n  def label\n    \"catalog\"\n  end\nend\n";
const LIMITS: &str = "module Limits\n  MAX = 3\nend\n";
const WIDGET: &str = "class Widget\n  def self.tag\n    :widget\n  end\nend\n";
const GADGET: &str = "class Gadget\n  def self.tag\n    :gadget\n  end\nend\n";

/// FileReferenceType's shape: a superclass, an include and a class-body
/// constant read at load; a nested T::Enum whose method body names
/// `Media`; a proc stored in a constant's hash and one passed to a
/// class macro.
const CATALOG: &str = r#"class Catalog < BaseType
  include Labels

  LIMIT = Limits::MAX
  OPTIONS = { type: proc { Widget }, label: "files" }.freeze

  class FileTypes < T::Enum
    enums do
      Image = new("Image")
      Video = new("Video")
    end

    def model_class
      case self
      when Image then Media
      else Media
      end
    end
  end

  mark_as_translatable :title, type: proc { Gadget }
end
"#;

/// Its class body reads `Catalog` as it loads.
const MEDIA: &str = "class Media\n  KINDS = Catalog::FileTypes.values.map(&:serialize).freeze\n\n  def self.kinds\n    KINDS\n  end\nend\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
        .write("app/models/base_type.rb", BASE)
        .write("app/models/labels.rb", LABELS)
        .write("app/models/limits.rb", LIMITS)
        .write("app/models/widget.rb", WIDGET)
        .write("app/models/gadget.rb", GADGET)
        .write("app/models/catalog.rb", CATALOG)
        .write("app/models/media.rb", MEDIA)
}

fn read(run: &emit_and_run::Run, path: &str) -> String {
    std::fs::read_to_string(run.emitted.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The header: the `require_relative` lines above the first `class` or
/// `module` line.
fn header(file: &str) -> Vec<&str> {
    file.lines()
        .take_while(|l| !l.starts_with("class ") && !l.starts_with("module "))
        .filter(|l| l.starts_with("require_relative "))
        .collect()
}

fn requires(file: &str, target: &str) -> bool {
    header(file).iter().any(|l| l.starts_with(&format!("require_relative {target:?}")))
}

const SCRIPT: &str = "p Media.kinds\np Catalog::FileTypes::Image.model_class\np Catalog::OPTIONS[:type].call.tag\np Catalog.translatable_types[:title].call.tag\np Catalog::LIMIT\np Catalog.new.label\n";
const STDOUT: &str = "[\"Image\", \"Video\"]\nMedia\n:widget\n:gadget\n3\n\"catalog\"\n";

#[test]
fn a_method_body_or_stored_proc_constant_is_no_header_require() {
    let run = app().run_ruby_env(SCRIPT, &[RUBYOPT]);
    let catalog = read(&run, "app/models/catalog.rb");
    let media = read(&run, "app/models/media.rb");
    // Read at load: the superclass, the include, a constant's value.
    for needed in ["base_type", "labels", "limits"] {
        assert!(requires(&catalog, needed), "catalog.rb must require {needed}:\n{catalog}");
    }
    // Read later: the enum's `model_class`, and the stored procs.
    for later in ["media", "widget", "gadget"] {
        assert!(!requires(&catalog, later), "catalog.rb must not require {later}:\n{catalog}");
    }
    // Media's class body reads Catalog as it loads.
    assert!(requires(&media, "catalog"), "{media}");
    for later in ["widget", "gadget"] {
        assert!(!requires(&media, later), "media.rb must not require {later}:\n{media}");
    }
    assert!(run.success, "{catalog}\n{media}\n{}\n{}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    assert_eq!(run.stdout, STDOUT, "{}", run.stderr);
}

/// Loading the class that names the other only in a method body first
/// works too: `Media` autoloads when `model_class` runs.
#[test]
fn the_class_with_the_method_body_reference_loads_first() {
    let script = "p Catalog::FileTypes::Video.model_class\np Media.kinds\n";
    let run = app().run_ruby_env(script, &[RUBYOPT]);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    assert_eq!(run.stdout, "Media\n[\"Image\", \"Video\"]\n", "{}", run.stderr);
}

/// A method the constants DO run at load stays a load-time need:
/// `ALL = [new(1)]` runs `initialize`, which reads `Limits::MAX`, so the
/// file requires limits.rb; `other` is not run and names `Widget`.
#[test]
fn a_method_the_constants_run_still_requires_what_it_reads() {
    let built = "class Built\n  ALL = [new(1)].freeze\n\n  def initialize(n)\n    @n = n * Limits::MAX\n  end\n\n  attr_reader :n\n\n  def other\n    Widget\n  end\nend\n";
    let run = app()
        .write("app/models/built.rb", built)
        .run_ruby_env("p Built::ALL.map(&:n)\np Built.new(2).other.tag\n", &[RUBYOPT]);
    let built = read(&run, "app/models/built.rb");
    assert!(requires(&built, "limits"), "{built}");
    assert!(!requires(&built, "widget"), "{built}");
    assert!(run.success, "{built}\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[3]\n:widget\n", "{}", run.stderr);
}

/// Sorbet's `T.type_alias { }` evaluates its block on first use, so a
/// class whose alias names a class nested in it does not load that
/// class: Core's `Merchandising::Taxonomy::Node::Alias = T.type_alias {
/// T.any(Taxonomy::Node, Taxonomy::Node::Decorator, ...) }`, where the
/// decorator's `ACCEPTED_CLASSES = [Node, ...]` reads the node back.
#[test]
fn a_type_alias_block_is_no_header_require() {
    // `keep` reads the alias, so the tree carries it.
    let shelf = "class Shelf\n  Item = T.type_alias { T.any(Shelf, Shelf::Slot) }\n\n  def self.size\n    2\n  end\n\n  def self.keep(x)\n    T.let(x, Item)\n  end\nend\n";
    let slot = "class Shelf\n  class Slot\n    ACCEPTED = [Shelf].freeze\n  end\nend\n";
    let run = app()
        .write("app/models/shelf.rb", shelf)
        .write("app/models/shelf/slot.rb", slot)
        .run_ruby_env("p Shelf::Slot::ACCEPTED\np Shelf.size\np Shelf.keep(Shelf::Slot.new).class\n", &[RUBYOPT]);
    let shelf = read(&run, "app/models/shelf.rb");
    let slot = read(&run, "app/models/shelf/slot.rb");
    assert!(shelf.contains("type_alias {"), "{shelf}");
    assert!(!requires(&shelf, "shelf/slot"), "{shelf}");
    assert!(run.success, "{shelf}\n{slot}\n{}\n{}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    assert_eq!(run.stdout, "[Shelf]\n2\nShelf::Slot\n", "{}", run.stderr);
}
