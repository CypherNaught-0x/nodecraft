//! Additional exact geometry decisions for alternate backends.
use super::*;
impl PreparedImageOperation<'_> {
    /// Cover-resize background rectangle; blurred padding's first RGBA8 stage.
    pub fn background_cover_rect(&self) -> Result<[f64; 4], E> {
        let req = ImageRequest::Resize {
            options: Resize {
                size: ResizeSize::Exact { width: self.plan.width, height: self.plan.height, fit: Fit::Cover, anchor: Point::default() },
                resampling: Resampling::Bilinear,
            },
        };
        let p = super::super::geometry::layout(&req, self.inputs.source.width(), self.inputs.source.height(), None)?;
        Ok([p.rect.x, p.rect.y, p.rect.width, p.rect.height])
    }
    /// Exact nearest/eligible raw-quarter-turn rotation addresses. MAX denotes outside.
    pub fn nearest_rotation_indices(&self) -> Result<Option<Vec<u32>>, E> {
        let ImageRequest::Rotate { options: r } = self.request else { return Ok(None) };
        let raw_quarter = r.background.a == 0 && r.angle.rem_euclid(90.) == 0.;
        if r.resampling != Resampling::Nearest && !raw_quarter {
            return Ok(None);
        }
        let (sn, cs) = super::super::geometry::angle(r.angle);
        let point = |x: u32, y: u32| {
            let dx = f64::from(x) + 0.5 - f64::from(self.plan.width) / 2.;
            let dy = f64::from(y) + 0.5 - f64::from(self.plan.height) / 2.;
            (cs * dx + sn * dy + f64::from(self.inputs.source.width()) / 2. - 0.5, -sn * dx + cs * dy + f64::from(self.inputs.source.height()) / 2. - 0.5)
        };
        if r.resampling != Resampling::Nearest {
            // Integer quarter turns have constant fractional coordinates across this lattice.
            let (x, y) = point(0, 0);
            if x.fract() != 0. || y.fract() != 0. {
                return Ok(None);
            }
        }
        let count = u64::from(self.plan.width) * u64::from(self.plan.height);
        self.admit_extra(count.checked_mul(4).ok_or(E::ScratchLimit)?, count)?;
        let mut indices = Vec::new();
        indices.try_reserve_exact(usize::try_from(count).map_err(|_| E::Allocation)?).map_err(|_| E::Allocation)?;
        self.admit_extra((indices.capacity() as u64).checked_mul(4).ok_or(E::ScratchLimit)?, count)?;
        for y in 0..self.plan.height {
            for x in 0..self.plan.width {
                let (sx, sy) = point(x, y);
                let x = sx.round() as i64;
                let y = sy.round() as i64;
                let index = if x < 0 || y < 0 || x >= i64::from(self.inputs.source.width()) || y >= i64::from(self.inputs.source.height()) {
                    u32::MAX
                } else {
                    (y as u32) * self.inputs.source.width() + x as u32
                };
                indices.push(index);
            }
        }
        Ok(Some(indices))
    }
    pub fn rotation_trig(&self) -> Option<(f64, f64)> {
        if let ImageRequest::Rotate { options } = self.request { Some(super::super::geometry::angle(options.angle)) } else { None }
    }
    pub fn nearest_overlay_axes(&self) -> Result<Option<PreparedAxes>, E> {
        let ImageRequest::Overlay { options: Overlay { resampling: Resampling::Nearest, .. } } = self.request else { return Ok(None) };
        let over = self.inputs.overlay.ok_or(E::InputRoles)?;
        let rect = self.plan.overlay.ok_or(E::InputRoles)?;
        let width = rect.width as u32;
        let height = rect.height as u32;
        let count = u64::from(width) + u64::from(height);
        self.admit_extra(count.checked_mul(4).ok_or(E::ScratchLimit)?, count)?;
        let axis = |length: u32, source: u32| -> Result<Vec<u32>, E> {
            let mut values = Vec::new();
            values.try_reserve_exact(length as usize).map_err(|_| E::Allocation)?;
            for v in 0..length {
                let s = (f64::from(v) + 0.5) * f64::from(source) / f64::from(length) - 0.5;
                values.push((s.round() as i64).clamp(0, i64::from(source) - 1) as u32);
            }
            Ok(values)
        };
        let x = axis(width, over.width())?;
        let y = axis(height, over.height())?;
        self.admit_extra(((x.capacity() as u64) + (y.capacity() as u64)).checked_mul(4).ok_or(E::ScratchLimit)?, count)?;
        Ok(Some((x, y)))
    }
}
