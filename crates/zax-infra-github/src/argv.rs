//! Pure argv construction for `gh`.

fn s(value: &str) -> String {
    value.to_owned()
}

pub fn version() -> Vec<String> {
    vec![s("--version")]
}

pub fn auth_status() -> Vec<String> {
    vec![s("auth"), s("status")]
}

pub fn current_login() -> Vec<String> {
    vec![s("api"), s("user"), s("-q"), s(".login")]
}

pub fn repo_name_with_owner() -> Vec<String> {
    vec![
        s("repo"),
        s("view"),
        s("--json"),
        s("nameWithOwner"),
        s("-q"),
        s(".nameWithOwner"),
    ]
}

/// `gh api graphql -f query=<q> -f <name>=<value> ...`
pub fn graphql_query(query: &str, variables: &[(&str, &str)]) -> Vec<String> {
    let mut argv = vec![s("api"), s("graphql"), s("-f"), format!("query={query}")];
    for (name, value) in variables {
        argv.push(s("-f"));
        argv.push(format!("{name}={value}"));
    }
    argv
}

pub const PR_HEAD_FIELDS: &str =
    "number,headRefName,headRepository,headRepositoryOwner,isCrossRepository";

pub fn pr_head(reference: &str) -> Vec<String> {
    vec![
        s("pr"),
        s("view"),
        s(reference),
        s("--json"),
        s(PR_HEAD_FIELDS),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_login_lookup_asks_for_just_the_login_field() {
        assert_eq!(current_login(), ["api", "user", "-q", ".login"]);
    }

    #[test]
    fn the_repo_lookup_asks_for_name_with_owner() {
        assert_eq!(
            repo_name_with_owner(),
            [
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner"
            ]
        );
    }

    #[test]
    fn a_graphql_call_passes_the_query_and_its_variables_as_string_fields() {
        let args = graphql_query("QUERY", &[("q", "is:pr"), ("owner", "acme")]);
        assert_eq!(args[0], "api");
        assert_eq!(args[1], "graphql");
        assert_eq!(args[2], "-f");
        assert_eq!(args[3], "query=QUERY");
        assert_eq!(args[4], "-f");
        assert_eq!(args[5], "q=is:pr");
        assert_eq!(args[6], "-f");
        assert_eq!(args[7], "owner=acme");
        assert_eq!(args.len(), 8);
    }

    #[test]
    fn the_pr_head_lookup_asks_for_exactly_the_fields_the_dto_reads() {
        assert_eq!(
            pr_head("1963"),
            [
                "pr",
                "view",
                "1963",
                "--json",
                "number,headRefName,headRepository,headRepositoryOwner,isCrossRepository",
            ]
        );
    }

    #[test]
    fn auth_status_and_version_are_plain_probes() {
        assert_eq!(auth_status(), ["auth", "status"]);
        assert_eq!(version(), ["--version"]);
    }
}
