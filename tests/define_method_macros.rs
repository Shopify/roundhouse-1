//! Literal `define_method` + concern class-method macros (#30) as a
//! general pattern: interned names (Symbol or String), literal args,
//! `send(:literal)` collapse, and `class_methods do` / `module ClassMethods`.
//! Overlays on tiny-blog and real-blog — not a named-app special case.
//! Dynamic names and `public_send` stay unexpanded or uncollapsed.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::dialect::{MethodReceiver, ModelBodyItem};
use roundhouse::expr::ExprNode;
use roundhouse::ingest::{ingest_app_from_tree, survey};

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn tiny_overlay(concern: &str, concern_name: &str, post_body: &str) -> HashMap<PathBuf, Vec<u8>> {
    tree(&[
        (
            "db/schema.rb",
            include_str!("../fixtures/tiny-blog/db/schema.rb"),
        ),
        ("app/models/concerns/labeled.rb", concern),
        (
            "app/models/post.rb",
            &format!(
                "class Post < ApplicationRecord\n  include {concern_name}\n  has_many :comments\n  {post_body}\nend\n"
            ),
        ),
        (
            "app/models/comment.rb",
            include_str!("../fixtures/tiny-blog/app/models/comment.rb"),
        ),
    ])
}

fn post(app: &roundhouse::App) -> &roundhouse::dialect::Model {
    app.models
        .iter()
        .find(|m| m.name.0.as_str() == "Post")
        .expect("Post")
}

fn instance<'a>(app: &'a roundhouse::App, name: &str) -> &'a roundhouse::dialect::MethodDef {
    post(app)
        .methods()
        .find(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == name)
        .unwrap_or_else(|| panic!("missing {name}"))
}

fn unexpanded(app: &roundhouse::App, name: &str) -> bool {
    post(app).body.iter().any(|item| {
        matches!(item, ModelBodyItem::Unknown { expr, .. }
            if matches!(&*expr.node, ExprNode::Send { method, .. } if method.as_str() == name))
    })
}

const CLASS_METHODS_DO: &str = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method :label_of do
        send(field)
      end
    end
  end
end
"#;

/// `define_method :name do send(field) end` with a symbol arg collapses
/// `send(:title)` to a direct `title` call.
#[test]
fn tiny_blog_symbol_define_method_collapses_send() {
    let app = ingest_app_from_tree(tiny_overlay(CLASS_METHODS_DO, "Labeled", "labeled :title"))
        .expect("ingest");
    assert!(!unexpanded(&app, "labeled"));
    let body = roundhouse::emit::ruby::emit_expr(&instance(&app, "label_of").body);
    assert!(body.contains("title") && !body.contains("send("), "{body}");
}

/// Brace-block + string name + string arg: `define_method("name") { send(field) }`.
#[test]
fn tiny_blog_string_name_and_brace_block_ingests() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method("label_of") { send(field) }
    end
  end
end
"#;
    let app = ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled \"title\""))
        .expect("ingest");
    let body = roundhouse::emit::ruby::emit_expr(&instance(&app, "label_of").body);
    assert!(body.contains("title") && !body.contains("send("), "{body}");
}

/// Bound interned name: `define_method(name) { 1 }` with `labeled :chosen`.
#[test]
fn tiny_blog_bound_define_method_name() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(name)
      define_method(name) do
        1
      end
      private name
    end
  end
end
"#;
    let app =
        ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled :chosen")).expect("ingest");
    let method = instance(&app, "chosen");
    assert_eq!(
        method.visibility,
        roundhouse::dialect::MethodVisibility::Private
    );
}

/// Concern's other class-side spelling: `module ClassMethods`.
#[test]
fn tiny_blog_class_methods_module_ingests() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  module ClassMethods
    def labeled(field)
      self.define_method :label_of do
        __send__(field)
      end
    end
  end
end
"#;
    let app =
        ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled :title")).expect("ingest");
    let body = roundhouse::emit::ruby::emit_expr(&instance(&app, "label_of").body);
    assert!(
        body.contains("title") && !body.contains("send(") && !body.contains("__send__"),
        "{body}"
    );
}

/// `public_send(:literal)` is not a `send` collapse — visibility stays.
#[test]
fn public_send_literal_is_not_collapsed() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method :label_of do
        public_send(field)
      end
    end
  end
end
"#;
    let app =
        ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled :title")).expect("ingest");
    let body = roundhouse::emit::ruby::emit_expr(&instance(&app, "label_of").body);
    assert!(body.contains("public_send"), "{body}");
}

/// `@record.send(:literal)` and bare `helper.send(:literal)` keep
/// `send` so a private method on that object still succeeds. A bare
/// `helper` is a receiverless Send, not a Var. Same-expansion helpers
/// (`positioning_parent.send(:leaves)`) still collapse — see
/// `tests/model_macro_expansion.rs`.
#[test]
fn send_on_explicit_receiver_is_not_collapsed() {
    for body_src in ["@article.send(field)", "helper.send(field)"] {
        let concern = format!(
            r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method :label_of do
        {body_src}
      end
    end
  end
end
"#
        );
        let app = ingest_app_from_tree(tiny_overlay(&concern, "Labeled", "labeled :title"))
            .expect("ingest");
        let body = roundhouse::emit::ruby::emit_expr(&instance(&app, "label_of").body);
        assert!(
            body.contains("send(") && !body.contains("public_send"),
            "{body_src} => {body}"
        );
    }
}

/// Nested `send(:send, :literal)` collapses all the way to the name.
#[test]
fn nested_send_literal_collapses() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method :label_of do
        send(:send, field)
      end
    end
  end
end
"#;
    let app =
        ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled :title")).expect("ingest");
    let body = roundhouse::emit::ruby::emit_expr(&instance(&app, "label_of").body);
    assert!(
        body.contains("title") && !body.contains("send("),
        "{body}"
    );
}

#[test]
fn dynamic_define_method_name_is_not_expanded() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method(field.to_s + "_of") do
        send(field)
      end
    end
  end
end
"#;
    survey::activate();
    let app = ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled :title"))
        .expect("survey ingest");
    let gaps = survey::drain();
    assert!(unexpanded(&app, "labeled"));
    assert!(post(&app).methods().all(|m| m.name.as_str() != "title_of"));
    assert!(gaps.iter().any(|g| matches!(
        g,
        roundhouse::ingest::IngestError::Unsupported { message, .. }
            if message.contains("model macro `labeled` not expanded")
    )));
}

#[test]
fn computed_send_name_is_not_expanded() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method :label_of do
        send(field.to_s)
      end
    end
  end
end
"#;
    survey::activate();
    let app = ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled :title"))
        .expect("survey ingest");
    survey::drain();
    assert!(unexpanded(&app, "labeled"));
}

/// A String binding read as a value would lose capture identity.
/// Name slots (`send(field)`) still expand; this body does not.
#[test]
fn string_binding_read_as_value_is_not_expanded() {
    let concern = r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method :label_of do
        field
      end
    end
  end
end
"#;
    survey::activate();
    let app = ingest_app_from_tree(tiny_overlay(concern, "Labeled", "labeled \"title\""))
        .expect("survey ingest");
    survey::drain();
    assert!(unexpanded(&app, "labeled"));
    assert!(post(&app).methods().all(|m| m.name.as_str() != "label_of"));
}

/// String-name `define_method` plus collapsed `send` on the blog overlay
/// is the support claim: zero check errors and the emitted program runs.
#[test]
fn emitted_string_define_method_and_send_collapse_run() {
    emit_and_run::real_blog()
        .write(
            "app/models/concerns/labeled.rb",
            r#"module Labeled
  extend ActiveSupport::Concern
  class_methods do
    def labeled(field)
      define_method("label_of") { send(field) }
    end
  end
end
"#,
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            "class Article < ApplicationRecord\n  include Labeled\n  labeled \"title\"\n",
        )
        .run_ruby(
            r#"
a = Article.new(title: "Hello world", body: "abcdefghij")
raise "string define_method send collapse" unless a.label_of == "Hello world"
puts "define_method string name passed"
"#,
        )
        .assert_passes();
}

#[test]
fn emitted_class_methods_module_and_send_collapse_run() {
    emit_and_run::real_blog()
        .write(
            "app/models/concerns/labeled.rb",
            r#"module Labeled
  extend ActiveSupport::Concern
  module ClassMethods
    def labeled(field)
      define_method :label_of do
        send(field)
      end
    end
  end
end
"#,
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            "class Article < ApplicationRecord\n  include Labeled\n  labeled :title\n",
        )
        .run_ruby(
            r#"
a = Article.new(title: "Hello world", body: "abcdefghij")
raise "ClassMethods define_method" unless a.label_of == "Hello world"
puts "ClassMethods define_method passed"
"#,
        )
        .assert_passes();
}
