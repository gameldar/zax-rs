//! acli's JSON, and the mapping onto the wire.

use serde::Deserialize;
use zax_protocol::{category, WireStatus, WireTicket};

#[derive(Deserialize)]
struct Item {
    key: String,
    fields: Fields,
}

#[derive(Deserialize)]
struct Fields {
    #[serde(default)]
    summary: String,
    status: Status,
    #[serde(default)]
    priority: Option<Named>,
    #[serde(default)]
    issuetype: Option<Named>,
}

#[derive(Deserialize)]
struct Status {
    name: String,
    #[serde(rename = "statusCategory")]
    status_category: CategoryRef,
}

#[derive(Deserialize)]
struct CategoryRef {
    key: String,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

/// The `Site:` line of `acli jira auth status`.
pub fn parse_site(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("Site:"))
        .map(|s| s.trim().to_owned())
        .find(|s| !s.is_empty())
}

/// Tickets, and a line for each item dropped because its status category is
/// not one of Jira's three.
pub fn parse_search(json: &str, site: &str) -> Result<(Vec<WireTicket>, Vec<String>), String> {
    let items: Vec<Item> =
        serde_json::from_str(json).map_err(|e| format!("could not read acli output: {e}"))?;
    let mut tickets = Vec::new();
    let mut dropped = Vec::new();
    for item in items {
        let category = match item.fields.status.status_category.key.as_str() {
            "new" => category::TO_DO,
            "indeterminate" => category::IN_PROGRESS,
            "done" => category::DONE,
            other => {
                dropped.push(format!("{}: unknown status category `{other}`", item.key));
                continue;
            }
        };
        tickets.push(WireTicket {
            url: format!("https://{site}/browse/{}", item.key),
            key: item.key,
            summary: item.fields.summary,
            status: WireStatus {
                category: category.to_owned(),
                name: item.fields.status.name,
            },
            priority: item.fields.priority.map(|p| p.name),
            kind: item.fields.issuetype.map(|t| t.name),
        });
    }
    Ok((tickets, dropped))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_site_comes_from_auth_status() {
        assert_eq!(
            parse_site(include_str!("../tests/fixtures/auth_status.txt")).as_deref(),
            Some("acme.atlassian.net")
        );
        assert_eq!(parse_site("nothing here"), None);
    }

    #[test]
    fn search_results_map_to_wire_tickets_and_unknown_categories_are_dropped() {
        let (tickets, dropped) = parse_search(
            include_str!("../tests/fixtures/search.json"),
            "acme.atlassian.net",
        )
        .unwrap();
        assert_eq!(tickets.len(), 2);
        assert_eq!(tickets[0].key, "ACME-12");
        assert_eq!(tickets[0].status.category, "in_progress");
        assert_eq!(tickets[0].status.name, "In Review");
        assert_eq!(tickets[0].priority.as_deref(), Some("High"));
        assert_eq!(tickets[0].kind.as_deref(), Some("Story"));
        assert_eq!(tickets[0].url, "https://acme.atlassian.net/browse/ACME-12");
        assert_eq!(tickets[1].priority, None);
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("ACME-14"));
    }
}
