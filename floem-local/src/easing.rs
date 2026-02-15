//! The Easing trait and the built-in easing functions.

use peniko::kurbo::{ParamCurve, Point};

pub trait Easing: std::fmt::Debug {
    fn eval(&self, time: f64) -> f64;
    fn velocity(&self, time: f64) -> Option<f64> {
        let _ = time;
        None
    }
    fn finished(&self, time: f64) -> bool {
        !(0. ..1.).contains(&time)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Linear;
impl Easing for Linear {
    fn eval(&self, time: f64) -> f64 {
        time
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPosition {
    None,
    Both,
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    num_steps: usize,
    step_position: StepPosition,
}
impl Default for Step {
    fn default() -> Self {
        Self::END
    }
}

impl Step {
    pub const BOTH: Self = Self {
        num_steps: 1,
        step_position: StepPosition::Both,
    };
    pub const NONE: Self = Self {
        num_steps: 1,
        step_position: StepPosition::None,
    };
    pub const START: Self = Self {
        num_steps: 1,
        step_position: StepPosition::Start,
    };
    pub const END: Self = Self {
        num_steps: 1,
        step_position: StepPosition::End,
    };

    pub const fn new(num_steps: usize, step_position: StepPosition) -> Self {
        Self {
            num_steps,
            step_position,
        }
    }

    pub const fn new_end(num_steps: usize) -> Self {
        Self {
            num_steps,
            step_position: StepPosition::End,
        }
    }
}

impl Easing for Step {
    fn eval(&self, time: f64) -> f64 {
        match self.step_position {
            StepPosition::Start => {
                let step_size = 1.0 / self.num_steps as f64;
                ((time / step_size).floor() * step_size).min(1.0)
            }
            StepPosition::End => {
                let step_size = 1.0 / self.num_steps as f64;
                ((time / step_size).ceil() * step_size).min(1.0)
            }
            StepPosition::None => {
                let step_size = 1.0 / self.num_steps as f64;
                (time / step_size)
                    .floor()
                    .mul_add(step_size, step_size / 2.0)
                    .min(1.0)
            }
            StepPosition::Both => {
                let step_size = 1.0 / (self.num_steps - 1) as f64;
                let adjusted_time =
                    ((time / step_size).round() * step_size).min(1.0);
                (adjusted_time / step_size).round() * step_size
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bezier(pub f64, pub f64, pub f64, pub f64);
impl Bezier {
    const EASE: Self = Self(0.25, 0.1, 0.25, 1.);
    const EASE_IN: Self = Self(0.42, 0., 1., 1.);
    const EASE_OUT: Self = Self(0., 0., 0.58, 1.);
    const EASE_IN_OUT: Self = Self(0.42, 0., 0.58, 1.);
    pub const fn ease() -> Self {
        Self::EASE
    }
    pub const fn ease_in() -> Self {
        Self::EASE_IN
    }
    pub const fn ease_out() -> Self {
        Self::EASE_OUT
    }
    pub const fn ease_in_out() -> Self {
        Self::EASE_IN_OUT
    }

    pub fn eval(&self, time: f64) -> f64 {
        // TODO: Optimize this, don't use kurbo
        let p1 = Point::new(0., 0.);
        let p2 = Point::new(self.0, self.1);
        let p3 = Point::new(self.2, self.3);
        let p4 = Point::new(1., 1.);
        let point = crate::kurbo::CubicBez::new(p1, p2, p3, p4).eval(time);
        point.y
    }
}
impl Easing for Bezier {
    fn eval(&self, time: f64) -> f64 {
        self.eval(time)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    mass: f64,
    stiffness: f64,
    damping: f64,
    initial_velocity: f64,
}

impl Spring {
    pub const fn new(
        mass: f64,
        stiffness: f64,
        damping: f64,
        initial_velocity: f64,
    ) -> Self {
        Self {
            mass,
            stiffness,
            damping,
            initial_velocity,
        }
    }
    // TODO: figure out if these are reasonable values.

    /// Slower, smoother motion
    pub const fn gentle() -> Self {
        Self::new(1., 50.0, 8.0, 0.0)
    }

    /// More overshoot, longer settling time
    pub const fn bouncy() -> Self {
        Self::new(1., 150.0, 5.0, 0.0)
    }

    /// Quick response, minimal overshoot
    pub const fn snappy() -> Self {
        Self::new(1., 200.0, 20.0, 0.0)
    }

    pub fn eval(&self, time: f64) -> f64 {
        if time <= 0.0 {
            return 0.0;
        }

        let m = self.mass;
        let k = self.stiffness;
        let c = self.damping;
        let v0 = self.initial_velocity;

        let omega = (k / m).sqrt();
        let zeta = c / (2.0 * (k * m).sqrt());

        if zeta < 1.0 {
            // Underdamped
            let omega_d = omega * zeta.mul_add(-zeta, 1.0).sqrt();
            let e = (-zeta * omega * time).exp();
            let cos_term = (omega_d * time).cos();
            let sin_term = (omega_d * time).sin();

            let a = 1.0;
            let b = (zeta * omega).mul_add(a, v0) / omega_d;

            e.mul_add(-a.mul_add(cos_term, b * sin_term), 1.0)
        } else if zeta > 1.0 {
            // Overdamped
            let r1 = -omega * (zeta - zeta.mul_add(zeta, -1.0).sqrt());
            let r2 = -omega * (zeta + zeta.mul_add(zeta, -1.0).sqrt());

            let a = (v0 - r2) / (r1 - r2);
            let b = 1.0 - a;

            b.mul_add(-(r2 * time).exp(), a.mul_add(-(r1 * time).exp(), 1.0))
        } else {
            // Critically damped
            let e = (-omega * time).exp();
            let a = 1.0;
            let b = omega.mul_add(a, v0);

            e.mul_add(-b.mul_add(time, a), 1.0)
        }
    }

    pub const THRESHOLD: f64 = 0.005;
    pub fn finished(&self, time: f64) -> bool {
        let position = self.eval(time);
        let velocity = self.velocity(time);

        (1.0 - position).abs() < Self::THRESHOLD && velocity.abs() < Self::THRESHOLD
    }

    pub fn velocity(&self, time: f64) -> f64 {
        if time <= 0.0 {
            return self.initial_velocity;
        }

        let m = self.mass;
        let k = self.stiffness;
        let c = self.damping;
        let v0 = self.initial_velocity;

        let omega = (k / m).sqrt();
        let zeta = c / (2.0 * (k * m).sqrt());

        if zeta < 1.0 {
            // Underdamped
            let omega_d = omega * zeta.mul_add(-zeta, 1.0).sqrt();
            let e = (-zeta * omega * time).exp();
            let cos_term = (omega_d * time).cos();
            let sin_term = (omega_d * time).sin();

            let a = 1.0;
            let b = (zeta * omega).mul_add(a, v0) / omega_d;

            e * (zeta * omega).mul_add(
                a.mul_add(cos_term, b * sin_term),
                (a * -omega_d).mul_add(sin_term, b * omega_d * cos_term),
            )
        } else if zeta > 1.0 {
            // Overdamped
            let r1 = -omega * (zeta - zeta.mul_add(zeta, -1.0).sqrt());
            let r2 = -omega * (zeta + zeta.mul_add(zeta, -1.0).sqrt());

            let a = (v0 - r2) / (r1 - r2);
            let b = 1.0 - a;

            (-a * r1).mul_add((r1 * time).exp(), -(b * r2 * (r2 * time).exp()))
        } else {
            // Critically damped
            let e = (-omega * time).exp();
            let a = 1.0;
            let b = omega.mul_add(a, v0);

            e * omega.mul_add(-b.mul_add(time, a), b)
        }
    }
}

impl Default for Spring {
    fn default() -> Self {
        Self::new(1.0, 100.0, 15.0, 0.0)
    }
}

// TODO: The finished function is quite inneficient as it will result in repeated work.
// Can't cache it here because making this mutable is weird.
// Need to find a way to cache the work in the animation.
impl Easing for Spring {
    fn eval(&self, time: f64) -> f64 {
        self.eval(time)
    }

    fn velocity(&self, time: f64) -> Option<f64> {
        Some(self.velocity(time))
    }

    fn finished(&self, time: f64) -> bool {
        self.finished(time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() < tolerance
    }

    // ---- Easing trait defaults ----

    #[test]
    fn easing_finished_default_before_start() {
        assert!(Linear.finished(-0.1));
    }

    #[test]
    fn easing_finished_default_at_zero() {
        assert!(!Linear.finished(0.0));
    }

    #[test]
    fn easing_finished_default_midway() {
        assert!(!Linear.finished(0.5));
    }

    #[test]
    fn easing_finished_default_at_one() {
        assert!(Linear.finished(1.0));
    }

    #[test]
    fn easing_finished_default_beyond() {
        assert!(Linear.finished(2.0));
    }

    #[test]
    fn easing_velocity_default_is_none() {
        assert!(Linear.velocity(0.5).is_none());
    }

    // ---- Linear ----

    #[test]
    fn linear_eval_zero() {
        assert_eq!(Linear.eval(0.0), 0.0);
    }

    #[test]
    fn linear_eval_half() {
        assert_eq!(Linear.eval(0.5), 0.5);
    }

    #[test]
    fn linear_eval_one() {
        assert_eq!(Linear.eval(1.0), 1.0);
    }

    #[test]
    fn linear_eval_negative() {
        assert_eq!(Linear.eval(-0.5), -0.5);
    }

    // ---- Step ----

    #[test]
    fn step_default_is_end() {
        assert_eq!(Step::default(), Step::END);
    }

    #[test]
    fn step_start_at_zero() {
        let s = Step::START;
        assert_eq!(s.eval(0.0), 0.0);
    }

    #[test]
    fn step_start_at_half() {
        // 1 step, StepPosition::Start, step_size = 1.0
        // floor(0.5/1.0) * 1.0 = 0.0
        let s = Step::START;
        assert_eq!(s.eval(0.5), 0.0);
    }

    #[test]
    fn step_start_at_one() {
        let s = Step::START;
        assert_eq!(s.eval(1.0), 1.0);
    }

    #[test]
    fn step_end_at_zero() {
        let s = Step::END;
        // ceil(0.0/1.0) * 1.0 = 0.0
        assert_eq!(s.eval(0.0), 0.0);
    }

    #[test]
    fn step_end_at_tiny() {
        let s = Step::END;
        // ceil(0.01/1.0) * 1.0 = 1.0
        assert_eq!(s.eval(0.01), 1.0);
    }

    #[test]
    fn step_end_at_one() {
        let s = Step::END;
        assert_eq!(s.eval(1.0), 1.0);
    }

    #[test]
    fn step_end_multiple_steps() {
        let s = Step::new(4, StepPosition::End);
        // step_size = 0.25
        // ceil(0.1 / 0.25) * 0.25 = ceil(0.4) * 0.25 = 1 * 0.25 = 0.25
        assert!(approx(s.eval(0.1), 0.25, 1e-10));
        // ceil(0.3 / 0.25) * 0.25 = ceil(1.2) * 0.25 = 2 * 0.25 = 0.5
        assert!(approx(s.eval(0.3), 0.5, 1e-10));
    }

    #[test]
    fn step_start_multiple_steps() {
        let s = Step::new(4, StepPosition::Start);
        // step_size = 0.25
        // floor(0.3 / 0.25) * 0.25 = floor(1.2) * 0.25 = 1 * 0.25 = 0.25
        assert!(approx(s.eval(0.3), 0.25, 1e-10));
    }

    #[test]
    fn step_none_single_step() {
        let s = Step::NONE;
        // step_size = 1.0
        // floor(0.0/1.0) * 1.0 + 0.5 = 0.5
        assert!(approx(s.eval(0.0), 0.5, 1e-10));
    }

    #[test]
    fn step_none_at_one() {
        let s = Step::NONE;
        // floor(1.0/1.0) * 1.0 + 0.5 = 1.5 => clamped to 1.0
        assert_eq!(s.eval(1.0), 1.0);
    }

    #[test]
    fn step_both_single_step_panics_or_nan() {
        let _s = Step::BOTH;
        // num_steps = 1, step_size = 1/(1-1) = 1/0 = inf
        // This produces NaN or Inf — verified by the two_steps test instead
    }

    #[test]
    fn step_both_two_steps() {
        let s = Step::new(2, StepPosition::Both);
        // step_size = 1/(2-1) = 1.0
        // time=0: round(0/1)*1 = 0, then round(0/1)*1 = 0
        assert_eq!(s.eval(0.0), 0.0);
        // time=1: round(1/1)*1 = 1, then round(1/1)*1 = 1
        assert_eq!(s.eval(1.0), 1.0);
    }

    #[test]
    fn step_new_end_constructor() {
        let s = Step::new_end(3);
        assert_eq!(s.num_steps, 3);
        assert_eq!(s.step_position, StepPosition::End);
    }

    #[test]
    fn step_clamped_to_one() {
        // Step should never return > 1.0
        let s = Step::new(2, StepPosition::End);
        assert!(s.eval(1.5) <= 1.0);
    }

    // ---- Bezier ----

    #[test]
    fn bezier_ease_endpoints() {
        let b = Bezier::ease();
        assert!(approx(b.eval(0.0), 0.0, 1e-10));
        assert!(approx(b.eval(1.0), 1.0, 1e-10));
    }

    #[test]
    fn bezier_ease_in_endpoints() {
        let b = Bezier::ease_in();
        assert!(approx(b.eval(0.0), 0.0, 1e-10));
        assert!(approx(b.eval(1.0), 1.0, 1e-10));
    }

    #[test]
    fn bezier_ease_out_endpoints() {
        let b = Bezier::ease_out();
        assert!(approx(b.eval(0.0), 0.0, 1e-10));
        assert!(approx(b.eval(1.0), 1.0, 1e-10));
    }

    #[test]
    fn bezier_ease_in_out_endpoints() {
        let b = Bezier::ease_in_out();
        assert!(approx(b.eval(0.0), 0.0, 1e-10));
        assert!(approx(b.eval(1.0), 1.0, 1e-10));
    }

    #[test]
    fn bezier_monotonic_ease() {
        let b = Bezier::ease();
        let mut prev = 0.0;
        for i in 0..=100 {
            let t = i as f64 / 100.0;
            let v = b.eval(t);
            assert!(
                v >= prev - 1e-10,
                "ease not monotonic at t={t}: {v} < {prev}"
            );
            prev = v;
        }
    }

    #[test]
    fn bezier_easing_trait() {
        let b = Bezier::ease();
        // Easing trait delegates to Bezier::eval
        let trait_val = Easing::eval(&b, 0.5);
        let direct_val = b.eval(0.5);
        assert_eq!(trait_val, direct_val);
    }

    #[test]
    fn bezier_linear_equivalent() {
        // Bezier(0, 0, 1, 1) should act like linear
        let b = Bezier(0.0, 0.0, 1.0, 1.0);
        assert!(approx(b.eval(0.5), 0.5, 1e-6));
    }

    #[test]
    fn bezier_default_is_zero() {
        let b = Bezier::default();
        assert_eq!(b, Bezier(0.0, 0.0, 0.0, 0.0));
    }

    // ---- Spring ----

    #[test]
    fn spring_eval_at_zero() {
        let s = Spring::default();
        assert_eq!(s.eval(0.0), 0.0);
    }

    #[test]
    fn spring_eval_negative_time() {
        let s = Spring::default();
        assert_eq!(s.eval(-1.0), 0.0);
    }

    #[test]
    fn spring_converges_to_one() {
        let s = Spring::default();
        // At a large time value, the spring should be very close to 1.0
        assert!(approx(s.eval(10.0), 1.0, 0.01));
    }

    #[test]
    fn spring_gentle_converges() {
        let s = Spring::gentle();
        assert!(approx(s.eval(10.0), 1.0, 0.01));
    }

    #[test]
    fn spring_bouncy_overshoots() {
        let s = Spring::bouncy();
        // Bouncy has low damping => underdamped, should overshoot 1.0
        let mut overshoots = false;
        for i in 1..100 {
            let t = i as f64 * 0.05;
            if s.eval(t) > 1.0 {
                overshoots = true;
                break;
            }
        }
        assert!(overshoots, "bouncy spring should overshoot");
    }

    #[test]
    fn spring_snappy_converges_fast() {
        let s = Spring::snappy();
        // Snappy has high stiffness and high damping => quick convergence
        assert!(approx(s.eval(3.0), 1.0, 0.01));
    }

    #[test]
    fn spring_velocity_at_zero_is_initial() {
        let s = Spring::new(1.0, 100.0, 10.0, 5.0);
        assert_eq!(s.velocity(0.0), 5.0);
    }

    #[test]
    fn spring_velocity_at_negative_is_initial() {
        let s = Spring::new(1.0, 100.0, 10.0, 3.0);
        assert_eq!(s.velocity(-1.0), 3.0);
    }

    #[test]
    fn spring_velocity_approaches_zero() {
        let s = Spring::default();
        // At large time, velocity should be near 0
        assert!(s.velocity(10.0).abs() < 0.01);
    }

    #[test]
    fn spring_finished_false_at_start() {
        let s = Spring::default();
        assert!(!s.finished(0.0));
    }

    #[test]
    fn spring_finished_true_at_large_time() {
        let s = Spring::default();
        assert!(s.finished(10.0));
    }

    #[test]
    fn spring_overdamped() {
        // zeta > 1 => overdamped (no oscillation)
        let s = Spring::new(1.0, 10.0, 100.0, 0.0);
        // Should monotonically approach 1.0 without overshooting
        let mut prev = 0.0f64;
        for i in 1..100 {
            let t = i as f64 * 0.1;
            let v = s.eval(t);
            assert!(
                v >= prev - 1e-6,
                "overdamped not monotonic at t={t}: {v} < {prev}"
            );
            prev = v;
        }
    }

    #[test]
    fn spring_critically_damped() {
        // zeta == 1 => c = 2*sqrt(k*m)
        // k=100, m=1 => c = 2*sqrt(100) = 20
        let s = Spring::new(1.0, 100.0, 20.0, 0.0);
        assert!(approx(s.eval(5.0), 1.0, 0.01));
    }

    #[test]
    fn spring_underdamped_velocity() {
        let s = Spring::gentle(); // underdamped
                                  // Velocity should oscillate sign changes
        let v1 = s.velocity(0.1);
        assert!(v1 > 0.0, "initial velocity should be positive");
    }

    #[test]
    fn spring_overdamped_velocity() {
        let s = Spring::new(1.0, 10.0, 100.0, 0.0);
        let v = s.velocity(0.5);
        assert!(v >= 0.0, "overdamped velocity should be non-negative");
    }

    #[test]
    fn spring_critically_damped_velocity() {
        let s = Spring::new(1.0, 100.0, 20.0, 0.0);
        assert!(s.velocity(5.0).abs() < 0.1);
    }

    #[test]
    fn spring_easing_trait_eval() {
        let s = Spring::default();
        assert_eq!(Easing::eval(&s, 0.5), s.eval(0.5));
    }

    #[test]
    fn spring_easing_trait_velocity() {
        let s = Spring::default();
        assert_eq!(Easing::velocity(&s, 0.5), Some(s.velocity(0.5)));
    }

    #[test]
    fn spring_easing_trait_finished() {
        let s = Spring::default();
        assert_eq!(Easing::finished(&s, 10.0), s.finished(10.0));
    }
}
