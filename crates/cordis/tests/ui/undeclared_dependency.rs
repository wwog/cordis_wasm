#[cordis::service]
trait Clock {
    async fn now(&self) -> Result<u64, String>;
}

#[cordis::component]
struct UndeclaredDependency;

#[cordis::component_impl]
impl UndeclaredDependency {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: cordis::ComponentContext<UndeclaredDependencyDependencies>,
        _config: (),
    ) -> Result<(), cordis::CordisError> {
        let _ = context.deps().clock.now().await;
        Ok(())
    }
}

fn main() {}
