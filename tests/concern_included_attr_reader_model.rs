//! `attr_reader` inside a Concern's `included do` defines the reader on each
//! model includer, as the same declaration in its class body would.
//!
//! Core's `ImageAttachment` declares `attr_reader :filename` there and writes
//! the ivar from `filename=`; `svg_filename?` reads the bare `filename`. The
//! reader never reached `ShopImage`, so the read raised NameError
//! ("undefined local variable or method 'filename'").
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ATTACHMENT: &str = r#"module Attachment
  extend ActiveSupport::Concern

  included do
    before_create :stamp
    validates :title, length: { maximum: 40 }
    attr_reader :filename
    attr_reader :require_upload
  end

  def filename=(name)
    @filename = name.to_s.strip unless name.nil?
  end

  def svg_filename?
    return false unless filename

    File.extname(filename).downcase == ".svg"
  end

  def stamp
    @require_upload = true
  end
end
"#;

const PICTURE: &str = r#"class Picture < ApplicationRecord
  include Attachment
end
"#;

const SCRIPT: &str = r#"pic = Picture.new(title: "t")
p pic.svg_filename?
pic.filename = " logo.SVG "
p [pic.filename, pic.svg_filename?]
pic.save!
p pic.require_upload
"#;

#[test]
fn a_concern_s_included_attr_reader_is_the_model_includer_s_reader() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(
            "require \"active_record\"\nActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\nActiveRecord::Schema.verbose = false\nActiveRecord::Schema.define { create_table(:pictures) { |t| t.string :title } }\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"
                .to_string()
                + ATTACHMENT
                + PICTURE
                + SCRIPT,
        )
        .output()
        .expect("CRuby control");
    let expected = "false\n[\"logo.SVG\", true]\ntrue\n";
    if native.status.success() {
        assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    }
    let run = emit_and_run::real_blog()
        .edit("db/schema.rb", "create_table \"articles\", force: :cascade do |t|", "create_table \"pictures\", force: :cascade do |t|\n    t.string \"title\"\n    t.datetime \"created_at\", null: false\n    t.datetime \"updated_at\", null: false\n  end\n\n  create_table \"articles\", force: :cascade do |t|")
        .write("app/models/concerns/attachment.rb", ATTACHMENT)
        .write("app/models/picture.rb", PICTURE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
