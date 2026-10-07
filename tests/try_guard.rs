//! `recv.try(:m)` guards DEFINEDNESS, not nilness.
//!
//! Ingest used to ground it to `recv && recv.m` — the `&.` desugar. The
//! two agree exactly when the receiver either is nil or DOES define the
//! method. campfire's own `turbo_test_helper` is where they part:
//!
//! ```ruby
//! streambles.collect { |s| s.try(:to_gid_param) || s }.join(":")
//! ```
//!
//! over `[room, :messages]`. `:messages` is not nil and answers no
//! `to_gid_param`, so Rails returns nil and takes the `|| s` arm; the
//! nil guard called it and raised.

use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "rooms", force: :cascade do |t|
    t.string "name", null: false
  end
  create_table "users", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#;

fn emit(files: &[(&str, &str)]) -> String {
    let mut all: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
        // campfire has this file, and its presence is what lets the
        // cover collapse to one arm: `ApplicationRecord` is an APP class
        // there, so it can carry the synthesized `to_gid_param` the
        // narrowing tests for.
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\nend\n",
        ),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  resources :rooms\nend\n",
        ),
    ];
    all.extend_from_slice(files);
    let tree: HashMap<PathBuf, Vec<u8>> = all
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .into_iter()
        .chain(ruby::emit_library(&app))
        .chain(ruby::emit_lowered_views(&app))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `to_gid_param` is synthesized onto EVERY model, so the test is the
/// base they share — one `is_a?`, not fifteen.
#[test]
fn a_universal_synthesized_method_narrows_to_the_shared_base() {
    let src = emit(&[(
        "app/models/user.rb",
        "class User < ApplicationRecord\n  def label(s)\n    s.try(:to_gid_param) || s\n  end\nend\n",
    )]);
    assert!(
        src.contains("is_a?(ApplicationRecord)"),
        "every model answers `to_gid_param`, so their base is the test:\n{src}"
    );
    assert!(
        !src.contains("s && s.to_gid_param"),
        "the nil guard is what raised on a Symbol:\n{src}"
    );
}

/// THE PARENTHESES ARE THE FIX AS MUCH AS THE NARROWING IS. A
/// modifier-`if` binds looser than `||`, so `s.to_gid_param if s.is_a?(X)
/// || s` swallows the fallback INTO the condition and answers nil.
#[test]
fn a_conditional_operand_is_parenthesized() {
    let src = emit(&[(
        "app/models/user.rb",
        "class User < ApplicationRecord\n  def label(s)\n    s.try(:to_gid_param) || s\n  end\nend\n",
    )]);
    let line = src
        .lines()
        .find(|l| l.contains("to_gid_param") && l.contains("|| s"))
        .expect("the try site")
        .trim();
    assert!(
        line.starts_with('(') && line.contains(") || s"),
        "without the parens the `|| s` lands inside the condition:\n{line}"
    );
}

/// A method the tree declares on ONE class narrows to that class. Nil
/// still answers nil — `nil.is_a?(X)` is false — so the narrowing does
/// everything the nil guard did.
#[test]
fn a_single_definer_narrows_to_itself() {
    let src = emit(&[(
        "app/models/user.rb",
        "class User < ApplicationRecord\n  def nickname\n    \"nick\"\n  end\n\n  \
         def show(other)\n    other.try(:nickname)\n  end\nend\n",
    )]);
    assert!(
        src.contains("is_a?(User)"),
        "only User answers `nickname`:\n{src}"
    );
}

/// A name no APP class declares may still be a RUNTIME method
/// (`to_param`, `strip`). This pass sees app classes only, so folding to
/// nil would be wrong for exactly those — the nil guard stays.
#[test]
fn an_unknown_name_keeps_the_nil_guard() {
    let src = emit(&[(
        "app/models/user.rb",
        "class User < ApplicationRecord\n  def show(other)\n    other.try(:strip)\n  end\nend\n",
    )]);
    assert!(
        src.contains("other && other.strip"),
        "a runtime method is invisible here; guessing nil would be worse:\n{src}"
    );
}

/// `try!` RAISES where `try` returns nil, so its semantics for a missing
/// method are already the nil guard's. Narrowing it into a silent nil
/// would be the wrong direction.
#[test]
fn try_bang_keeps_the_nil_guard() {
    let src = emit(&[(
        "app/models/user.rb",
        "class User < ApplicationRecord\n  def nickname\n    \"nick\"\n  end\n\n  \
         def show(other)\n    other.try!(:nickname)\n  end\nend\n",
    )]);
    assert!(
        src.contains("other && other.nickname"),
        "try! must not become a silent nil:\n{src}"
    );
}

/// THE DRIFT GUARD on `SYNTHESIZED_ON_EVERY_MODEL`. That list exists
/// because `to_gid_param` is pushed at emit-prep time, after this pass
/// runs — so the pass cannot see it and has to be told. If the synthesis
/// moves or stops, the list silently narrows `try(:to_gid_param)` to
/// classes that no longer answer it.
#[test]
fn the_synthesized_method_list_still_matches_the_synthesizer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(root.join("src/lower/broadcasts.rs"))
        .expect("src/lower/broadcasts.rs");
    assert!(
        src.contains("pub fn push_to_gid_param"),
        "`lower::try_guard::SYNTHESIZED_ON_EVERY_MODEL` names `to_gid_param` because \
         `lower::broadcasts::push_to_gid_param` puts it on every model. That function \
         is gone or renamed — the list is now a claim nothing backs."
    );
}

/// A VIEW is a body too. The ingest desugar this pass replaced was
/// universal; `for_each_hook_body` is not, and views are the bodies it
/// leaves out. lobsters' layout is where that showed: `<body
/// data-username='<%= @user.try(:username) %>'>` reached the Ruby lane
/// as `user.try(:username)`, and every request raised before the page
/// rendered — no target's runtime defines `try`.
#[test]
fn a_try_in_a_view_is_grounded_like_any_other_body() {
    let src = emit(&[
        (
            "app/models/user.rb",
            "class User < ApplicationRecord\n  def nickname\n    \"nick\"\n  end\nend\n",
        ),
        (
            "app/controllers/rooms_controller.rb",
            "class RoomsController < ApplicationController\n  def show\n    @user = User.first\n  end\nend\n",
        ),
        ("app/views/rooms/show.html.erb", "<p><%= @user.try(:nickname) %></p>\n"),
    ]);
    assert!(
        !src.contains(".try("),
        "a view keeps no raw `try` — nothing at runtime answers it:\n{src}"
    );
    assert!(
        src.contains("is_a?(User)"),
        "only User answers `nickname`, in a view as in a method:\n{src}"
    );
}

/// A `try` on a name only the RUNTIME defines keeps the nil guard — and
/// the call it desugars to carries the type the analyzer gave the
/// `try`, so the post-lowering diagnose does not read a typed receiver
/// with an untyped send as a dispatch failure. The Action Text blob
/// partial's `blob.try(:caption)` — `ActionText::Attachment` is a
/// registry class, not an app class — is the site that turned a clean
/// `check` into a transpile error.
#[test]
fn a_runtime_method_try_keeps_its_type_through_the_guard() {
    let mut all: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/room.rb", "class Room < ApplicationRecord\n  has_rich_text :body\nend\n"),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\nend\n",
        ),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms\nend\n"),
        (
            "app/views/active_storage/blobs/_blob.html.erb",
            "<figcaption><% if caption = blob.try(:caption) %><%= caption %><% end %></figcaption>\n",
        ),
    ];
    all.sort();
    let tree: HashMap<PathBuf, Vec<u8>> = all
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == roundhouse::analyze::Severity::Error)
        .map(|d| d.message)
        .collect();
    assert!(
        errors.is_empty(),
        "the desugared `blob.caption` must not diagnose as a failed dispatch:\n{errors:#?}"
    );
}

/// Scale, the shape full Core has: a name with a thousand definers under
/// one base, and many `try` sites. The cover must still be the one base,
/// and a name with more unrelated definers than `MAX_ARMS` keeps the nil
/// guard.
///
/// This is also the regression test for a post-analyze stall. Climbing
/// used to re-check "every descendant answers" for each defining
/// ancestor by scanning every class in the tree, |definers| * |classes|
/// * depth^2 per site; on the v3.1 Core slice that held the fused
/// context rewrites for 13+ minutes. On this fixture the old check made
/// the debug test take 114 s, with the same output; now it is one
/// ancestry walk per definer per site and the test takes under a second.
#[test]
fn a_thousand_definers_under_one_base_narrow_to_the_base_quickly() {
    const WIDGETS: usize = 1000;
    const SITES: usize = 40;
    let mut owned: Vec<(String, String)> = Vec::new();
    owned.push((
        "app/models/widget.rb".into(),
        "class Widget\n  def title\n    \"w\"\n  end\nend\n".into(),
    ));
    for i in 0..WIDGETS {
        owned.push((
            format!("app/models/widget{i}.rb"),
            format!("class Widget{i} < Widget\n  def title\n    \"w{i}\"\n  end\nend\n"),
        ));
    }
    for i in 0..6 {
        owned.push((
            format!("app/models/gadget{i}.rb"),
            format!("class Gadget{i}\n  def label\n    \"g{i}\"\n  end\nend\n"),
        ));
    }
    let mut user = String::from("class User < ApplicationRecord\n");
    for i in 0..SITES {
        user.push_str(&format!(
            "  def show{i}(w, g)\n    [w.try(:title), g.try(:label)]\n  end\n\n"
        ));
    }
    user.push_str("end\n");
    owned.push(("app/models/user.rb".into(), user));
    let files: Vec<(&str, &str)> =
        owned.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let src = emit(&files);
    let shown = src.find("def show0").map(|i| &src[i..src.len().min(i + 1500)]).unwrap_or(&src[..src.len().min(1500)]);
    assert_eq!(
        src.matches("(w.title if w.is_a?(Widget))").count(),
        SITES,
        "every Widget subclass inherits `title` from Widget, so Widget is the one arm:\n{shown}"
    );
    assert_eq!(
        src.matches("g && g.label").count(),
        SITES,
        "six unrelated definers is past MAX_ARMS, so the nil guard stays:\n{shown}",
    );
}
