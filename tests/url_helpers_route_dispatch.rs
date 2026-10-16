//! `Rails.application.routes.url_helpers` answers every named route helper
//! of the drawn route set, as Rails' does: `<name>_path` is the emitted
//! RouteHelpers' answer, `<name>_url` the same path under
//! `protocol://host[:port]`, on the module itself and through a class that
//! includes it. The module was a stub with `root_url` alone, so Core's
//! `UrlHelpers.root_path` (`graphql_preloadable_assertions.rb`) was an
//! undefined method. A name no drawn route explains stays a NoMethodError,
//! and once initializers have appended route blocks this tree does not draw
//! it is a refusal that names them.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SCRIPT: &str = r##"helpers = Rails.application.routes.url_helpers
puts helpers.articles_path
puts helpers.article_path(3)
puts helpers.root_path
puts helpers.articles_url(host: "blog.test", protocol: "https")
puts helpers.article_url(3, host: "blog.test", port: 3000)
puts helpers.article_url(3, only_path: true)
puts helpers.respond_to?(:articles_path)
puts helpers.respond_to?(:nope_path)
puts helpers.respond_to?(:articles_url)
begin
  helpers.articles_url
rescue ArgumentError => e
  puts "ArgumentError #{e.message[0, 25].strip}"
end
begin
  helpers.nope_path
rescue NoMethodError => e
  puts "NoMethodError #{e.name}"
end
includer = Class.new { include Rails.application.routes.url_helpers }
puts includer.new.article_path(5)
extender = Module.new { class << self; include Rails.application.routes.url_helpers; end }
puts extender.article_path(6)
Rails.application.routes.append { }
begin
  helpers.nope_path
rescue NotImplementedError => e
  puts e.message[0, 62]
end
puts helpers.article_path(7)
"##;

#[test]
fn url_helpers_answer_every_named_route_helper() {
    let run = emit_and_run::real_blog().run_ruby(SCRIPT);
    run.assert_passes();
    let expected = "/articles\n/articles/3\n/\nhttps://blog.test/articles\nhttp://blog.test:3000/articles/3\n/articles/3\ntrue\nfalse\ntrue\n\
ArgumentError Missing host to link to!\nNoMethodError nope_path\n/articles/5\n/articles/6\n\
error[route_set_not_drawn]: nope_path: no route of this tree's\n/articles/7\n";
    assert_eq!(run.stdout, expected);
}

/// Core's shape: no `config/routes.rb` in the tree, an app module that
/// includes the url_helpers inside `class << self`, route blocks appended by
/// an initializer and never drawn. `UrlHelpers.root_path` is a refusal that
/// names the undrawn blocks, where it was a NoMethodError for a module.
#[test]
fn an_undrawn_route_set_refuses_by_name_on_the_app_url_helpers_module() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write(
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\n  def checkout_link\n    cart_path\n  end\nend\n",
        )
        .write(
            "app/models/url_helpers.rb",
            "module UrlHelpers\n  class << self\n    self.include(Rails.application.routes.url_helpers)\n  end\nend\n",
        )
        .emit(BuildTarget::Ruby);
    let script = "require_relative 'runtime/rails_application_routes'\n\
                  require_relative 'app/models/url_helpers'\n\
                  begin\n  UrlHelpers.root_path\nrescue NoMethodError => e\n  puts \"NoMethodError #{e.name}\"\nend\n\
                  Rails::Application::RouteSet.append { }\n\
                  begin\n  UrlHelpers.root_path\nrescue NotImplementedError => e\n  puts e.message[0, 72]\nend\n";
    let out = emit_and_run::ruby().args(["-e", script]).current_dir(&*dir).output().expect("spawn ruby");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "NoMethodError root_path\nerror[route_set_not_drawn]: root_path: no route of this tree's drawn rou\n"
    );
}
