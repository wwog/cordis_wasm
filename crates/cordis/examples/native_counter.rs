use cordis::{Component, ComponentContext, Context, EffectSet, Runtime, ServiceCallError};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

#[cordis::service(name = "example.counter")]
pub trait Counter {
    async fn add(&self, amount: i64) -> Result<i64, String>;
}

#[derive(Debug, Default)]
struct AtomicCounter {
    value: AtomicI64,
}

impl Counter for AtomicCounter {
    fn add(&self, amount: i64) -> impl Future<Output = Result<i64, String>> + Send {
        let result = self
            .value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(amount)
            })
            .map(|previous| previous + amount)
            .map_err(|_| "counter overflow".to_owned());
        std::future::ready(result)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CounterConfig {
    amount: i64,
}

#[derive(Debug)]
#[cordis::component(name = "counter-consumer", config = CounterConfig)]
#[cordis::inject(Counter)]
pub struct CounterConsumer;

#[cordis::component_impl]
impl CounterConsumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: ComponentContext<CounterConsumerDependencies>,
        config: CounterConfig,
    ) -> Result<(), cordis::CordisError> {
        let value = context
            .deps()
            .counter
            .add(config.amount)
            .await
            .map_err(service_error)?;
        println!("counter value: {value}");
        Ok(())
    }
}

fn service_error(error: ServiceCallError<String>) -> cordis::CordisError {
    match error {
        ServiceCallError::Transport(error) => error,
        ServiceCallError::Service(message) => cordis::CordisError::SupervisorFailed { message },
    }
}

#[tokio::main]
async fn main() -> Result<(), cordis::CordisError> {
    let runtime = Runtime::start();
    let fiber = runtime.handle().create_fiber(None).await?;
    let counter = Arc::new(AtomicCounter::default());
    let client = CounterClient::from_native(counter);
    let context = ComponentContext::new(
        Context::root(fiber),
        CounterConsumerDependencies::new(client),
        EffectSet::new("counter-consumer"),
    );

    let effects = CounterConsumer
        .apply(context, CounterConfig { amount: 3 })
        .await?;
    effects
        .effect_set()
        .dispose()
        .await
        .map_err(|error| cordis::CordisError::DisposerFailed {
            message: error.to_string(),
        })?;
    runtime.shutdown().await?;
    Ok(())
}
