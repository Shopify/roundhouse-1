//! `Builder.new.build` on a plain class is a constructor and an ordinary
//! method, not `owner.assoc.build`: the association rewrite read `new` as
//! the association and wrote `::New.new("...builder_id": Builder.id)`
//! ("uninitialized constant New"). Core's
//! `Checkouts::One::ProposalBuilder.new.build` in a test helper.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_plain_classs_new_dot_build_is_not_an_association_build() {
    let run = emit_and_run::real_blog()
        .write("app/models/proposal_builder.rb", "class ProposalBuilder\n  def build = :built\nend\n")
        .write(
            "test/models/proposal_builder_test.rb",
            "require \"test_helper\"\n\nclass ProposalBuilderTest < ActiveSupport::TestCase\n  test \"builds\" do\n    assert_equal :built, ProposalBuilder.new.build\n    article = articles(:one)\n    comment = article.comments.build(commenter: \"x\", body: \"y\")\n    assert_equal article.id, comment.article_id\n  end\nend\n",
        )
        .run_test("test/models/proposal_builder_test.rb");
    run.assert_passes();
}
