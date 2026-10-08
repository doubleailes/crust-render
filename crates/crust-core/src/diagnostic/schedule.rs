//! The time budget (design D10). Pure bookkeeping over seconds: the caller
//! reports what each step took and asks before starting one, so tests can
//! drive it with a fake clock.

/// Of what remains once the baseline has run: tier 1 (unbiased swaps),
/// tier 2 (sample budget), tier 3 (picture-changing settings).
pub const TIER_SHARES: [f64; 3] = [0.70, 0.15, 0.15];

/// The largest trial sample count tried: past it, a crop's error is far
/// below anything the timing noise can separate.
pub const MAX_TRIAL_SPP: u32 = 256;

/// The fewest samples a trial renders: one sample has no variance.
pub const MIN_TRIAL_SPP: u32 = 2;

/// Seconds spent against the budget, and where each tier ends.
#[derive(Debug, Clone)]
pub struct Schedule {
    budget_s: f64,
    spent_s: f64,
    /// Where tiers 1, 2 and 3 end, in seconds since the start: cumulative,
    /// so what a tier leaves unspent rolls forward into the next one.
    tier_end_s: [f64; 3],
}

impl Schedule {
    pub fn new(budget_s: f64) -> Self {
        Schedule {
            budget_s,
            spent_s: 0.0,
            tier_end_s: [budget_s; 3],
        }
    }

    pub fn remaining_s(&self) -> f64 {
        (self.budget_s - self.spent_s).max(0.0)
    }

    /// Sets the time spent so far (the caller's clock).
    pub fn set_spent(&mut self, spent_s: f64) {
        self.spent_s = spent_s;
    }

    /// Whether the budget is already gone.
    pub fn exhausted(&self) -> bool {
        self.spent_s >= self.budget_s
    }

    /// Splits what remains between the tiers ([`TIER_SHARES`]); called once
    /// the baseline has run.
    pub fn open_tiers(&mut self) {
        let left = self.remaining_s();
        let mut end = self.spent_s;
        for (k, share) in TIER_SHARES.iter().enumerate() {
            end += share * left;
            self.tier_end_s[k] = end;
        }
        // Whatever rounding leaves, the last tier ends with the budget.
        self.tier_end_s[2] = self.budget_s.max(self.spent_s);
    }

    /// Seconds tier `tier` (1-based) may still spend: up to its end, which
    /// includes whatever the earlier tiers left.
    pub fn tier_left_s(&self, tier: u8) -> f64 {
        (self.tier_end_s[(tier - 1) as usize] - self.spent_s).max(0.0)
    }

    /// Whether a step of tier `tier` estimated at `estimate_s` may start:
    /// never one that would overrun its tier.
    pub fn fits(&self, tier: u8, estimate_s: f64) -> bool {
        estimate_s.is_finite() && estimate_s <= self.tier_left_s(tier)
    }
}

/// The estimated cost of one trial (D10): the baseline's sampling seconds
/// per pixel per sample, times the crops' pixels, the trial's spp and both
/// sides of `repeats` pairs, plus the setup the last trial of its kind took.
pub fn trial_cost_s(
    s_per_pixel_spp: f64,
    crop_pixels: usize,
    spp: u32,
    repeats: u32,
    setup_s: f64,
) -> f64 {
    s_per_pixel_spp * crop_pixels as f64 * spp as f64 * 2.0 * repeats as f64 + setup_s
}

/// The trials' sample count: the largest power of two, from
/// [`MIN_TRIAL_SPP`] to [`MAX_TRIAL_SPP`], at which all `trials` fit in
/// `share_s` ([`trial_cost_s`] each, without setup). When even the minimum
/// does not fit, the minimum — and the budget then decides which trials run.
pub fn trial_spp(
    s_per_pixel_spp: f64,
    crop_pixels: usize,
    repeats: u32,
    trials: usize,
    share_s: f64,
) -> u32 {
    let mut spp = MIN_TRIAL_SPP;
    while spp < MAX_TRIAL_SPP {
        let next = spp * 2;
        let cost = trials as f64 * trial_cost_s(s_per_pixel_spp, crop_pixels, next, repeats, 0.0);
        if cost > share_s {
            break;
        }
        spp = next;
    }
    spp
}

/// The baseline's sample count from a timed 1 spp calibration render (D3):
/// a quarter of the budget, between 4 and 64 samples.
pub fn baseline_spp(budget_s: f64, calibration_s: f64) -> u32 {
    if calibration_s.is_nan() || calibration_s <= 0.0 {
        return 64;
    }
    ((0.25 * budget_s / calibration_s).floor()).clamp(4.0, 64.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_split_what_the_baseline_leaves() {
        let mut s = Schedule::new(100.0);
        s.set_spent(20.0);
        s.open_tiers();
        assert!((s.tier_left_s(1) - 56.0).abs() < 1e-9);
        assert!((s.tier_left_s(2) - 68.0).abs() < 1e-9);
        assert!((s.tier_left_s(3) - 80.0).abs() < 1e-9);
    }

    #[test]
    fn unspent_time_rolls_forward() {
        let mut s = Schedule::new(100.0);
        s.open_tiers();
        // Tier 1 (70 s) finishes after 30 s: tier 2 has its own 15 and the
        // 40 tier 1 left.
        s.set_spent(30.0);
        assert!((s.tier_left_s(2) - 55.0).abs() < 1e-9);
        s.set_spent(40.0);
        assert!((s.tier_left_s(3) - 60.0).abs() < 1e-9);
    }

    #[test]
    fn a_step_that_would_overrun_never_starts() {
        let mut s = Schedule::new(10.0);
        s.set_spent(8.0);
        s.open_tiers();
        assert!(!s.fits(1, 1.5));
        assert!(s.fits(1, 1.0));
        assert!(!s.fits(1, f64::NAN));
        s.set_spent(13.0);
        assert!(s.exhausted());
        assert!(!s.fits(3, 0.0001));
        assert_eq!(s.remaining_s(), 0.0);
    }

    #[test]
    fn a_baseline_past_the_budget_leaves_nothing() {
        // The spec's scenario: a 10 s budget, a baseline of 8 s plus 3 s.
        let mut s = Schedule::new(10.0);
        s.set_spent(11.0);
        s.open_tiers();
        for tier in 1..=3 {
            assert_eq!(s.tier_left_s(tier), 0.0);
            assert!(!s.fits(tier, 0.1));
        }
    }

    #[test]
    fn trial_spp_is_the_largest_power_of_two_that_fits() {
        // 1 µs per pixel-sample, 3 crops of 128², R = 3, 8 trials:
        // one trial at spp s costs 49152 · s · 6 µs = 0.295 s · s.
        let px = 3 * 128 * 128;
        let one = trial_cost_s(1e-6, px, 1, 3, 0.0);
        assert!((one - 0.294912).abs() < 1e-9);
        // 8 trials in 40 s: 8 · 0.295 · s ≤ 40 → s ≤ 16.9.
        assert_eq!(trial_spp(1e-6, px, 3, 8, 40.0), 16);
        assert_eq!(trial_spp(1e-6, px, 3, 8, 0.0), MIN_TRIAL_SPP);
        assert_eq!(trial_spp(1e-12, px, 3, 8, 1e9), MAX_TRIAL_SPP);
    }

    #[test]
    fn the_baseline_spp_takes_a_quarter_of_the_budget() {
        assert_eq!(baseline_spp(120.0, 1.0), 30);
        assert_eq!(baseline_spp(120.0, 0.01), 64);
        assert_eq!(baseline_spp(120.0, 100.0), 4);
        assert_eq!(baseline_spp(120.0, 0.0), 64);
    }

    #[test]
    fn a_trial_costs_both_sides_of_every_pair_plus_its_setup() {
        assert_eq!(trial_cost_s(1.0, 10, 2, 3, 5.0), 10.0 * 2.0 * 6.0 + 5.0);
    }
}
