//! `git worktree list --porcelain`: blank-line-separated stanzas of
//! `key value` lines, where `bare` and `detached` are bare keys.

use std::path::PathBuf;

use zax_app::WorktreeEntry;
use zax_domain::BranchName;

pub fn parse_worktree_list(text: &str) -> Result<Vec<WorktreeEntry>, String> {
    let mut entries = Vec::new();

    // Normalise line endings before splitting on the blank line. `\r\n\r\n`
    // contains no `\n\n`, so on CRLF input the raw split would return the whole
    // listing as one stanza and quietly report it as a single worktree carrying
    // whichever flags happened to appear anywhere in the output. Silent nonsense
    // is worse than an error, and this costs one allocation on a listing that is
    // a handful of lines long.
    let text = text.replace("\r\n", "\n");

    for (index, stanza) in text.split("\n\n").enumerate() {
        if stanza.trim().is_empty() {
            continue;
        }

        let mut path: Option<PathBuf> = None;
        let mut branch: Option<BranchName> = None;
        let mut detached = false;
        let mut bare = false;

        for line in stanza.lines() {
            let line = line.trim_end();
            if line == "detached" {
                detached = true;
            } else if line == "bare" {
                bare = true;
            } else if let Some(rest) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(rest));
            } else if let Some(rest) = line.strip_prefix("branch ") {
                let name = rest.strip_prefix("refs/heads/").unwrap_or(rest);
                // Reading is not creating: an existing branch that breaks the
                // project's naming rule must still be listed.
                branch = BranchName::parse_foreign(name).ok();
            }
        }

        let path = path
            .ok_or_else(|| format!("worktree stanza {index} has no `worktree` line: {stanza:?}"))?;

        entries.push(WorktreeEntry {
            path,
            branch,
            // Documented behaviour, not an inference: git-worktree(1) says
            // "The main worktree is listed first, followed by each of the
            // linked worktrees." The order of the linked ones is not specified
            // and nothing here depends on it.
            is_main: index == 0,
            is_detached: detached || bare,
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;

    use super::*;

    const SAMPLE: &str = "\
worktree /Users/me/src/acme
HEAD 7f3b1c2d4e5f60718293a4b5c6d7e8f901234567
branch refs/heads/main

worktree /Users/me/src/worktrees/acme/OMEGA-1963-stamp
HEAD 0123456789abcdef0123456789abcdef01234567
branch refs/heads/OMEGA-1963-stamp

worktree /Users/me/src/worktrees/acme/spike
HEAD fedcba9876543210fedcba9876543210fedcba98
detached

";

    #[test]
    fn the_first_entry_is_the_main_working_tree() {
        let entries = parse_worktree_list(SAMPLE).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_main);
        assert!(!entries[1].is_main);
        assert_eq!(entries[0].path, PathBuf::from("/Users/me/src/acme"));
    }

    #[test]
    fn branch_refs_are_stripped_to_branch_names() {
        let entries = parse_worktree_list(SAMPLE).unwrap();
        assert_eq!(
            entries[1].branch.as_ref().map(|b| b.as_str()),
            Some("OMEGA-1963-stamp")
        );
    }

    #[test]
    fn a_detached_worktree_has_no_branch() {
        let entries = parse_worktree_list(SAMPLE).unwrap();
        assert!(entries[2].is_detached);
        assert_eq!(entries[2].branch, None);
    }

    #[test]
    fn a_branch_that_breaks_our_naming_rule_is_still_listed() {
        // Reading is not creating: an old `feat/thing` branch must not make the
        // whole listing fail.
        let entries = parse_worktree_list(
            "worktree /w/feat/thing\nHEAD 0000000000000000000000000000000000000000\n\
             branch refs/heads/feat/thing\n\n",
        )
        .unwrap();
        assert_eq!(
            entries[0].branch.as_ref().map(|b| b.as_str()),
            Some("feat/thing")
        );
    }

    #[test]
    fn a_bare_main_repository_is_reported_as_main() {
        let entries = parse_worktree_list("worktree /srv/git/acme.git\nbare\n\n").unwrap();
        assert!(entries[0].is_main);
        assert_eq!(entries[0].branch, None);
    }

    #[test]
    fn crlf_output_parses_the_same_as_lf() {
        // A raw split on a blank line finds none in CRLF text, which would
        // collapse the whole listing into one entry reporting the wrong flags.
        let lf = parse_worktree_list(SAMPLE).unwrap();
        let crlf = parse_worktree_list(&SAMPLE.replace('\n', "\r\n")).unwrap();
        assert_eq!(lf, crlf);
        assert_eq!(crlf.len(), 3);
    }

    #[test]
    fn empty_output_yields_no_entries() {
        assert_eq!(parse_worktree_list("").unwrap(), vec![]);
    }

    #[test]
    fn a_stanza_with_no_worktree_line_is_an_error_not_a_silent_drop() {
        assert!(parse_worktree_list("HEAD deadbeef\nbranch refs/heads/x\n\n").is_err());
    }
}
