//! A model body calls class macros the app defines: a concern's
//! `class_methods` on ApplicationRecord (Core's `metadata_accessor`,
//! `enable_reaper!`), an app module the base extends (`extend
//! Component::TrustedIdAttributes`, `extend AcceleratedBelongsTo`), and
//! extends modules whose `extended` hook configures the class (Core's
//! `extend(FixMeMyModelHasImplicitAssociationValidation)`, whose hook sets
//! `belongs_to_required_by_default = false`, on 368 models).
//!
//! Each call whose class-body effects the lowered class answers is
//! replayed where the source has it; a module's `extended` hook that only
//! sets `belongs_to_required_by_default` is applied to the `belongs_to`
//! declarations after it and in subclasses, as ActiveRecord's
//! `belongs_to` reads the class attribute when it declares the
//! association (`optional:` / `required:` on the declaration win). The
//! same app runs natively on Core's pinned activerecord with sqlite3 and
//! emitted on the SQLite CRuby tree, and prints the same.
//!
//! A macro that reaches model DSL (`belongs_to`, `attribute`,
//! `query_constraints`, …), which the lowered model declares statically
//! and the runtime base does not answer at class-body time, is refused
//! with the path to the DSL, as is an `extend` whose hook calls a method
//! the lowered class lacks, and every macro that module gives.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const FILES: &[(&str, &str)] = &[
    (
        "app/models/concerns/accessors.rb",
        r##"module Accessors
  extend ActiveSupport::Concern

  class_methods do
    #: -> untyped
    def flag_methods
      @flag_methods ||= Module.new.tap { |m| include m }
    end

    def flagged(name)
      flag_methods.send(:define_method, "#{name}_flag") { send(name).to_s.upcase }
    end

    def metadata_accessor(name, default: nil)
      @metadata_names ||= []
      @metadata_names << name.to_s
      define_method("#{name}=") { |value| (@metadata ||= {})[name] = value }
      define_method(name) { (@metadata ||= {}).fetch(name, default) }
    end

    def metadata_names
      @metadata_names.to_a
    end

    def enable_audit!
      audit_with(:default)
    end

    def audit_with(level)
      @audit_level = level
    end

    def audit_level
      @audit_level
    end
  end
end
"##,
    ),
    (
        "app/models/concerns/readonly_refs.rb",
        r##"module ReadonlyRefs
  extend ActiveSupport::Concern

  class_methods do
    def readonly_ref(name)
      belongs_to(name, optional: true, autosave: false)
      define_method("#{name}_ro") { send(name) }
    end

    def checks_presence(*columns)
      untyped_self = self #: as untyped
      untyped_self.validates(*columns, presence: true) if ENV["CHECKED"]
    end
  end
end
"##,
    ),
    (
        "app/models/concerns/tagging.rb",
        r##"module Tagging
  def tag_with(*names)
    names.each do |tag|
      define_method("#{tag}_tag") { "#{self.class.name.downcase}:#{tag}" }
    end
  end
end
"##,
    ),
    (
        "app/models/concerns/marking.rb",
        r##"module Marking
  def self.extended(base)
    base.instance_variable_set(:@marked, true)
  end

  def marked?
    @marked == true
  end
end
"##,
    ),
    (
        "app/models/concerns/optional_belongs_to.rb",
        r##"module OptionalBelongsTo
  extend self

  def extended(base)
    base.belongs_to_required_by_default = false
  end
end
"##,
    ),
    (
        "app/models/application_record.rb",
        "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\n  include Accessors\n  include ReadonlyRefs\n  extend Tagging\n  extend Marking\nend\n",
    ),
    ("app/models/author.rb", "class Author < ApplicationRecord\n  def greet(greeting) = \"#{greeting}, #{name}\"\nend\n"),
    (
        "app/models/post.rb",
        "class Post < ApplicationRecord\n  belongs_to :author\n  metadata_accessor :color, default: \"red\"\n  flagged :title\n  delegate :name, to: :author, prefix: true, allow_nil: true\n  delegate :greet, to: :author\n\n  def hello = greet(\"hi\")\nend\n",
    ),
    (
        "app/models/note.rb",
        r##"class Note < ApplicationRecord
  belongs_to :author
  extend(OptionalBelongsTo)
  belongs_to :editor, class_name: "Author"
  belongs_to :reviewer, class_name: "Author", required: true
  tag_with :draft, :final
  enable_audit!
end
"##,
    ),
    ("app/models/memo.rb", "class Memo < Note\n  extend(OptionalBelongsTo)\n  metadata_accessor :size, default: 0\n  audit_with :strict\nend\n"),
    (
        "app/models/optional_record.rb",
        "class OptionalRecord < ApplicationRecord\n  self.abstract_class = true\n  extend(OptionalBelongsTo)\nend\n",
    ),
    ("app/models/comment.rb", "class Comment < OptionalRecord\n  belongs_to :author\nend\n"),
];

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "authors", force: :cascade do |t|
    t.string "name"
  end
  create_table "posts", force: :cascade do |t|
    t.integer "author_id"
    t.string "title"
  end
  create_table "notes", force: :cascade do |t|
    t.string "type"
    t.integer "author_id"
    t.integer "editor_id"
    t.integer "reviewer_id"
    t.string "title"
  end
  create_table "comments", force: :cascade do |t|
    t.integer "author_id"
    t.string "body"
  end
end
"#;

const OBSERVE: &str = r#"
a = Author.create!(name: "a")
p [:post_without_author, Post.new(title: "x").valid?]
p [:post_with_author, Post.new(title: "x", author_id: a.id).valid?]
p [:note_without_editor, Note.new(author_id: a.id, reviewer_id: a.id).valid?]
p [:note_without_author, Note.new(reviewer_id: a.id).valid?]
p [:note_without_reviewer, Note.new(author_id: a.id).valid?]
p [:comment_without_author, Comment.new(body: "c").valid?]
post = Post.new
p [:color, post.color]
post.color = "blue"
p [:color, post.color]
p [:post_metadata, Post.metadata_names]
p [:tags, Note.new.draft_tag, Note.new.final_tag]
p [:audit, Note.audit_level, Post.audit_level, Memo.audit_level]
p [:marked, ApplicationRecord.marked?, Note.marked?]
memo = Memo.new
p [:memo, memo.size, Memo.metadata_names, memo.draft_tag]
p [:memo_without_author, Memo.new(reviewer_id: a.id).valid?]
written = Post.new(author_id: a.id)
p [:flag, Post.new(title: "x").title_flag]
p [:delegate, written.author_name, Post.new.author_name, written.hello, written.greet("yo")]
"#;

const EXPECTED: &str = r#"[:post_without_author, false]
[:post_with_author, true]
[:note_without_editor, true]
[:note_without_author, false]
[:note_without_reviewer, false]
[:comment_without_author, true]
[:color, "red"]
[:color, "blue"]
[:post_metadata, ["color"]]
[:tags, "note:draft", "note:final"]
[:audit, :default, nil, :strict]
[:marked, true, false]
[:memo, 0, ["size"], "memo:draft"]
[:memo_without_author, false]
[:flag, "X"]
[:delegate, "a", nil, "hi, a", "yo, a"]
"#;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app();
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    app.write("db/schema.rb", SCHEMA)
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_model_macro_replay_native_{}", std::process::id()));
    for (path, text) in FILES.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    // A Rails app's `load_defaults` makes belongs_to required by default;
    // a bare `require "active_record"` does not.
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}{OBSERVE}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn app_class_macros_replay_like_activerecord() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
    let note = read("app/models/note.rb");
    for line in ["tag_with :draft, :final", "enable_audit!"] {
        assert!(note.contains(line), "note.rb lacks {line:?}:\n{note}");
    }
    // The hook's setting is applied statically, not replayed.
    assert!(!note.contains("OptionalBelongsTo"), "{note}");
    assert!(read("app/models/application_record.rb").contains("extend Marking"));
    assert!(read("app/models/memo.rb").contains("metadata_accessor :size, default: 0"));
    // An STI subclass's setting with no `belongs_to` to apply to is not run.
    assert!(!read("app/models/memo.rb").contains("OptionalBelongsTo"));

    let script = format!("require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\n{OBSERVE}");
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W0", "-e", &script]).env("BLOG_DB", ":memory:").output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, EXPECTED);
    assert_eq!(stdout, native(), "emitted vs native activerecord");
}

/// A macro that reaches model DSL (directly, or through a cast alias of
/// `self` under a runtime condition), an `extend` whose hook calls a method
/// the lowered class lacks, and a macro of that module are refused, each
/// naming the reason.
#[test]
fn macros_the_lowered_model_cannot_run_are_refused_with_the_reason() {
    let app = app()
        .edit("app/models/post.rb", "  belongs_to :author\n", "  belongs_to :author\n  readonly_ref :author\n  checks_presence :author_id\n")
        .write(
            "app/models/concerns/attributed.rb",
            "module Attributed\n  def self.extended(base)\n    base.class_attribute(:attributed, default: {})\n  end\n\n  def attribute_flag(name)\n    self.attributed = attributed.merge(name => true)\n  end\nend\n",
        )
        .edit("app/models/author.rb", "class Author < ApplicationRecord\n", "class Author < ApplicationRecord\n  extend Attributed\n  attribute_flag :name\n")
        .edit("app/models/memo.rb", "  extend(OptionalBelongsTo)\n", "  extend(OptionalBelongsTo)\n  belongs_to :owner, class_name: \"Author\"\n");
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    for expected in [
        "readonly_ref not supported (all targets): model macro call on `Post` not lowered; `readonly_ref` reaches `belongs_to`, which the lowered model declares statically from its body, not at class-body run time",
        "checks_presence not supported (all targets): model macro call on `Post` not lowered; `checks_presence` reaches `validates`, which the lowered model declares statically from its body, not at class-body run time",
        "extend not supported (all targets): model macro call on `Author` not lowered; `extend Attributed`: `Attributed.extended` calls `class_attribute`, which the lowered `Author` does not define",
        "extend not supported (all targets): model macro call on `Memo` not lowered; it sets `belongs_to_required_by_default`, which is applied to a lowered model's `belongs_to` declarations, and an STI subclass's are not lowered as a model's",
        "attribute_flag not supported (all targets): model macro call on `Author` not lowered; `attribute_flag` comes from `Attributed`, whose extend is not replayed",
    ] {
        assert!(errors.iter().any(|e| e.contains(expected)), "{expected}: {errors:#?}");
    }
    // The STI subclass's own `belongs_to :owner` is lowered (it is not
    // among the refusals).
    assert_eq!(errors.len(), 5, "{errors:#?}");
}
