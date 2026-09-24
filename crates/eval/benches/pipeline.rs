//! Pipeline micro-benchmarks — the hot paths inside one engine step:
//! observation signature, structural verification, candidate
//! generation, target resolution, and a full act/verify cycle on sim.
//! Numbers here isolate engine cost from OS latency (which the eval
//! scenarios report via `observe_ms`/`verify_ms`).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use dexter_core::{
    Element, ElementId, ExpectedState, Observation, ObservationId, SemanticTarget, ValuePredicate,
    Window,
};
use dexter_decision::{CandidateGenerator, HeuristicGenerator};
use dexter_engine::{Engine, RunConfig, Step};
use dexter_policy::Policy;
use dexter_sim::SimDriver;
use std::time::{Duration, SystemTime};

fn element(i: u64) -> Element {
    Element {
        id: ElementId(i),
        parent: if i > 0 { Some(ElementId(i / 4)) } else { None },
        depth: (i % 6) as u32,
        role: Some(
            [
                "button",
                "static_text",
                "text_field",
                "check_box",
                "row",
                "menu_item",
            ][(i % 6) as usize]
                .into(),
        ),
        name: Some(format!("elemento {i}")),
        value: Some(format!("valor {i}")),
        enabled: Some(i % 5 != 0),
        actions: vec!["press".into(), "open".into()],
        bounds: Some(dexter_core::Rect {
            x: (i % 10) as f64 * 80.0,
            y: (i / 10) as f64 * 24.0,
            w: 78.0,
            h: 22.0,
        }),
        ..Default::default()
    }
}

fn observation(n: usize) -> Observation {
    Observation {
        id: ObservationId(1),
        timestamp: SystemTime::now(),
        windows: vec![Window {
            id: 1,
            pid: 1,
            app: "sim".into(),
            title: Some("sim".into()),
            bounds: dexter_core::Rect {
                x: 0.0,
                y: 0.0,
                w: 1200.0,
                h: 800.0,
            },
            on_screen: true,
            layer: 0,
        }],
        elements: (0..n as u64).map(element).collect(),
        ..Default::default()
    }
}

fn bench_signature(c: &mut Criterion) {
    let mut g = c.benchmark_group("signature");
    for n in [100usize, 500, 2000] {
        let obs = observation(n);
        g.bench_with_input(BenchmarkId::from_parameter(n), &obs, |b, obs| {
            b.iter(|| dexter_world_model::signature(obs))
        });
    }
    g.finish();
}

fn bench_verify(c: &mut Criterion) {
    let mut g = c.benchmark_group("verify");
    for n in [100usize, 500, 2000] {
        let obs = observation(n);
        let expect = ExpectedState::ElementValue {
            target: SemanticTarget {
                role: Some("static_text".into()),
                name_contains: Some("elemento".into()),
                ..Default::default()
            },
            predicate: ValuePredicate::Contains("valor".into()),
        };
        g.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(&obs, &expect),
            |b, (o, e)| b.iter(|| dexter_verify::verify(o, e)),
        );
    }
    g.finish();
}

fn bench_generate(c: &mut Criterion) {
    let mut g = c.benchmark_group("generate");
    let gen = HeuristicGenerator::default();
    for n in [100usize, 500, 2000] {
        let obs = observation(n);
        g.bench_with_input(BenchmarkId::from_parameter(n), &obs, |b, obs| {
            b.iter(|| {
                gen.generate(
                    obs,
                    "abrir elemento 42",
                    &dexter_decision::GenHistory::default(),
                )
            })
        });
    }
    g.finish();
}

fn make_world() -> SimDriver {
    let sim = SimDriver::new(
        (0..200u64)
            .map(|i| {
                let mut e = element(i);
                if i == 42 {
                    e.role = Some("button".into());
                    e.name = Some("Guardar".into());
                }
                e
            })
            .collect(),
    );
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        dexter_sim::Effect::Spawn(element(0)),
    );
    sim
}

fn bench_step(c: &mut Criterion) {
    // One full engine step on sim: plan → policy → act → verify-poll.
    // The number that says what a verified act costs end-to-end.
    let cfg = RunConfig::default();
    c.bench_function("step_click_verified_200", |b| {
        b.iter_batched(
            || {
                Engine::new(
                    make_world(),
                    Policy::from_toml("[defaults]\nmutating = \"allow\"").unwrap(),
                    Duration::from_secs(10),
                )
            },
            |mut engine| {
                let step = Step {
                    note: None,
                    action: dexter_core::Action::Click {
                        target: dexter_core::Target::Semantic(SemanticTarget {
                            role: Some("button".into()),
                            name: Some("Guardar".into()),
                            ..Default::default()
                        }),
                        button: dexter_core::MouseButton::Left,
                        count: 1,
                    },
                    expect: Some(ExpectedState::ElementExists {
                        target: SemanticTarget {
                            role: Some("button".into()),
                            ..Default::default()
                        },
                    }),
                    max_attempts: Some(1),
                    app: None,
                };
                engine.run_step(&step, &cfg)
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_signature,
    bench_verify,
    bench_generate,
    bench_step
);
criterion_main!(benches);
