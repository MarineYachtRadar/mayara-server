use ndarray::{Array2, ArrayBase, Dim, OwnedRepr};

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy)]
pub struct PointInt {
    pub x: i16,
    pub y: i16,
}

#[allow(dead_code)]
pub struct PolarToCartesianLookup {
    spokes_per_revolution: usize,
    spoke_len: usize,
    xy: ArrayBase<OwnedRepr<Point>, Dim<[usize; 2]>>,
    xyi: ArrayBase<OwnedRepr<PointInt>, Dim<[usize; 2]>>,
}

impl PolarToCartesianLookup {
    pub fn new(spokes_per_revolution: usize, spoke_len: usize) -> Self {
        let mut xy = Vec::with_capacity(spokes_per_revolution * spoke_len);
        let mut xyi = Vec::with_capacity(spokes_per_revolution * spoke_len);
        for arc in 0..spokes_per_revolution {
            let sine = (arc as f64 * std::f64::consts::TAU / spokes_per_revolution as f64).sin();
            let cosine = (arc as f64 * std::f64::consts::TAU / spokes_per_revolution as f64).cos();
            for radius in 0..spoke_len {
                let x = radius as f64 * cosine;
                let y = radius as f64 * sine;
                xy.push(Point { x, y });
                xyi.push(PointInt {
                    x: x as i16,
                    y: y as i16,
                });
            }
        }
        let xy = Array2::from_shape_vec((spokes_per_revolution, spoke_len), xy).unwrap();
        let xyi = Array2::from_shape_vec((spokes_per_revolution, spoke_len), xyi).unwrap();
        PolarToCartesianLookup {
            spokes_per_revolution,
            spoke_len,
            xy,
            xyi,
        }
    }

    #[allow(dead_code)]
    pub fn get_point(&self, angle: usize, radius: usize) -> &Point {
        let angle = (angle + self.spokes_per_revolution) % self.spokes_per_revolution;
        &self.xy[[angle, radius]]
    }

    pub fn get_point_int(&self, angle: usize, radius: usize) -> &PointInt {
        let angle = (angle + self.spokes_per_revolution) % self.spokes_per_revolution;
        &self.xyi[[angle, radius]]
    }
}
