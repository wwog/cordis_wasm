use cordis_core::{
    Context, DependencyResolution, DesiredEpoch, DesiredState, FiberId, FiberMachine, RealmId,
    Runtime, ServiceId,
};
use std::hint::black_box;
use std::time::Instant;

const CONTEXT_ITERATIONS: u32 = 1_000_000;
const LIFECYCLE_ITERATIONS: u32 = 250_000;

fn main() {
    let async_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime builds");
    let (runtime, fiber, realm) = async_runtime.block_on(async {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let fiber = handle
            .create_fiber(None)
            .await
            .expect("fiber allocation works");
        let realm = handle
            .allocate_realm()
            .await
            .expect("realm allocation works");
        (runtime, fiber, realm)
    });

    context_resolution(fiber, realm);
    lifecycle_round_trip(fiber);
    async_runtime
        .block_on(runtime.shutdown())
        .expect("benchmark runtime shuts down");
}

fn context_resolution(fiber: FiberId, expected: RealmId) {
    let service = ServiceId::new("benchmark.service", [7; 32]);
    let mut context = Context::root(fiber).isolate(service.clone(), expected);
    for _ in 0..32 {
        context = context.extend(fiber);
    }

    let started = Instant::now();
    for _ in 0..CONTEXT_ITERATIONS {
        assert_eq!(
            black_box(&context).resolve_realm(black_box(&service)),
            Ok(expected)
        );
    }
    report("context_resolve_depth_32", CONTEXT_ITERATIONS, started);
}

fn lifecycle_round_trip(fiber: FiberId) {
    let epoch = DesiredEpoch::from_resolution(&DependencyResolution::default())
        .expect("an empty dependency set is ready");
    let mut machine = FiberMachine::new(fiber);

    let started = Instant::now();
    for _ in 0..LIFECYCLE_ITERATIONS {
        let load = machine
            .set_desired(DesiredState::Ready(epoch.clone()))
            .expect("pending machine starts loading");
        black_box(machine.complete(load.generation, Ok(())));
        let unload = machine
            .set_desired(DesiredState::Waiting)
            .expect("active machine starts unloading");
        black_box(machine.complete(unload.generation, Ok(())));
    }
    report(
        "fiber_load_unload_round_trip",
        LIFECYCLE_ITERATIONS,
        started,
    );
}

fn report(name: &str, iterations: u32, started: Instant) {
    let elapsed = started.elapsed();
    let nanos = elapsed.as_nanos() / u128::from(iterations);
    println!("{name}: {nanos} ns/op ({iterations} iterations, {elapsed:?})");
}
