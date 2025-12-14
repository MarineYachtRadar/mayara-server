use ndarray::{Array2, ArrayBase, Dim, OwnedRepr};

#[derive(Clone, Copy)]
pub struct PointInt {
    pub x: i16,
    pub y: i16,
}

pub struct PolarToCartesianLookup {
    spokes_per_revolution: usize,
    xyi: ArrayBase<OwnedRepr<PointInt>, Dim<[usize; 2]>>,
}

impl PolarToCartesianLookup {
    pub fn new(spokes_per_revolution: usize, spoke_len: usize) -> Self {
        let mut xyi = Vec::with_capacity(spokes_per_revolution * spoke_len);
        for arc in 0..spokes_per_revolution {
            let sine =
                (arc as f32 * 2.0 * std::f32::consts::PI / spokes_per_revolution as f32).sin();
            let cosine =
                (arc as f32 * 2.0 * std::f32::consts::PI / spokes_per_revolution as f32).cos();
            for radius in 0..spoke_len {
                let x = radius as f32 * cosine;
                let y = radius as f32 * sine;
                xyi.push(PointInt {
                    x: x as i16,
                    y: y as i16,
                });
            }
        }
        let xyi = Array2::from_shape_vec((spokes_per_revolution, spoke_len), xyi).unwrap();
        PolarToCartesianLookup {
            spokes_per_revolution,
            xyi,
        }
    }

    pub fn get_point_int(&self, angle: usize, radius: usize) -> &PointInt {
        let angle = (angle + self.spokes_per_revolution) % self.spokes_per_revolution;
        &self.xyi[[angle, radius]]
    }
}
