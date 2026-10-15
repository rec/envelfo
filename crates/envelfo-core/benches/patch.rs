//! Run with `cargo bench -p envelfo-core --bench patch --locked`.
//! Counting is isolated from timings and from the library's unsafe-free code.
#![deny(unsafe_op_in_unsafe_fn)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use envelfo_core::envelope::{Envelope, Marker, Segment, Start};
use envelfo_core::lfo::{LFO, Shape};
use envelfo_core::patch::{
    Child, ChildCommand, ChildEvent, Connection, ConnectionAction, Node, Output, Patch, Seconds,
};
use envelfo_core::sample_hold::SampleHold;
use envelfo_core::signal::Operator;
use envelfo_core::stages::{Body, Stage, Stages};
use envelfo_core::time::Time;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
static COUNTING: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
const WARMUP: usize = 256;
const ITERATIONS: usize = 4000;

fn main() {
    println!("workload\tmedian_ns/op\tmin_ns/op\tmax_ns/op\talloc+realloc/op\trequested_bytes/op");
    measure("scalar-lfo", || {
        let body = wave();
        let state = body.initial(Time::ZERO).unwrap();
        let mut tick = 0;
        move || {
            tick += 1;
            black_box(body.value(&state, time(tick)).unwrap());
        }
    });
    for (name, body, command) in [
        ("arpeg-observe", arpeg(), false),
        ("arpeg-start+observe", arpeg(), true),
        ("signals-128", signals(), false),
        ("stages-16", staged(), false),
        ("fanout-128", fanout(1.0), false),
        ("chance-fanout-128", fanout(0.5), false),
    ] {
        measure(name, || playback(body.clone(), command));
        measure(&format!("clone/{name}"), || {
            let mut state = body
                .initial(Time::ZERO, "audit", 81, None, &Seconds)
                .unwrap();
            for tick in 1..=WARMUP {
                body.advance(&mut state, time(tick), time(tick + 1), &Seconds)
                    .unwrap();
            }
            move || {
                black_box(state.clone());
            }
        });
    }
}

fn measure<Work: FnMut()>(name: &str, setup: impl Fn() -> Work) {
    let mut samples = [0; 5];
    for sample in &mut samples {
        let mut operation = setup();
        for _ in 0..WARMUP {
            operation();
        }
        let started = Instant::now();
        for _ in 0..ITERATIONS {
            operation();
        }
        *sample = started.elapsed().as_nanos() / ITERATIONS as u128;
    }
    samples.sort_unstable();
    let mut operation = setup();
    for _ in 0..WARMUP {
        operation();
    }
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    for _ in 0..ITERATIONS {
        operation();
    }
    COUNTING.store(false, Ordering::Relaxed);
    println!(
        "{name}\t{}\t{}\t{}\t{:.2}\t{:.2}",
        samples[2],
        samples[0],
        samples[4],
        CALLS.load(Ordering::Relaxed) as f64 / ITERATIONS as f64,
        BYTES.load(Ordering::Relaxed) as f64 / ITERATIONS as f64
    );
}

fn playback(body: Patch, command: bool) -> impl FnMut() {
    let mut state = body
        .initial(Time::ZERO, "audit", 81, None, &Seconds)
        .unwrap();
    let mut tick = 0;
    move || {
        tick += 1;
        if command {
            black_box(
                body.command(
                    &mut state,
                    "attack",
                    &ChildEvent {
                        at: time(tick),
                        ordinal: 0,
                        command: ChildCommand::Start,
                    },
                    &Seconds,
                    false,
                )
                .unwrap(),
            );
        }
        black_box(
            body.advance(&mut state, time(tick), time(tick + 1), &Seconds)
                .unwrap(),
        );
    }
}

fn time(tick: usize) -> Time {
    Time::new(tick as i64, 1024).unwrap()
}

fn wave() -> LFO {
    let mut body = LFO::new(Time::ONE).unwrap();
    body.shape = Shape::Triangle;
    body
}

fn arpeg() -> Patch {
    let mut envelope = Envelope::new(
        0.0,
        vec![Segment {
            duration: Some(Time::new(1, 4).unwrap()),
            to: 1.0,
            curve: 0.0,
        }],
    )
    .unwrap();
    envelope.start = Start::Event;
    Patch {
        children: vec![
            Child {
                name: "wave".into(),
                node: Node::LFO {
                    body: wave(),
                    markers: vec![],
                },
            },
            Child {
                name: "attack".into(),
                node: Node::Envelope(envelope),
            },
        ],
        outputs: vec![
            Output {
                name: "breath".into(),
                source: "wave".into(),
            },
            Output {
                name: "accent".into(),
                source: "attack".into(),
            },
        ],
        event_outputs: vec![],
        connections: vec![],
    }
}

fn signals() -> Patch {
    let mut children = vec![];
    let mut outputs = vec![];
    for i in 0..64 {
        let name = format!("wave-{i}");
        let output = format!("slew-{i}");
        children.push(Child {
            name: name.clone(),
            node: Node::LFO {
                body: wave(),
                markers: vec![],
            },
        });
        children.push(Child {
            name: output.clone(),
            node: Node::Signal(Operator::Slew {
                input: name,
                rise: 2.0,
                fall: 2.0,
                initial: None,
            }),
        });
        outputs.push(Output {
            name: output.clone(),
            source: output,
        });
    }
    Patch {
        children,
        outputs,
        event_outputs: vec![],
        connections: vec![],
    }
}

fn staged() -> Patch {
    Patch {
        children: (0..16)
            .map(|i| Child {
                name: format!("stage-{i}"),
                node: Node::Stages(Stages {
                    initial_stage: "sustain".into(),
                    stages: vec![Stage {
                        name: "sustain".into(),
                        body: Body::LFO(wave()),
                        markers: vec![Marker {
                            name: "middle".into(),
                            position: Time::new(1, 2).unwrap(),
                        }],
                    }],
                    transitions: vec![],
                }),
            })
            .collect(),
        outputs: (0..16)
            .map(|i| Output {
                name: format!("value-{i}"),
                source: format!("stage-{i}"),
            })
            .collect(),
        event_outputs: vec![],
        connections: vec![],
    }
}

fn fanout(probability: f64) -> Patch {
    let mut body = wave();
    body.rate = Time::from_integer(64);
    let mut children = vec![Child {
        name: "clock".into(),
        node: Node::LFO {
            body,
            markers: vec![Marker {
                name: "tick".into(),
                position: Time::new(1, 4).unwrap(),
            }],
        },
    }];
    let mut connections = vec![];
    for i in 0..128 {
        let name = format!("held-{i}");
        children.push(Child {
            name: name.clone(),
            node: Node::SampleHold(SampleHold {
                minimum: 0.0,
                maximum: 1.0,
            }),
        });
        connections.push(Connection {
            source: envelfo_core::patch::Port {
                child: "clock".into(),
                port: "tick".into(),
            },
            target: name,
            action: ConnectionAction::Sample,
            every: 1,
            offset: 0,
            probability,
            delay: Time::new(1, 16).unwrap(),
        });
    }
    Patch {
        children,
        outputs: vec![Output {
            name: "value".into(),
            source: "held-0".into(),
        }],
        event_outputs: vec![],
        connections,
    }
}

struct CountingAllocator;

// SAFETY: Every operation forwards the original pointer and layout unchanged
// to System; relaxed counters only observe requested allocation sizes.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: The caller supplies the layout required by GlobalAlloc.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: The caller supplies the layout required by GlobalAlloc.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        // SAFETY: The caller supplies a System-owned pointer and its layout.
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The caller supplies a System-owned pointer and its layout.
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn record(size: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }
}
