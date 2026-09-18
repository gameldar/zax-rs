//! Each pane process polls on its own interval with jitter, so a screenful of
//! panes does not stampede `gh` on the same second.

use std::time::Duration;

const MIN_DELAY: Duration = Duration::from_secs(1);

/// `base` scaled by `1 +/- jitter_fraction`, with `roll` in `[0, 1]` selecting a
/// point in the band. Taking the roll as a parameter keeps this pure: the caller
/// supplies the randomness.
pub fn next_delay(base: Duration, jitter_fraction: f64, roll: f64) -> Duration {
    let roll = if roll.is_nan() {
        0.5
    } else {
        roll.clamp(0.0, 1.0)
    };
    let jitter = jitter_fraction.clamp(0.0, 1.0);
    let scale = 1.0 + jitter * (2.0 * roll - 1.0);
    let secs = (base.as_secs_f64() * scale).max(MIN_DELAY.as_secs_f64());
    Duration::from_secs_f64(secs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_middle_of_the_roll_is_the_base_interval() {
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.2, 0.5),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn the_extremes_of_the_roll_are_the_edges_of_the_jitter_band() {
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.25, 0.0),
            Duration::from_secs(45)
        );
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.25, 1.0),
            Duration::from_secs(75)
        );
    }

    #[test]
    fn no_jitter_means_no_jitter() {
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.0, 0.9),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn the_delay_never_drops_below_one_second() {
        // Otherwise a small interval plus jitter could spin.
        assert_eq!(
            next_delay(Duration::from_secs(1), 1.0, 0.0),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn a_roll_outside_the_unit_interval_is_clamped_rather_than_trusted() {
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.25, -5.0),
            Duration::from_secs(45)
        );
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.25, 5.0),
            Duration::from_secs(75)
        );
        assert_eq!(
            next_delay(Duration::from_secs(60), 0.25, f64::NAN),
            Duration::from_secs(60)
        );
    }
}
