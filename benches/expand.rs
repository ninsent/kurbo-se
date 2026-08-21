// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Expansion benchmarks over representative paths.

#![allow(missing_docs)] // criterion macros generate undocumented items

use criterion::{Criterion, criterion_group, criterion_main};
use kurbo::{BezPath, Circle, Shape};
use kurbo_se::{
    AlignedStrokeCtx, DashStyle, StrokeAlignment, StrokeStyle, stroke_aligned, stroke_aligned_with,
};
use std::hint::black_box;

fn wavy(n: usize) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    for i in 0..n {
        let x = i as f64 * 10.0;
        p.curve_to((x + 3.0, 15.0), (x + 7.0, -15.0), (x + 10.0, 0.0));
    }
    p
}

fn star() -> BezPath {
    let mut p = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { 100.0 } else { 40.0 };
        let a = std::f64::consts::PI * (i as f64) / 5.0;
        let pt = (150.0 + r * a.cos(), 150.0 + r * a.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

fn donut() -> BezPath {
    let mut p = Circle::new((130.0, 130.0), 100.0).to_path(1e-9);
    p.extend(
        Circle::new((130.0, 130.0), 45.0)
            .to_path(1e-9)
            .reverse_subpaths()
            .iter(),
    );
    p
}

fn bench_expand(c: &mut Criterion) {
    let circle = Circle::new((100.0, 100.0), 50.0).to_path(1e-9);
    let wavy40 = wavy(40);
    let star = star();
    let donut = donut();
    let tol = 0.25; // display tolerance

    let solid_inside = StrokeStyle::new(8.0).with_alignment(StrokeAlignment::Inside);
    let solid_center = StrokeStyle::new(8.0);
    let dashed_inside = solid_inside
        .clone()
        .with_dash(DashStyle::new(12.0, 6.0).with_cap(kurbo_se::Cap::Round));

    let mut g = c.benchmark_group("expand");
    g.bench_function("circle/inside/solid", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&circle), &solid_inside, tol)));
    });
    g.bench_function("circle/center/solid", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&circle), &solid_center, tol)));
    });
    g.bench_function("circle/inside/dashed", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&circle), &dashed_inside, tol)));
    });
    g.bench_function("star/inside/solid", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&star), &solid_inside, tol)));
    });
    // Two contours through the region pipeline (hole-aware erosion).
    g.bench_function("donut/inside/solid", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&donut), &solid_inside, tol)));
    });
    g.bench_function("wavy40/inside/solid", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&wavy40), &solid_inside, tol)));
    });
    g.bench_function("wavy40/inside/dashed", |b| {
        b.iter(|| black_box(stroke_aligned(black_box(&wavy40), &dashed_inside, tol)));
    });
    // Allocation-reuse variant: the animated-dash_offset hot path.
    g.bench_function("circle/inside/dashed/ctx-reuse", |b| {
        let mut ctx = AlignedStrokeCtx::new();
        let mut offset = 0.0;
        b.iter(|| {
            offset += 0.7;
            let mut style = dashed_inside.clone();
            if let Some(d) = &mut style.dash {
                d.offset = offset;
            }
            black_box(
                stroke_aligned_with(black_box(&circle), &style, tol, &mut ctx)
                    .elements()
                    .len(),
            )
        });
    });
    // kurbo's native stroker for scale (center + solid only).
    g.bench_function("circle/kurbo-stroke-reference", |b| {
        let kstyle: kurbo::Stroke = (&solid_center).into();
        b.iter(|| {
            black_box(kurbo::stroke(
                black_box(&circle).iter(),
                &kstyle,
                &kurbo::StrokeOpts::default(),
                tol,
            ))
        });
    });
    g.finish();
}

criterion_group!(benches, bench_expand);
criterion_main!(benches);
