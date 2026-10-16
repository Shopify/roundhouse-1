//! `UrlHelpers.article_url(id: 3, host: "blog.test", protocol: "https")` —
//! Core's `UrlHelpers` answers every named route helper (it includes
//! `Rails.application.routes.url_helpers`), and its callers pass path params
//! as keywords. The call was retargeted to the generated RouteHelpers after
//! the passes that spell path params positionally and ground `host:` had
//! looked only at bare calls, so the keywords reached a positional helper
//! (`DeprecatedCustomerExportTest`'s `admin_export_download_url`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn keyword_path_params_on_a_url_helpers_receiver_reach_the_positional_helper() {
    let run = emit_and_run::real_blog()
        .write(
            "app/models/link_helpers.rb",
            "module LinkHelpers\n  class << self\n    self.include(Rails.application.routes.url_helpers)\n  end\nend\n",
        )
        .write(
            "app/models/link_builder.rb",
            "class LinkBuilder\n  def self.full(id)\n    LinkHelpers.article_url(id: id, host: \"blog.test\", protocol: \"https\")\n  end\n\n  def self.path(id)\n    LinkHelpers.article_path(id: id)\n  end\nend\n",
        )
        .run_ruby("puts LinkBuilder.full(3)\nputs LinkBuilder.path(4)\n");
    run.assert_passes();
    assert_eq!(run.stdout, "https://blog.test/articles/3\n/articles/4\n");
}
