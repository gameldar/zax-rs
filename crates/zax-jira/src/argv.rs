//! The acli command lines, built in one place so tests can pin them.

fn s(v: &str) -> String {
    v.to_owned()
}

pub fn auth_status() -> Vec<String> {
    vec![s("jira"), s("auth"), s("status")]
}

pub fn search(jql: &str) -> Vec<String> {
    vec![
        s("jira"),
        s("workitem"),
        s("search"),
        s("--jql"),
        s(jql),
        s("--fields"),
        s("key,summary,status,priority,issuetype"),
        s("--paginate"),
        s("--json"),
    ]
}

pub fn transition(key: &str, status: &str) -> Vec<String> {
    vec![
        s("jira"),
        s("workitem"),
        s("transition"),
        s("--key"),
        s(key),
        s("--status"),
        s(status),
        s("--yes"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_asks_for_exactly_the_fields_it_reads() {
        assert_eq!(
            search("assignee = currentUser()"),
            [
                "jira",
                "workitem",
                "search",
                "--jql",
                "assignee = currentUser()",
                "--fields",
                "key,summary,status,priority,issuetype",
                "--paginate",
                "--json",
            ]
        );
    }

    #[test]
    fn transition_never_prompts() {
        assert_eq!(
            transition("ACME-1", "In Review"),
            [
                "jira",
                "workitem",
                "transition",
                "--key",
                "ACME-1",
                "--status",
                "In Review",
                "--yes"
            ]
        );
    }
}
