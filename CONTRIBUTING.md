# Contributing Guidelines

Thank you for your interest in contributing to our project. Whether it's a bug report, new feature, correction, or additional
documentation, we greatly value feedback and contributions from our community.

Please read through this document before submitting any issues or pull requests to ensure we have all the necessary
information to effectively respond to your bug report or contribution.


## Reporting Bugs/Feature Requests

We welcome you to use the GitHub issue tracker to report bugs or suggest features.

When filing an issue, please check existing open, or recently closed, issues to make sure somebody else hasn't already
reported the issue. Please try to include as much information as you can. Details like these are incredibly useful:

* A reproducible test case or series of steps
* The version of our code being used
* Any modifications you've made relevant to the bug
* Anything unusual about your environment or deployment


## Contributing via Pull Requests
Contributions via pull requests are much appreciated. Before sending us a pull request, please ensure that:

1. You are working against the latest source on the *main* branch.
2. You check existing open, and recently merged, pull requests to make sure someone else hasn't addressed the problem already.
3. You open an issue to discuss any significant work - we would hate for your time to be wasted.

To send us a pull request, please:

1. Fork the repository.
2. Modify the source; please focus on the specific change you are contributing. If you also reformat all the code, it will be hard for us to focus on your change.
3. Ensure local tests pass.
4. Commit to your fork using clear commit messages.
5. Send us a pull request, answering any default questions in the pull request interface.
6. Pay attention to any automated CI failures reported in the pull request, and stay involved in the conversation.

GitHub provides additional document on [forking a repository](https://help.github.com/articles/fork-a-repo/) and
[creating a pull request](https://help.github.com/articles/creating-a-pull-request/).


## Finding contributions to work on
Looking at the existing issues is a great way to find something to contribute on. As our projects, by default, use the default GitHub issue labels (enhancement/bug/duplicate/help wanted/invalid/question/wontfix), looking at any 'help wanted' issues is a great place to start.


## Code of Conduct
This project has adopted the [Amazon Open Source Code of Conduct](https://aws.github.io/code-of-conduct).
For more information see the [Code of Conduct FAQ](https://aws.github.io/code-of-conduct-faq) or contact
opensource-codeofconduct@amazon.com with any additional questions or comments.


## Security issue notifications
If you discover a potential security issue in this project we ask that you notify AWS/Amazon Security via our [vulnerability reporting page](http://aws.amazon.com/security/vulnerability-reporting/). Please do **not** create a public github issue.


## Licensing

See the [LICENSE](LICENSE) file for our project's licensing. We will ask you to confirm the licensing of your contribution.


## Grammar changes

`dsql_grammar.ebnf` is the source of truth for what DSQL accepts. dsql-lint
encodes a subset of the same knowledge in hand-written rules. CI keeps them
aligned via the corpus under [`dsql-lint/tests/grammar/`](dsql-lint/tests/grammar/).

When you push a change to `dsql_grammar.ebnf`:

1. Run `dsql-lint/scripts/grammar_diff.sh` to list productions that changed.
   Paste the output into your PR description.
2. For each production whose semantics changed, update the corresponding
   fixture(s) under `accept/`, `reject/`, and/or `fixed/`.
3. If your change relaxes the grammar (something now allowed): move the
   relevant fixture from `reject/` to `accept/`. CI will then fail because
   the rule still flags it — remove or loosen the rule in the same PR.
4. If your change tightens the grammar (something newly disallowed): add
   a `reject/` fixture and a corresponding rule. If the rule has a `--fix`,
   add a paired `fixed/` golden:

   ```bash
   BLESS=1 cargo test -p dsql-lint --test grammar_oracle corpus_contract_test
   ```

   then inspect the regenerated `fixed/*.sql` files and commit.

The corpus contract test (`corpus_contract_test`) gates merges; the coverage
test (`corpus_coverage_test`) is informational.
