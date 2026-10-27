//! An app with no `db/schema.rb` folds its migrations into the schema,
//! `change_table` blocks included. A collation-only `t.change` must keep
//! the column's `NOT NULL` and default, the raw-SQL table collation
//! change that follows it must fold, and the renamed, added and
//! defaulted columns must reach the emitted DDL and models.

use super::emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "db/migrate/20260101000000_create_posts.rb",
            r#"class CreatePosts < ActiveRecord::Migration[8.1]
  def change
    create_table :posts do |t|
      t.string :title, null: false, default: "Untitled"
      t.text :body
      t.string :legacy
      t.integer :views
    end
  end
end
"#,
        )
        .write(
            "db/migrate/20260102000000_rework_posts.rb",
            r#"class ReworkPosts < ActiveRecord::Migration[8.1]
  def change
    change_table(:posts, bulk: true) do |t|
      t.change(:title, :string, collation: "binary")
      t.rename :body, :content
      t.remove :legacy
      t.string :slug, null: false, default: ""
      t.change_default :views, from: nil, to: 0
      t.timestamps null: true
    end
  end
end
"#,
        )
        .write(
            "db/migrate/20260103000000_posts_default_collation.rb",
            r#"class PostsDefaultCollation < ActiveRecord::Migration[8.1]
  def up
    execute("ALTER TABLE posts COLLATE=utf8mb4_bin")
    execute <<~SQL
      ALTER TABLE posts DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
    SQL
  end
end
"#,
        )
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        )
        .write("app/models/post.rb", "class Post < ApplicationRecord\nend\n")
        .write(
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        )
}

#[test]
fn change_table_migrations_fold_into_the_running_schema() {
    let run = app().run_ruby(
        r#"
post = Post.create!(content: "Hello", slug: "hello")
raise "title default: #{post.title.inspect}" unless post.title == "Untitled"
raise "views default: #{post.views.inspect}" unless post.views == 0
reloaded = Post.find(post.id)
raise "content: #{reloaded.content.inspect}" unless reloaded.content == "Hello"
raise "slug: #{reloaded.slug.inspect}" unless reloaded.slug == "hello"
puts "change_table schema passed"
"#,
    );
    run.assert_passes();
    assert!(run.stdout.contains("change_table schema passed"));
}
