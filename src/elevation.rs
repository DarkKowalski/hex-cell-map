//! Continuous local-relief metadata enhances detail without filtering the DEM.
use crate::{
    hydrology::Hydrology,
    map_core::{HeightField, HeightSettings, MapDocument},
};
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HeightSample {
    pub height: f64,
    pub detail: f64,
    pub relief: f64,
    pub dry: f64,
}
impl HeightSample {
    pub fn meters(self, settings: HeightSettings) -> f64 {
        let mountain = ((self.relief - 400.) / 800.).clamp(0., 1.);
        let gain = f64::from(settings.hill_boost) * (1. - mountain * 0.65);
        let limit = settings.compression_m * 0.35;
        let detail = self.detail.signum() * limit * (self.detail.abs() / limit).tanh();
        settings.meters(self.height) + f64::from(settings.scale) * gain * detail * self.dry
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayVertex {
    pub samples: [HeightSample; 5],
    pub epsilon: f64,
}
impl DisplayVertex {
    pub fn water(height: f64, gradient: [f64; 2]) -> Self {
        let sample = |height| HeightSample {
            height,
            ..Default::default()
        };
        Self {
            samples: [
                sample(height),
                sample(height + gradient[0]),
                sample(height - gradient[0]),
                sample(height + gradient[1]),
                sample(height - gradient[1]),
            ],
            epsilon: 1.,
        }
    }
    pub fn meters(self, settings: HeightSettings) -> f64 {
        self.samples[0].meters(settings)
    }
    pub fn normal(self, settings: HeightSettings) -> [f32; 3] {
        let h = self.samples.map(|s| s.meters(settings));
        let v = [
            (-(h[1] - h[2]) / (2. * self.epsilon)) as f32,
            1.,
            ((h[3] - h[4]) / (2. * self.epsilon)) as f32,
        ];
        let length = v.iter().map(|n| n * n).sum::<f32>().sqrt();
        v.map(|n| n / length)
    }
}
pub struct ReliefField {
    baseline: Vec<f32>,
    relief: Vec<f32>,
}
impl ReliefField {
    pub fn build(field: &HeightField, spacing: f64) -> Self {
        let radius = (spacing * 1.5 / field.step_m).round().max(1.) as isize;
        let mut baseline = Vec::with_capacity(field.elevations_m.len());
        let mut relief = Vec::with_capacity(field.elevations_m.len());
        for i in 0..field.elevations_m.len() {
            let x = (i % field.width) as isize;
            let y = (i / field.width) as isize;
            let mut sum = 0.;
            let mut total = 0.;
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            for (dx, dy, w) in [
                (0, 0, 4.),
                (-1, 0, 2.),
                (1, 0, 2.),
                (0, -1, 2.),
                (0, 1, 2.),
                (-1, -1, 1.),
                (-1, 1, 1.),
                (1, -1, 1.),
                (1, 1, 1.),
            ] {
                let xx = (x + dx * radius).clamp(0, field.width as isize - 1) as usize;
                let yy = (y + dy * radius).clamp(0, field.height as isize - 1) as usize;
                let h = field.elevations_m[yy * field.width + xx];
                sum += h * w;
                total += w;
                low = low.min(h);
                high = high.max(h);
            }
            baseline.push(sum / total);
            relief.push(high - low);
        }
        Self { baseline, relief }
    }
    pub fn sample(
        &self,
        document: &MapDocument,
        hydrology: &Hydrology,
        p: [f64; 2],
        height: f64,
    ) -> HeightSample {
        let f = &document.height_field;
        let x = ((p[0] - f.origin_m[0]) / f.step_m).clamp(0., (f.width - 1) as f64);
        let y = ((p[1] - f.origin_m[1]) / f.step_m).clamp(0., (f.height - 1) as f64);
        let ix = (x.floor() as usize).min(f.width - 2);
        let iy = (y.floor() as usize).min(f.height - 2);
        let tx = x - ix as f64;
        let ty = y - iy as f64;
        let interpolate = |values: &[f32]| {
            f64::from(values[iy * f.width + ix]) * (1. - tx) * (1. - ty)
                + f64::from(values[iy * f.width + ix + 1]) * tx * (1. - ty)
                + f64::from(values[(iy + 1) * f.width + ix]) * (1. - tx) * ty
                + f64::from(values[(iy + 1) * f.width + ix + 1]) * tx * ty
        };
        HeightSample {
            height,
            detail: height - interpolate(&self.baseline),
            relief: interpolate(&self.relief),
            dry: hydrology.dry_weight(document, p),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_relief_survives_at_altitude_extremes_compress_and_water_stays_level() {
        let s = HeightSettings::default();
        assert!(s.meters(4000.) > 3500.);
        assert!(s.meters(10000.) < 8000.);
        let hill = |height, detail| {
            HeightSample {
                height,
                detail,
                relief: 200.,
                dry: 1.,
            }
            .meters(s)
        };
        assert!(hill(150., 50.) - hill(100., 0.) > 80.);
        assert!(hill(2050., 50.) - hill(2000., 0.) > 65.);
        let mountain = |height, detail| {
            HeightSample {
                height,
                detail,
                relief: 1600.,
                dry: 1.,
            }
            .meters(s)
        };
        assert!(mountain(3000., 600.) - mountain(2000., -400.) > 850.);
        for height in [0., 100., 1500., 4000.] {
            let shoreline = HeightSample {
                height,
                detail: 700.,
                relief: 1000.,
                dry: 0.,
            };
            assert_eq!(shoreline.meters(s), s.meters(height));
            assert!(s.meters(height + 1.) > s.meters(height));
            assert!((s.inverse_meters(s.meters(height)) - height).abs() < 1e-6);
        }
    }
}
