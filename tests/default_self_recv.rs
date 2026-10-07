//! Bare sends in parameter defaults → `self.<method>` (Spinel AOT).

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

#[test]
fn default_bare_send_gets_self_receiver() {
    let mut app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "push_subscriptions", force: :cascade do |t|
    t.integer "user_id"
  end
  create_table "users", force: :cascade do |t|
    t.string "name"
  end
  create_table "memberships", force: :cascade do |t|
    t.integer "user_id"
    t.integer "room_id"
  end
end
"#,
        ),
        (
            "app/models/user.rb",
            r#"class User < ApplicationRecord
  has_many :memberships
end
"#,
        ),
        (
            "app/models/membership.rb",
            r#"class Membership < ApplicationRecord
  belongs_to :user
  scope :unread, -> { where(room_id: 1) }
end
"#,
        ),
        (
            "app/models/push/subscription.rb",
            r#"class Push::Subscription < ApplicationRecord
  belongs_to :user

  def notification(badge: user.memberships.unread.count, **params)
    badge
  end
end
"#,
        ),
    ]))
    .expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = ruby::emit_lowered_models(&app);
    let src = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("subscription") && f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content.as_str())
        .unwrap_or("");
    assert!(
        src.contains("self.user"),
        "expected bare user in default to become self.user:\n{src}"
    );
    assert!(
        !src.contains("badge: user.memberships"),
        "bare user default must not survive:\n{src}"
    );
}

#[test]
fn default_bare_send_skips_class_method() {
    let mut app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "push_subscriptions", force: :cascade do |t|
    t.integer "user_id"
  end
  create_table "users", force: :cascade do |t|
    t.string "name"
  end
  create_table "memberships", force: :cascade do |t|
    t.integer "user_id"
    t.integer "room_id"
  end
end
"#,
        ),
        (
            "app/models/user.rb",
            r#"class User < ApplicationRecord
  has_many :memberships
end
"#,
        ),
        (
            "app/models/membership.rb",
            r#"class Membership < ApplicationRecord
  belongs_to :user
  scope :unread, -> { where(room_id: 1) }
end
"#,
        ),
        (
            "app/models/push/subscription.rb",
            r#"class Push::Subscription < ApplicationRecord
  belongs_to :user

  def self.notification(badge: user.memberships.unread.count)
    badge
  end
end
"#,
        ),
    ]))
    .expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = ruby::emit_lowered_models(&app);
    let src = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("subscription") && f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content.as_str())
        .unwrap_or("");
    assert!(
        src.contains("badge: user.memberships"),
        "class-method default must stay a bare send (no instance-typed self):\n{src}"
    );
    assert!(
        !src.contains("badge: self.user"),
        "must not stamp instance self on a class-method default:\n{src}"
    );
}
