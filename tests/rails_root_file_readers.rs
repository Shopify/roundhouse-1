//! `Rails.root.join(...)` answers Pathname's file readers.
//!
//! A class-body constant is often built from
//! `Rails.root.join("db/data/...").readlines(chomp: true)`. The emitted
//! `Rails::AppPath` had no `readlines`, so Kernel's private one answered
//! and the file failed to load ("private method 'readlines' called").
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn rails_root_paths_read_the_file_they_name() {
    let script = r#"File.write("rails_root_list.txt", "alpha\nbeta\n")
path = Rails.root.join("rails_root_list.txt")
p path.readlines(chomp: true), path.readlines, path.read, path.exist?, Rails.root.join("missing.txt").exist?
"#;
    let run = emit_and_run::real_blog().run_ruby(script);
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "[\"alpha\", \"beta\"]\n[\"alpha\\n\", \"beta\\n\"]\n\"alpha\\nbeta\\n\"\ntrue\nfalse\n"
    );
}

#[test]
fn rails_root_paths_answer_directory_and_file_predicates() {
    let script = r#"Dir.mkdir("rails_root_dir") unless Dir.exist?("rails_root_dir")
File.write("rails_root_dir/a.lua", "x")
dir = Rails.root.join("rails_root_dir")
p dir.directory?, dir.file?, dir.join("a.lua").file?, dir.join("a.lua").directory?, dir.join("missing").directory?
"#;
    let run = emit_and_run::real_blog().run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "true\nfalse\ntrue\nfalse\nfalse\n");
}
