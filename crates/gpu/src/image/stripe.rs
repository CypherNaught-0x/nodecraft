//! Allocation-free global-halo geometry and checked analytic work inventory.
use super::GpuImageError as E;
#[derive(Clone, Copy, Debug)]
pub(super) struct StripePlan {
    pub height: u32,
    pub block: u32,
    pub radius: u32,
    pub count: u32,
    pub rows: u32,
    pub sum_rows: u64,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Stripe {
    pub start: u32,
    pub rows: u32,
    pub halo_start: u32,
    pub halo_rows: u32,
}
fn add(a: u64, b: u64) -> Result<u64, E> {
    a.checked_add(b).ok_or(E::Resource)
}
fn mul(a: u64, b: u64) -> Result<u64, E> {
    a.checked_mul(b).ok_or(E::Resource)
}
fn triangular(n: u64) -> Result<u64, E> {
    Ok(mul(n, n.saturating_sub(1))? / 2)
}
impl StripePlan {
    pub fn new(height: u32, block: u32, radius: u32) -> Result<Self, E> {
        if height == 0 || block == 0 || block > 128 || height > 16384 || radius > 192 {
            return Err(E::Resource);
        }
        let block = block.min(height);
        let count = height.div_ceil(block);
        let n = u64::from(count);
        let b = u64::from(block);
        let r = u64::from(radius);
        let h = u64::from(height);
        let t = h.checked_sub(mul(n - 1, b)?).ok_or(E::Resource)?;
        let k = n.min(r.div_ceil(b));
        let left = add(mul(b, triangular(k)?)?, mul(n - k, r)?)?;
        let k = if r <= t { 0 } else { (n - 1).min((r - t).div_ceil(b)) };
        let right = add(add(mul(k, t)?, mul(b, triangular(k)?)?)?, mul(n - 1 - k, r)?)?;
        let mut plan = Self { height, block, radius, count, rows: 0, sum_rows: add(add(h, left)?, right)? };
        for index in [0, count - 1, count.saturating_sub(2), radius / block, radius.div_ceil(block)] {
            plan.rows = plan.rows.max(plan.stripe(index.min(count - 1))?.halo_rows);
        }
        Ok(plan)
    }
    pub fn stripe(self, index: u32) -> Result<Stripe, E> {
        if index >= self.count {
            return Err(E::Resource);
        }
        let start = index.checked_mul(self.block).ok_or(E::Resource)?;
        let end = start.checked_add(self.block).ok_or(E::Resource)?.min(self.height);
        let lo = start.saturating_sub(self.radius);
        let hi = end.checked_add(self.radius).ok_or(E::Resource)?.min(self.height);
        Ok(Stripe { start, rows: end - start, halo_start: lo, halo_rows: hi - lo })
    }
    pub fn float_bytes(self, width: u32) -> Result<u64, E> {
        mul(mul(u64::from(self.rows), u64::from(width))?, 16)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analytic_halos_match_enumeration() {
        for h in 1..=64 {
            for b in 1..=h.min(128) {
                for r in [0, 1, 2, 5, 16, 64, 192] {
                    let p = StripePlan::new(h, b, r).unwrap();
                    let stripes: Vec<_> = (0..p.count).map(|i| p.stripe(i).unwrap()).collect();
                    assert_eq!(p.sum_rows, stripes.iter().map(|s| u64::from(s.halo_rows)).sum::<u64>());
                    assert_eq!(p.rows, stripes.iter().map(|s| s.halo_rows).max().unwrap());
                }
            }
        }
    }
    #[test]
    fn four_k_float_storage_is_bounded() {
        let p = StripePlan::new(2160, 128, 24).unwrap();
        assert_eq!(p.count, 17);
        assert_eq!(p.rows, 176);
        assert_eq!(p.float_bytes(3840).unwrap(), 10_813_440);
        assert!(p.float_bytes(3840).unwrap() < 3840 * 2160 * 16 / 10);
    }
}
