//! `define_method` over a statically known set of names, from a class
//! body: the three shapes Core writes.
//!
//! 1. `instance_eval { FEATURES.each { |name, type| define_method(name) … } }`
//!    over a frozen literal hash constant (FeatureSet::Core).
//! 2. A concern's `class_methods` macro, `session_accessor(*names)`, that
//!    walks `Array.wrap(names)` and is called with literal symbols
//!    (SessionAccessorConcern / UserSessionAttributes).
//! 3. An `extend`ed module's macro, `define_named_app(name)`, that reads
//!    `const_get("#{name.upcase}_ID")` and defines a predicate and a
//!    class-method finder (ApiClient::Identification on a model).
//!
//! Each is expanded at ingest into the methods Ruby defines, and the
//! emitted app is run against the same classes under native Rails (the
//! pinned oracle, tests/support/rails_oracle.rb). A shape whose names or
//! bodies are not static stays a gap with a diagnostic.
//!
//! Fixtures are written into the Overlay's scratch copy, never into
//! `fixtures/real-blog` (a link shared by every worktree).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

/// Runs `program` (a file: the oracle re-executes it under jemalloc).
fn native(name: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rh_define_method_loops_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rb"), format!("require {ORACLE:?}\n{program}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native {name} failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

const NATIVE_PRELUDE: &str = "RailsOracle.activate(\"activesupport\", \"activemodel\", \"activerecord\")\n\
     require \"active_support\"\nrequire \"active_support/concern\"\nrequire \"active_support/core_ext/array/wrap\"\n\
     require \"active_record\"\n";

// ---- Shape 1: instance_eval over a frozen literal hash -------------------

const FEATURE_SET_CORE: &str = r##"class FeatureSet
  module Core
    extend ActiveSupport::Concern

    FEATURES = {
      storefront: :boolean,
      theme_limit: :integer,
      gift_cards: :boolean,
    }.freeze

    #: (Symbol) -> untyped
    def [](name)
      @values[name]
    end

    #: (Symbol, untyped) -> untyped
    def []=(name, value)
      @values[name] = value
    end

    private

    instance_eval do
      FEATURES.each do |name, type|
        define_method(name) do
          self[name]
        end
        define_method("#{name}=") do |value|
          self[name] = value
        end
        next unless type == :boolean

        define_method("#{name}?") do
          !!self[name]
        end
      end
    end

    # A private helper: `private` above does not reach define_method in
    # the instance_eval block, which defines public methods.
    def secret = :hidden
  end
end
"##;

const FEATURE_SET: &str = r##"class FeatureSet
  include Core

  def initialize
    @values = {}
  end
end
"##;

const FEATURE_SCRIPT: &str = r##"f = FeatureSet.new
p f.storefront
f.storefront = true
p f.storefront
p f.storefront?
f.theme_limit = 3
p f.theme_limit
p f.respond_to?(:theme_limit?)
f.gift_cards = nil
p f.gift_cards?
p FeatureSet.instance_method(:theme_limit=).arity
p FeatureSet.instance_method(:storefront).arity
p FeatureSet.public_method_defined?(:gift_cards?)
p FeatureSet.private_method_defined?(:secret)
"##;

#[test]
fn instance_eval_over_a_frozen_literal_hash_matches_native() {
    let expected = native(
        "feature_set",
        &format!("{NATIVE_PRELUDE}{FEATURE_SET_CORE}{FEATURE_SET}{FEATURE_SCRIPT}"),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(expected, ["nil", "true", "true", "3", "false", "false", "1", "0", "true", "true"], "native control");
    let run = emit_and_run::real_blog()
        .write("app/models/feature_set/core.rb", FEATURE_SET_CORE)
        .write("app/models/feature_set.rb", FEATURE_SET)
        .run_ruby(FEATURE_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from native line for line");
}

// ---- Shape 2: a concern macro called with literal symbols ----------------

const SESSION_ACCESSOR_CONCERN: &str = r##"module SessionAccessorConcern
  extend ActiveSupport::Concern

  class_methods do
    def session_accessor(*names)
      Array.wrap(names).each do |name|
        define_method("#{name}=") do |value|
          key = [session_accessor_prefix, name].compact.join("_").to_sym
          @session[key] = value
        end
        define_method(name.to_s) do
          key = [session_accessor_prefix, name].compact.join("_").to_sym
          @session[key]
        end
      end
    end
  end
end
"##;

const USER_SESSION_ATTRIBUTES: &str = r##"class UserSessionAttributes
  include SessionAccessorConcern

  attr_reader :session_accessor_prefix

  session_accessor :login_at,
    :user_id

  def initialize(session, prefix = nil)
    @session = session
    @session_accessor_prefix = prefix
  end

  def identity_login?
    !!user_id
  end
end
"##;

const EMPLOYEE_SESSION_ATTRIBUTES: &str = r##"class EmployeeSessionAttributes
  include SessionAccessorConcern

  attr_reader :session_accessor_prefix

  def initialize(session)
    @session = session
    @session_accessor_prefix = "staff"
  end

  private

  # Public all the same: define_method in the macro's frame ignores the
  # class body's default visibility.
  session_accessor :employee_id

  def helper = :hidden
end
"##;

const SESSION_SCRIPT: &str = r##"session = {}
u = UserSessionAttributes.new(session)
p u.user_id
p u.identity_login?
u.user_id = 7
u.login_at = 42
p u.user_id
p u.identity_login?
p session
w = UserSessionAttributes.new(session, "my_prefix")
w.user_id = 9
p w.user_id
p session.keys
e = EmployeeSessionAttributes.new(session)
e.employee_id = 3
p e.employee_id
p session[:staff_employee_id]
p UserSessionAttributes.instance_method(:user_id=).arity
p UserSessionAttributes.instance_method(:login_at).arity
p EmployeeSessionAttributes.method_defined?(:user_id)
p EmployeeSessionAttributes.public_method_defined?(:employee_id=)
p EmployeeSessionAttributes.private_method_defined?(:helper)
"##;

#[test]
fn a_concern_macro_called_with_literal_symbols_matches_native() {
    let expected = native(
        "session_accessor",
        &format!(
            "{NATIVE_PRELUDE}{SESSION_ACCESSOR_CONCERN}{USER_SESSION_ATTRIBUTES}{EMPLOYEE_SESSION_ATTRIBUTES}{SESSION_SCRIPT}"
        ),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            "nil",
            "false",
            "7",
            "true",
            "{user_id: 7, login_at: 42}",
            "9",
            "[:user_id, :login_at, :my_prefix_user_id]",
            "3",
            "3",
            "1",
            "0",
            "false",
            "true",
            "true",
        ],
        "native control"
    );
    let run = emit_and_run::real_blog()
        .write("app/models/concerns/session_accessor_concern.rb", SESSION_ACCESSOR_CONCERN)
        .write("app/models/user_session_attributes.rb", USER_SESSION_ATTRIBUTES)
        .write("app/models/employee_session_attributes.rb", EMPLOYEE_SESSION_ATTRIBUTES)
        .run_ruby(SESSION_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from native line for line");
}

/// An app's own top-level `class Array` (a core-extension file) reopens the
/// core class: `Array.wrap(names)` in a macro still names the core class, so
/// the macro expands as it does without the reopen.
const ARRAY_REOPEN: &str = "class Array\n  def second = self[1]\nend\n";

#[test]
fn a_macro_wrapping_with_array_expands_when_the_app_reopens_array() {
    let expected = native(
        "session_accessor_array_reopen",
        &format!(
            "{NATIVE_PRELUDE}{ARRAY_REOPEN}{SESSION_ACCESSOR_CONCERN}{USER_SESSION_ATTRIBUTES}{EMPLOYEE_SESSION_ATTRIBUTES}{SESSION_SCRIPT}"
        ),
    );
    let run = emit_and_run::real_blog()
        .write("app/models/array.rb", ARRAY_REOPEN)
        .write("app/models/concerns/session_accessor_concern.rb", SESSION_ACCESSOR_CONCERN)
        .write("app/models/user_session_attributes.rb", USER_SESSION_ATTRIBUTES)
        .write("app/models/employee_session_attributes.rb", EMPLOYEE_SESSION_ATTRIBUTES)
        .run_ruby(SESSION_SCRIPT);
    run.assert_passes();
    assert!(!run.errors.iter().any(|e| e.contains("define_method_expansion")), "{:#?}", run.errors);
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected.lines().collect::<Vec<_>>(), "emitted Ruby diverges from native line for line");
}

// ---- Shape 3: an extended module's macro on a model ----------------------

const IDENTIFICATION: &str = r##"class Article
  module Identification
    def define_named_article(defined_name)
      @named_article_ids ||= []

      identified_id = const_get("#{defined_name.upcase}_ID")
      @named_article_ids << identified_id

      define_method("#{defined_name}?") do
        id == identified_id
      end

      define_singleton_method(defined_name) do
        # Core writes `ApiClient.where(id: identified_id).exists?`; the
        # blog target's arel lowering materializes `where(id: 1)` and
        # calls `exists?` on the Array, handwritten or expanded alike,
        # so this reads the id form the runtime models.
        unless Article.exists?(identified_id)
          message = <<~MSG
            Cannot find #{defined_name} Article.
            Seed Article id #{identified_id} first.
          MSG

          raise ArgumentError, message
        end

        Article.find_by(id: identified_id)
      end
    end

    def named_article_ids
      @named_article_ids || []
    end
  end
end
"##;

const ARTICLE_FIND: &str = "  validates :body, presence: true, length: { minimum: 10 }\n";
const ARTICLE_NAMED: &str = "  validates :body, presence: true, length: { minimum: 10 }\n\n  \
     extend Article::Identification\n\n  PINNED_ID = 1\n  FEATURED_ID = 2\n\n  private\n\n  \
     define_named_article :pinned\n  define_named_article :featured\n\n  def secret_note = :hidden\n";

const NAMED_SCRIPT: &str = r##"a = Article.create!(title: "One", body: "a body long enough")
b = Article.create!(title: "Two", body: "a body long enough")
p Article.pinned.title
p Article.featured.title
p [a.pinned?, a.featured?, b.featured?]
p Article.instance_variable_get(:@named_article_ids)
p Article.method(:pinned).arity
p Article.instance_method(:featured?).arity
p Article.public_method_defined?(:pinned?)
p Article.private_method_defined?(:secret_note)
b.destroy
begin
  Article.featured
rescue ArgumentError => e
  puts e.message
end
"##;

#[test]
fn an_extended_macro_on_a_model_matches_native() {
    let expected = native(
        "named_article",
        &format!(
            "{NATIVE_PRELUDE}\
             ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
             ActiveRecord::Schema.verbose = false\n\
             ActiveRecord::Schema.define {{ create_table(:articles) {{ |t| t.string :title; t.text :body; t.timestamps }} }}\n\
             class Article < ActiveRecord::Base\nend\n\
             {IDENTIFICATION}\
             class Article < ActiveRecord::Base\n  validates :title, presence: true\n{}end\n\
             {NAMED_SCRIPT}",
            ARTICLE_NAMED
        ),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#""One""#,
            r#""Two""#,
            "[true, false, true]",
            "[1, 2]",
            "0",
            "0",
            "true",
            "true",
            "Cannot find featured Article.",
            "Seed Article id 2 first.",
        ],
        "native control"
    );
    let run = emit_and_run::real_blog()
        .write("app/models/article/identification.rb", IDENTIFICATION)
        .edit("app/models/article.rb", ARTICLE_FIND, ARTICLE_NAMED)
        .run_ruby(NAMED_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from native line for line");
}

// ---- Gaps stay explicit ---------------------------------------------------

fn refusals(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("define_method")).collect()
}

/// A name set computed at run time, or a body that needs the closure,
/// is not expanded: the statement keeps a diagnostic saying why.
#[test]
fn a_dynamic_name_set_or_closure_is_refused() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .write(
            "app/models/feature_set/core.rb",
            &FEATURE_SET_CORE.replace("}.freeze\n", "}\n"),
        )
        .write("app/models/feature_set.rb", FEATURE_SET)
        .write(
            "app/lib/cs/counter.rb",
            "module Cs\n  class Counter\n    NAMES = %i[a b].freeze\n\n    instance_eval do\n      \
             count = 0\n      NAMES.each do |name|\n        define_method(name) do\n          count += 1\n        end\n      end\n    end\n  end\nend\n",
        )
        .write("app/models/concerns/session_accessor_concern.rb", SESSION_ACCESSOR_CONCERN)
        .write(
            "app/models/user_session_attributes.rb",
            &USER_SESSION_ATTRIBUTES.replace("  session_accessor :login_at,\n    :user_id\n", "  session_accessor(*%w[login_at user_id].map(&:to_sym))\n"),
        )
        .emit(BuildTarget::Ruby);
    let refused = refusals(&errors);
    for expected in [
        "constant `FEATURES` is a collection that is not frozen",
        "it writes or shadows the captured local `count`",
        "`session_accessor` is called with splatted or keyword arguments",
    ] {
        assert!(
            refused.iter().any(|e| e.contains(expected)),
            "no refusal containing {expected:?} in\n{}",
            errors.join("\n")
        );
    }
}

/// An expansion that would redefine a method the class writes itself is
/// refused: which one Ruby keeps depends on order the emitted class
/// does not reproduce.
#[test]
fn a_collision_with_an_existing_method_is_refused() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .write("app/models/feature_set/core.rb", FEATURE_SET_CORE)
        .write(
            "app/models/feature_set.rb",
            &FEATURE_SET.replace("  def initialize", "  def storefront = :mine\n\n  def initialize"),
        )
        .emit(BuildTarget::Ruby);
    assert!(refusals(&errors).is_empty(), "FeatureSet's own def is no collision on Core:\n{}", errors.join("\n"));
    // `storefront` is FeatureSet's own; the expansion lands on Core, an
    // ancestor, where FeatureSet's def wins in Ruby as in the emitted
    // tree: no collision on Core itself.
    let (_emitted, errors) = emit_and_run::real_blog()
        .write(
            "app/models/feature_set/core.rb",
            &FEATURE_SET_CORE.replace("    #: (Symbol) -> untyped\n    def [](name)", "    def storefront = :mine\n\n    #: (Symbol) -> untyped\n    def [](name)"),
        )
        .write("app/models/feature_set.rb", FEATURE_SET)
        .emit(BuildTarget::Ruby);
    assert!(
        refusals(&errors).iter().any(|e| e.contains("defines the method `storefront`, which the class also defines")),
        "no collision refusal in\n{}",
        errors.join("\n")
    );
}
