//! `Relation#extending` as Rails 8.2 takes it: modules as arguments or as
//! a list (`extending([A, B])`, Core's `Shop#fulfillment_services`), and a
//! block that is a module of its own. Several modules are extended in one
//! call, so the first listed answers first.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const EXTENSIONS: &str = r#"module FirstExtension
  def which = "first"
  def only_first = :first
end

module SecondExtension
  def which = "second"
  def only_second = :second
end
"#;

const SCRIPT: &str = r#"Article.create!(title: 'A title long enough', body: 'A body long enough to pass validation')
listed = Article.all.extending([FirstExtension, SecondExtension])
puts "list: #{listed.which} #{listed.only_first} #{listed.only_second} #{listed.to_a.size}"
spread = Article.all.extending(SecondExtension, FirstExtension)
puts "spread: #{spread.which}"
blocked = Article.all.extending { def from_block = :block }
puts "block: #{blocked.from_block} #{blocked.to_a.size}"
puts "mixed: #{Article.where("1=1").extending([FirstExtension]) { def extra = :extra }.then { |r| [r.which, r.extra].join(",") }}"
"#;

#[test]
fn extending_takes_lists_and_blocks_as_rails_does() {
    let run = emit_and_run::real_blog().write("app/models/extensions.rb", EXTENSIONS).run_ruby(SCRIPT);
    run.assert_passes();
    for line in ["list: first first second 1", "spread: second", "block: block 1", "mixed: first,extra"] {
        assert!(run.stdout.lines().any(|l| l == line), "missing {line:?}\n{}", run.stdout);
    }
}
