//! Shared schema overlay for has_one autosave / preload emit_and_run pins.

/// Insert a `profiles` table before `comments` in real-blog's schema.rb.
pub fn profile_schema_edit() -> (&'static str, &'static str, &'static str) {
    (
        "db/schema.rb",
        "  create_table \"comments\", force: :cascade do |t|",
        "  create_table \"profiles\", force: :cascade do |t|\n    t.integer \"article_id\"\n    t.string \"bio\"\n  end\n\n  create_table \"comments\", force: :cascade do |t|",
    )
}
