//! `Forge` via the `gh` CLI. zax never manages GitHub auth; it inherits gh's.

pub mod argv;
pub mod dto;
pub mod graphql;

use std::path::PathBuf;
use std::time::SystemTime;

use zax_app::{Forge, PortError, PullRequestHead};
use zax_domain::{BranchName, PrFilter, PrRef, PullRequest, PullRequestStatus, RepoSlug};
use zax_exec::{Exec, ExecError, Output};

const TOOL: &str = "gh";

pub struct GhCli<E: Exec> {
    exec: E,
    cwd: PathBuf,
    program: String,
}

impl<E: Exec> GhCli<E> {
    pub fn new(exec: E, cwd: PathBuf) -> Self {
        Self::with_program(exec, cwd, TOOL.to_owned())
    }

    pub fn with_program(exec: E, cwd: PathBuf, program: String) -> Self {
        Self { exec, cwd, program }
    }

    pub fn exec_ref(&self) -> &E {
        &self.exec
    }

    fn run_ok(&self, args: Vec<String>) -> Result<Output, PortError> {
        let out = self
            .exec
            .exec(&self.program, &args, Some(&self.cwd), &[])
            .map_err(map_exec_error)?;
        if out.ok() {
            return Ok(out);
        }
        Err(classify_failure(out))
    }

    fn login(&self) -> Result<String, PortError> {
        Ok(self
            .run_ok(argv::current_login())?
            .trimmed_stdout()
            .to_owned())
    }

    fn repo_slug(&self) -> Result<RepoSlug, PortError> {
        let text = self.run_ok(argv::repo_name_with_owner())?;
        RepoSlug::parse(text.trimmed_stdout()).map_err(|e| PortError::Unparseable {
            tool: TOOL,
            detail: e.to_string(),
        })
    }
}

fn map_exec_error(error: ExecError) -> PortError {
    match error {
        ExecError::NotFound { .. } => PortError::MissingTool { tool: TOOL },
        ExecError::Io { detail, .. } => PortError::Unparseable { tool: TOOL, detail },
    }
}

/// gh reports an expired or missing token on stderr rather than with a distinct
/// exit code, so a pane can tell "log in again" apart from "GitHub is down".
fn classify_failure(out: Output) -> PortError {
    let stderr = out.stderr.trim().to_owned();
    let lowered = stderr.to_ascii_lowercase();
    if lowered.contains("gh auth login")
        || lowered.contains("authentication")
        || lowered.contains("not logged into")
    {
        PortError::NotAuthenticated {
            tool: TOOL,
            detail: stderr,
        }
    } else {
        PortError::CommandFailed {
            tool: TOOL,
            status: out.status,
            stderr,
        }
    }
}

fn unparseable(detail: String) -> PortError {
    PortError::Unparseable { tool: TOOL, detail }
}

impl<E: Exec> Forge for GhCli<E> {
    fn review_requests(&self, filter: &PrFilter) -> Result<Vec<PullRequest>, PortError> {
        let login = self.login()?;
        let query = graphql::review_requested_query(&login, filter, SystemTime::now());
        let out = self.run_ok(argv::graphql_query(
            graphql::SEARCH_REVIEW_REQUESTS,
            &[("q", &query)],
        ))?;
        dto::parse_search_response(&out.stdout).map_err(unparseable)
    }

    fn current_repo(&self) -> Result<RepoSlug, PortError> {
        self.repo_slug()
    }

    fn branch_status(&self, branch: &BranchName) -> Result<Option<PullRequestStatus>, PortError> {
        let slug = self.repo_slug()?;
        let out = self.run_ok(argv::graphql_query(
            graphql::BRANCH_STATUS,
            &[
                ("owner", slug.owner()),
                ("name", slug.name()),
                ("branch", branch.as_str()),
            ],
        ))?;
        dto::parse_branch_status_response(&out.stdout).map_err(unparseable)
    }

    fn pull_request_head(&self, pr: &PrRef) -> Result<PullRequestHead, PortError> {
        let out = self.run_ok(argv::pr_head(&pr.as_gh_arg()))?;
        dto::parse_pr_head_response(&out.stdout).map_err(unparseable)
    }

    fn auth_status(&self) -> Result<String, PortError> {
        let out = self.run_ok(argv::auth_status())?;
        // gh writes the status report to stderr on success.
        let report = if out.stderr.trim().is_empty() {
            out.trimmed_stdout().to_owned()
        } else {
            out.stderr.trim().to_owned()
        };
        Ok(report)
    }

    fn probe(&self) -> Result<String, PortError> {
        Ok(self
            .run_ok(argv::version())?
            .stdout
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;

    use zax_app::{Forge, PortError};
    use zax_domain::{BranchName, PrFilter, PrRef};
    use zax_exec::{Output, RecordingExec};

    use super::*;

    fn ok(stdout: &str) -> Output {
        Output {
            status: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn cli(responses: Vec<Output>) -> GhCli<RecordingExec> {
        GhCli::new(
            RecordingExec::with_responses(responses),
            PathBuf::from("/w"),
        )
    }

    #[test]
    fn review_requests_resolve_the_login_before_searching() {
        let gh = cli(vec![
            ok("octocat\n"),
            ok(include_str!("../tests/fixtures/search_prs.json")),
        ]);

        let prs = gh.review_requests(&PrFilter::default()).unwrap();
        assert_eq!(prs.len(), 3);

        let calls = gh.exec_ref().calls();
        assert_eq!(calls[0].args, ["api", "user", "-q", ".login"]);
        assert_eq!(calls[1].args[0], "api");
        assert_eq!(calls[1].args[1], "graphql");
        assert!(
            calls[1]
                .args
                .iter()
                .any(|a| a.contains("review-requested:octocat")),
            "the resolved login must reach the search query: {:?}",
            calls[1].args
        );
    }

    #[test]
    fn the_filter_narrows_the_search_gh_is_asked_to_run() {
        // The search is capped at 50 results, so an excluded repository that
        // only the pane filtered out would still crowd the queue.
        let gh = cli(vec![
            ok("octocat\n"),
            ok(include_str!("../tests/fixtures/search_prs.json")),
        ]);
        let filter = PrFilter {
            exclude: vec![zax_domain::RepoPattern::parse("acme/legacy-api").unwrap()],
            ..PrFilter::default()
        };

        gh.review_requests(&filter).unwrap();

        let calls = gh.exec_ref().calls();
        assert!(
            calls[1]
                .args
                .iter()
                .any(|a| a.contains("-repo:acme/legacy-api")),
            "the filter must reach the search: {:?}",
            calls[1].args
        );
    }

    #[test]
    fn branch_status_resolves_the_repository_then_queries_it() {
        let gh = cli(vec![
            ok("acme/omega\n"),
            ok(include_str!("../tests/fixtures/branch_status.json")),
        ]);
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();

        let status = gh.branch_status(&branch).unwrap().unwrap();
        assert_eq!(status.unresolved_threads, 2);

        let calls = gh.exec_ref().calls();
        assert_eq!(calls[0].args[0], "repo");
        assert!(calls[1].args.iter().any(|a| a == "owner=acme"));
        assert!(calls[1].args.iter().any(|a| a == "name=omega"));
        assert!(calls[1].args.iter().any(|a| a == "branch=OMEGA-1963-stamp"));
    }

    #[test]
    fn a_pull_request_head_comes_from_gh_pr_view() {
        let gh = cli(vec![ok(include_str!("../tests/fixtures/pr_head.json"))]);
        let head = gh.pull_request_head(&PrRef::Number(1963)).unwrap();
        assert_eq!(head.head_ref, "patch-1");
        assert_eq!(gh.exec_ref().args(0)[2], "1963");
    }

    #[test]
    fn a_url_reference_is_passed_through_unchanged() {
        let gh = cli(vec![ok(include_str!("../tests/fixtures/pr_head.json"))]);
        let url = "https://github.com/acme/omega/pull/1963".to_owned();
        gh.pull_request_head(&PrRef::Url(url.clone())).unwrap();
        assert_eq!(gh.exec_ref().args(0)[2], url);
    }

    #[test]
    fn an_unauthenticated_gh_is_reported_as_not_authenticated() {
        let gh = cli(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "gh: To get started with GitHub CLI, please run: gh auth login".to_owned(),
        }]);
        let err = gh.review_requests(&PrFilter::default()).unwrap_err();
        assert!(
            matches!(err, PortError::NotAuthenticated { tool: "gh", .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn any_other_failure_keeps_ghs_stderr() {
        let gh = cli(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "HTTP 502".to_owned(),
        }]);
        let err = gh.review_requests(&PrFilter::default()).unwrap_err();
        assert!(err.to_string().contains("502"), "got: {err}");
    }

    #[test]
    fn a_missing_gh_binary_is_reported_as_a_missing_tool() {
        let gh = GhCli::with_program(
            zax_exec::SystemExec,
            PathBuf::from("/"),
            "definitely-not-gh-zax".to_owned(),
        );
        assert!(matches!(
            gh.probe().unwrap_err(),
            PortError::MissingTool { tool: "gh" }
        ));
    }
}
