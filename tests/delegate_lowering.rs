//! ActiveSupport's `delegate :a, to: :b` becomes real methods
//! (`ingest::delegate`).
//!
//! Rails defines these with `class_eval` at load time, so nothing about
//! them reaches an emitted tree: the declaration lands in
//! `unknown_calls` and every call to a delegated name is a bare send no
//! class defines. Where the caller rescues, the failure is invisible —
//! campfire's `message_presentation` wraps its body in `rescue
//! Exception` and returns `""`, so a missing `fragment` drew every
//! message with an EMPTY body and no error anywhere.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn emit(helper: &str) -> String {
    let app = ingest_app_from_tree(tree(helper)).expect("ingest");
    ruby::emit_library(&app)
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("filter.rb"))
        .map(|f| f.content.clone())
        .expect("filter.rb")
}

fn tree(helper: &str) -> HashMap<PathBuf, Vec<u8>> {
    let files: Vec<(&str, String)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"accounts\" do |t|\n    t.string \"name\"\n  end\nend\n".to_string(),
        ),
        ("app/models/account.rb", "class Account < ApplicationRecord\nend\n".to_string()),
        ("app/helpers/filter.rb", helper.to_string()),
    ];
    files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.into_bytes()))
        .collect()
}

fn emit_model(app: &mut roundhouse::App, suffix: &str) -> String {
    roundhouse::session::analyze_and_lower(app);
    ruby::emit_lowered_models(app)
        .into_iter()
        .find(|file| file.path.to_string_lossy().ends_with(suffix))
        .unwrap_or_else(|| panic!("missing emitted model {suffix}"))
        .content
}

/// Rails defines a forwarder with a string `module_eval`, whose fresh
/// scope is public, so a bare `private` above `delegate` does not reach
/// it: under ActiveSupport 8.1.3, `private; delegate :display_name, to:
/// :profile` leaves `display_name` public (`private_method_defined?` is
/// false). Both the model and the library-class forwarder stay public.
#[test]
fn a_bare_private_does_not_reach_delegate_forwarders_as_in_rails() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  has_one :profile\n  private\n  delegate :display_name, to: :profile\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("db/schema.rb"),
        b"ActiveRecord::Schema.define do\n  create_table \"accounts\" do |t|\n    t.string \"name\"\n  end\n  create_table \"profiles\" do |t|\n    t.integer \"account_id\"\n    t.string \"display_name\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/profile.rb"),
        b"class Profile < ApplicationRecord\n  belongs_to :account\n  def display_name\n    \"profile\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("lib/private_service.rb"),
        b"class PrivateService\n  attr_reader :profile\n  private\n  delegate :display_name, to: :profile\nend\n".to_vec(),
    );

    let app = ingest_app_from_tree(files).expect("ingest");
    let account = app
        .models
        .iter()
        .find(|model| model.name.0.as_str() == "Account")
        .expect("Account");
    let account_delegate = account
        .methods()
        .find(|method| method.name.as_str() == "display_name")
        .expect("model delegate");
    assert_eq!(
        account_delegate.visibility,
        roundhouse::dialect::MethodVisibility::Public
    );
    let service = app
        .library_classes
        .iter()
        .find(|class| class.name.0.as_str() == "PrivateService")
        .expect("PrivateService");
    let service_delegate = service
        .methods
        .iter()
        .find(|method| method.name.as_str() == "display_name")
        .expect("library-class delegate");
    assert_eq!(
        service_delegate.visibility,
        roundhouse::dialect::MethodVisibility::Public
    );
}

#[test]
fn delegate_target_surface_respects_ruby_concern_precedence() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("db/schema.rb"),
        b"ActiveRecord::Schema.define do\n  create_table \"profiles\" do |t|\n    t.string \"name\"\n  end\n  create_table \"pages\" do |t|\n    t.integer \"profile_id\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/concerns/private_name.rb"),
        b"module PrivateName\n  private\n  def name\n    \"private\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/concerns/public_name.rb"),
        b"module PublicName\n  def name\n    \"public\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/profile.rb"),
        b"class Profile < ApplicationRecord\n  include PrivateName\n  include PublicName\nend\n"
            .to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/page.rb"),
        b"class Page < ApplicationRecord\n  belongs_to :profile\n  delegate :name, to: :profile\nend\n".to_vec(),
    );

    let app = ingest_app_from_tree(files.clone()).expect("ingest");
    let page = app
        .models
        .iter()
        .find(|model| model.name.0.as_str() == "Page")
        .expect("Page");
    assert!(
        page.methods().any(|method| method.name.as_str() == "name"),
        "a later public concern method must be eligible for delegation"
    );

    files.insert(
        PathBuf::from("app/models/profile.rb"),
        b"class Profile < ApplicationRecord\n  include PublicName\n  include PrivateName\nend\n"
            .to_vec(),
    );
    let app = ingest_app_from_tree(files).expect("ingest");
    let page = app
        .models
        .iter()
        .find(|model| model.name.0.as_str() == "Page")
        .expect("Page");
    // activesupport's delegate.rb defines the forwarder with `module_eval`
    // whatever the target's visibility, and calling a private target then
    // raises NoMethodError. The expansion no longer reads the target's
    // surface, so the forwarder is declared in both orders.
    assert!(
        page.methods().any(|method| method.name.as_str() == "name"),
        "a delegate is a forwarder whatever the target's concern precedence"
    );
}

#[test]
fn model_delegates_forward_collection_receivers_and_targets_that_yield() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("db/schema.rb"),
        b"ActiveRecord::Schema.define do\n  create_table \"accounts\" do |t|\n    t.string \"name\"\n  end\n  create_table \"comments\" do |t|\n    t.integer \"account_id\"\n    t.string \"email\"\n  end\n  create_table \"profiles\" do |t|\n    t.integer \"account_id\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  has_many :comments\n  has_one :profile\n  delegate :email, to: :comments, prefix: true\n  delegate :render, :block_sensitive?, to: :profile\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/comment.rb"),
        b"class Comment < ApplicationRecord\n  belongs_to :account\n  def email\n    \"comment@example.test\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/profile.rb"),
        b"class Profile < ApplicationRecord\n  belongs_to :account\n  def render\n    yield\n  end\n  def block_sensitive?\n    block_given?\n  end\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    let output = emit_model(&mut app, "account.rb");
    // Rails' forwarders are `(...)` calls on the target as it stands, so a
    // collection receiver or a yielding target is the caller's to get
    // right; a zero-argument, block-less forwarder would be wrong for both.
    for (name, target) in [
        ("comments_email", "comments.email"),
        ("render", "profile.render"),
        ("block_sensitive?", "profile.block_sensitive?"),
    ] {
        assert!(
            output.contains(&format!("def {name}(...)\n    {target}(...)\n  end")),
            "{name} forwards its arguments and block:\n{output}"
        );
        assert!(
            !output.contains(&format!("def {name}\n")),
            "{name} has no zero-argument form:\n{output}"
        );
    }
}

/// The plain form: the forwarder CALLS the target, which is what Rails'
/// generated body does and what makes it work when the target is an
/// `attr_reader` rather than a raw ivar. On the Ruby target it forwards
/// `(...)` as Rails' does (`App::ruby_delegate_forwarders`); the IR's is
/// zero-arg for the strict targets.
#[test]
fn a_delegated_reader_becomes_a_forwarding_method() {
    let out = emit(
        r#"class Filter
  attr_reader :content

  def initialize(content)
    @content = content
  end

  delegate :fragment, :to_plain_text, to: :content
end
"#,
    );
    assert!(out.contains("def fragment(...)\n    content.fragment(...)\n  end"), "got:\n{out}");
    assert!(
        out.contains("def to_plain_text(...)\n    content.to_plain_text(...)\n  end"),
        "got:\n{out}"
    );
    // The declaration is CONSUMED, not replayed beside its expansion.
    assert!(!out.contains("delegate"), "got:\n{out}");
}

/// `prefix: true` and `allow_nil: true` — campfire's `Current` spells
/// both, and the general pass has to answer them the same way. The
/// nil guard is written as a ternary so the body still ends in a
/// read; the ruby emitter renders it as the if/else it lowers to.
#[test]
fn prefix_and_allow_nil_are_honoured() {
    let out = emit(
        r#"class Filter
  attr_reader :request

  delegate :host, to: :request, prefix: true, allow_nil: true
end
"#,
    );
    assert!(
        out.contains(
            "def request_host(...)\n    __delegate_target = request\n    if __delegate_target.nil? && !(nil.respond_to?(:host))\n      nil\n    else\n      __delegate_target.host(...)\n    end\n  end"
        ),
        "got:\n{out}"
    );
}

/// An explicit `prefix: :parent` or `prefix: "writer"` names the
/// forwarder `<prefix>_<method>`, as ActiveSupport does. On the Ruby
/// target a reader forwards `(...)` (`App::ruby_delegate_forwarders`).
#[test]
fn explicit_symbol_and_string_prefixes_are_honoured() {
    let out = emit(
        r#"class Filter
  delegate :title, to: :article, prefix: :parent
  delegate :author, to: :article, prefix: "writer", allow_nil: false
end
"#,
    );
    assert!(
        out.contains("def parent_title(...)\n    article.title(...)\n  end"),
        "got:\n{out}"
    );
    assert!(
        out.contains("def writer_author(...)\n    article.author(...)\n  end"),
        "got:\n{out}"
    );
}

/// DECLINED: a delegated name this class calls with ARGUMENTS. Rails
/// forwards them with `*args, &block`, which the strict targets do not
/// lower, and a zero-arg forwarder for a method that takes two is an
/// arity error standing in for a NameError. campfire's
/// `Messages::AttachmentPresentation` is exactly this shape. The
/// declaration stays in `unknown_calls` and the IR has no forwarder; only
/// the Ruby target, which has no argument-forwarding gap, writes the
/// `(...)` one Rails generates.
#[test]
fn a_delegation_called_with_arguments_is_left_alone() {
    let helper = r#"class Filter
  attr_reader :context

  delegate :link_to, to: :context

  def render
    link_to "text", "/path"
  end
end
"#;
    let app = ingest_app_from_tree(tree(helper)).expect("ingest");
    let filter = app.library_classes.iter().find(|c| c.name.0.as_str() == "Filter").expect("Filter");
    assert!(!filter.methods.iter().any(|m| m.name.as_str() == "link_to"), "{:?}", filter.methods);
    assert!(
        filter.unknown_calls.iter().any(|c| matches!(&*c.node,
            roundhouse::expr::ExprNode::Send { method, .. } if method.as_str() == "delegate")),
        "the declaration stays visible"
    );
    let out = emit(helper);
    assert!(out.contains("def link_to(...)\n    context.link_to(...)\n  end"), "got:\n{out}");
}

#[test]
fn a_delegate_to_methods_with_required_or_optional_arguments_is_left_alone() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  def formatted_name(format)\n    format\n  end\n\n  def display_name(format = :short)\n    format.to_s\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/post.rb"),
        b"class Post < ApplicationRecord\n  belongs_to :account\n  delegate :formatted_name, :display_name, to: :account\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    let emitted = emit_model(&mut app, "post.rb");
    assert!(
        !emitted.contains("def formatted_name\n"),
        "a zero-argument forwarder must not be synthesized for a required-argument target method:\n{emitted}"
    );
    assert!(
        emitted.contains("def formatted_name(...)\n    account.formatted_name(...)\n  end"),
        "the required argument is forwarded:\n{emitted}"
    );
    assert!(
        emitted.contains("def display_name(...)\n    account.display_name(...)\n  end"),
        "optional arguments are forwarded, not dropped:\n{emitted}"
    );
}

#[test]
fn model_delegate_setters_and_fixed_arity_operators_are_synthesized() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  def +(other)\n    other\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/post.rb"),
        b"class Post < ApplicationRecord\n  belongs_to :account\n  delegate :name=, to: :account\n  delegate :+, to: :account\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    let post = emit_model(&mut app, "post.rb");
    assert!(
        post.contains("def name=(value)"),
        "setter forwarder missing:\n{post}"
    );
    assert!(
        post.contains("def +(other)"),
        "binary-operator forwarder missing:\n{post}"
    );
}

#[test]
fn a_later_unsupported_delegate_does_not_leave_an_earlier_forwarder() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/profile.rb"),
        b"class Profile < ApplicationRecord\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/post.rb"),
        b"class Post < ApplicationRecord\n  belongs_to :account\n  belongs_to :profile\n  delegate :name, to: :account\n  delegate :name, to: :profile\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    let post = emit_model(&mut app, "post.rb");
    assert!(
        !post.contains("def name\n"),
        "the earlier delegate must not override Rails' later unsupported declaration:\n{post}"
    );
}

#[test]
fn a_later_model_delegate_replaces_an_earlier_method_with_the_same_name() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  def title\n    \"account title\"\n  end\nend\n"
            .to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/page.rb"),
        b"class Page < ApplicationRecord\n  belongs_to :account\n  def title\n    \"local title\"\n  end\n  delegate :title, to: :account\nend\n".to_vec(),
    );

    let mut app = ingest_app_from_tree(files).expect("ingest");
    let page = emit_model(&mut app, "page.rb");
    assert!(
        page.contains("def title(...)\n    account.title(...)\n  end"),
        "a delegate declared after a same-named method must win in source order:\n{page}"
    );
    assert!(
        !page.contains("local title"),
        "the earlier method body must not survive the later delegate:\n{page}"
    );
}

#[test]
fn class_method_calls_do_not_suppress_instance_delegates() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  def title\n    \"account title\"\n  end\nend\n"
            .to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/concerns/title_calls.rb"),
        b"module TitleCalls\n  def self.preview\n    title(\"from concern\")\n  end\nend\n"
            .to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/page.rb"),
        b"class Page < ApplicationRecord\n  include TitleCalls\n  belongs_to :account\n  def self.preview\n    title(\"from model\")\n  end\n  delegate :title, to: :account\nend\n".to_vec(),
    );

    let mut app = ingest_app_from_tree(files).expect("ingest");
    let page = emit_model(&mut app, "page.rb");
    assert!(
        page.contains("def title(...)\n    account.title(...)\n  end"),
        "class-side calls must not require instance delegate argument forwarding:\n{page}"
    );
}

#[test]
fn a_delegate_to_a_private_target_method_is_left_alone() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  private\n  def internal_name\n    \"hidden\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/post.rb"),
        b"class Post < ApplicationRecord\n  belongs_to :account\n  delegate :internal_name, to: :account\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    let emitted = emit_model(&mut app, "post.rb");
    // Rails defines the forwarder regardless; calling it reaches the private
    // target through the association reader, so Ruby raises NoMethodError
    // exactly as Rails' does.
    assert!(
        emitted.contains("def internal_name(...)\n    account.internal_name(...)\n  end"),
        "the forwarder reaches the target through its reader:\n{emitted}"
    );
}

#[test]
fn a_private_override_blocks_an_inherited_concern_method_from_delegation() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/concerns/public_name.rb"),
        b"module PublicName\n  def internal_name\n    \"public\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  include PublicName\n  private\n  def internal_name\n    \"hidden\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/post.rb"),
        b"class Post < ApplicationRecord\n  belongs_to :account\n  delegate :internal_name, to: :account\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let post = ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|file| file.path.to_string_lossy().ends_with("post.rb"))
        .expect("post.rb")
        .content;
    assert!(
        post.contains("def internal_name(...)\n    account.internal_name(...)\n  end"),
        "the forwarder is Rails' whatever the target's override; a private one raises when called:\n{post}"
    );
}

#[test]
fn a_public_model_override_remains_available_for_delegation() {
    let mut files = tree("");
    files.insert(
        PathBuf::from("app/models/concerns/private_name.rb"),
        b"module PrivateName\n  private\n  def internal_name\n    \"hidden\"\n  end\nend\n"
            .to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/account.rb"),
        b"class Account < ApplicationRecord\n  include PrivateName\n  def internal_name\n    \"visible\"\n  end\nend\n".to_vec(),
    );
    files.insert(
        PathBuf::from("app/models/post.rb"),
        b"class Post < ApplicationRecord\n  belongs_to :account\n  delegate :internal_name, to: :account\nend\n".to_vec(),
    );
    let mut app = ingest_app_from_tree(files).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let post = ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|file| file.path.to_string_lossy().ends_with("post.rb"))
        .expect("post.rb")
        .content;
    assert!(
        post.contains("def internal_name(...)\n    account.internal_name(...)\n  end"),
        "a public model override must outrank the concern's private method:\n{post}"
    );
}

/// DECLINED: an option this pass does not reproduce. Half-expanding a
/// declaration is worse than leaving it visible.
#[test]
fn an_unreproducible_option_is_left_alone() {
    let out = emit(
        r#"class Filter
  attr_reader :content

  delegate :fragment, to: :content, private: true
end
"#,
    );
    assert!(!out.contains("def fragment"), "got:\n{out}");
}

#[test]
fn an_invalid_explicit_prefix_is_left_alone() {
    let out = emit(
        r#"class Filter
  delegate :title, to: :article, prefix: "bad;raise"
end
"#,
    );
    assert!(
        !out.contains("def bad"),
        "invalid prefix must not create source code:\n{out}"
    );
}

/// A method the class writes itself WINS — the expansion fills gaps, it
/// does not overwrite.
#[test]
fn a_hand_written_method_is_not_replaced() {
    let out = emit(
        r#"class Filter
  attr_reader :content

  delegate :fragment, to: :content

  def fragment
    "already mine"
  end
end
"#,
    );
    assert!(out.contains("\"already mine\""), "got:\n{out}");
    assert_eq!(out.matches("def fragment").count(), 1, "got:\n{out}");
}

/// `to: :class` (or any other Ruby keyword) is `self.class` in the
/// forwarder, as Rails writes it: a bare `class.label` does not parse.
/// Shopify core has dozens (`delegate :context, to: :class`).
#[test]
fn a_keyword_target_is_read_through_self() {
    let out = emit(
        r#"class Filter
  attr_reader :return
  delegate :label, to: :class
  delegate :id, to: :return

  def self.label
    "f"
  end
end
"#,
    );
    assert!(out.contains("self.class.label"), "got:\n{out}");
    assert!(out.contains("self.return.id"), "got:\n{out}");
}

#[test]
fn a_delegated_writer_forwards_its_value() {
    // billing's `delegate :request_id, :request_id=, to: :class`: the
    // writer needs a parameter, and a zero-arg `def request_id=` is not
    // Ruby at all — the snippet failed to parse and took the reader
    // down with it.
    let out = emit(
        r#"class Filter
  delegate :request_id, :request_id=, to: :class

  def self.request_id
    @request_id
  end
end
"#,
    );
    assert!(out.contains("self.class.request_id"), "got:\n{out}");
    assert!(out.contains("self.class.request_id = value"), "got:\n{out}");
}

#[test]
fn delegated_operators_forward_their_operands() {
    // Binary operators have a fixed single-argument contract; unary
    // operators remain valid delegates, forwarding `(...)` on the Ruby
    // target as Rails generates them.
    let out = emit(
        r#"class Filter
  delegate :<<, :==, :!, :-@, to: :@set

  def initialize
    @set = {}
  end
end
"#,
    );
    assert!(out.contains("@set << other"), "got:\n{out}");
    assert!(out.contains("@set == other"), "got:\n{out}");
    assert!(out.contains("def !(...)\n    @set.!(...)\n  end"), "got:\n{out}");
    assert!(out.contains("def -@(...)\n    @set.-@(...)\n  end"), "got:\n{out}");
}

#[test]
fn a_fixed_arity_operator_call_does_not_suppress_its_delegate() {
    let out = emit(
        r#"class Filter
  delegate :+, to: :@value

  def initialize
    @value = 1
  end

  def add(other)
    self + other
  end
end
"#,
    );
    assert!(
        out.contains("def +(other)\n    @value + other\n  end"),
        "the fixed-arity operator delegate must remain available to its caller:\n{out}"
    );
}

/// `[]` and `[]=` take any positional arity, so on the Ruby target they
/// forward `(...)`, as Rails generates them; a fixed `(key)` signature
/// would reject `filter[a, b]`.
#[test]
fn index_operators_forward_any_arity_on_the_ruby_target() {
    let source = r#"class Filter
  delegate :[], :[]=, to: :@items

  def initialize
    @items = Grid.new
  end

  def items
    @items
  end
end
"#;
    let out = emit(source);
    assert!(out.contains("def [](...)\n    @items.[](...)\n  end"), "got:\n{out}");
    assert!(out.contains("def []=(...)\n    @items.[]=(...)\n  end"), "got:\n{out}");
    assert_eq!(
        run_emitted(
            source,
            "class Grid; def [](*a) = a; def []=(*a); @last = a; end; attr_reader :last; end\nf = Filter.new; p f[1, 2]; f[1, 2] = 3; p f.items.last"
        ),
        "[1, 2]\n[1, 2, 3]\n"
    );
}

fn run_emitted(source: &str, exercise: &str) -> String {
    let emitted = emit(source);
    let script = format!("{emitted}\n{exercise}");
    let output = std::process::Command::new("ruby")
        .args(["-e", &script])
        .output()
        .expect("ruby");
    assert!(
        output.status.success(),
        "{}\n{script}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8")
}

#[test]
fn writer_arguments_do_not_shadow_the_target_method() {
    let source = r#"class Filter
  attr_reader :value
  def initialize(value)
    @value = value
  end
  delegate :title=, to: :value
end
"#;
    assert_eq!(
        run_emitted(
            source,
            "target = Struct.new(:title).new; Filter.new(target).title = 'changed'; puts target.title"
        ),
        "changed\n"
    );
}

/// Rails' `allow_nil: true` forwarder is `if !_.nil? ||
/// nil.respond_to?(:m)`: a method nil implements (`to_s`) runs on nil,
/// and only one it lacks (`host`) answers nil.
#[test]
fn allow_nil_runs_methods_nil_implements_as_rails_does() {
    let source = r#"class Filter
  attr_reader :request

  delegate :to_s, to: :request, allow_nil: true
  delegate :host, to: :request, allow_nil: true
end
"#;
    assert_eq!(run_emitted(source, "f = Filter.new; p f.to_s, f.host"), "\"\"\nnil\n");
}
