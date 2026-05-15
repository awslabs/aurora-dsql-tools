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


## Grammar oracle (dsql-lint)

`dsql_grammar.ebnf` at the repo root is the upstream definition of what
DSQL accepts. A test-time recognizer parses it and asserts dsql-lint and
the grammar agree on every case in `dsql-lint/tests/integration_test.rs`.
Cluster tests remain authoritative for correctness — the oracle is a
second signal that surfaces drift to maintainers.

When `dsql_lint_agrees_with_grammar` fails, the message names the
disagreement kind:

- **`GrammarRejectsLintQuiet`** — grammar rejects, dsql-lint silent.
  dsql-lint is missing a rule (the common case). Add one in
  `dsql-lint/src/rules/errors.rs`.
- **`LintFlagsGrammarAccepts`** — dsql-lint flags it, grammar accepts.
  Either the grammar relaxed (remove or loosen the rule) or dsql-lint
  is over-flagging.

If a fix is non-trivial, add the SQL string to `EXPECTED_DRIFT` in
`dsql-lint/tests/grammar_oracle/drift.rs` with a one-line `//` comment
explaining why. The list fails CI on stale entries, so it shrinks
naturally as fixes land — every removal corresponds to either a new
rule, a recognizer fix, or a grammar update.
