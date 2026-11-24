//! Differential checks of the Vesper port against the frozen Rust seed copy
//! (`vesper-kinetic-seed`, here `vesper_kinetic_rust`) that the flip gate
//! runs (crates/vesper-kinetic/flip-gate.toml, SH6 lane 9, #1404). Only the
//! port-backed package runs this file; the seed has no Rust build to compare.

use vesper_kinetic::gjk::{GjkError, Shape, separation};
use vesper_kinetic::lbvh::{Aabb, Lbvh};
use vesper_kinetic::sap::{SweepAndPrune, sortable};
use vesper_kinetic::{Colorer, ConstraintBodies};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn unit(&mut self) -> f64 {
        f64::from(self.next()) / f64::from(u32::MAX)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// The port's colourings (`Colorer`, `MultiColorer`) against the frozen Rust
/// build, visiting order, colours and layers, from small graphs (the
/// comparison sort) to large ones (the port's radix sort of packed keys) and
/// body counts too wide to pack (SH6 lane 9, #1404).
#[test]
fn colourings_match_the_rust_build() {
    use vesper_kinetic_rust as rust;
    let mut rng = Lcg(7);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for round in 0..300u32 {
        let bodies = match round % 4 {
            0 => 2 + rng.next() % 30,
            1 => 64 + rng.next() % 4096,
            2 => 1 << (12 + rng.next() % 9),
            _ => 3_000_000 + rng.next() % 1_000_000,
        };
        let count = match round % 3 {
            0 => rng.next() % 300,
            1 => 256 + rng.next() % 2048,
            _ => rng.next() % 5000,
        } as usize;
        let mut cs = Vec::with_capacity(count);
        for _ in 0..count {
            let a = rng.next() % bodies;
            let b = match rng.next() % 4 {
                0 => None,
                1 => Some((a + 1) % bodies),
                _ => Some(rng.next() % bodies),
            };
            let b = b.filter(|&b| b != a);
            cs.push(ConstraintBodies { a, b });
        }
        let rust_cs: Vec<_> = cs
            .iter()
            .map(|c| rust::ConstraintBodies { a: c.a, b: c.b })
            .collect();
        let mut p = Colorer::new(bodies as usize, count);
        let mut r = rust::Colorer::new(bodies as usize, count);
        let (pr, rr) = (p.color(&cs), r.color(&rust_cs));
        cases += 1;
        assert_eq!(pr.is_ok(), rr.is_ok(), "round {round}: {pr:?} / {rr:?}");
        if pr.is_ok()
            && (p.visiting_order() != r.visiting_order()
                || p.layer_count() != r.layer_count()
                || (0..count).any(|i| p.color_of(i) != r.color_of(i))
                || (0..p.layer_count()).any(|l| p.layer(l) != r.layer(l)))
        {
            differences += 1;
        }

        // MultiColorer over sets of one to four distinct bodies.
        let mut flat = Vec::new();
        let mut offsets = vec![0u32];
        for _ in 0..count {
            let arity = 1 + rng.next() % 4;
            let start = flat.len();
            while flat.len() - start < arity as usize {
                let body = rng.next() % bodies;
                if !flat[start..].contains(&body) {
                    flat.push(body);
                }
                if bodies < 4 && flat.len() - start == bodies as usize {
                    break;
                }
            }
            offsets.push(flat.len() as u32);
        }
        let mut pm = vesper_kinetic::MultiColorer::new(bodies as usize, count);
        let mut rm = rust::MultiColorer::new(bodies as usize, count);
        let (pr, rr) = (pm.color(&flat, &offsets), rm.color(&flat, &offsets));
        cases += 1;
        assert_eq!(pr.is_ok(), rr.is_ok(), "round {round}: {pr:?} / {rr:?}");
        if pr.is_ok()
            && (pm.visiting_order() != rm.visiting_order()
                || pm.layer_count() != rm.layer_count()
                || (0..count).any(|i| pm.color_of(i) != rm.color_of(i))
                || (0..pm.layer_count()).any(|l| pm.layer(l) != rm.layer(l)))
        {
            differences += 1;
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

fn rand_vec(rng: &mut Lcg, span: f64) -> [f64; 3] {
    [
        rng.range(-span, span),
        rng.range(-span, span),
        rng.range(-span, span),
    ]
}

fn rand_polytope(rng: &mut Lcg, center: [f64; 3], n: usize, scale: f64) -> Vec<[f64; 3]> {
    (0..n)
        .map(|_| {
            let v = rand_vec(rng, scale);
            [center[0] + v[0], center[1] + v[1], center[2] + v[2]]
        })
        .collect()
}

fn to_rust_shape<'a>(s: &Shape<'a>) -> vesper_kinetic_rust::gjk::Shape<'a> {
    match *s {
        Shape::Sphere { center, radius } => {
            vesper_kinetic_rust::gjk::Shape::Sphere { center, radius }
        }
        Shape::Capsule { a, b, radius } => {
            vesper_kinetic_rust::gjk::Shape::Capsule { a, b, radius }
        }
        Shape::Polytope { vertices } => vesper_kinetic_rust::gjk::Shape::Polytope { vertices },
    }
}

fn to_rust_err(e: GjkError) -> vesper_kinetic_rust::gjk::GjkError {
    match e {
        GjkError::InvalidShape => vesper_kinetic_rust::gjk::GjkError::InvalidShape,
        GjkError::NoConvergence => vesper_kinetic_rust::gjk::GjkError::NoConvergence,
        GjkError::Degenerate => vesper_kinetic_rust::gjk::GjkError::Degenerate,
    }
}

fn vec_bits_eq(a: [f64; 3], b: [f64; 3]) -> bool {
    a[0].to_bits() == b[0].to_bits()
        && a[1].to_bits() == b[1].to_bits()
        && a[2].to_bits() == b[2].to_bits()
}

/// The port's GJK distance and EPA penetration queries against the frozen
/// Rust seed engine across spheres, capsules, polytopes and error cases.
#[test]
fn gjk_matches_the_rust_build() {
    use vesper_kinetic_rust::gjk as rust;
    let mut rng = Lcg(0x616B_0001);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for round in 0..500u32 {
        let pts_a;
        let pts_b;
        let shift = if round % 3 == 0 { 0.4 } else { 2.2 };
        let ca = rand_vec(&mut rng, 0.5);
        let cb = [ca[0] + shift, ca[1] + rng.range(-0.5, 0.5), ca[2]];
        let sa = match round % 3 {
            0 => Shape::Sphere {
                center: ca,
                radius: rng.range(0.1, 1.5),
            },
            1 => Shape::Capsule {
                a: ca,
                b: [ca[0] + 0.3, ca[1] - 0.2, ca[2] + 0.4],
                radius: rng.range(0.05, 0.9),
            },
            _ => {
                let n = 4 + (rng.next() % 10) as usize;
                pts_a = rand_polytope(&mut rng, ca, n, 1.2);
                Shape::Polytope { vertices: &pts_a }
            }
        };
        let sb = match (round / 3) % 3 {
            0 => Shape::Sphere {
                center: cb,
                radius: rng.range(0.1, 1.5),
            },
            1 => Shape::Capsule {
                a: cb,
                b: [cb[0] - 0.2, cb[1] + 0.3, cb[2] - 0.1],
                radius: rng.range(0.05, 0.9),
            },
            _ => {
                let n = 4 + (rng.next() % 10) as usize;
                pts_b = rand_polytope(&mut rng, cb, n, 1.2);
                Shape::Polytope { vertices: &pts_b }
            }
        };
        let ra = to_rust_shape(&sa);
        let rb = to_rust_shape(&sb);
        let pr = separation(&sa, &sb);
        let rr = rust::separation(&ra, &rb);
        cases += 1;
        match (pr, rr) {
            (Ok(ps), Ok(rs)) => {
                if ps.distance.to_bits() != rs.distance.to_bits()
                    || !vec_bits_eq(ps.normal, rs.normal)
                    || !vec_bits_eq(ps.point_a, rs.point_a)
                    || !vec_bits_eq(ps.point_b, rs.point_b)
                {
                    differences += 1;
                }
            }
            (Err(pe), Err(re)) => {
                if to_rust_err(pe) != re {
                    differences += 1;
                }
            }
            _ => differences += 1,
        }
    }
    let bad = Shape::Sphere {
        center: [0.0, f64::NAN, 0.0],
        radius: 1.0,
    };
    let good = Shape::Sphere {
        center: [0.0; 3],
        radius: 1.0,
    };
    cases += 1;
    if separation(&bad, &good).err().map(to_rust_err)
        != rust::separation(&to_rust_shape(&bad), &to_rust_shape(&good)).err()
    {
        differences += 1;
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

fn rand_boxes(
    rng: &mut Lcg,
    n: usize,
    world: f64,
    max_half: f64,
) -> (Vec<Aabb>, Vec<vesper_kinetic_rust::lbvh::Aabb>) {
    let mut p = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for _ in 0..n {
        let c = [
            rng.range(-world, world) as f32,
            rng.range(-world, world) as f32,
            rng.range(-world, world) as f32,
        ];
        let h = [
            rng.range(0.0, max_half) as f32,
            rng.range(0.0, max_half) as f32,
            rng.range(0.0, max_half) as f32,
        ];
        let min = [c[0] - h[0], c[1] - h[1], c[2] - h[2]];
        let max = [c[0] + h[0], c[1] + h[1], c[2] + h[2]];
        p.push(Aabb { min, max });
        r.push(vesper_kinetic_rust::lbvh::Aabb { min, max });
    }
    (p, r)
}

/// The port's SAP broadphase (`sortable`, `build`, `overlapping_pairs`)
/// against the frozen Rust seed engine.
#[test]
fn sap_matches_the_rust_build() {
    use vesper_kinetic_rust::sap as rust;
    let mut rng = Lcg(0x5A90_0002);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for &x in &[
        -f32::INFINITY,
        -1e9,
        -1.0,
        -0.0,
        0.0,
        1.0,
        1e9,
        f32::INFINITY,
    ] {
        cases += 1;
        if sortable(x) != rust::sortable(x) {
            differences += 1;
        }
    }
    let mut psap = SweepAndPrune::with_capacity(512);
    let mut rsap = rust::SweepAndPrune::with_capacity(512);
    let mut po = Vec::new();
    let mut ro = Vec::new();
    for round in 0..120u32 {
        let n = match round {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 1 + (rng.next() % 512) as usize,
        };
        let (pb, rb) = rand_boxes(&mut rng, n, 25.0, 2.5);
        let pr = psap.build(&pb);
        let rr = rsap.build(&rb);
        cases += 1;
        if pr.is_ok() != rr.is_ok() {
            differences += 1;
            continue;
        }
        if pr.is_ok() {
            if psap.axis() != rsap.axis() || psap.order() != rsap.order() {
                differences += 1;
            }
            let pq = psap.overlapping_pairs(&mut po, 20_000);
            let rq = rsap.overlapping_pairs(&mut ro, 20_000);
            cases += 1;
            if pq.is_ok() != rq.is_ok() || (pq.is_ok() && po != ro) {
                differences += 1;
            }
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's LBVH (`morton64`, `build`, `overlapping_pairs`) against the
/// Rust build (not yet flipped; checks 0 differences for the speed edits).
#[test]
fn lbvh_matches_the_rust_build() {
    use vesper_kinetic::lbvh::{demorton64, morton64};
    use vesper_kinetic_rust::lbvh as rust;
    let mut rng = Lcg(0x1B04_0003);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for _ in 0..200 {
        let x = rng.next() & 0x1f_ffff;
        let y = rng.next() & 0x1f_ffff;
        let z = rng.next() & 0x1f_ffff;
        cases += 1;
        if morton64(x, y, z) != rust::morton64(x, y, z)
            || demorton64(morton64(x, y, z)) != rust::demorton64(rust::morton64(x, y, z))
        {
            differences += 1;
        }
    }
    let mut pl = Lbvh::with_capacity(512);
    let mut rl = rust::Lbvh::with_capacity(512);
    let mut po = Vec::new();
    let mut ro = Vec::new();
    for round in 0..80u32 {
        let n = match round {
            0 => 0,
            1 => 1,
            _ => 1 + (rng.next() % 512) as usize,
        };
        let (pb, rb) = rand_boxes(&mut rng, n, 25.0, 2.5);
        let pr = pl.build(&pb);
        let rr = rl.build(&rb);
        cases += 1;
        if pr.is_ok() != rr.is_ok() {
            differences += 1;
            continue;
        }
        if pr.is_ok() {
            let pq = pl.overlapping_pairs(&mut po, 20_000);
            let rq = rl.overlapping_pairs(&mut ro, 20_000);
            cases += 1;
            if pq.is_ok() != rq.is_ok() || (pq.is_ok() && po != ro) {
                differences += 1;
            }
        }
    }
    // Benchmark-sized trees, and small pair bounds (the port's pair scratch
    // then holds fewer entries than the tree has nodes, so the query tests
    // every subtree instead of pruning the ones below the query leaf).
    let mut pl = Lbvh::with_capacity(4096);
    let mut rl = rust::Lbvh::with_capacity(4096);
    for round in 0..12u32 {
        let n = 2048 + (rng.next() % 2049) as usize;
        let (pb, rb) = rand_boxes(&mut rng, n, 64.0, 0.8);
        cases += 1;
        if pl.build(&pb).is_ok() != rl.build(&rb).is_ok() {
            differences += 1;
            continue;
        }
        for max in [usize::MAX, 64, 1 + (rng.next() % 4096) as usize] {
            let max = if round % 2 == 0 { max } else { max.min(n / 2) };
            let pq = pl.overlapping_pairs(&mut po, max);
            let rq = rl.overlapping_pairs(&mut ro, max);
            cases += 1;
            if format!("{pq:?}") != format!("{rq:?}") || (pq.is_ok() && po != ro) {
                differences += 1;
            }
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's fast math (`sin`, `cos`, `atan2`, `exp`, `log` and their
/// binary32 forms) against the frozen Rust build, bit for bit: random
/// arguments over the fast reduction's range and beyond it (the correctly
/// rounded fallback), every binade's edges, random bit patterns, and the
/// special values (SH6 lane 9, #1404). A NaN result only has to be a NaN:
/// Rust leaves the payload of an operation on two NaNs unspecified (LLVM
/// may swap the operands of `x + y`), so the Rust build's own payload is not
/// stable either.
#[test]
fn fastmath_matches_the_rust_build() {
    use vesper_kinetic::fastmath as port;
    use vesper_kinetic_rust::fastmath as rust;
    let mut rng = Lcg(0x1B04_0004);
    let mut args: Vec<f64> = vec![
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        f64::MIN,
        std::f64::consts::FRAC_PI_4,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
        1_647_099.0,
        -1_647_099.0,
        709.782_712_893_384,
        -745.133_219_101_941_1,
        1.5,
        -1.5,
    ];
    for e in -1074i64..=1023 {
        // 2^e from its bits (powi underflows to 0 for the smallest e).
        let p = f64::from_bits(if e >= -1022 {
            ((e + 1023) as u64) << 52
        } else {
            1 << (e + 1074)
        });
        args.extend([
            p,
            -p,
            f64::from_bits(p.to_bits() + 1),
            f64::from_bits(p.to_bits() - 1),
        ]);
    }
    for _ in 0..20_000 {
        args.push(rng.range(-200.0, 200.0));
        args.push(rng.range(-2.0e6, 2.0e6));
        args.push(f64::from_bits(
            (u64::from(rng.next()) << 33) | u64::from(rng.next()),
        ));
    }
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut check = |what: &str, x: f64, port: f64, rust: f64| {
        cases += 1;
        if port.to_bits() != rust.to_bits() && !(port.is_nan() && rust.is_nan()) {
            differences += 1;
            println!(
                "{what}({x:e} = {:#x}): port {:#x}, rust {:#x}",
                x.to_bits(),
                port.to_bits(),
                rust.to_bits()
            );
        }
    };
    for (i, &x) in args.iter().enumerate() {
        let other = args[(i * 7 + 3) % args.len()];
        check("sin", x, port::sin(x), rust::sin(x));
        check("cos", x, port::cos(x), rust::cos(x));
        check("exp", x, port::exp(x), rust::exp(x));
        check("log", x, port::log(x), rust::log(x));
        check("atan2", x, port::atan2(x, 1.5), rust::atan2(x, 1.5));
        check("atan2", x, port::atan2(x, other), rust::atan2(x, other));
        let (f, g) = (x as f32, other as f32);
        let wide = |v: f32| f64::from(v);
        check("sinf", x, wide(port::sinf(f)), wide(rust::sinf(f)));
        check("cosf", x, wide(port::cosf(f)), wide(rust::cosf(f)));
        check("expf", x, wide(port::expf(f)), wide(rust::expf(f)));
        check("logf", x, wide(port::logf(f)), wide(rust::logf(f)));
        check(
            "atan2f",
            x,
            wide(port::atan2f(f, g)),
            wide(rust::atan2f(f, g)),
        );
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's 30-bit Morton codes (by lookup tables) against the frozen
/// Rust build (magic bits): every in-range axis value through `spread`,
/// `compact` and the masked forms, random and out-of-range coordinates and
/// codes through the checked forms, errors included (SH6 lane 9, #1404).
#[test]
fn morton_matches_the_rust_build() {
    use vesper_kinetic::morton as port;
    use vesper_kinetic_rust::morton as rust;
    let mut rng = Lcg(0x1B04_0005);
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut check = |same: bool| {
        cases += 1;
        if !same {
            differences += 1;
        }
    };
    for v in 0..=u32::from(u16::MAX) {
        check(port::spread(v) == rust::spread(v));
        check(port::compact(v) == rust::compact(v));
    }
    let edges = [0u16, 1, 511, 512, 1022, 1023, 1024, 1025, 2047, u16::MAX];
    for &x in &edges {
        for &y in &edges {
            for &z in &edges {
                check(
                    format!("{:?}", port::encode(x, y, z))
                        == format!("{:?}", rust::encode(x, y, z)),
                );
                check(port::encode_masked(x, y, z) == rust::encode_masked(x, y, z));
            }
        }
    }
    for _ in 0..200_000 {
        let r = rng.next();
        let (x, y, z) = (
            r as u16 & 0x7ff,
            (r >> 11) as u16 & 0x7ff,
            rng.next() as u16,
        );
        check(format!("{:?}", port::encode(x, y, z)) == format!("{:?}", rust::encode(x, y, z)));
        check(port::encode_masked(x, y, z) == rust::encode_masked(x, y, z));
        let code = rng.next() ^ (rng.next() << 1);
        check(format!("{:?}", port::decode(code)) == format!("{:?}", rust::decode(code)));
        check(port::decode_masked(code) == rust::decode_masked(code));
        let valid = code & port::CODE_MASK;
        check(format!("{:?}", port::decode(valid)) == format!("{:?}", rust::decode(valid)));
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's correctly rounded `sin`, `cos`, `exp`, `log` and `atan2`
/// (Vesper bignum, 64-bit limbs) against the frozen Rust build
/// (`num-bigint`), bit for bit in both formats: the Annex F specials and
/// edges, then random inputs over the ranges the reductions and the
/// over/underflow limits split (SH6 lane 9, #1404).
#[test]
fn crmath_matches_the_rust_build() {
    use vesper_kinetic::crmath as port;
    use vesper_kinetic_rust::crmath as rust;
    let mut rng = Lcg(0x1B04_0006);
    let mut differences = 0usize;
    let mut cases = 0usize;
    // Results are correctly rounded, so they agree bit for bit; NaNs are
    // the same canonical NaN, compared as NaN.
    let mut check64 = |a: f64, b: f64| {
        cases += 1;
        if a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()) {
            differences += 1;
        }
    };
    let mut inputs = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        2.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        -f64::MAX,
        std::f64::consts::PI,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::LN_2,
        709.782712893384,
        -745.1332191019411,
        1e300,
        1e-300,
    ];
    for _ in 0..1_500 {
        inputs.push(rng.range(-8.0, 8.0));
        inputs.push(rng.range(-800.0, 800.0));
        inputs.push(rng.range(-1e-3, 1e-3));
        let bits = (u64::from(rng.next()) << 32) | u64::from(rng.next());
        inputs.push(f64::from_bits(bits));
    }
    for &x in &inputs {
        check64(port::sin(x), rust::sin(x));
        check64(port::cos(x), rust::cos(x));
        check64(port::exp(x), rust::exp(x));
        check64(port::log(x), rust::log(x));
        check64(port::log(x.abs()), rust::log(x.abs()));
        check64(port::atan2(x, 1.5), rust::atan2(x, 1.5));
        check64(port::atan2(-0.75, x), rust::atan2(-0.75, x));
        let xf = x as f32;
        let yf = (x * 0.37) as f32;
        check64(f64::from(port::sinf(xf)), f64::from(rust::sinf(xf)));
        check64(f64::from(port::cosf(xf)), f64::from(rust::cosf(xf)));
        check64(f64::from(port::expf(xf)), f64::from(rust::expf(xf)));
        check64(
            f64::from(port::logf(xf.abs())),
            f64::from(rust::logf(xf.abs())),
        );
        check64(
            f64::from(port::atan2f(yf, xf)),
            f64::from(rust::atan2f(yf, xf)),
        );
    }
    for &y in &inputs[..20] {
        for &x in &inputs[..20] {
            check64(port::atan2(y, x), rust::atan2(y, x));
            check64(
                f64::from(port::atan2f(y as f32, x as f32)),
                f64::from(rust::atan2f(y as f32, x as f32)),
            );
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}
