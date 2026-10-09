//! How git reaches a remote, which is what decides the credential it needs.

/// The transport of a remote URL.
///
/// zax shells out to git with every prompt moved off the terminal, so the
/// credential a remote needs has to be in place beforehand. Which credential
/// that is follows from the transport and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Needs a key, and so an agent holding it or a helper to unlock it with.
    Ssh,
    /// Needs a token, which `gh auth` already puts in a credential helper.
    Https,
    /// A local path or the bare git protocol: no credential at all.
    Other,
}

impl Transport {
    /// The transport of `url`, which is a classification rather than a parse:
    /// git accepts more spellings than zax has any reason to reject, and an
    /// unrecognised one needing no credential is the safe reading.
    pub fn of(url: &str) -> Self {
        let url = url.trim();
        if let Some((scheme, _)) = url.split_once("://") {
            return match scheme {
                "ssh" | "git+ssh" => Self::Ssh,
                "https" | "http" => Self::Https,
                _ => Self::Other,
            };
        }
        // scp syntax: `git@github.com:owner/repo.git`. The colon has to come
        // before any slash, or this is a path that merely contains one.
        let host = url.split('/').next().unwrap_or_default();
        if host.contains(':') {
            Self::Ssh
        } else {
            Self::Other
        }
    }

    /// Whether a locked ssh key would stop a fetch from this remote.
    pub fn needs_ssh_key(self) -> bool {
        matches!(self, Self::Ssh)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_scp_spelling_github_hands_out_is_ssh() {
        assert_eq!(
            Transport::of("git@github.com:gameldar/zax.git"),
            Transport::Ssh
        );
    }

    #[test]
    fn an_explicit_ssh_url_is_ssh() {
        assert_eq!(
            Transport::of("ssh://git@github.com/gameldar/zax.git"),
            Transport::Ssh
        );
    }

    #[test]
    fn an_https_remote_needs_a_token_rather_than_a_key() {
        assert_eq!(
            Transport::of("https://github.com/gameldar/zax.git"),
            Transport::Https
        );
        assert!(!Transport::of("https://github.com/gameldar/zax.git").needs_ssh_key());
    }

    #[test]
    fn a_local_path_needs_no_credential() {
        assert_eq!(Transport::of("/srv/git/zax.git"), Transport::Other);
        assert_eq!(Transport::of("../zax"), Transport::Other);
    }

    #[test]
    fn a_path_with_a_colon_further_along_is_still_a_path() {
        // The colon has to be in the host part. A directory named oddly is not
        // an ssh remote.
        assert_eq!(Transport::of("/srv/git/weird:name.git"), Transport::Other);
    }

    #[test]
    fn the_bare_git_protocol_carries_no_credential() {
        assert_eq!(
            Transport::of("git://github.com/gameldar/zax.git"),
            Transport::Other
        );
    }

    #[test]
    fn surrounding_whitespace_does_not_change_the_answer() {
        // `git remote get-url` ends its output with a newline.
        assert_eq!(
            Transport::of("  git@github.com:gameldar/zax.git\n"),
            Transport::Ssh
        );
    }
}
